//! Bootblock and raminit-time PCH setup: coreboot `ibexpeak/bootblock.c`,
//! `early_pch.c`, `early_cir.c`, `early_usb.c`, `early_thermal.c` and
//! `bd82x6x/early_rcba.c`.

use super::regs::{
    ACPI_CNTL, DIP, DIR, ETR3, FD, FD2, GCS, GEN_PMCON_2, GEN_PMCON_3, GPIO_CNTL, HPTC, LPC_EN,
    OIC, RC, RCBA, cir, pm,
};
use super::{IntelIbexPeak, PchFunction};
use crate::MmioBar;
use crate::southbridge::pmio_ich as pmio;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// GEN_PMCON_3, accessed bytewise where coreboot does.
const GEN_PMCON_3_OFFSET: u16 = 0xa4;

/// INTx pin numbers in DxxIP (coreboot `INTA`..`INTD`).
const INTA: u32 = 1;
const INTB: u32 = 2;
const INTC: u32 = 3;
const INTD: u32 = 4;

/// PIRQ selectors in DxxIR (coreboot `PIRQA`..).
const PIRQA: u16 = 0;
const PIRQB: u16 = 1;
const PIRQC: u16 = 2;
const PIRQD: u16 = 3;

impl IntelIbexPeak {
    /// coreboot `bootblock_early_southbridge_init()`.
    pub(super) fn bootblock_init(&self) {
        // SAFETY: fixed LPC function; ECAM is up.
        unsafe { crate::southbridge::lpc::flash_config() }.enable_prefetching_and_caching();
        let lpc = self.lpc_regs();
        lpc.rcba
            .write(RCBA::BASE.val(u32::from(self.config.rcba as u32 >> 14)) + RCBA::ENABLE::SET);
        let rcba = self.rcba();
        // Port 80h cycles go to LPC.
        rcba.gcs.modify(GCS::RPR::CLEAR);
        self.set_spi_speed();
        // Upper 128 bytes of CMOS.
        rcba.rc.write(RC::UPPER_128_ENABLE::SET);
        self.early_lpc_init();
    }

    /// coreboot `set_spi_speed()`: run software sequencing at the
    /// write/erase frequency the flash descriptor names.
    fn set_spi_speed(&self) {
        const FDOC: u32 = 0x38b0;
        const FDOD: u32 = 0x38b4;
        const SSFC_FREQ: u32 = 0x3893;
        let rcba = self.rcba_raw();
        // Observe descriptor component section 0.
        rcba.write32(FDOC, 0x1000);
        let frequency = ((rcba.read32(FDOD) >> 24) & 7) as u8;
        rcba.clrsetbits8(SSFC_FREQ, 7, frequency);
    }

    /// coreboot `early_lpc_init()`: common legacy decodes, PWR_FLR, CF9GR
    /// and the board's generic decodes.
    fn early_lpc_init(&self) {
        let lpc = self.lpc_regs();
        lpc.lpc_en.write(
            LPC_EN::CNF2::SET
                + LPC_EN::CNF1::SET
                + LPC_EN::MC::SET
                + LPC_EN::KBC::SET
                + LPC_EN::GAMEH::SET
                + LPC_EN::GAMEL::SET
                + LPC_EN::FDD::SET
                + LPC_EN::LPT::SET
                + LPC_EN::COMB::SET
                + LPC_EN::COMA::SET,
        );
        // COMA 0x3f8, COMB 0x2f8.
        lpc.lpc_io_dec.set(0x10);
        // coreboot's byte write: PWR_FLR written 0, S5 after power failure
        // until the ramstage applies the board policy.
        let pmcon3 = self.lpc().read8(GEN_PMCON_3_OFFSET);
        self.lpc().write8(
            GEN_PMCON_3_OFFSET,
            (pmcon3 & !(GEN_PMCON_3::PWR_FLR::SET.value as u8))
                | GEN_PMCON_3::AFTER_G3::SET.value as u8,
        );
        lpc.etr3.modify(ETR3::CF9GR::CLEAR);
        let decodes = self.config.lpc_generic_io.as_slice();
        for (index, register) in lpc.gen_dec.iter().enumerate() {
            register.set(
                decodes
                    .get(index)
                    .and_then(|window| window.encode())
                    .unwrap_or(0),
            );
        }
    }

    /// coreboot `ibexpeak_setup_bars()`: PMBASE, ACPI decode, the
    /// watchdog and the early HECI BAR; and the GPIO BAR (`early_gpio_init()`).
    pub(super) fn setup_bars(&self) {
        let lpc = self.lpc_regs();
        lpc.pmbase.set(u32::from(self.config.pmbase) | 1);
        lpc.acpi_cntl.write(ACPI_CNTL::ACPI_EN::SET);
        // No reset on a watchdog timeout, and halt the TCO timer.
        self.rcba().gcs.modify(GCS::NO_REBOOT::SET);
        let tco = self.pm().tco();
        tco.write16(pmio::TCO1_CNT, 1 << 11);
        tco.write16(pmio::TCO2_STS, tco.read16(pmio::TCO2_STS) | 2);
        // The ME's HECI BAR for the raminit UMA handshake.
        let heci = fstart_pci::ecam::EcamDevice::new(0, 0x16, 0);
        heci.write32(0x10, super::DEFAULT_HECIBAR);
        heci.write32(0x04, 0x6); // memory decode and bus master
        lpc.gpiobase.set(u32::from(self.config.gpiobase) | 1);
        lpc.gpio_cntl.write(GPIO_CNTL::GPIO_EN::SET);
    }

    /// coreboot `pch_default_disable()`: hide the performance counters and
    /// the UHCI functions the rate-matching hubs replace. OR-ed in so that
    /// the mainstage's repeat keeps later function disables.
    pub(super) fn default_disable(&self) {
        let rcba = self.rcba();
        rcba.fd
            .modify(FD::PERF::SET + FD::UHCI.val(0xff) + FD::BIOS_MUST_WRITE::SET);
        // Reserved bit, set to 1.
        rcba.fd2.modify(FD2::DBDF::SET);
    }

    /// coreboot `early_usb_init()`: per-port drive current, the EDS USB
    /// initialization values, rate-matching hub mode, port disables and
    /// over-current mapping.
    pub(super) fn early_usb_init(&self) {
        let pm = self.pm();
        let rcba = self.rcba();
        pm.write16(pm::UPRWC, pm.read16(pm::UPRWC) | pm::UPRWC_WR_EN);
        for (register, port) in rcba.usbir.iter().zip(self.config.usb_ports.iter()) {
            register.modify(super::regs::USBIR::CURRENT.val(port.current.setting()));
        }
        rcba.usbirc.set(rcba.usbirc.get() & !(1 << 8));
        rcba.usbira
            .set(rcba.usbira.get() | (7 << 12) | (7 << 8) | (7 << 4) | 2);
        rcba.usbirb.set(
            (rcba.usbirb.get() & !0x617f0)
                | (3 << 17)
                | (1 << 12)
                | (1 << 10)
                | (1 << 8)
                | (4 << 4),
        );
        // Rate-matching hub mode makes the EHCI functions appear at .0.
        rcba.rmh_mode.set(0);
        let ports = &self.config.usb_ports;
        rcba.usbpdo.set(
            ports
                .iter()
                .enumerate()
                .filter(|(_, port)| !port.enabled)
                .fold(0, |mask, (index, _)| mask | 1 << index),
        );
        let oc_map = |range: core::ops::Range<usize>, first_pin: u8| {
            range.fold(0u32, |map, index| {
                let port = ports[index];
                match port.oc_pin {
                    Some(pin) if port.enabled && pin >= first_pin && pin < first_pin + 4 => {
                        map | 1
                            << ((index - range_start(first_pin)) + 8 * usize::from(pin - first_pin))
                    }
                    _ => map,
                }
            })
        };
        // OC0..3 map ports 0..7, OC4..7 map ports 8..13.
        rcba.usbocm1.set(oc_map(0..8, 0));
        rcba.usbocm2.set(oc_map(8..14, 4));
        pm.write16(pm::UPRWC, 0);
    }

    /// coreboot `pch_setup_cir()`: the Chipset Initialization Registers.
    pub(super) fn setup_cir(&self) {
        let lpc = self.lpc_regs();
        let rcba = self.rcba();
        let raw = self.rcba_raw();
        // Byte OR like coreboot; RTC_BATTERY_DEAD (W1C) stays untouched.
        self.lpc().or8(GEN_PMCON_3_OFFSET, 0xfb);
        rcba.gcs.modify(GCS::FERR_MUX::SET);
        raw.setbits8(0x3430, 0xfc);
        raw.write32(cir::CIR7, 0xf);
        raw.write32(cir::CIR9, 0);
        if matches!(
            lpc.device_id.get(),
            0x3b01 | 0x3b03 | 0x3b05 | 0x3b07 | 0x3b09 | 0x3b0b | 0x3b0d | 0x3b0f
        ) {
            raw.modify32(cir::CIR6, 0xff1f_ff7f, 0x0060_0000);
        }
        raw.setbits32(0x3310, 0x31);
        // EDS 13.8.1.1.
        if self.config.desktop {
            lpc.gen_pmcon_1
                .modify(super::regs::GEN_PMCON_1::PSEUDO_CLKRUN::SET);
        }
        lpc.cir4.set(0x45);
        raw.write32(cir::CIR8, 0x0400_0000);
        lpc.etr3.set(0xc000_0300);
        raw.write32(0x3318, 0x0102_0000);
        let model = fstart_arch::x86::mp::CpuIdentity::current().model();
        let cir22 = if model == 0x1e {
            // Lynnfield/Clarksfield.
            raw.write32(cir::CIR13, 0xfffff);
            raw.write32(cir::CIR14, 0x61080);
            raw.write32(cir::CIR16, 0x7f8f_9f80);
            raw.write32(cir::CIR18, 0x3900);
            raw.write32(cir::CIR19, 0x20002);
            raw.write32(cir::CIR20, 0x44b00);
            raw.write32(cir::CIR21, 0x02000);
            0x20000
        } else {
            // Auburndale/Havendale and Arrandale/Clarkdale (0x1f, 0x25).
            if model != 0x1f && model != 0x25 {
                fstart_log::error!("ibexpeak: unsupported CPU model {:#x} for CIR", model);
            }
            raw.write32(cir::CIR10, 0xfff80);
            raw.write32(cir::CIR15, 0x7f8f_9fff);
            raw.write32(cir::CIR17, 0x2900);
            raw.write32(cir::CIR19, 0x10001);
            raw.write32(cir::CIR20, 0x1004b);
            raw.write32(cir::CIR21, 0x8);
            0x10000
        };
        // EDS 10.1.77: after every register in 0x3330..0x33d3 and CIR4.
        raw.write32(cir::CIR22, cir22);
    }

    /// coreboot `southbridge_configure_default_intmap()`: linear INTx pins
    /// and PIRQ A..D for the PCH functions, then the I/O APIC enable.
    pub(super) fn configure_default_intmap(&self) {
        let rcba = self.rcba();
        let route = |a: u16, b: u16, c: u16, d: u16| {
            DIR::IAR.val(a) + DIR::IBR.val(b) + DIR::ICR.val(c) + DIR::IDR.val(d)
        };
        // TTIP=D, SIP2=C, SMIP=B, SIP=A.
        rcba.d31ip
            .write(DIP::F6.val(INTD) + DIP::F5.val(INTC) + DIP::F3.val(INTB) + DIP::F2.val(INTA));
        rcba.d29ip.write(DIP::F0.val(INTA));
        rcba.d28ip.write(
            DIP::F7.val(INTD)
                + DIP::F6.val(INTC)
                + DIP::F5.val(INTB)
                + DIP::F4.val(INTA)
                + DIP::F3.val(INTD)
                + DIP::F2.val(INTC)
                + DIP::F1.val(INTB)
                + DIP::F0.val(INTA),
        );
        rcba.d27ip.write(DIP::F0.val(INTA));
        rcba.d26ip.write(DIP::F0.val(INTA));
        rcba.d25ip.write(DIP::F0.val(INTA));
        rcba.d22ip.write(DIP::F0.val(INTA));
        rcba.d20ip.write(DIP::F0.val(INTA));
        rcba.d31ir.write(route(PIRQA, PIRQA, PIRQB, PIRQB));
        rcba.d29ir.write(route(PIRQC, 0, 0, 0));
        rcba.d28ir.write(route(PIRQA, PIRQB, PIRQC, PIRQD));
        rcba.d27ir.write(route(PIRQD, 0, 0, 0));
        rcba.d26ir.write(route(PIRQD, 0, 0, 0));
        rcba.d25ir.write(route(PIRQB, 0, 0, 0));
        rcba.d22ir.write(route(PIRQA, 0, 0, 0));
        rcba.d20ir.write(route(PIRQB, 0, 0, 0));
        // Enable the I/O APIC; the BWG asks for a read back.
        rcba.oic.write(OIC::AEN::SET);
        let _ = rcba.oic.get();
    }

    /// coreboot `early_thermal_init()`: TjMax and thermal reporting through
    /// a temporary thermal BAR, before the ME gets its memory.
    pub(super) fn early_thermal_init(&self) {
        const TEMP_BAR: u32 = 0x4000_0000;
        let thermal = fstart_pci::ecam::EcamDevice::new(0, 0x1f, 6);
        thermal.write32(0x40, TEMP_BAR);
        thermal.write32(0x44, 0);
        thermal.write32(0x40, thermal.read32(0x40) | 5);
        // SAFETY: model 2065x implements MSR_TEMPERATURE_TARGET.
        let tj_max = unsafe {
            use fstart_arch::x86::cpu::intel::model_2065x::{
                TEMPERATURE_TARGET, TemperatureTarget,
            };
            use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
            NativeMsr::new()
                .reg(TEMPERATURE_TARGET)
                .read(TemperatureTarget::TJ_MAX)
        };
        let bar = ThermalBar(TEMP_BAR as usize);
        bar.write16(0x12, (tj_max as u16) << 6);
        // Northbridge temperature slope and offset.
        bar.write16(0x16, 0x7746);
        // Thermal data reporting: processor, PCH and northbridge.
        bar.write16(0x1a, (bar.read16(0x1a) & !0xf) | 0x10f0);
        thermal.write32(0x40, thermal.read32(0x40) & !1);
        thermal.write32(0x40, 0);
    }

    /// coreboot `southbridge_detect_s3_resume()` with the romstage
    /// `GEN_PMCON_2` self-refresh check: a wake from S3 whose memory was
    /// placed in self-refresh. Consumes SLP_TYP.
    pub(super) fn detect_s3(&self) -> bool {
        let lpc = self.lpc_regs();
        let pm = self.pm();
        if pm.read16(pmio::PM1_STS) & pmio::WAK_STS == 0 {
            return false;
        }
        let pm1_cnt = pm.read32(pmio::PM1_CNT);
        if (pm1_cnt >> pmio::SLP_TYP_SHIFT) & 7 != 5 {
            return false;
        }
        pm.write32(pmio::PM1_CNT, pm1_cnt & !pmio::SLP_TYP_MASK);
        if !lpc.gen_pmcon_2.is_set(GEN_PMCON_2::MEM_SR) {
            fstart_log::error!("ibexpeak: bad resume from S3 detected");
            return false;
        }
        true
    }

    /// coreboot `pch_lpc_enable()` and `pch_enable()` for every function:
    /// enabled ones get SERR reporting, disabled ones are quiesced and
    /// hidden through the function-disable registers.
    pub(super) fn enable_functions(&self) {
        let rcba = self.rcba();
        // Enable the PCH display port link.
        rcba.dispbdf.set(0x0010);
        rcba.fd2.modify(FD2::DBDF::SET);
        for (dev, func) in PRESENT_FUNCTIONS {
            let device = fstart_pci::ecam::EcamDevice::new(0, *dev, *func);
            let function = function_of(*dev, *func);
            if function.is_some_and(|function| !self.config.enabled(function)) {
                if device.is_present() {
                    // Memory, I/O and bus master off.
                    device.write16(0x04, device.read16(0x04) & !0x7);
                }
                self.disable_function(function.expect("checked above"));
            } else if device.is_present() {
                device.or16(0x04, 1 << 8); // SERR
            }
        }
    }

    /// coreboot `pch_disable_devfn()`.
    fn disable_function(&self, function: PchFunction) {
        let rcba = self.rcba();
        match function {
            PchFunction::Mei1 => rcba.fd2.modify(FD2::MEI1::SET),
            PchFunction::Mei2 => rcba.fd2.modify(FD2::MEI2::SET),
            PchFunction::Ider => rcba.fd2.modify(FD2::IDER::SET),
            PchFunction::Kt => rcba.fd2.modify(FD2::KT::SET),
            PchFunction::Gbe => rcba.buc.modify(super::regs::BUC::GBE_DISABLE::SET),
            PchFunction::Ehci2 => rcba.fd.modify(FD::EHCI2::SET),
            PchFunction::Hda => rcba.fd.modify(FD::HDA::SET),
            PchFunction::PcieRootPort(port) => {
                rcba.fd.set(rcba.fd.get() | FD::PCIE.val(1 << port).value)
            }
            PchFunction::Ehci1 => rcba.fd.modify(FD::EHCI1::SET),
            PchFunction::Lpc => rcba.fd.modify(FD::LPC::SET),
            PchFunction::Sata1 => rcba.fd.modify(FD::SATA1::SET),
            PchFunction::Smbus => rcba.fd.modify(FD::SMBUS::SET),
            PchFunction::Sata2 => rcba.fd.modify(FD::SATA2::SET),
            PchFunction::Thermal => rcba.fd.modify(FD::THERMAL::SET),
        }
    }

    /// coreboot `enable_hpet()`.
    pub(super) fn enable_hpet(&self) {
        let rcba = self.rcba();
        rcba.hptc
            .modify(HPTC::ENABLE::SET + HPTC::ADDRESS_SELECT.val(0));
        let _ = rcba.hptc.get();
        let hpet = HpetConfig(super::HPET_BASE);
        hpet.setbits32(0x10, 1);
    }
}

/// The first USB port an over-current group maps.
const fn range_start(first_pin: u8) -> usize {
    if first_pin == 0 { 0 } else { 8 }
}

/// Every PCH function `pch_enable()` can see, in devicetree order.
const PRESENT_FUNCTIONS: &[(u8, u8)] = &[
    (0x16, 0),
    (0x16, 1),
    (0x16, 2),
    (0x16, 3),
    (0x19, 0),
    (0x1a, 0),
    (0x1b, 0),
    (0x1c, 0),
    (0x1c, 1),
    (0x1c, 2),
    (0x1c, 3),
    (0x1c, 4),
    (0x1c, 5),
    (0x1c, 6),
    (0x1c, 7),
    (0x1d, 0),
    (0x1e, 0),
    (0x1f, 0),
    (0x1f, 2),
    (0x1f, 3),
    (0x1f, 5),
    (0x1f, 6),
];

fn function_of(dev: u8, func: u8) -> Option<PchFunction> {
    Some(match (dev, func) {
        (0x16, 0) => PchFunction::Mei1,
        (0x16, 1) => PchFunction::Mei2,
        (0x16, 2) => PchFunction::Ider,
        (0x16, 3) => PchFunction::Kt,
        (0x19, 0) => PchFunction::Gbe,
        (0x1a, 0) => PchFunction::Ehci2,
        (0x1b, 0) => PchFunction::Hda,
        (0x1c, port) => PchFunction::PcieRootPort(port),
        (0x1d, 0) => PchFunction::Ehci1,
        (0x1f, 0) => PchFunction::Lpc,
        (0x1f, 2) => PchFunction::Sata1,
        (0x1f, 3) => PchFunction::Smbus,
        (0x1f, 5) => PchFunction::Sata2,
        (0x1f, 6) => PchFunction::Thermal,
        _ => return None,
    })
}

/// The thermal subsystem's temporary MMIO BAR during raminit.
#[derive(Clone, Copy)]
struct ThermalBar(usize);
impl MmioBar for ThermalBar {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x1000)
    }
}

/// The HPET general configuration block.
#[derive(Clone, Copy)]
pub(super) struct HpetConfig(pub(super) usize);
impl MmioBar for HpetConfig {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x400)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn over_current_groups_start_at_their_first_port() {
        assert_eq!(super::range_start(0), 0);
        assert_eq!(super::range_start(4), 8);
    }
}
