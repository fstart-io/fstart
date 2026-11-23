//! Device enables before resource allocation; AHCI, Serial IO, and HDA
//! programming after the generic PCI allocator assigns their BARs.

use super::{
    LynxPoint, LynxPointConfig, hsio, lpc_regs, rcba_regs,
    regs::{
        BUC, FD, FD2, GEN_PMCON_3, HPET_GC, HPTC, HpetMmio, RcbaRegs, SERIAL_IO_GENERAL,
        SERIAL_IO_RESET, SERIRQ_CNTL, SdioMmio, SerialIoMmio, dev, lp_gpio,
    },
    usb,
};
use crate::southbridge::{ahci, hda::HdaController, ich, pmio_ich::PmIo};
use fstart_core::{mmio::MmioWindow, services::ServiceError};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

tock_registers::register_bitfields![u32,
    /// SATA SIR 0x60 (32 bits), Power Optimizer, BWG programming steps 3-5.
    Sir60 [
        STEP3 OFFSET(3) NUMBITS(1) [],
        STEP4 OFFSET(0) NUMBITS(1) [],
        STEP5 OFFSET(1) NUMBITS(1) []
    ],
    /// SATA SIR 0x68 (32 bits), Power Optimizer step 2, preserve high word.
    Sir68 [
        OPTIMIZER OFFSET(0) NUMBITS(16) []
    ]
];

pub(super) fn serial(i: u8) -> EcamDevice {
    if i == 7 {
        EcamDevice::from_devfn(0, dev::SDIO)
    } else {
        EcamDevice::new(0, dev::SERIAL_IO, i)
    }
}

/// ABAR capabilities and `PxCMD` are R/WO: apply all board and silicon policy
/// in the first write to each register (in particular LP's `SAM`).
fn setup_abar(a: &ahci::AhciRegs, c: &LynxPointConfig) {
    use ahci::{CAP, CAP2, GHC, PXCMD};
    let lp = c.gpio.lp();
    a.ghc.modify(GHC::AE::SET);
    let caps = CAP::PSC::SET
        + CAP::SSC::SET
        + CAP::SALP::SET
        + CAP::SSS::SET
        + CAP::SXS::CLEAR
        + CAP::EMS::CLEAR
        + CAP::PMS::CLEAR;
    a.cap.modify(if lp { caps + CAP::SAM::SET } else { caps });
    a.pi.set(c.sata_port_map.into());
    let _ = a.pi.get();
    let _ = a.pi.get();
    a.cap2.modify(if !lp {
        CAP2::NVMP::CLEAR
    } else if c.sata_devslp_disable {
        CAP2::SDS::CLEAR
    } else {
        CAP2::APST::SET + CAP2::SDS::SET + CAP2::SADM::SET + CAP2::DESO::SET
    });
    for (i, port) in a.ports.iter().take(if lp { 4 } else { 6 }).enumerate() {
        let hpcp = if c.sata_hotplug_map & (1 << i) != 0 {
            PXCMD::HPCP::SET.value
        } else {
            0
        };
        port.cmd.set(port.cmd.get() | hpcp);
    }
}

/// Disable unused bus-0 PCH functions through `FD`, `FD2`, and `BUC` before
/// PCI enumeration.
///
/// On Lynx Point-H, primary SATA1 (`00:1f.2`) always runs in AHCI mode, so the
/// legacy secondary SATA2 function (`00:1f.5`) is hidden via `FD.SATA2`.
fn disable_unused_functions(
    c: &LynxPointConfig,
    debug_devfn: Option<u8>,
    rcba: &RcbaRegs,
    device: impl Fn(u8) -> EcamDevice,
) {
    let lp = c.gpio.lp();
    for (devfn, field, disabled) in [
        (dev::XHCI, FD::XHCI::SET, !c.xhci_enabled),
        (dev::HDA, FD::HDA::SET, c.hda.is_none()),
        (dev::SATA1, FD::SATA1::SET, c.sata_port_map == 0),
        (dev::SATA2, FD::SATA2::SET, !lp),
        (dev::SMBUS, FD::SMBUS::SET, !c.smbus_enabled),
        (dev::THERMAL, FD::THERMAL::SET, !c.thermal_enabled),
        (
            dev::EHCI1,
            FD::EHCI1::SET,
            !c.ehci_enabled && debug_devfn != Some(dev::EHCI1),
        ),
        (
            dev::EHCI2,
            FD::EHCI2::SET,
            !lp && !c.ehci_enabled && debug_devfn != Some(dev::EHCI2),
        ),
        (dev::ADSP, FD::ADSP::SET, lp && !c.adsp),
    ] {
        if disabled {
            let d = device(devfn);
            if d.is_present() {
                d.and16(4, !7);
            }
            rcba.fd.modify(field);
        }
    }
    for (i, (devfn, field)) in [
        (dev::MEI2, FD2::MEI2::SET),
        (dev::IDER, FD2::IDER::SET),
        (dev::KT, FD2::KT::SET),
    ]
    .into_iter()
    .enumerate()
    {
        if c.mei_aux & (1 << i) == 0 {
            let d = device(devfn);
            if d.is_present() {
                d.and16(4, !7);
            }
            rcba.fd2.modify(field);
        }
    }
    if !c.gbe {
        let d = device(dev::GBE);
        if d.is_present() {
            d.and16(4, !7);
        }
        rcba.buc.modify(BUC::GBE_DISABLE::SET);
    }
}

/// Program `PCI_INTERRUPT_LINE = 0x80` on every present bus-0 INTx function.
///
/// Must run before SATA init so SATA's `PCI_INTERRUPT_LINE = 0x0a` (matching
/// coreboot `lpc.c` -> `sata.c`) is not overwritten.
fn program_bus0_interrupt_lines(device: impl Fn(u8, u8) -> EcamDevice) {
    for slot in 0..32 {
        for func in 0..8 {
            let d = device(slot, func);
            if d.is_present() {
                LynxPoint::program_interrupt_line(d);
            }
        }
    }
}

impl LynxPoint {
    /// Reconcile `PCI_INTERRUPT_LINE` on a present function with the disabled
    /// PIRQ-to-PIC route. Also applicable to devices from the platform PCI list.
    pub fn program_interrupt_line(d: EcamDevice) {
        if (1..=4).contains(&d.read8(0x3d)) {
            d.write8(0x3c, 0x80);
        }
    }

    pub(super) fn enable_devices(&mut self) -> Result<(), ServiceError> {
        let c = self.config;
        let lp = c.gpio.lp();
        let r = rcba_regs();
        self.configure_root_ports();
        disable_unused_functions(c, self.ehci_debug.map(|(d, _)| d), r, |devfn| {
            EcamDevice::from_devfn(0, devfn)
        });
        if lp {
            for i in 0..8 {
                if c.serialio_enable & (1 << i) == 0 {
                    let d = serial(i);
                    if d.is_present() {
                        d.and16(4, !7);
                        d.or32(0x84, 3);
                    }
                    hsio::update(
                        if i == 7 {
                            0xce00_ae07
                        } else {
                            0xce00_aa07 + 0x40 * u32::from(i)
                        },
                        0,
                        256,
                    );
                }
            }
        }
        if c.sata_port_map != 0 {
            EcamDevice::from_devfn(0, dev::SATA1).write16(
                0x90,
                0x60 | (u16::from(c.sata_port_map ^ self.sata_mask()) << 8),
            );
        }
        Ok(())
    }

    fn ahci(&self) -> Result<(), ServiceError> {
        let d = EcamDevice::from_devfn(0, dev::SATA1);
        let lp = self.config.gpio.lp();
        let ports = self.config.sata_port_map;
        let Some(base) = d.memory_bar(0x24) else {
            fstart_log::warn!("LPT SATA: missing AHCI BAR");
            return Ok(());
        };
        d.or16(4, 3);
        d.write8(0x3c, 10);
        d.write16(0x40, 0x8000);
        d.write16(0x42, 0x8000);
        d.write16(
            0x92,
            (d.read16(0x92) & !u16::from(self.sata_mask())) | 0x8000 | u16::from(ports),
        );
        fstart_arch::x86::udelay(2);
        d.modify32(
            0x98,
            if lp { 0x3fff_e01f } else { !0x1fe0 },
            0x205c_0220 | if lp { 0x0180_0000 } else { 0 },
        );
        d.write16(0x9c, 32);
        d.write32(
            0x94,
            0x183
                | (u32::from(ports ^ self.sata_mask()) << 24)
                | (u32::from(self.config.sata_devslp_mux) << 15),
        );
        // SAFETY: ABAR (BAR5) was assigned by the PCI allocator and memory-enabled above.
        setup_abar(
            unsafe { &*(base as usize as *const ahci::AhciRegs) },
            self.config,
        );
        for (addr, tx) in [0xea00_0151, 0xea00_0051]
            .into_iter()
            .zip(self.config.sata_gen3_tx)
        {
            if tx != 0 {
                hsio::update(addr, u32::MAX, tx);
            }
        }
        for (i, trim) in self.config.sata_gen3_dtle.into_iter().enumerate() {
            if let Some(v) = trim.filter(|v| *v != 0) {
                let a = [0xea00_2750, 0xea00_2550][i];
                hsio::update(a, 15 << 24, u32::from(v) << 24);
                hsio::update(a + 4, 15 << 16, u32::from(v) << 16);
            }
        }
        // The three optimizer enables are separate hardware programming steps.
        // SAFETY: `d` is the present, exclusively owned PCH SATA function.
        let sir = |idx| unsafe { ahci::sir::<()>(d, idx) };
        sir(0x64).set(if lp { 0x883c_9003 } else { 0x883c_9001 });
        unsafe { ahci::sir::<Sir68::Register>(d, 0x68) }.modify(Sir68::OPTIMIZER.val(0x880a));
        let opt = unsafe { ahci::sir::<Sir60::Register>(d, 0x60) };
        opt.modify(Sir60::STEP3::SET);
        opt.modify(Sir60::STEP4::SET);
        opt.modify(Sir60::STEP5::SET);
        sir(0x70).set(0x3f00_bf1f);
        if lp {
            for (idx, v) in [(0x54, 0xcf00_0f0f), (0x58, 0x0019_0000)] {
                sir(idx).set(v);
            }
            let r = rcba_regs();
            r.pm_333c.set((r.pm_333c.get() & 0xffcf_ffff) | 0x00c0_0000);
        }
        d.or32(0x300, 0xe003_0000);
        Ok(())
    }

    fn serialio(&mut self) -> Result<(), ServiceError> {
        let c = self.config;
        if c.sio_acpi_mode && c.serialio_enable != 0 {
            let r = rcba_regs();
            r.sio_irq_ctrl.set(r.sio_irq_ctrl.get() | 0x20e0);
        }
        for i in 0..8u8 {
            if c.serialio_enable & (1 << i) == 0 {
                continue;
            }
            let d = serial(i);
            let (Some(a), Some(b)) = (d.memory_bar(0x10), d.memory_bar(0x14)) else {
                if self.resume && self.serial_bars[i as usize].is_some() {
                    return Err(ServiceError::HardwareError);
                }
                fstart_log::warn!("LPT Serial IO {}: missing BAR, skipping", i);
                if self.serial_bars[i as usize].take().is_some() {
                    self.s3 = false;
                }
                continue;
            };
            if c.sio_acpi_mode && self.serial_bars[i as usize].is_none() {
                continue;
            }
            if c.sio_acpi_mode
                && self.serial_bars[i as usize].map(|(x, y)| (u64::from(x), u64::from(y)))
                    != Some((a, b))
            {
                fstart_log::warn!("Serial IO {}: allocation changed after snapshot", i);
                if self.resume {
                    return Err(ServiceError::HardwareError);
                }
                self.serial_bars[i as usize] = None;
                self.s3 = false;
                continue;
            }
            d.or16(4, 6);
            let pin = match i {
                0 => 2,
                1..=4 => 3,
                5..=6 => 4,
                _ => 1,
            };
            if i == 0 {
                if !c.sio_acpi_mode {
                    // SAFETY: Serial IO DMA BAR0 is assigned and memory-enabled above.
                    let win = unsafe { MmioWindow::new(a as usize, 0x1000) };
                    win.setbits32(0x800, 1);
                }
                hsio::update(0xcb00_0154, 0x131f, 0x131f);
                hsio::update(0xcb00_0180, 63, 63);
            } else if i < 7 {
                // SAFETY: Serial IO BAR0 is assigned and memory-enabled above.
                let regs = unsafe { &*(a as usize as *const SerialIoMmio) };
                if !c.sio_acpi_mode {
                    regs.clocks.set(regs.clocks.get() | 1);
                }
                regs.general.modify(SERIAL_IO_GENERAL::LTR_MODE::CLEAR);
                regs.resets.modify(SERIAL_IO_RESET::FUNC_RESET.val(0));
                regs.resets.modify(SERIAL_IO_RESET::FUNC_RESET.val(3));
                regs.ltr_ctrl.set(0);
                if i <= 2 {
                    regs.general.modify(
                        SERIAL_IO_GENERAL::VOLT_1V8
                            .val(u32::from(c.sio_i2c_1v8[usize::from(i - 1)])),
                    );
                }
            } else {
                // SAFETY: SDIO BAR0 is assigned and memory-enabled above.
                let regs = unsafe { &*(a as usize as *const SdioMmio) };
                if !c.sio_acpi_mode {
                    let win = unsafe { MmioWindow::new(a as usize, 0x1000) };
                    win.setbits32(0x800, 1);
                }
                regs.cap_bypass_ctrl.set(regs.cap_bypass_ctrl.get() | 4);
                regs.cap_bypass_reg1.set(0);
                regs.host_ctrl2.set(regs.host_ctrl2.get() | (1 << 30));
            }
            let routed = if i < 7 {
                hsio::update(
                    0xcb00_0240 + 8 * u32::from(i),
                    u32::MAX,
                    2 | (1 << 18) | (pin << 2) | if c.sio_acpi_mode { 3 << 20 } else { 0 },
                )
            } else {
                hsio::update(0xcb00_0014, u32::MAX, 1 << 13);
                hsio::update(
                    0xcb00_0000,
                    u32::MAX,
                    if c.sio_acpi_mode { 0x30 } else { 0 },
                )
            };
            if c.sio_acpi_mode && !routed {
                // Do not publish a second ACPI driver for a function whose
                // PCI-to-ACPI handoff did not complete. The cold snapshot
                // no longer describes the DSDT, so do not advertise resume.
                if self.resume {
                    return Err(ServiceError::HardwareError);
                }
                self.serial_bars[i as usize] = None;
                self.s3 = false;
            }
        }
        Ok(())
    }

    fn hda(&self) {
        if let Some(cfg) = self.config.hda {
            let d = EcamDevice::from_devfn(0, dev::HDA);
            if let Some(base) = d.memory_bar(0x10) {
                let lp = self.config.gpio.lp();
                d.or16(4, 6);
                let vc = rcba_regs().vcpctl.get();
                if vc >> 31 != 0 {
                    d.modify32(0x120, 0xf8ff_ff01, (1 << 25) | (vc & 0xfe));
                    if !lp {
                        d.and16(0x78, !(1 << 11));
                    }
                }
                d.and32(0x114, !0xfe);
                if d.read32(0x120) & 0x0700_0000 != 0 {
                    d.modify32(0x120, 0x7fff_ffff, if lp { 0 } else { 1 << 31 });
                }
                if lp {
                    d.and8(0x43, !64);
                    d.or32(0xc4, 1 << 24);
                } else {
                    d.or8(0x43, 16);
                    d.or32(0xc0, 1 << 17);
                    d.or32(0xc4, 1 << 14);
                    d.and32(0xd0, 0x7fff_ffff);
                }
                d.and8(0x4d, 0x7f);
                if lp {
                    // SAFETY: HDA BAR0 is assigned and memory-enabled above; preserve 16-bit RMW.
                    let win = unsafe { MmioWindow::new(base as usize, 0x20) };
                    win.setbits16(0x12, 1);
                    d.or8(0x42, 4);
                }
                let codec = HdaController::new(base as usize);
                codec.program_verb_tables(cfg, codec.detect_sdins(15));
            } else {
                fstart_log::warn!("LPT HDA: no BAR, codec setup skipped");
            }
        }
    }

    pub(super) fn init_devices(&mut self) -> Result<(), ServiceError> {
        let c = self.config;
        let l = lpc_regs();
        let lp = c.gpio.lp();
        let r = rcba_regs();
        self.d28ir = r.d28ir.get();
        self.init_bridge_resources();
        let io = unsafe { fstart_arch::x86::ioapic::IoApic::new(0xfec0_0000) };
        io.set_id(2);
        if lp {
            if !io.set_redirection_entries(40) {
                fstart_log::warn!("LPT-LP: IOAPIC redirection entry count did not latch 40");
            }
        } else {
            io.lock_redirection_entries();
        }
        io.mask_all();
        io.route_i8259((fstart_arch::x86::cpuid(1).1 >> 24) as u8);
        l.serirq_cntl.write(
            SERIRQ_CNTL::ENABLE::SET
                + SERIRQ_CNTL::CONTINUOUS::SET
                + SERIRQ_CNTL::FRAME_SIZE.val(4),
        );
        l.serirq_cntl
            .write(SERIRQ_CNTL::ENABLE::SET + SERIRQ_CNTL::FRAME_SIZE.val(4));
        for reg in &l.pirq_a_d {
            reg.set(0x80);
        }
        for reg in &l.pirq_e_h {
            reg.set(0x80);
        }
        if c.smbus_enabled {
            self.bus.pch_ramstage(EcamDevice::from_devfn(0, dev::SMBUS));
        }
        let rtc_failed = l.gen_pmcon_3.is_set(GEN_PMCON_3::RTC_BATTERY_DEAD);
        if rtc_failed {
            l.gen_pmcon_3.modify(GEN_PMCON_3::RTC_BATTERY_DEAD::CLEAR);
            fstart_log::warn!("LPT RTC battery dead");
        }
        if !self.resume {
            fstart_driver_rtc::init_clock("lynxpoint", rtc_failed);
        }
        ich::init_isa_dma();
        ich::init_i8259_sci_irq9();
        l.ibdf.set(0xfaf8);
        for (i, reg) in l.hbdf.iter().enumerate() {
            reg.set(0xfa78 + i as u16);
        }
        r.hptc
            .modify(HPTC::ADDRESS_SELECT.val(0) + HPTC::ENABLE::SET);
        let _ = r.hptc.get();
        // SAFETY: HPET decode at 0xfed0_0000 was enabled by HPTC above.
        let hpet = unsafe { &*(0xfed0_0000usize as *const HpetMmio) };
        hpet.gcfg.modify(HPET_GC::ENABLE::SET);
        self.hpet_id = hpet.gcid.get();
        let pm = self.pm();
        for (i, enable) in c.gpe0_en.iter().enumerate().take(if lp { 4 } else { 2 }) {
            pm.write32((if lp { 0x90 } else { 0x28 }) + 4 * i as u16, *enable);
        }
        // Reinstall the graphics TCO SCI after board GPE programming, on
        // cold boot and S3, even when the OS owns scanout restoration.
        if EcamDevice::new(0, 2, 0).is_present() {
            pm.tco().write16(4, 1 << 9);
            pm.write32(if lp { 0x8c } else { 0x20 }, 1 << 6);
            pm.setbits32(if lp { 0x9c } else { 0x28 }, 1 << 6);
        }
        if lp {
            PmIo::new(c.gpio_base()).setbits32(lp_gpio::ALT_GPI_SMI_EN, c.alt_gp_smi_en);
        } else {
            pm.setbits16(0x38, c.alt_gp_smi_en as u16);
            pm.setbits16(0x5c, (c.alt_gp_smi_en >> 16) as u16);
        }
        // Enable pre-MP power options and SCI only after all board event masks
        // are installed; PM_SYNC, dynamic clock gating, and DRAM_INIT run in
        // `post_mp_init()` strictly after CPU C-state init and `BIOS_RESET_CPL`.
        self.power_options();
        // PIRQ routes are disabled in legacy mode (0x80); program the same line
        // on every present onboard INTx function before SATA sets 0x0a and
        // before Serial IO hides.
        program_bus0_interrupt_lines(|slot, func| EcamDevice::new(0, slot, func));
        if c.sata_port_map != 0 {
            self.ahci()?;
        }
        usb::post_dram(c, self.resume, self.ehci_debug.map(|(d, _)| d))?;
        self.hda();
        if lp {
            self.serialio()?;
        }
        fstart_log::info!("LPT: USB routing is fixed at boot; APM 0xca unsupported (APM_STS=ff)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lynxpoint::{Gpios, gpio};
    use crate::southbridge::gpio_ich::GpioConfig;
    use fstart_pci::{PciAddress, PciEcam, PciRoot, PciRootInfo};

    fn fake_ecam(backing: &mut [u8]) -> PciEcam {
        let root = PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: backing.as_mut_ptr().addr().next_multiple_of(1 << 20) as u64,
                bus_start: 0,
                bus_end: 0,
            },
            [],
        )
        .unwrap();
        PciEcam::new([root]).unwrap()
    }

    #[test]
    fn lp_abar_caps_devslp_and_hotplug() {
        static PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Native); 95];
        for disable in [true, false] {
            let config = LynxPointConfig {
                sata_port_map: 7,
                sata_hotplug_map: 4,
                sata_devslp_disable: disable,
                ..LynxPointConfig::new(Gpios::Lp(&PADS))
            };
            let mut buf = [0u32; 0x400 / 4];
            buf[0x24 / 4] = 8;
            // SAFETY: aligned ABAR buffer covering the shared AHCI layout.
            let abar = unsafe { &*buf.as_mut_ptr().cast::<ahci::AhciRegs>() };
            setup_abar(abar, &config);
            assert_eq!(abar.cap.get(), 0x0c04_6000);
            assert_eq!(abar.pi.get(), 7);
            assert_eq!(abar.cap2.get(), if disable { 0 } else { 0x3c });
            assert_eq!(abar.ports[1].cmd.get(), 0);
            assert_eq!(abar.ports[2].cmd.get(), 1 << 18);
        }
    }

    #[test]
    fn non_lp_hides_secondary_sata2_function_while_lp_leaves_bit_clear() {
        static LP_PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Native); 95];
        static H_GPIO: GpioConfig = GpioConfig::new();

        let mut backing = alloc::vec![0xffu8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let sata2 = pci.device(PciAddress::new(0, 0, 31, 5)).unwrap();
        sata2.write16(0, 0x8086);
        sata2.write16(4, 0x0007);

        let mut rcba_h = alloc::vec![0u32; 0x4000 / 4];
        // SAFETY: aligned 16 KiB buffer matching RcbaRegs.
        let r_h = unsafe { &*rcba_h.as_mut_ptr().cast::<RcbaRegs>() };
        let cfg_h = LynxPointConfig::new(Gpios::Traditional(&H_GPIO));
        disable_unused_functions(&cfg_h, None, r_h, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert!(r_h.fd.is_set(FD::SATA2));
        assert_eq!(sata2.read16(4), 0);

        let mut rcba_lp = alloc::vec![0u32; 0x4000 / 4];
        // SAFETY: aligned 16 KiB buffer matching RcbaRegs.
        let r_lp = unsafe { &*rcba_lp.as_mut_ptr().cast::<RcbaRegs>() };
        let cfg_lp = LynxPointConfig::new(Gpios::Lp(&LP_PADS));
        disable_unused_functions(&cfg_lp, None, r_lp, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert!(!r_lp.fd.is_set(FD::SATA2));
    }

    #[test]
    fn bus0_interrupt_line_pass_precedes_sata_irq10_override() {
        let mut backing = alloc::vec![0xffu8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let xhci = pci.device(PciAddress::new(0, 0, 20, 0)).unwrap();
        let sata = pci.device(PciAddress::new(0, 0, 31, 2)).unwrap();
        xhci.write16(0, 0x8086);
        xhci.write8(0x3d, 1);
        sata.write16(0, 0x8086);
        sata.write8(0x3d, 2);

        program_bus0_interrupt_lines(|slot, func| {
            pci.device(PciAddress::new(0, 0, slot, func)).unwrap()
        });
        assert_eq!(xhci.read8(0x3c), 0x80);
        assert_eq!(sata.read8(0x3c), 0x80);
        // SATA init runs afterwards and programs IRQ 10.
        sata.write8(0x3c, 10);
        assert_eq!(xhci.read8(0x3c), 0x80);
        assert_eq!(sata.read8(0x3c), 10);
    }
}
