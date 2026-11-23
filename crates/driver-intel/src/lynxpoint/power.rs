//! PCH power optimizer and dynamic clock gating (Lynx Point `lpc.c`).

use super::{
    LynxPoint, hsio, lpc_regs, rcba_regs, rcba_window,
    regs::{BUC, ETR3, FD, dev},
};
use crate::southbridge::{lpc, options, pmio_ich as pmio};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

impl LynxPoint {
    /// Pre-MP power options and initial `PM1_CNT.SCI_EN` state (cleared on cold
    /// boot by `IntelSmm::relocate()` and preserved on S3 resume).
    pub(super) fn power_options(&self) {
        let c = self.config;
        let l_regs = lpc_regs();
        let lp = c.gpio.lp();

        // AFTER_G3 is clear in S0 for both On and Keep. For Keep the S5
        // handler changes it to Off; an outage then restores the old state.
        l_regs.gen_pmcon_3.set(
            (l_regs.gen_pmcon_3.get() & !0x431)
                | 0x1808
                | u16::from(self.options.power_on_after_fail == 0),
        );
        options::nmi(self.options.nmi);
        l_regs.gen_pmcon_1.set(l_regs.gen_pmcon_1.get() & !0x403);
        if !lp {
            l_regs
                .gpio_rout
                .set(crate::southbridge::gpio_ich::encode_gpi_routes(
                    &c.gpi_routing,
                ));
        }
        let pm = self.pm();
        pm.write32(
            pmio::PM1_CNT,
            (pm.read32(pmio::PM1_CNT) & !pmio::SLP_TYP_MASK) | pmio::SCI_EN,
        );
    }

    /// Post-MP / post-`BIOS_RESET_CPL` PCH power management optimizer, PM_SYNC
    /// enable (`RCBA[0x33c8] |= 1<<15`), and dynamic clock gating (coreboot
    /// `lpc_init`: `lpt_lp_pm_init` / `lpt_pm_init` + `enable_*clock_gating`).
    ///
    /// Does not modify `PM1_CNT.SCI_EN`, preserving SMM's cold-boot/S3 state.
    pub(super) fn power_management(&self) -> Result<(), ServiceError> {
        let c = self.config;
        let l = lpc::device();
        let l_regs = lpc_regs();
        let r = rcba_window();
        let r_regs = rcba_regs();
        let lp = c.gpio.lp();

        r.setbits32(0x3310, 0x31);
        r.clrbits16(0x3f02, 15);
        r.setbits32(0x2314, 0x8000_0080);
        r.setbits16(0x1114, 0xc000);
        l_regs.cir4.set(0x46);
        let sata_gating = if c.sata_port_map & 3 == 0 {
            0x0014_0000
        } else {
            0
        } | if c.sata_port_map & 12 == 0 {
            0x0500_0000
        } else {
            0
        };
        let root = EcamDevice::new(0, dev::PCIE, 0);
        if lp {
            r.clrbits32(0x232c, 1);
            r.setbits32(0x1100, 0xc13f);
            r.clrsetbits32(0x2320, 0x60, 0x10);
            for (off, v) in [
                (0x3314, 0x0001_2fff),
                (0x3318, 0x0dcf_0400),
                (0x3324, 0x0400_0000),
                (0x3368, 0x0004_1400),
                (0x3388, 0x3f8d_dbff),
                (0x33ac, 0x0000_7001),
                (0x33b0, 0x0018_1900),
                (0x33c0, 0x0006_0a00),
                (0x33d0, 0x0620_0840),
                (0x3a28, 0x0101_0101),
                (0x3a2c, 0x0404_0404),
                (0x2b1c, 0x0380_8033),
                (0x2b34, 0x8000_0009),
                (0x3348, 0x022d_dfff),
                (0x334c, 1),
                (0x3358, 0x0001_c000),
                (0x3380, 0x3f8d_dbff),
                (0x3384, 0x0001_c7e1),
                (0x338c, 0x0001_c7e1),
                (0x3398, 0x0001_c000),
                (0x33a8, 0x0018_1900),
                (0x33dc, 0x0008_0000),
                (0x33e0, 1),
                (0x3a20, 0x0404),
                (0x3a24, 0x0101_0101),
                (0x3a30, 0x0101_0101),
            ] {
                r.write32(off, v);
            }
            for (off, v) in [(0x410, 3), (0x2618, 0x0800_0000), (0x2300, 2), (0x2600, 8)] {
                r.setbits32(off, v);
            }
            r.write32(0x33b4, 0x7001);
            r.write32(0x3350, 0x022d_dfff);
            r.write32(0x3354, 1);
            r.setbits32(0x33d4, 1 << 27);
            r.setbits32(0x33c8, 128);
            for (off, v) in [
                (0x2b10, 0x883c),
                (0x2b14, 0x1e0a_4616),
                (0x2b24, 0x4000_0005),
                (0x2b20, 0x0005_db01),
                (0x3a80, 0x0514_5005),
            ] {
                r.write32(off, v);
            }
            l_regs.etr3.modify(ETR3::LP_PM_BIT21::SET);
            for (a, clear, set) in [
                (0xed00_015c, 1 << 11, 0x3700),
                (0xed00_0118, 0, 0x00c0_0000),
                (0xed00_0120, 0, 0x0024_0000),
                (0xca00_0000, 0, 9),
            ] {
                hsio::update(a, clear, set);
            }
            r.write32(0x3a84, 0x1005 | sata_gating);
            if r_regs.fd.is_set(FD::ADSP) {
                r.setbits32(0x2b1c, 1 << 29);
            }
            r.setbits32(0x33d4, 0x2fff_2fb1);
            r.setbits32(0x33c8, 1 << 15);
        } else {
            l_regs.etr3.modify(ETR3::CF9LOCK::SET);
            r.write32(0x2238, 0x941);
            r.setbits32(0x232c, 1);
            r.setbits32(0x1100, 3 << 13);
            r.write32(0x2304, 0xc07b_8400);
            r.setbits32(0x2314, (1 << 23) | 32);
            if root.is_present() {
                root.and8_or8(0xf5, !15, 5);
            }
            r.setbits32(0x2320, 2);
            r.write32(0x3314, 0x7bf);
            r.setbits32(0x3318, 0x0dcf_0000);
            r.write32(0x3324, 0x0400_0000);
            r.write32(0x3340, 0x020d_dbff);
            r.setbits32(0x3344, 1);
            for (off, v) in [
                (0x3368, 0x0004_1000),
                (0x3378, 0x3f8d_dbff),
                (0x337c, 0x1e1),
                (0x3388, 0x1000),
                (0x33a0, 0x800),
                (0x33ac, 0x1000),
                (0x33b0, 0x1000),
                (0x33c0, 0x0001_1900),
                (0x33d0, 0x0600_0802),
                (0x3a28, 0x0101_0000),
                (0x3a2c, 0x0101_0404),
            ] {
                r.write32(off, v);
            }
            r.setbits32(0x33a4, 1);
            r.setbits32(0x33d4, 1 << 27);
            r.setbits32(0x33c8, 1 << 27);
            for (off, v) in [
                (0x2b14, 0x1e0a_0317),
                (0x2b24, 0x4000_000b),
                (0x2b28, 2),
                (0x2b2c, 0x8813),
                (0x3a80, 0x0104_0000),
                (0x3a84, 0x0104_1001 | sata_gating),
                (0x3a88, 1),
                (0x33d4, 0xc80b_c000),
            ] {
                r.write32(off, v);
            }
            let el1 = if root.is_present() && root.read8(0xf5) & 1 == 0 {
                2
            } else {
                4
            };
            r.clrsetbits32(0x21a4, 0x3f000, (3 << 12) | (el1 << 15));
            r.clrbits32(0x2348, 15);
            r.clrbits32(0x2304, 1 << 10);
            r.setbits32(0x21a4, 3 << 10);
            r.setbits16(0x21a8, 3);
            r.setbits32(0x2304, 1 << 10);
        }
        r.setbits32(0x2234, 15);
        if lp {
            l_regs
                .gen_pmcon_1
                .set((l_regs.gen_pmcon_1.get() & !0x4800) | 0x30e4);
            l.or32(0x64, 64);
            r.clrsetbits32(0x2614, 0x7400_0000, 0x0a20_6500);
            let gfx = EcamDevice::new(0, 2, 0);
            if gfx.is_present() && gfx.read8(8) >= 11 {
                r.setbits32(0x2614, 1 << 26);
            }
            r.setbits32(0x900, 0x31f);
            r.clrsetbits32(
                0x341c,
                1 << 29,
                0xdc41_0000
                    | if r.read32(0x3454) & 16 == 0 {
                        1 << 29
                    } else {
                        0
                    }
                    | if r_regs.buc.is_set(BUC::GBE_DISABLE) {
                        1 << 23
                    } else {
                        0
                    },
            );
            r.setbits32(0x3434, 7);
            r.setbits32(0x38c0, 0x3c07);
            hsio::update(0xcf00_0000, 0, 0x7001);
            hsio::update(0xce00_c000, 1, 0);
        } else {
            l_regs.gen_pmcon_1.set(l_regs.gen_pmcon_1.get() | 0x5804);
            r.setbits32(0x900, 1 << 14);
            r.setbits32(0x341c, 0x9841_0000);
            r.setbits32(0x38c0, 7);
        }
        Ok(())
    }
}
