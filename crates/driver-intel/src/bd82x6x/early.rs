//! Bootblock and pre-DRAM PCH programming (coreboot `bootblock.c`,
//! `early_pch.c`, `early_usb.c`, `early_thermal.c`, `early_rcba.c` and the
//! model_206ax bootblock's soft-reset straps).

use super::regs::{CIR0, GCS, GEN_PMCON_3, GEN_PMCON_LOCK, UPRWC, UPRWC_OFFSET, VC_STS};
use super::*;
use fstart_core::mmio::MmioReadWrite;
use tock_registers::register_structs;

register_structs! {
    /// The thermal sensor block behind the temporary thermal BAR.
    ThermalRegs {
        (0x00 => _reserved0),
        (0x01 => tsc1: MmioReadWrite<u8>),
        (0x02 => _reserved1),
        (0x04 => tsc: MmioReadWrite<u16>),
        (0x06 => _reserved2),
        (0x0c => tsel: MmioReadWrite<u8>),
        (0x0d => tses: MmioReadWrite<u8>),
        (0x0e => tsgpen: MmioReadWrite<u8>),
        (0x0f => _reserved3),
        (0x12 => tjmax: MmioReadWrite<u16>),
        (0x14 => nb_offset: MmioReadWrite<u16>),
        (0x16 => nb_slope: MmioReadWrite<u16>),
        (0x18 => _reserved4),
        (0x1a => report: MmioReadWrite<u16>),
        (0x1c => _reserved5),
        (0x82 => tspm82: MmioReadWrite<u8>),
        (0x83 => _reserved6),
        (0x84 => @END),
    }
}

/// coreboot `early_pch_init_native` IOBP writes, in order.
const IOBP_NATIVE: [(u32, u32); 59] = [
    (0xea00_7f62, 0x0059_0133),
    (0xec00_7f62, 0x0059_0133),
    (0xec00_7f64, 0x5955_5588),
    (0xea00_40b9, 0x0001_051c),
    (0xeb00_40a1, 0x8000_84ff),
    (0xec00_40a1, 0x8000_84ff),
    (0xea00_4001, 0x0000_8400),
    (0xeb00_4002, 0x4020_1758),
    (0xec00_4002, 0x4020_1758),
    (0xea00_4002, 0x0060_1758),
    (0xea00_40a1, 0x8100_84ff),
    (0xeb00_40b1, 0x0001_c598),
    (0xec00_40b1, 0x0001_c598),
    (0xeb00_40b6, 0x0001_c598),
    (0xea00_00a9, 0x80ff_969f),
    (0xea00_01a9, 0x80ff_969f),
    (0xeb00_40b2, 0x0001_c396),
    (0xeb00_40b3, 0x0001_c396),
    (0xec00_40b2, 0x0001_c396),
    (0xea00_01a9, 0x80ff_94ff),
    (SATA_IOBP_SP0G3IR, 0x0088_037f),
    (0xea00_00a9, 0x80ff_94ff),
    (SATA_IOBP_SP1G3IR, 0x0088_037f),
    (0xea00_7f05, 0x0001_0642),
    (0xea00_40b7, 0x0001_c91c),
    (0xea00_40b8, 0x0001_c91c),
    (0xeb00_40a1, 0x8200_84ff),
    (0xec00_40a1, 0x8200_84ff),
    (0xea00_7f0a, 0xc248_0000),
    (0xec00_404d, 0x01ff_177f),
    (0xec00_0084, 0x5a60_0000),
    (0xec00_0184, 0x5a60_0000),
    (0xec00_0284, 0x5a60_0000),
    (0xec00_0384, 0x5a60_0000),
    (0xec00_0094, 0x000f_0501),
    (0xec00_0194, 0x000f_0501),
    (0xec00_0294, 0x000f_0501),
    (0xec00_0394, 0x000f_0501),
    (0xec00_0096, 0x0000_0001),
    (0xec00_0196, 0x0000_0001),
    (0xec00_0296, 0x0000_0001),
    (0xec00_0396, 0x0000_0001),
    (0xec00_0001, 0x0000_8c08),
    (0xec00_0101, 0x0000_8c08),
    (0xec00_0201, 0x0000_8c08),
    (0xec00_0301, 0x0000_8c08),
    (0xec00_40b5, 0x0001_c518),
    (0xec00_0087, 0x0607_7597),
    (0xec00_0187, 0x0607_7597),
    (0xec00_0287, 0x0607_7597),
    (0xec00_0387, 0x0607_7597),
    (0xea00_0050, 0x00bb_0157),
    (0xea00_0150, 0x00bb_0157),
    (0xec00_7f60, 0x7777_7d77),
    (0xea00_008d, 0x0132_0000),
    (0xea00_018d, 0x0132_0000),
    (0xec00_07b2, 0x0451_4b5e),
    (0xec00_078c, 0x4000_0200),
    (0xec00_0780, 0x0200_0020),
];

/// coreboot `early_usb_init` transmitter gain table (`usb_current_settings`).
const USB_CURRENTS: [u32; 13] = [
    0x2000_0153, // mobile low
    0x2000_0f57, // desktop low
    0x2000_055b, // desktop high
    0x2000_0f51, // desktop medium
    0x2000_094a, // desktop low-medium
    0x2000_035f, // desktop very high
    0x2000_0f53, // mobile medium
    0x2000_0f5b, // desktop high medium
    0x2000_0f57, // reserved
    0x2000_0553, // mobile medium-low
    0x2000_0053, // mobile very low
    0x2000_055f, // mobile high
    0x2000_0f5f, // desktop very-very high
];

impl IntelBd82x6x {
    /// coreboot `early_pch_init` (bootblock half: no GbE strap, no SMBus).
    pub(super) fn early_pch_init(&self) {
        let lpc = self.lpc_regs();
        // COMA 0x3f8, COMB 0x2f8.
        lpc.lpc_io_dec.set(0x0010);
        // CNF2, CNF1, MC, KBC, FDD, LPT, COMB, COMA.
        lpc.lpc_en.set(0x3c0f);
        let windows = self.config.lpc_generic_io.as_slice();
        for (index, reg) in lpc.gen_dec.iter().enumerate() {
            reg.set(
                windows
                    .get(index)
                    .and_then(|window| window.encode())
                    .unwrap_or(0),
            );
        }
        lpc.rcba.set(self.config.rcba as u32 | 1);
        lpc.pmbase.set(u32::from(self.config.pmbase) | 1);
        lpc.acpi_cntl.set(1 << 7);
        lpc.gpiobase.set(u32::from(self.config.gpiobase) | 1);
        lpc.gpio_cntl.set(0x10);
        // pch_generic_setup: no reboot on the second TCO timeout, halt it.
        self.rcba().gcs.modify(GCS::NO_REBOOT::SET);
        self.pm().tco().write16(pmio::TCO1_CNT, 1 << 11);
    }

    /// coreboot `set_spi_speed`.
    pub(super) fn set_spi_speed(&self) {
        let spi = self.spi();
        spi.fdoc.set(0x1000);
        let freq = ((spi.fdod.get() >> 24) & 7) as u8;
        spi.ssfc[2].set((spi.ssfc[2].get() & !7) | freq);
    }

    /// coreboot `pch_enable_gbe`: the BUC GbE disable follows the board; a
    /// change only takes effect after a full reset.
    pub(super) fn enable_gbe(&self) {
        let buc = &self.rcba().buc;
        let wanted = if self.config.enabled(PchFunction::Gbe) {
            buc.get() & !(1 << 5)
        } else {
            buc.get() | 1 << 5
        };
        if buc.get() == wanted {
            return;
        }
        buc.set(wanted);
        if buc.get() != wanted {
            return;
        }
        fstart_log::info!("bd82x6x: GbE enable changed, full reset");
        fstart_arch::x86::boot::full_reset();
    }

    /// model_206ax `set_soft_reset`: the configurable-TDP flex ratio and the
    /// hyper-threading choice latch through the soft-reset data register;
    /// a change needs one CPU-only reset.
    pub(super) fn cpu_soft_reset(&self) {
        use fstart_arch::x86::cpu::intel::model_206ax;
        let ratio = model_206ax::set_flex_ratio_to_tdp_nominal();
        let spi = self.spi();
        let mut data = spi.soft_reset_data.get();
        if ratio != 0 {
            data = (data & !(0x3f << 6)) | (u32::from(ratio) & 0x3f) << 6;
        }
        if model_206ax::ht_supported() {
            data = (data & !1) | u32::from(!self.config.hyper_threading);
        }
        if spi.soft_reset_data.get() == data {
            spi.soft_reset_lock.set(spi.soft_reset_lock.get() | 1);
            return;
        }
        spi.soft_reset_data.set(data);
        spi.soft_reset_ctrl.set(spi.soft_reset_ctrl.get() | 1);
        fstart_arch::x86::boot::system_reset(true);
    }

    /// coreboot `early_usb_init`.
    pub(super) fn early_usb_init(&self) {
        let r = self.rcba();
        let mobile = self.is_mobile();
        let rmw = |reg: &MmioReadWrite<u32>, and: u32, or: u32| reg.set((reg.get() & and) | or);
        rmw(&r.usb_3560, !0, 0x020c_8000);
        rmw(&r.usb_3564, !0x0000_135c, 0x0000_04a3);
        rmw(
            &r.usb_3570,
            !0x0400_0111,
            if mobile { 0x0200_0771 } else { 0x0200_0661 },
        );
        rmw(
            &r.usb_357c,
            !0x0006_03c0,
            if mobile { 0x0000_1630 } else { 0x0000_1530 },
        );
        rmw(&r.usb_3580, !0x3, 0);
        rmw(&r.usb_3598, !0x1, 0);
        // The USB per-port registers are write-protected by UPRWC.
        self.pm().write16(
            UPRWC_OFFSET,
            self.pm().read16(UPRWC_OFFSET) | UPRWC::WR_EN::SET.value,
        );
        for (index, port) in self.config.usb_ports.iter().enumerate() {
            let value = match USB_CURRENTS.get(usize::from(port.current)) {
                None => 0x2000_0000 | (u32::from(port.current) & 0xfff),
                Some(&gain) if port.enabled && !mobile && gain == USB_CURRENTS[0] => {
                    fstart_log::error!(
                        "bd82x6x: USB{}: mobile-low gain is invalid on desktop",
                        index
                    );
                    USB_CURRENTS[1]
                }
                Some(&gain) => gain,
            };
            r.usbir[index].set(value);
        }
        let disabled = self
            .config
            .usb_ports
            .iter()
            .enumerate()
            .filter(|(_, port)| !port.enabled)
            .fold(0u32, |bits, (index, _)| bits | 1 << index);
        r.usbpdo.set(disabled & 0x3fff);
        // OC pins 0..3 map ports 0..7, OC pins 4..7 ports 8..13.
        let mut ocm = [0u32; 2];
        for (index, port) in self.config.usb_ports.iter().enumerate() {
            let Some(pin) = port.oc_pin.filter(|_| port.enabled) else {
                continue;
            };
            let (reg, base_pin, base_port) = if index < 8 { (0, 0, 0) } else { (1, 4, 8) };
            if (base_pin..base_pin + 4).contains(&pin) {
                ocm[reg] |= 1 << (index - base_port + 8 * usize::from(pin - base_pin));
            }
        }
        r.usbocm1.set(ocm[0]);
        r.usbocm2.set(ocm[1]);
        // xHCI USB 2.0 port routing to EHCI.
        let xhci = EcamDevice::from_devfn(0, dev::XHCI);
        if xhci.is_present() {
            xhci.write32(0xe4, 0);
        }
        self.pm().write16(UPRWC_OFFSET, 0);
    }

    /// coreboot `early_pch.c` `write_iobp`: the raw pre-DRAM sequence.
    fn write_iobp(&self, address: u32, value: u32) {
        let r = self.rcba();
        let kick = || r.iobps.set((r.iobps.get() & 0x1ff) | 0x600);
        r.iobpiri.set(address);
        kick();
        self.wait_iobp();
        let _ = r.iobpd.get();
        self.wait_iobp();
        let _ = r.iobps.get();
        kick();
        self.wait_iobp();
        r.iobpd.set(value);
        self.wait_iobp();
        kick();
        let _ = r.iobps.get();
    }

    /// coreboot `early_pch_init_native`.
    pub(super) fn early_pch_init_native(&self) {
        for func in 0..self.root_ports() {
            let port = EcamDevice::new(0, 0x1c, func);
            port.write32(0x338, port.read32(0x338) & !(1 << 26));
        }
        self.lpc_regs()
            .gen_pmcon_lock
            .modify(GEN_PMCON_LOCK::ACPI_BASE_LOCK::SET);
        let r = self.rcba();
        r.cir1.set(0x0010_9000);
        r.rec.set(r.rec.get() | 1 << 30);
        r.cir_100c.set(0x0111_0000);
        // Gen 1 common-clock N_FTS: a byte write.
        r.cir_2340.set((r.cir_2340.get() & !0xff) | 0x1b);
        r.cir6.set(r.cir6.get() | 1 << 21);
        r.cir_2310.set((r.cir_2310.get() & !(3 << 29)) | 1 << 29);
        r.dmc2.set(0x0085_4c74);
        r.cir_2310.set(r.cir_2310.get() & !(3 << 22));
        r.cir_2310.set(r.cir_2310.get() & !(3 << 20));
        for (address, value) in IOBP_NATIVE {
            self.write_iobp(address, value);
        }
    }

    /// coreboot `early_pch_init_native_dmi_pre`.
    pub(super) fn dmi_pre(&self) {
        let r = self.rcba();
        // L0s and L1 supported; L0s 128-256 ns, L1 2-4 us.
        r.lcap
            .set((r.lcap.get() & !0x3fc00) | 3 << 10 | 2 << 12 | 2 << 15);
        r.cir_2340
            .set((r.cir_2340.get() & !0x00ff_0000) | 0x3a << 16);
        r.dlctl2.set((r.dlctl2.get() & !0xf) | 2);
    }

    /// coreboot `early_pch_init_native_dmi_post`: the PCH virtual channels,
    /// matching the system agent's DMIBAR.
    pub(super) fn dmi_post(&self) -> Result<(), ServiceError> {
        let r = self.rcba();
        let cir0 = (r.cir0.get() & !(0xff << 16 | 0xfff)) | 0x654 | 0x20 << 16 | 0x0a << 16;
        r.cir0.set(cir0);
        let _ = r.cir0.get();
        r.updcr.set(r.updcr.get() | 1 << 2 | 1);
        // VC0: TC0/3/4; VC1: TC1/5; VCp: TC2/6; VCm: TC7.
        r.v0ctl.set(1 << 31 | 0x0c << 1 | 1);
        r.v1ctl.set(1 << 31 | 1 << 24 | 0x11 << 1);
        let _ = r.v1ctl.get();
        r.cir31.set(1 << 31 | 2 << 24 | 0x22 << 1);
        let _ = r.cir31.get();
        r.cir32.set(1 << 31 | 7 << 24 | 0x40 << 1);
        r.cir0.modify(CIR0::TC_LOCKDOWN::SET);
        let _ = r.cir0.get();
        for status in [&r.v0sts, &r.v1sts, &r.vcpsts, &r.vcmsts] {
            if !fstart_arch::x86::wait_us(100_000, || !status.is_set(VC_STS::NEGOTIATION_PENDING)) {
                fstart_log::error!("bd82x6x: DMI VC negotiation timeout");
                return Err(ServiceError::Timeout);
            }
        }
        Ok(())
    }

    /// coreboot `early_thermal_init`, before the ME gets its memory.
    pub(super) fn early_thermal_init(&self) {
        const TBARB_TEMP: u32 = 0x4000_0000;
        let thermal = EcamDevice::new(0, 0x1f, 6);
        thermal.write32(0x40, TBARB_TEMP);
        thermal.write32(0x44, 0);
        thermal.or32(0x40, 5);
        // SAFETY: the thermal BAR decodes at TBARB_TEMP until cleared below.
        let tbar = unsafe { &*(TBARB_TEMP as usize as *const ThermalRegs) };
        tbar.tsc.set(0x3a2b);
        tbar.tsel.set(0xff);
        tbar.tses.set(0x00);
        tbar.tsgpen.set(0x40);
        tbar.tspm82.set(0x00);
        tbar.tsc1.set(0xba);
        let tjmax = u16::from(fstart_arch::x86::cpu::intel::model_206ax::tjmax());
        tbar.tjmax.set(tjmax << 6);
        tbar.nb_slope.set(0x808c);
        tbar.nb_offset.set(0xde87);
        // Thermal data reporting: processor, PCH and northbridge.
        tbar.report.set((tbar.report.get() & !0xf) | 0x10f0);
        thermal.and32(0x40, !1);
        thermal.write32(0x40, 0);
        let fdoc = &self.spi().fdoc;
        fdoc.set((fdoc.get() & 0xffff_8003) | 0x403c);
    }

    /// coreboot `southbridge_detect_s3_resume`.
    pub(super) fn detect_s3_resume(&self) -> bool {
        if self
            .lpc_regs()
            .gen_pmcon_3
            .is_set(GEN_PMCON_3::RTC_POWER_FAILED)
        {
            return false;
        }
        if self.pm().read16(pmio::PM1_STS) & pmio::WAK_STS == 0 {
            return false;
        }
        self.pm().sleep_type() == 5
    }

    /// coreboot `southbridge_configure_default_intmap`.
    pub(super) fn configure_default_intmap(&self) {
        const INTA: u32 = 1;
        const INTB: u32 = 2;
        const INTC: u32 = 3;
        const INTD: u32 = 4;
        /// `DIR_ROUTE(a, b, c, d)` with PIRQA..D = 0..3.
        const fn route(a: u16, b: u16, c: u16, d: u16) -> u16 {
            d << 12 | c << 8 | b << 4 | a
        }
        let r = self.rcba();
        r.d31ip
            .set(INTD << 24 | INTC << 20 | INTB << 12 | INTA << 8);
        r.d29ip.set(INTA);
        r.d28ip.set(
            INTD << 28
                | INTC << 24
                | INTB << 20
                | INTA << 16
                | INTD << 12
                | INTC << 8
                | INTB << 4
                | INTA,
        );
        r.d27ip.set(INTA);
        r.d26ip.set(INTA);
        r.d25ip.set(INTA);
        r.d22ip.set(INTA);
        r.d20ip.set(INTA);
        r.d31ir.set(route(0, 0, 1, 1));
        r.d29ir.set(route(2, 0, 0, 0));
        r.d28ir.set(route(0, 1, 2, 3));
        r.d27ir.set(route(3, 0, 0, 0));
        r.d26ir.set(route(3, 0, 0, 0));
        r.d25ir.set(route(1, 0, 0, 0));
        r.d22ir.set(route(0, 0, 0, 0));
        r.d20ir.set(route(1, 0, 0, 0));
        // Enable the I/O APIC; the BWG asks for a read back.
        r.oic.set(0x0100);
        let _ = r.oic.get();
    }
}
