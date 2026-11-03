//! Mainstage PCH device init: coreboot `ibexpeak/{lpc,sata,azalia,usb_ehci,
//! smbus,thermal}.c`, `bd82x6x/pci.c` and the common finalize/SPI lockdown.

use super::regs::{FDSW, GCS, GEN_PMCON_1, GEN_PMCON_LOCK, PRSTS, SERIRQ_CNTL, TCLOCKDN};
use super::{IntelIbexPeak, SataConfig, SataMode};
use crate::MmioBar;
use crate::southbridge::hda::{HdaConfig, HdaController};
use crate::southbridge::lpc::BIOS_CONTROL;
use crate::southbridge::pmio_ich as pmio;
use fstart_pci::ecam::EcamDevice;
use fstart_pci::{PCI_COMMAND_BITS, PciType0Config, PciType1Config};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// The PIRQ every PCH interrupt is routed to in PIC mode (coreboot
/// `pch_pirq_init()`: IRQ11 is never used by a legacy device).
const PIRQ_PIC_IRQ: u8 = 11;

/// coreboot `mobile5_pm_init()` tables: (RCBA offset, values), each value
/// written and read back.
const MOBILE5_PM: &[(u32, &[u32])] = &[
    (0x1d44, &[0x0000_0000]),
    (0x1d48, &[0x0003_0000]),
    (
        0x2010,
        &[
            0x0018_8200,
            0x1400_0016,
            0xbc4a_bcb5,
            0x0000_0000,
            0xf0c9_605b,
            0x1368_3040,
            0x04c8_f16e,
            0x09e9_0170,
        ],
    ),
    (0x2100, &[0x0000_0000, 0x0000_0757, 0x0017_0001]),
    (0x211c, &[0x0000_0000, 0x0001_0000]),
    (
        0x21fc,
        &[0x0000_0000, 0x2000_0044, 0x0000_0001, 0x0000_3457],
    ),
    (
        0x2210,
        &[
            0x0000_0000,
            0x0000_0001,
            0xa0ff_f210,
            0x0000_df00,
            0x00e3_0880,
            0x0000_0070,
            0x0000_4000,
            0x0000_0000,
            0x00e3_0880,
            0x0000_0070,
            0x0000_4000,
            0x0000_0000,
            0x0000_2301,
            0x3600_0000,
            0x0001_0107,
            0x0016_0000,
            0x0000_1b01,
            0x3600_0000,
            0x0001_0107,
            0x0016_0000,
            0x0000_0601,
            0x1600_0000,
            0x0001_0107,
            0x0016_0000,
            0x0000_1c01,
            0x1600_0000,
            0x0001_0107,
            0x0016_0000,
        ],
    ),
    (
        0x2300,
        &[
            0x0000_0000,
            0x4000_0000,
            0x4646_827b,
            0x6e80_3131,
            0x32c7_7887,
            0x0007_7733,
            0x0000_7447,
            0x0000_0040,
            0xcccc_0cfc,
            0x0fbb_0fff,
        ],
    ),
    (0x37fc, &[0x0000_0000]),
    (0x3dfc, &[0x0000_0000]),
    (0x3e7c, &[0xffff_ffff]),
    (0x3efc, &[0x0000_0000]),
    (0x3f00, &[0x0000_010b]),
];

/// Run `f` with the EHCI's write-protected registers (subsystem ID,
/// HCSPARAMS) unlocked through ACCESS_CNTL, restoring the protection after.
pub(super) fn with_ehci_unlocked<T>(ehci: EcamDevice, f: impl FnOnce() -> T) -> T {
    const ACCESS_CNTL: u16 = 0x80;
    let access = ehci.read8(ACCESS_CNTL);
    ehci.write8(ACCESS_CNTL, access | 1);
    let result = f();
    ehci.write8(ACCESS_CNTL, access);
    result
}

/// An assigned memory BAR of a PCH function.
#[derive(Clone, Copy)]
struct Bar(usize);
impl MmioBar for Bar {
    fn base(self) -> usize {
        self.0
    }
}

impl IntelIbexPeak {
    /// coreboot `lpc_init()`, without `pch_pirq_init()` (run once every
    /// function is initialized) and `pch_set_acpi_mode()` (the SMM
    /// installer owns `SCI_EN`).
    pub(super) fn lpc_init(&self) {
        self.enable_ioapic();
        self.enable_serial_irqs();
        self.power_options();
        self.mobile5_pm_init();
        // Like coreboot's cmos_init(), leave the clock the OS owns on resume.
        if !self.resume {
            self.rtc_init();
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fstart_arch::x86::legacy_pc::initialize_isa_dma();
        self.enable_hpet();
        self.enable_clock_gating();
        self.setup_i8259();
        self.fixups();
    }

    /// coreboot `pch_enable_ioapic()`.
    fn enable_ioapic(&self) {
        // SAFETY: the PCH I/O APIC is decoded at its fixed base (OIC.AEN).
        let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(super::IOAPIC_BASE) };
        ioapic.lock_redirection_entries();
        // ioapic_setup_gsi0(): ID 0, every entry masked, i8259 virtual wire.
        ioapic.set_id(0);
        ioapic.mask_all();
        ioapic.route_i8259(fstart_arch::x86::lapic::Lapic::from_msr().id() as u8);
    }

    /// coreboot `pch_enable_serial_irqs()`: 21-frame packets, one
    /// continuous frame, then quiet mode.
    fn enable_serial_irqs(&self) {
        let lpc = self.lpc_regs();
        let quiet = SERIRQ_CNTL::ENABLE::SET + SERIRQ_CNTL::FRAME_SIZE.val(21 - 17);
        lpc.serirq_cntl.write(quiet + SERIRQ_CNTL::CONTINUOUS::SET);
        lpc.serirq_cntl.write(quiet);
    }

    /// coreboot `pch_power_options()`.
    fn power_options(&self) {
        use super::regs::GEN_PMCON_3 as P3;
        let lpc = self.lpc_regs();
        let mut pmcon3 = lpc.gen_pmcon_3.extract();
        pmcon3.modify(P3::AFTER_G3.val(u16::from(self.config.power_on_after_fail == 0)));
        // SLP_S4# assertion stretch 4 s, SLP_S3# 50 ms, no stretch after SUS.
        pmcon3.modify(
            P3::SLP_S4_MIN.val(0)
                + P3::SLP_S4_STRETCH_EN::SET
                + P3::SLP_S3_MIN.val(2)
                + P3::SLP_STRETCH_SUS_DIS::SET,
        );
        // RTC_BATTERY_DEAD and PWR_FLR are W1C: write them back as zero.
        pmcon3.modify(P3::RTC_BATTERY_DEAD::CLEAR + P3::PWR_FLR::CLEAR);
        lpc.gen_pmcon_3.set(pmcon3.get());
        fstart_log::info!(
            "ibexpeak: power {} after power failure",
            match self.config.power_on_after_fail {
                0 => "off",
                1 => "on",
                _ => "state keep",
            }
        );

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        // SAFETY: the legacy NMI control ports of the PCH.
        unsafe {
            let mut port61 = fstart_core::pio::inb(0x61) & 0x0f;
            port61 &= !(1 << 3); // IOCHK# NMI enable
            port61 |= 1 << 2; // PCI SERR# NMI disabled for now
            fstart_core::pio::outb(0x61, port61);
            // Port 0x70 is write-only on this PCH; 0x74 is its readable alias.
            let mut nmi = fstart_core::pio::inb(0x74);
            if self.config.nmi {
                nmi &= !(1 << 7);
            } else {
                nmi |= 1 << 7;
            }
            fstart_core::pio::outb(0x70, nmi);
            fstart_log::info!(
                "ibexpeak: NMI sources {}",
                if self.config.nmi {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }

        // CPU_SLP# and SpeedStep stay, SMI# rate 1 minute, native PME.
        lpc.gen_pmcon_1
            .modify(GEN_PMCON_1::SMI_RATE.val(0) + GEN_PMCON_1::BIOS_PCI_EXP_EN::CLEAR);
        lpc.gpio_rout
            .set(crate::southbridge::gpio_ich::encode_gpi_routes(
                &self.config.gpi_routing,
            ));
        let pm = self.pm();
        pm.write32(super::regs::pm::GPE0_EN, self.config.gpe0_en);
        pm.write16(pmio::ALT_GP_SMI_EN, self.config.alt_gp_smi_en);
        // SLP_TYP clear, SCI_EN set; the SMM installer then sets the ACPI
        // mode the boot path needs.
        let pm1 = (pm.read32(pmio::PM1_CNT) & !pmio::SLP_TYP_MASK) | pmio::SCI_EN;
        pm.write32(pmio::PM1_CNT, pm1);
        // Clear the ME wake status bits that would cause an unexpected wake.
        self.rcba()
            .prsts
            .modify(PRSTS::ME_HRST_COLD::SET + PRSTS::ME_HOST_POWERDOWN::SET + PRSTS::ME_WAKE::SET);
        // coreboot clears this undocumented nibble too.
        let raw = self.rcba_raw();
        raw.write32(0x3f02, raw.read32(0x3f02) & !0xf);
    }

    /// coreboot `mobile5_pm_init()`.
    fn mobile5_pm_init(&self) {
        self.lpc_regs().cir4.set(0x47);
        let raw = self.rcba_raw();
        for (base, values) in MOBILE5_PM {
            for (index, value) in values.iter().enumerate() {
                let offset = base + 4 * index as u32;
                raw.write32(offset, *value);
                let _ = raw.read32(offset);
            }
        }
    }

    /// coreboot `pch_rtc_init()`.
    fn rtc_init(&self) {
        let lpc = self.lpc_regs();
        let failed = lpc
            .gen_pmcon_3
            .is_set(super::regs::GEN_PMCON_3::RTC_BATTERY_DEAD);
        if failed {
            lpc.gen_pmcon_3
                .modify(super::regs::GEN_PMCON_3::RTC_BATTERY_DEAD::SET);
        }
        crate::southbridge::rtc::init_clock("ibexpeak", failed);
    }

    /// coreboot `enable_clock_gating()`.
    fn enable_clock_gating(&self) {
        let raw = self.rcba_raw();
        raw.setbits32(0x2234, 0xf);
        self.lpc_regs()
            .gen_pmcon_1
            .modify(GEN_PMCON_1::CLKRUN_EN::SET + GEN_PMCON_1::PWRBTN_LVL::SET);
        let rcba = self.rcba();
        let mut cg = rcba.cg.get();
        cg |= (1 << 31) | (1 << 29) | (1 << 28);
        cg |= (1 << 27) | (1 << 26) | (1 << 25) | (1 << 24);
        cg |= (1 << 16) | (1 << 17) | (1 << 18) | (1 << 22) | (1 << 23);
        cg &= !(1 << 20);
        cg |= 1 << 19;
        cg |= 1 | (0xf << 1);
        rcba.cg.set(cg);
        raw.setbits32(0x38c0, 0x7);
        raw.setbits32(0x36d4, 0x6680_c004);
        raw.setbits32(0x3564, 0x3);
    }

    /// coreboot `setup_i8259()` and the IRQ9 (SCI) level trigger.
    fn setup_i8259(&self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        // SAFETY: legacy PIC ports, serialized ramstage init.
        unsafe {
            fstart_arch::x86::legacy_pc::initialize_pic(0x20, 0x28);
            fstart_core::pio::outb(0x21, 0xfb);
            fstart_core::pio::outb(0xa1, 0xff);
            let elcr2 = fstart_core::pio::inb(0x4d1);
            fstart_core::pio::outb(0x4d1, elcr2 | (1 << 1));
        }
    }

    /// coreboot `pch_fixups()`: DMI ASPM in the PCH.
    fn fixups(&self) {
        let raw = self.rcba_raw();
        raw.clrbits32(0x2304, 1 << 10);
        raw.setbits32(0x21a4, (1 << 11) | (1 << 10));
        raw.setbits32(0x21a8, 0x3);
    }

    /// coreboot `pch_pirq_init()`: every PIRQ to IRQ11 in PIC mode, and
    /// every enabled function's INTERRUPT_LINE to match.
    pub(super) fn pirq_init(&self) {
        let lpc = self.lpc_regs();
        for route in lpc.pirq_a_d.iter().chain(lpc.pirq_e_h.iter()) {
            route.set(PIRQ_PIC_IRQ);
        }
        let functions =
            |bus: u8| (0u8..32).flat_map(move |dev| (0u8..8).map(move |func| (bus, dev, func)));
        let present = |(bus, dev, func): (u8, u8, u8)| {
            let pci = EcamDevice::new(bus, dev, func);
            pci.is_present().then_some(pci)
        };
        let last_bus = functions(0)
            .filter_map(present)
            .filter(|pci| pci.read8(0x0e) & 0x7f == 1)
            .map(|bridge| bridge.read8(0x1a))
            .max()
            .unwrap_or(0);
        for pci in (0..=last_bus).flat_map(functions).filter_map(present) {
            if pci.read8(0x3d) != 0 {
                pci.write8(0x3c, PIRQ_PIC_IRQ);
            }
        }
    }

    /// coreboot `usb_ehci_init()`.
    pub(super) fn ehci_init(&self, (dev, func): (u8, u8)) {
        let ehci = EcamDevice::new(0, dev, func);
        if !ehci.is_present() {
            return;
        }
        // Disable wake on disconnect in the rate-matching hub.
        let rcba = self.rcba();
        rcba.rmhwkctl.set(rcba.rmhwkctl.get() | 0x22);
        ehci.write32(0x84, 0x130c_8911);
        ehci.write32(0x88, 0xa0);
        ehci.write32(0xf4, 0x8080_8588);
        ehci.write32(0xf4, 0x0080_8588);
        ehci.write32(0xf4, 0x0080_8588);
        ehci.write32(0xfc, 0x301b_1728);
        // SAFETY: a present type-0 PCH function.
        let header = unsafe { ehci.regs::<PciType0Config>() };
        header.command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        with_ehci_unlocked(ehci, || {
            if let Some(bar) = ehci.memory_bar(0x10) {
                // HCSPARAMS: port count and companion controllers.
                let bar = Bar(bar as usize);
                bar.write32(4, (bar.read32(4) & 0xfff0_0000) | 2);
            }
        });
    }

    /// coreboot `azalia_init()`.
    pub(super) fn hda_init(&self, config: &HdaConfig) {
        let (dev, func) = super::regs::dev::HDA;
        let hda = EcamDevice::new(0, dev, func);
        if !hda.is_present() {
            return;
        }
        let Some(base) = hda.memory_bar(0x10) else {
            fstart_log::error!("ibexpeak: HDA BAR0 unassigned");
            return;
        };
        let raw = self.rcba_raw();
        // DMI VC1, as the PCH virtual channel was negotiated.
        let v1ctl = raw.read32(0x2030);
        if v1ctl & (1 << 31) != 0 {
            let vc1 = (hda.read32(0x120) & 0xf8ff_ff01) | (1 << 24) | (v1ctl & 0xfe);
            hda.write32(0x120, vc1);
            hda.or16(0x78, 1 << 11);
        } else {
            fstart_log::info!("ibexpeak: HDA V1CTL disabled");
        }
        hda.and32(0x114, !0xfe);
        hda.or32(0x120, 1 << 31);
        // Enable the HDMI codec.
        hda.or32(0xc4, 1 << 1);
        hda.or8(0x43, 1 << 6);
        hda.and32(0xd0, !(1 << 31));
        // SAFETY: a present type-0 PCH function.
        let header = unsafe { hda.regs::<PciType0Config>() };
        header.command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        hda.write8(0x3c, 0x0a);
        let controller = HdaController::new(base as usize);
        if !controller.exit_reset() {
            fstart_log::error!("ibexpeak: HDA did not leave reset");
        }
        fstart_arch::x86::udelay(1000);
        // Azalia mode; docking not supported.
        hda.or8(0x40, 1);
        hda.and8(0x4d, !(1 << 7));
        let codecs = controller.detect_codecs() & 0x0f;
        if codecs != 0 {
            let programmed = controller.program_verb_tables(config, codecs);
            fstart_log::info!(
                "ibexpeak: HDA codec mask {:#x}, {} verb tables",
                u32::from(codecs),
                u32::from(programmed)
            );
        } else {
            fstart_log::info!("ibexpeak: HDA has no codec");
        }
        // Dynamic clock gating.
        hda.and8_or8(0x43, !0x07, (1 << 2) | 1);
    }

    /// coreboot `pci_init()` of the PCI-to-PCI bridge (00:1e.0).
    pub(super) fn pci_bridge_init(&self) {
        let (dev, func) = super::regs::dev::PCI_BRIDGE;
        let bridge = EcamDevice::new(0, dev, func);
        if !bridge.is_present() {
            return;
        }
        // SAFETY: a present type-1 PCH function.
        let header = unsafe { bridge.regs::<PciType1Config>() };
        header.command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        // This device has no interrupt.
        header.interrupt_line.set(0xff);
        // No parity error response or SERR forwarding.
        header
            .bridge_control
            .set(header.bridge_control.get() & !0x3);
        // Master latency count must be 4.
        bridge.and8_or8(0x1b, 0x07, 0x04 << 3);
        header.status_raw.set(header.status_raw.get());
        header
            .secondary_status_raw
            .set(header.secondary_status_raw.get());
    }

    /// coreboot `sata_enable()`: the controller mode before allocation, so
    /// the BARs the mode exposes are sized.
    pub(super) fn sata_enable(&self, config: &SataConfig) {
        let (dev, func) = super::regs::dev::SATA1;
        let sata = EcamDevice::new(0, dev, func);
        if !sata.is_present() {
            return;
        }
        sata.write16(0x90, sata_map(config));
    }

    /// coreboot `sata_init()`.
    pub(super) fn sata_init(&self, config: &SataConfig) {
        const IDE_DECODE_ENABLE: u16 = 1 << 15;
        let (dev, func) = super::regs::dev::SATA1;
        let sata = EcamDevice::new(0, dev, func);
        if !sata.is_present() {
            return;
        }
        // SAFETY: a present type-0 PCH function.
        let header = unsafe { sata.regs::<PciType0Config>() };
        header.command.set(
            (PCI_COMMAND_BITS::BUS_MASTER::SET
                + PCI_COMMAND_BITS::MEMORY_SPACE::SET
                + PCI_COMMAND_BITS::IO_SPACE::SET)
                .value,
        );
        let ports = u32::from(config.ports);
        match config.mode {
            SataMode::Ahci => {
                fstart_log::info!("ibexpeak: SATA in AHCI mode");
                header.interrupt_line.set(0x0b);
                sata.write16(0x40, IDE_DECODE_ENABLE);
                sata.write16(0x42, IDE_DECODE_ENABLE);
                // 6-port SKU and OOB retry; ports enabled in ABAR.
                sata.write16(0x92, (sata.read16(0x92) & !0x3f) | 0x8100 | ports as u16);
                sata.write32(0x94, ((ports ^ 0x3f) << 24) | 0x183 | 0x4000_0000);
                sata.write32(0x98, 0x0059_0200);
                match sata.memory_bar(0x24) {
                    Some(abar) => ahci_init(Bar(abar as usize), config),
                    None => fstart_log::error!("ibexpeak: SATA ABAR unassigned"),
                }
            }
            SataMode::Ide => {
                fstart_log::info!("ibexpeak: SATA in IDE mode");
                header.bar[5].set(0);
                header.command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR);
                // Native mode on both channels.
                sata.write8(0x09, 0x8f);
                header.interrupt_line.set(0xff);
                sata.write16(0x40, IDE_DECODE_ENABLE);
                sata.write16(0x42, IDE_DECODE_ENABLE);
                sata.write16(0x92, (sata.read16(0x92) & !0x3f) | ports as u16);
                sata.write32(0x94, ((ports ^ 0x3f) << 24) | 0x183);
            }
        }
        // Additional programming requirements (SIRI/SIRD indexed).
        let sir_write = |index: u32, value: u32| {
            sata.write32(0xa0, index);
            sata.write32(0xa4, value);
        };
        let sir_read = |index: u32| {
            sata.write32(0xa0, index);
            sata.read32(0xa4)
        };
        sir_write(0x04, 0);
        sir_write(0x28, 0x0a00_0033);
        sir_write(0x54, (sir_read(0x54) & 0xff00_0000) | 0x55_5555);
        sir_write(0x64, 0xcccc_cccc);
        sir_write(0x68, (sir_read(0x68) & 0xffff_0000) | 0xcccc);
        sir_write(0x78, (sir_read(0x78) & 0x0000_ffff) | 0x8888_0000);
        sir_write(0x84, 0x001c_7000);
        sir_write(0x88, 0x8888_8888);
        sir_write(0xa0, 0x001c_7000);
        sir_write(0xc4, 0x0c0c_0c0c);
        sir_write(0xc8, 0x0c0c_0c0c);
        sir_write(0xd4, 0x1000_0000);
    }

    /// coreboot `pch_smbus_init()`.
    pub(super) fn smbus_init(&self) {
        let (dev, func) = super::regs::dev::SMBUS;
        let smbus = EcamDevice::new(0, dev, func);
        if !smbus.is_present() {
            return;
        }
        // Clock gating.
        smbus.and16(0x80, !((1 << 8) | (1 << 10) | (1 << 12) | (1 << 14)));
        if let Some(bus) = self.smbus.as_ref() {
            bus.set_slave_address(super::SMBUS_SLAVE_ADDR);
        }
    }

    /// coreboot `thermal_init()`.
    pub(super) fn thermal_init(&self) {
        let (dev, func) = super::regs::dev::THERMAL;
        let thermal = EcamDevice::new(0, dev, func);
        let Some(base) = thermal.memory_bar(0x10) else {
            return;
        };
        let bar = Bar(base as usize);
        bar.write32(4, 0x3a2b);
        bar.write8(0xe, 0x40);
        bar.write16(0x56, 0xffff);
        bar.write16(0x64, 0xffff);
        bar.write16(0x66, 0xffff);
        bar.write16(0x68, 0xfa);
        bar.write8(1, 0xb8);
        fstart_log::info!("ibexpeak: thermal init done");
    }

    /// coreboot `spi_finalize_ops()` for the ICH9-style SPI controller:
    /// the software-sequencing opcode menu flashrom expects, and BIOS
    /// writes left to the OS (no SMM BIOS write protection).
    pub(super) fn spi_finalize(&self) {
        const HSFS: u32 = 0x3804;
        const PREOP: u32 = 0x3894;
        const OPTYPE: u32 = 0x3896;
        const OPMENU: u32 = 0x3898;
        const FLOCKDN: u16 = 1 << 15;
        // (opcode, type): 0 read/no address, 1 write/no address,
        // 2 read/address, 3 write/address.
        const MENU: [(u8, u16); 8] = [
            (0x01, 1), // WRSR
            (0x02, 3), // byte program
            (0x03, 2), // read
            (0x05, 0), // RDSR
            (0x20, 3), // sector erase
            (0x9f, 0), // RDID
            (0xd8, 3), // block erase
            (0x0b, 2), // fast read
        ];
        let raw = self.rcba_raw();
        if raw.read16(HSFS) & FLOCKDN != 0 {
            return;
        }
        // Prefixes: EWSR and WREN.
        raw.write16(PREOP, 0x5006);
        let mut optype = 0u16;
        for (index, (opcode, kind)) in MENU.iter().enumerate() {
            optype |= kind << (2 * index);
            raw.write8(OPMENU + index as u32, *opcode);
        }
        raw.write16(OPTYPE, optype);
        // SAFETY: fixed LPC function; ECAM is up.
        let flash = unsafe { crate::southbridge::lpc::flash_config() };
        flash.bios_control.modify(
            BIOS_CONTROL::LOCK_ENABLE::CLEAR
                + BIOS_CONTROL::SMM_WRITE_PROTECT::CLEAR
                + BIOS_CONTROL::WRITE_ENABLE::SET,
        );
    }

    /// coreboot `intel_pch_finalize_smm()`, run from the ramstage before
    /// the payload rather than from the `APM_CNT_FINALIZE` SMI.
    pub(super) fn finalize(&self) {
        const HSFS: u32 = 0x3804;
        let raw = self.rcba_raw();
        let rcba = self.rcba();
        // Lock SPIBAR (HSFS.FLOCKDN).
        raw.setbits32(HSFS, 1 << 15);
        rcba.tclockdn.modify(TCLOCKDN::LOCK::SET);
        rcba.gcs.modify(GCS::BILD::SET);
        rcba.fdsw.modify(FDSW::LOCK::SET);
        let lpc = self.lpc_regs();
        lpc.gen_pmcon_1.modify(GEN_PMCON_1::SMI_LOCK::SET);
        lpc.gen_pmcon_lock
            .modify(GEN_PMCON_LOCK::ACPI_BASE_LOCK::SET + GEN_PMCON_LOCK::SLP_STR_POL_LOCK::SET);
        lpc.etr3
            .modify(super::regs::ETR3::CF9GR::CLEAR + super::regs::ETR3::CF9LOCK::SET);
        // R/WO registers.
        raw.write32(0x21a4, raw.read32(0x21a4));
        let hda = EcamDevice::new(0, 0x1b, 0);
        if hda.is_present() {
            hda.write32(0x74, hda.read32(0x74));
        }
        let tco = self.pm().tco();
        tco.write16(pmio::TCO1_CNT, tco.read16(pmio::TCO1_CNT) | pmio::TCO_LOCK);
        fstart_log::info!("ibexpeak: chipset locked down");
    }
}

/// SATA MAP: the mode and the disabled ports, before allocation.
fn sata_map(config: &SataConfig) -> u16 {
    let mode = if config.mode == SataMode::Ahci {
        0x60
    } else {
        0
    };
    mode | (u16::from(config.ports ^ 0x3f) << 8)
}

/// AHCI memory-mapped init (coreboot `sata_init()` AHCI branch).
fn ahci_init(abar: Bar, config: &SataConfig) {
    const CAP: u32 = 0x00;
    const PI: u32 = 0x0c;
    const CAP2: u32 = 0x24;
    const VSP: u32 = 0xa0;
    // PSC, SSC, SALP and SSS set; SXS, EMS and PMS clear.
    let mut cap = (abar.read32(CAP) | 0x0c00_6000) & !0x0002_0060;
    if config.interface_speed != 0 {
        cap = (cap & !0x00f0_0000) | (u32::from(config.interface_speed & 3) << 20);
    }
    abar.write32(CAP, cap);
    abar.write32(PI, u32::from(config.ports));
    let _ = abar.read32(PI);
    let _ = abar.read32(PI);
    abar.clrbits32(CAP2, 1 << 1);
    abar.clrbits32(VSP, 0x5);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sata_map_matches_coreboot() {
        let ahci = SataConfig {
            mode: SataMode::Ahci,
            ports: 0x03,
            interface_speed: 0,
        };
        assert_eq!(sata_map(&ahci), 0x3c60);
        let ide = SataConfig {
            mode: SataMode::Ide,
            ..ahci
        };
        assert_eq!(sata_map(&ide), 0x3c00);
    }

    #[test]
    fn mobile5_tables_cover_coreboot_ranges() {
        let words: usize = MOBILE5_PM.iter().map(|(_, values)| values.len()).sum();
        assert_eq!(words, 2 + 8 + 3 + 2 + 4 + 28 + 10 + 5);
    }
}
