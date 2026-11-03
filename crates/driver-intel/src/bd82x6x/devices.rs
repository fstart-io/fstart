//! PCH function enable/hide and the per-device ramstage init (coreboot
//! `pch.c`, `pcie.c`, `sata.c`, `usb_ehci.c`, `usb_xhci.c`, `azalia.c`).

use super::regs::FD;
use super::*;
use crate::southbridge::hda::HdaController;

use fstart_pci::{
    PCI_CMD_BUS_MASTER as CMD_MASTER, PCI_CMD_IO as CMD_IO, PCI_CMD_MEMORY as CMD_MEMORY,
    PCI_COMMAND,
};

/// SERR# enable.
const CMD_SERR: u16 = 1 << 8;
const PCI_CAP_ID_PCIE: u8 = 0x10;
const PCI_EXP_FLAGS: u16 = 0x02;
const PCI_EXP_FLAGS_SLOT: u16 = 1 << 8;
const PCI_EXP_LNKCAP: u16 = 0x0c;
const PCI_EXP_LNKCTL: u16 = 0x10;
const PCI_EXP_LNKCTL_LD: u32 = 1 << 4;
const PCI_EXP_SLTCAP: u16 = 0x14;
const PCI_EXP_SLTSTA: u16 = 0x1a;
const PCI_EXP_SLTSTA_PDS: u16 = 1 << 6;
/// RPFN fields of root port `port`.
const fn rpfn_hide(port: u8) -> u32 {
    1 << (port * 4 + 3)
}
const fn rpfn_mask(port: u8) -> u32 {
    7 << (port * 4)
}
const fn rpfn_get(rpfn: u32, port: u8) -> u32 {
    (rpfn >> (port * 4)) & 7
}
const fn rpfn_set(port: u8, func: u32) -> u32 {
    (func & 7) << (port * 4)
}

impl IntelBd82x6x {
    /// coreboot `pch_hide_devfn`.
    fn hide(&self, dev: u8, func: u8) {
        let r = self.rcba();
        match (dev, func) {
            (0x14, 0) if self.pch_type() == PCH_TYPE_PPT => r.fd.modify(FD::XHCI::SET),
            (0x16, 0) => r.fd2.modify(FD2::MEI1::SET),
            (0x16, 1) => r.fd2.modify(FD2::MEI2::SET),
            (0x16, 2) => r.fd2.modify(FD2::IDER::SET),
            (0x16, 3) => r.fd2.modify(FD2::KT::SET),
            (0x1a, 0) => r.fd.modify(FD::EHCI2::SET),
            (0x1b, 0) => r.fd.modify(FD::HDA::SET),
            (0x1c, port) => r.fd.set(r.fd.get() | 1 << (16 + u32::from(port))),
            (0x1d, 0) => r.fd.modify(FD::EHCI1::SET),
            (0x1e, 0) => r.fd.modify(FD::P2P::SET),
            (0x1f, 2) => r.fd.modify(FD::SATA1::SET),
            (0x1f, 3) => r.fd.modify(FD::SMBUS::SET),
            (0x1f, 5) => r.fd.modify(FD::SATA2::SET),
            (0x1f, 6) => r.fd.modify(FD::THERMAL::SET),
            // GbE goes through BUC in early init.
            _ => {}
        }
    }

    /// coreboot `pch_enable` for a function outside the root ports.
    fn enable_function(&self, dev: u8, func: u8, enabled: bool) {
        let pci = EcamDevice::new(0, dev, func);
        if !enabled {
            if pci.is_present() {
                pci.and16(PCI_COMMAND, !(CMD_MASTER | CMD_MEMORY | CMD_IO));
            }
            self.hide(dev, func);
        } else if pci.is_present() {
            pci.or16(PCI_COMMAND, CMD_SERR);
        }
    }

    /// coreboot `check_device_present`: in-band presence through the slot
    /// status; empty fixed ports are parked and disabled.
    fn pcie_check_present(&mut self, port: u8) {
        let pci = EcamDevice::new(0, 0x1c, port);
        let Some(cap) = pci.find_capability(PCI_CAP_ID_PCIE) else {
            return;
        };
        pci.or16(cap + PCI_EXP_FLAGS, PCI_EXP_FLAGS_SLOT);
        let hotplug = self.config.pcie_hotplug[usize::from(port)];
        let present = pci.read16(cap + PCI_EXP_SLTSTA) & PCI_EXP_SLTSTA_PDS != 0;
        fstart_log::debug!(
            "bd82x6x: RP{} {} downstream device",
            port + 1,
            if present { "found a" } else { "no" }
        );
        let enabled = &mut self.pcie_enabled[usize::from(port)];
        if !present && !hotplug {
            pci.or32(0x338, 1 << 26);
            fstart_arch::x86::wait_us(50_000, || pci.read32(0x328) & (0x1f << 23) == 0);
            *enabled = false;
        } else if present && !hotplug && !*enabled {
            pci.or32(cap + PCI_EXP_LNKCTL, PCI_EXP_LNKCTL_LD);
        }
    }

    /// coreboot `pch_pcie_pm_early`.
    fn pcie_pm_early(&self, port: u8, slot_number: &mut u32) {
        let pci = EcamDevice::new(0, 0x1c, port);
        let rpc = self.rcba().rpc.get();
        let width = |config: u32| match config & 3 {
            3 => 4,
            1 | 2 => 2,
            _ => 1,
        };
        let (p0, p4) = (width(rpc), width(rpc >> 2));
        let mut power_limit = 10u32;
        let mut e1 = pci.read8(0xe1);
        match port {
            0 | 4 => {
                let lanes = if port == 0 { p0 } else { p4 };
                power_limit = match lanes {
                    4 => 40,
                    2 => 20,
                    _ => 10,
                };
                e1 |= 0x3f;
            }
            1 if p0 == 1 => e1 |= 3,
            2 | 3 if p0 <= 2 => e1 |= 3,
            5 if p4 == 1 => e1 |= 3,
            6 | 7 if p4 <= 2 => e1 |= 3,
            _ => {}
        }
        pci.write8(0xe1, e1);
        pci.or32(0xe8, 1);
        // Native PME: no PME# SCI.
        pci.modify32(0xd8, !(1 << 17 | 1 << 31), 1 << 16 | 1 << 15);
        let Some(cap) = pci.find_capability(PCI_CAP_ID_PCIE) else {
            return;
        };
        let l1_exit = if self.rcba().cir9.get() & 1 << 16 != 0 {
            4
        } else {
            2
        };
        pci.modify32(cap + PCI_EXP_LNKCAP, !(7 << 15), l1_exit << 15);
        let hotplug = self.config.pcie_hotplug[usize::from(port)];
        let mut sltcap = pci.read32(cap + PCI_EXP_SLTCAP);
        // HPS | HPC.
        if hotplug {
            sltcap |= 1 << 5 | 1 << 6;
        } else {
            sltcap &= !(1 << 5 | 1 << 6);
        }
        // Slot power scale/limit and physical slot number.
        sltcap &= !(3 << 15 | 0xff << 7 | 0x1fff << 19);
        if !self.config.pcie_onboard[usize::from(port)] {
            sltcap |= power_limit << 7 | *slot_number << 19;
            *slot_number += 1;
        }
        pci.write32(cap + PCI_EXP_SLTCAP, sltcap);
    }

    /// coreboot `pch_pcie_enable` over all root ports (devicetree order):
    /// presence, power saving for disabled sets, coalescing and RPFN.
    fn enable_root_ports(&mut self) {
        let ports = self.root_ports();
        let mut rpfn = self.rcba().rpfn.get();
        let mut coalesce = self.config.pcie_port_coalesce;
        let mut slot_number = 1;
        for port in 0..8u8 {
            let pci = EcamDevice::new(0, 0x1c, port);
            if port >= ports || !pci.is_present() {
                self.pcie_enabled[usize::from(port)] = false;
                continue;
            }
            self.pcie_check_present(port);
            let enabled = self.pcie_enabled[usize::from(port)];
            if port == 0 && !enabled {
                coalesce = true;
            }
            if !enabled {
                if port == 0 || port == 4 {
                    let set = usize::from(port)..usize::from(port) + 4;
                    if self.silicon_supported(PCH_TYPE_CPT, PCH_STEP_B1)
                        && !self.pcie_enabled[set].iter().any(|on| *on)
                    {
                        pci.or8(0xe2, 1);
                    }
                    // Clock gating of the shared resources.
                    pci.write8(0xe1, 0x3c);
                }
                pci.and16(PCI_COMMAND, !(CMD_MASTER | CMD_MEMORY | CMD_IO));
                rpfn |= rpfn_hide(port);
                self.hide(0x1c, port);
            } else {
                // coreboot `pch_pcie_pm_early` runs from the port's enable.
                self.pcie_pm_early(port, &mut slot_number);
                if coalesce && let Some(lower) = (0..port).find(|fn_| rpfn & rpfn_hide(*fn_) != 0) {
                    fstart_log::debug!("bd82x6x: remap PCIe function {} to {}", port, lower);
                    let old = rpfn;
                    rpfn &= !(rpfn_mask(port) | rpfn_mask(lower));
                    rpfn |= rpfn_set(port, rpfn_get(old, lower));
                    rpfn |= rpfn_set(lower, rpfn_get(old, port));
                    self.pcie_hotplug
                        .swap(usize::from(port), usize::from(lower));
                }
                pci.or16(PCI_COMMAND, CMD_SERR);
            }
        }
        if coalesce {
            fstart_log::info!("bd82x6x: PCIe root port coalescing is enabled");
        }
        self.rcba().rpfn.set(rpfn);
    }

    /// coreboot chip `enable_dev` over the PCH functions, before
    /// enumeration: function disable, root ports, SATA port map, the PCH
    /// display BDF, and the EHCI subsystem-ID unlock.
    pub(super) fn enable_devices(&mut self) {
        // pch_lpc_enable: PCH display port.
        let r = self.rcba();
        r.dispbdf.set(0x0010);
        r.fd2.modify(FD2::DBDF::SET);
        let enable = |function: PchFunction| {
            let (dev, func) = fstart_pci::devfn_parts(function.devfn());
            self.enable_function(dev, func, self.config.enabled(function));
        };
        for function in [
            PchFunction::Xhci,
            PchFunction::Mei1,
            PchFunction::Mei2,
            PchFunction::Ider,
            PchFunction::Kt,
            PchFunction::Gbe,
            PchFunction::Ehci2,
            PchFunction::Hda,
        ] {
            enable(function);
        }
        self.enable_root_ports();
        let enable = |function: PchFunction| {
            let (dev, func) = fstart_pci::devfn_parts(function.devfn());
            self.enable_function(dev, func, self.config.enabled(function));
        };
        enable(PchFunction::Ehci1);
        enable(PchFunction::PciBridge);
        self.enable_function(0x1f, 0, true);
        for function in [
            PchFunction::Sata1,
            PchFunction::Smbus,
            PchFunction::Sata2,
            PchFunction::Thermal,
        ] {
            enable(function);
        }
        if let Some(sata) = self.sata() {
            // sata_enable: port map and AHCI select.
            let mut map = u16::from(sata.ports ^ 0x3f) << 8;
            if sata.mode == SataMode::Ahci {
                map |= 0x60;
            }
            EcamDevice::from_devfn(0, dev::SATA1).write16(0x90, map);
        }
        // The EHCI subsystem IDs are written by the generic PCI pass with
        // the access-control write enable (coreboot `usb_ehci_set_subsystem`).
        for dev in [0x1d, 0x1a] {
            let ehci = EcamDevice::new(0, dev, 0);
            if ehci.is_present() {
                ehci.or8(0x80, 1);
            }
        }
    }

    /// coreboot PCIe `pci_init` and `pch_pcie_pm_late` per enabled port.
    pub(super) fn pcie_init(&self) {
        for func in 0..8u8 {
            let pci = EcamDevice::new(0, 0x1c, func);
            if !pci.is_present() {
                continue;
            }
            let hotplug = self.pcie_hotplug[usize::from(func)];
            pci.or16(PCI_COMMAND, CMD_MASTER);
            pci.write8(0x0c, 0x10);
            // Bridge control: no parity error response.
            pci.and16(0x3e, !1);
            pci.write16(0x06, pci.read16(0x06));
            pci.write16(0x1e, pci.read16(0x1e));
            if hotplug {
                pci.or32(0xd8, 1 << 30);
            }
            // pch_pcie_pm_late.
            pci.write32(0x314, 0x743a_361b);
            pci.modify32(0x318, 0x0000_ffff, 0x1414_0000);
            pci.or32(0x324, 1 << 5);
            pci.modify32(0x330, !0xff, 0x40);
            pci.modify32(0x33c, 0xff00_0000, 0x0085_4c74);
            pci.or32(0xd4, 1 << 1);
            // The ASPM override follows the port's physical number.
            let physical = (0..8u8)
                .find(|port| rpfn_get(self.rcba().rpfn.get(), *port) == u32::from(func))
                .unwrap_or(func);
            let mut aspm = u32::from(self.config.pcie_aspm[usize::from(physical)]);
            if aspm != 0 {
                pci.or32(0xd4, aspm << 2 | 1 << 4);
            } else {
                aspm = pci.read32(0x50) & 3;
            }
            if aspm == 3 {
                pci.or32(0xe8, 1 << 1);
            }
        }
    }

    /// coreboot `sata_init`.
    pub(super) fn sata_init(&self, sata: &SataConfig) {
        let pci = EcamDevice::from_devfn(0, dev::SATA1);
        if !pci.is_present() {
            return;
        }
        const IDE_DECODE_ENABLE: u16 = 1 << 15;
        let map = u16::from(sata.ports);
        pci.write16(PCI_COMMAND, CMD_MASTER | CMD_MEMORY | CMD_IO);
        if sata.mode == SataMode::Ahci {
            pci.write16(0x40, IDE_DECODE_ENABLE);
            pci.write16(0x42, IDE_DECODE_ENABLE);
            pci.write16(0x92, (pci.read16(0x92) & !0x3f) | 0x8000 | map);
            pci.write32(0x94, u32::from(sata.ports ^ 0x3f) << 24 | 0x183);
            match pci.memory_bar(0x24) {
                Some(abar) if abar != 0 => ahci_init(abar as usize, sata),
                _ => fstart_log::error!("bd82x6x: SATA ABAR is unassigned"),
            }
        } else {
            // coreboot's "Compatible" mode: native IDE on both channels.
            pci.and16(PCI_COMMAND, !CMD_MEMORY);
            pci.or8(0x09, 0x05);
            pci.write16(0x40, IDE_DECODE_ENABLE);
            pci.write16(0x42, IDE_DECODE_ENABLE);
            pci.write16(0x92, (pci.read16(0x92) & !0x3f) | 0x8000 | map);
            pci.write32(0x94, u32::from(sata.ports ^ 0x3f) << 24 | 0x183);
        }
        if sata.port0_gen3_tx != 0 {
            self.iobp_update(SATA_IOBP_SP0G3IR, 0, sata.port0_gen3_tx);
        }
        if sata.port1_gen3_tx != 0 {
            self.iobp_update(SATA_IOBP_SP1G3IR, 0, sata.port1_gen3_tx);
        }
        let sir_read = |index: u32| {
            pci.write32(0xa0, index);
            pci.read32(0xa4)
        };
        let sir_write = |index: u32, value: u32| {
            pci.write32(0xa0, index);
            pci.write32(0xa4, value);
        };
        sir_write(0x04, 0x0000_1600);
        sir_write(0x28, 0xa000_0033);
        sir_write(0x54, (sir_read(0x54) & 0xff00_0000) | 0x0055_55aa);
        sir_write(0x64, 0xcccc_8484);
        sir_write(0x68, (sir_read(0x68) & 0xffff_0000) | 0xcccc);
        sir_write(0x78, (sir_read(0x78) & 0x0000_ffff) | 0x8888_0000);
        sir_write(0x84, 0x001c_7000);
        sir_write(0x88, 0x8833_8822);
        sir_write(0xa0, 0x001c_7000);
        sir_write(0xc4, 0x0c0c_0c0c);
        sir_write(0xc8, 0x0c0c_0c0c);
        sir_write(0xd4, 0x1000_0000);
        self.iobp_update(0xea00_4001, 0x3fff_ffff, 0xc000_0000);
        self.iobp_update(0xea00_408a, 0xffff_fcff, 0x0000_0100);
        pci.modify32(
            0x98,
            !(1 << 16 | 0x3f << 7 | 3 << 5 | 3 << 3),
            1 << 24 | 1 << 22 | 1 << 20 | 1 << 19 | 1 << 18 | 1 << 14 | 0x04 << 7 | 1 << 3,
        );
        self.iobp_update(0xea00_7f07, !0, 1 << 31);
        self.iobp_update(0xea00_4000, !0, 1 << 7);
    }

    /// coreboot `usb_ehci_init` (native raminit) and `usb_xhci_init`.
    pub(super) fn usb_init(&self) {
        let r = self.rcba();
        for dev in [0x1d, 0x1a] {
            let pci = EcamDevice::new(0, dev, 0);
            if !pci.is_present() {
                continue;
            }
            r.rmhwkctl.set(r.rmhwkctl.get() | 0x22);
            pci.modify32(0x84, 0x600, 0x1004_0010);
            pci.or32(0x88, 0x2008_0d20);
            pci.or32(0xf4, 1 << 31);
            pci.modify32(0xf4, !(1 << 22), 1 << 23);
            pci.and32(0xf4, !(1 << 31));
            pci.or32(0xfc, 1 << 17);
            pci.or16(PCI_COMMAND, CMD_MASTER);
            // With the access-control write enable set in enable_devices:
            // the HCSPARAMS port routing rules.
            if let Some(bar) = pci.memory_bar(0x10).filter(|bar| *bar != 0) {
                // SAFETY: BAR0 is the EHCI register window enumeration
                // assigned; HCSPARAMS is the dword at offset 4.
                let hcsparams = unsafe {
                    &*((bar as usize + 4) as *const fstart_core::mmio::MmioReadWrite<u32>)
                };
                hcsparams.set((hcsparams.get() & 0xfff0_0000) | 3);
            }
            pci.and8(0x80, !1);
        }
        let xhci = EcamDevice::from_devfn(0, dev::XHCI);
        if self.config.enabled(PchFunction::Xhci) && xhci.is_present() {
            xhci.write32(0xc0, r.usbocm1.get() & 0x0f0f_0f0f);
            xhci.or32(0x44, 1);
            xhci.write32(0xd4, self.config.xhci_switchable_ports);
            xhci.write32(0xdc, self.config.superspeed_capable_ports);
            xhci.modify32(
                0x40,
                !(1 << 20 | 1 << 21),
                1 << 19 | 1 << 18 | 1 << 17 | 1 << 10 | 1 << 9 | 1 << 8 | 1 << 31,
            );
        }
    }

    /// coreboot `azalia_init`.
    pub(super) fn hda_init(&self) {
        let pci = EcamDevice::new(0, 0x1b, 0);
        if !pci.is_present() {
            return;
        }
        let Some(base) = pci.memory_bar(0x10).filter(|bar| *bar != 0) else {
            fstart_log::error!("bd82x6x: HDA BAR0 is unassigned");
            return;
        };
        let r = self.rcba();
        let v1ctl = r.v1ctl.get();
        if v1ctl & 1 << 31 != 0 {
            pci.modify32(0x120, 0xf8ff_ff01, 1 << 24 | (v1ctl & 0xfe));
            pci.or16(0x78, 1 << 11);
        }
        pci.and32(0x114, !0xfe);
        pci.or32(0x120, 1 << 31);
        // HDMI codec.
        pci.or32(0xc4, 1 << 1);
        pci.or8(0x43, 1 << 6);
        pci.or32(0xc4, 1 << 13);
        pci.or32(0xc4, 1 << 10);
        pci.and32(0xd0, !(1 << 31));
        if pci.device_id() == 0x1e20 {
            pci.or32(0xc4, 1 << 17);
        }
        pci.or16(PCI_COMMAND, CMD_MASTER);
        pci.write8(0x3c, 0x0a);
        let controller = HdaController::new(base as usize);
        let _ = controller.exit_reset();
        fstart_arch::udelay(1000);
        // Azalia mode; no docking.
        pci.or8(0x40, 1);
        pci.and8(0x4d, !(1 << 7));
        let codecs = controller.detect_codecs();
        if codecs != 0 {
            if let Some(hda) = self.config.hda.as_ref() {
                let programmed = controller.program_verb_tables(hda, codecs);
                fstart_log::info!(
                    "bd82x6x: HDA codec mask {:#x}, {} verb tables",
                    codecs,
                    programmed
                );
            }
        } else {
            let _ = controller.enter_reset();
        }
        pci.and8_or8(0x43, !0x07, 1 << 2 | 1);
    }

    /// coreboot `pch_pirq_init` tail: every enabled function with an
    /// interrupt pin reports IRQ 11.
    pub(super) fn write_interrupt_lines(&self) {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let pci = EcamDevice::new(0, dev, func);
                if !pci.is_present() {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                if pci.read8(0x3d) != 0 {
                    pci.write8(0x3c, 11);
                }
                if func == 0 && pci.read8(0x0e) & 0x80 == 0 {
                    break;
                }
            }
        }
    }
}

/// The AHCI half of `sata_init`.
fn ahci_init(abar: usize, sata: &SataConfig) {
    use crate::southbridge::ahci::{AhciRegs, CAP, GHC, PXCMD};
    // SAFETY: ABAR is the controller's assigned and decoded MMIO window.
    let hba = unsafe { &*(abar as *const AhciRegs) };
    hba.ghc.modify(GHC::AE::SET);
    let mut cap = hba.cap.extract();
    cap.modify(
        CAP::PSC::SET
            + CAP::SSC::SET
            + CAP::SALP::SET
            + CAP::SSS::SET
            + CAP::SXS::CLEAR
            + CAP::EMS::CLEAR
            + CAP::PMS::CLEAR,
    );
    if sata.interface_speed != 0 {
        cap.modify(CAP::ISS.val(u32::from(sata.interface_speed) & 3));
    }
    hba.cap.set(cap.get());
    hba.pi.set(u32::from(sata.ports));
    // Read back twice so the write-once value latches.
    let _ = hba.pi.get();
    let _ = hba.pi.get();
    hba.cap2.set(hba.cap2.get() & !2);
    hba.vsp.set(hba.vsp.get() & !5);
    for (index, port) in hba.ports.iter().enumerate() {
        let mut cmd = port.cmd.extract();
        if sata.hotplug_ports & 1 << index != 0 {
            cmd.modify(PXCMD::HPCP::SET);
        }
        port.cmd.set(cmd.get());
    }
}
