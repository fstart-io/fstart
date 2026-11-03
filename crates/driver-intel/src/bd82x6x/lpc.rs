//! The LPC bridge's ramstage init (coreboot `lpc.c` `lpc_init`).

use super::regs::{GEN_PMCON_1, GEN_PMCON_3, HPTC};
use super::*;
use crate::southbridge::gpio_ich::encode_gpi_routes;
use fstart_core::mmio::MmioReadWrite;

fn or32(reg: &MmioReadWrite<u32>, bits: u32) {
    reg.set(reg.get() | bits);
}

fn and_or32(reg: &MmioReadWrite<u32>, and: u32, or: u32) {
    reg.set((reg.get() & and) | or);
}

impl IntelBd82x6x {
    pub(super) fn lpc_init(&self) {
        let lpc = self.lpc_regs();
        fstart_log::info!(
            "bd82x6x: PCH {:#06x} rev {:#x}",
            self.device_id(),
            self.pch_revision()
        );
        // pch_enable_ioapic: unique requester ID for the I/O APIC; the
        // platform's I/O APIC setup locks the entries and routes GSI0.
        lpc.ibdf
            .set(u16::from(IOAPIC_BUS) << 8 | u16::from(IOAPIC_DEV) << 3);
        // pch_enable_serial_irqs: 21-frame packets, continuous for one frame.
        lpc.serirq_cntl.set(1 << 7 | 1 << 6 | (21 - 17) << 2);
        if !self.config.serirq_continuous {
            lpc.serirq_cntl.set(1 << 7 | (21 - 17) << 2);
        }
        // pch_pirq_init: IRQ 11 for every PIRQ.
        for pirq in lpc.pirq_abcd.iter().chain(lpc.pirq_efgh.iter()) {
            pirq.set(11);
        }
        self.power_options();
        match self.pch_type() {
            PCH_TYPE_CPT => self.pm_init(false),
            PCH_TYPE_PPT => self.pm_init(true),
            _ => fstart_log::error!("bd82x6x: unknown chipset {:#x}", self.device_id()),
        }
        // Like coreboot, not on resume: the OS owns the clock and its alarm.
        if !self.resume {
            let pmcon3 = &lpc.gen_pmcon_3;
            let battery_dead = pmcon3.is_set(GEN_PMCON_3::RTC_BATTERY_DEAD);
            if battery_dead {
                pmcon3.modify(GEN_PMCON_3::RTC_BATTERY_DEAD::CLEAR);
            }
            fstart_driver_rtc::init_clock("bd82x6x", battery_dead);
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fstart_arch::x86::legacy_pc::initialize_isa_dma();
        self.enable_hpet();
        self.enable_clock_gating();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        // SAFETY: legacy PIC and ELCR ports owned by firmware here.
        unsafe {
            fstart_arch::x86::legacy_pc::initialize_pic(0x20, 0x28);
            fstart_core::pio::outb(0x21, 0xfb);
            fstart_core::pio::outb(0xa1, 0xff);
            // IRQ 9 (SCI) is level triggered.
            let elcr2 = fstart_core::pio::inb(0x4d1);
            fstart_core::pio::outb(0x4d1, elcr2 | 1 << 1);
        }
        self.fixups();
        self.spi()
            .set_vscc(self.config.spi_uvscc, self.config.spi_lvscc);
    }

    /// coreboot `pch_power_options`.
    fn power_options(&self) {
        let lpc = self.lpc_regs();
        let mut pmcon3 = lpc.gen_pmcon_3.extract();
        // AFTER_G3 set means stay off.
        pmcon3.modify(GEN_PMCON_3::AFTER_G3.val(u16::from(self.options.power_on_after_fail == 0)));
        // SLP_S4# stretch 4-5 s enabled, SLP_S3# minimum 50 ms, no stretch
        // after the SUS well.
        pmcon3.modify(
            GEN_PMCON_3::SLP_S4_MIN.val(0)
                + GEN_PMCON_3::SLP_S4_STRETCH_EN::SET
                + GEN_PMCON_3::SLP_S3_MIN.val(2)
                + GEN_PMCON_3::DISABLE_SLP_STRETCH_SUS::SET,
        );
        lpc.gen_pmcon_3.set(pmcon3.get());
        fstart_log::info!(
            "bd82x6x: power {} after power failure",
            match self.options.power_on_after_fail {
                0 => "off",
                1 => "on",
                _ => "state keep",
            }
        );
        crate::southbridge::options::nmi(self.options.nmi);
        // SMI# rate 1 minute; native PME (no BIOS_PCI_EXP_EN).
        lpc.gen_pmcon_1
            .modify(GEN_PMCON_1::SMI_RATE.val(0) + GEN_PMCON_1::BIOS_PCI_EXP_EN::CLEAR);
        lpc.gpio_rout
            .set(encode_gpi_routes(&self.config.gpi_routing));
        let pm = self.pm();
        pm.write32(
            crate::southbridge::smi::ICH8_GPE0.en_offset,
            self.config.gpe0_en,
        );
        pm.write16(pmio::ALT_GP_SMI_EN, self.config.alt_gp_smi_en);
        // SCI_EN set later by the platform after SMM installation; clear
        // SLP_TYP here.
        let pm1 = pm.read32(pmio::PM1_CNT) & !pmio::SLP_TYP_MASK;
        pm.write32(pmio::PM1_CNT, pm1);
        // Clear the magic status bits that would cause an unexpected wake.
        let r = self.rcba();
        or32(&r.prsts, 1 << 4 | 1 << 5 | 1);
        r.cir_3f02.set(r.cir_3f02.get() & !0xf);
    }

    /// coreboot `cpt_pm_init` / `ppt_pm_init`.
    fn pm_init(&self, ppt: bool) {
        let r = self.rcba();
        self.lpc_regs().cxstate_cnf.set(0x47);
        or32(&r.cir30, if ppt { 1 } else { 1 << 6 | 1 });
        or32(&r.cir5, 1);
        r.cir3.set(r.cir3.get() | 1 << 13 | 1 << 14);
        r.cir2.set(r.cir2.get() | 1 << 14);
        r.dmc.set(if ppt { 0xc03b_8400 } else { 0xc038_8400 });
        or32(&r.cir6, 1 << 5 | 1 << 18);
        or32(&r.cir9, 1 << 15 | 1 << 1);
        and_or32(&r.cir7, !0x1f, 0xf);
        r.pm_cfg.set(if ppt { 0x054f_0000 } else { 0x050f_0000 });
        r.cir8.set(0x0400_0000);
        or32(&r.cir10, 0xfffff);
        or32(&r.cir11, if ppt { 1 << 1 | 1 } else { 1 << 1 });
        r.cir12.set(0x0001_c000);
        r.cir14.set(0x0006_1100);
        r.cir15.set(0x7f8f_dfff);
        r.cir13.set(if ppt { 0x0000_03fd } else { 0x0000_03fc });
        r.cir16.set(0x0000_1000);
        r.cir18.set(0x0001_c000);
        r.cir17.set(0x0000_0800);
        r.cir23.set(0x0000_1000);
        r.cir19.set(0x0009_3900);
        r.cir20.set(0x2465_3002);
        r.cir21.set(if ppt { 0x0673_88fe } else { 0x0621_08fe });
        and_or32(&r.cir22, 0xf000_f000, 0x0067_0060);
        r.cir24.set(0x0101_0000);
        r.cir25.set(0x0101_0404);
        r.cir27.set(if ppt { 0x0104_0000 } else { 0x0104_1041 });
        and_or32(&r.cir28, !0x0000_ffff, 0x0000_1001);
        // SATA 2/3 and 4/5 disabled.
        or32(&r.cir28, 1 << 24);
        or32(&r.cir29, 1);
        r.cir26.set(0x0000_0001);
        and_or32(&r.cir_2344, 0x00ff_ff00, 0xff00_000c);
        and_or32(&r.cir_80c, !(0xff << 20), 0x11 << 20);
        if ppt {
            or32(&r.cir_33a4, 1);
        }
        r.pmsync_cfg.set(0);
        or32(&r.dlctl2, 0xf);
    }

    /// coreboot `enable_hpet`.
    fn enable_hpet(&self) {
        let lpc = self.lpc_regs();
        for (index, bdf) in lpc.hpet_bdf.iter().enumerate() {
            bdf.set(u16::from(HPET_BUS) << 8 | u16::from(HPET_DEV) << 3 | index as u16);
        }
        self.rcba()
            .hptc
            .modify(HPTC::ENABLE::SET + HPTC::ADDRESS_SELECT.val(0));
    }

    /// coreboot `enable_clock_gating`.
    fn enable_clock_gating(&self) {
        let r = self.rcba();
        or32(&r.dmic, 0xf);
        let lpc = self.lpc_regs();
        let mobile = self.mobile_platform();
        lpc.gen_pmcon_1.modify(
            GEN_PMCON_1::CLKRUN_EN.val(u16::from(mobile))
                + GEN_PMCON_1::PSEUDO_CLKRUN_EN.val(u16::from(!mobile))
                + GEN_PMCON_1::BIT11::SET,
        );
        self.iobp_update(0xeb00_7f07, !0, 1 << 31);
        self.iobp_update(0xeb00_4000, !0, 1 << 7);
        self.iobp_update(0xec00_7f07, !0, 1 << 31);
        self.iobp_update(0xec00_4000, !0, 1 << 7);
        let cg = (r.cg.get()
            | 1 << 31
            | 1 << 29
            | 1 << 28
            | 0xf << 24
            | 1 << 16
            | 1 << 17
            | 1 << 18
            | 1 << 22
            | 1 << 23
            | 1 << 19
            | 1
            | 0xf << 1)
            & !(1 << 20);
        r.cg.set(cg);
        or32(&r.spi.afc, 0x7);
        or32(&r.cir_36d4, 0x6680_c004);
        or32(&r.usb_3564, 0x3);
    }

    /// coreboot `pch_fixups`.
    fn fixups(&self) {
        // DRAM init done, for the S3 path.
        let lpc = self.lpc_regs();
        lpc.gen_pmcon_2.set(lpc.gen_pmcon_2.get() | 1 << 7);
        // DMI ASPM in the PCH.
        let r = self.rcba();
        r.dmc.set(r.dmc.get() & !(1 << 10));
        or32(&r.lcap, 1 << 11 | 1 << 10);
        r.lctl.set(r.lctl.get() | 3);
    }
}
