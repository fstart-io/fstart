//! Sandy Bridge / Ivy Bridge IGD (coreboot `northbridge/intel/sandybridge/
//! gma.c`): GT power management (RC6, turbo, power meter weights), panel
//! power sequencing and backlight on the PCH, the OpRegion, native graphics
//! through the shared GMA library, and the graphics SWSCI.

use super::{IntelSandybridge, powermeter};
use crate::igd::IgdBars;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::Readable;

const IGD_DEV: u8 = 2;
const IGD_MSAC: u16 = 0x62;
/// GTTMMADR: 2 MiB MMIO plus the 2 MiB GTT.
const GTTMMADR_SIZE: u32 = 4 << 20;
const GTT_OFFSET: u64 = 2 << 20;
const GTT_SIZE: u32 = 2 << 20;
const GTT_RETRY: u32 = 1000;

pub use crate::igd::pch::PanelPort;

/// Board IGD policy (coreboot `northbridge_intel_sandybridge_config` GPU
/// fields and the mainboard's libgfxinit port list).
pub type SandybridgeIgdConfig = crate::igd::pch::PchIgdConfig;

/// GTTMMADR register access (coreboot `gtt_read`/`gtt_write`/`gtt_poll`).
#[derive(Clone, Copy)]
struct Gtt(u64);

impl Gtt {
    fn read(self, reg: u32) -> u32 {
        crate::igd::mmio_read32(self.0, reg)
    }
    fn write(self, reg: u32, value: u32) {
        crate::igd::mmio_write32(self.0, reg, value);
    }
    fn modify(self, reg: u32, clear: u32, set: u32) {
        self.write(reg, (self.read(reg) & !clear) | set);
    }
    fn poll(self, reg: u32, mask: u32, value: u32) -> bool {
        for _ in 0..GTT_RETRY {
            if self.read(reg) & mask == value {
                return true;
            }
            fstart_arch::udelay(10);
        }
        fstart_log::error!("sandybridge: GT init timeout");
        false
    }
    fn powermeter(self, table: &[(u32, u32)]) {
        for &(reg, value) in table {
            self.write(reg, value);
        }
    }
}

impl IntelSandybridge {
    fn igd(&self) -> EcamDevice {
        EcamDevice::new(0, IGD_DEV, 0)
    }

    /// coreboot only initializes a decoded IGD.
    pub(super) fn igd_enabled(&self) -> bool {
        self.config.devices.igd
            && self.host().deven.is_set(super::regs::DEVEN::IGD)
            && self.igd().is_present()
    }

    fn stepping(&self) -> u32 {
        self.cpuid() & 0xf
    }

    /// coreboot `gma_pm_init_pre_vbios`.
    fn gma_pm_init_pre_vbios(&self, gtt: Gtt) {
        let sandy = self.is_sandy();
        let mch = self.mch();
        // IVB C0: 4
        let old_force_wake = sandy || self.stepping() < 4;
        if old_force_wake {
            gtt.write(0xa18c, 1);
            gtt.poll(0x130090, 1, 1);
        } else {
            gtt.write(0xa180, 1 << 5);
            gtt.write(0xa188, 0xffff_0001);
            gtt.poll(0x130040, 1, 1);
        }
        if sandy {
            // GTT+0x42004 [15:14] = 11 (SNB C1+).
            gtt.modify(0x42004, 0, 1 << 14 | 1 << 15);
        } else {
            // Display reset acknowledge settings.
            gtt.modify(0x45010, 0, 1 << 1 | 1);
        }
        let gt1 = gtt.read(0x911c) & (1 << 13) != 0;
        if sandy {
            gtt.powermeter(if gt1 {
                &powermeter::SNB_PM_GT1
            } else {
                &powermeter::SNB_PM_GT2
            });
        } else if gt1 {
            gtt.powermeter(&powermeter::IVB_PM_GT1);
        } else {
            let unit = mch.pkg_power_sku_unit.get() & 0xf;
            let tdp = (mch.pkg_power_sku.get() & 0x7fff) / (1 << unit);
            gtt.powermeter(if tdp <= 17 {
                &powermeter::IVB_PM_GT2_17W
            } else {
                &powermeter::IVB_PM_GT2_35W
            });
        }
        // Gear ratio map, GFXPAUSE, dynamic EU trip control.
        gtt.write(0xa004, 0x0000_0010);
        gtt.write(0xa000, 0x0007_0020);
        gtt.write(0xa080, 0x0000_0004);
        // ECO bits; bit 20 for SNB D1+ (stepping 6) / IVB.
        let mut eco = gtt.read(0xa180) | 1 << 26 | 1 << 31;
        if !sandy || self.stepping() >= 6 {
            eco |= 1 << 20;
        }
        gtt.write(0xa180, eco);
        // SNB D2+ (stepping 7).
        if sandy && self.stepping() >= 7 {
            gtt.modify(0x9400, 0, 1 << 7);
            let reg = (gtt.read(0x941c) & 0xf) | 1 << 1;
            gtt.write(0x941c, reg);
            gtt.poll(0x941c, 1 << 1, 0);
        }
        if sandy {
            gtt.write(0x9424, 0);
        } else {
            gtt.modify(0x907c, 0, 1 << 16);
            gtt.write(0x9424, 1);
        }
        // RC6 VID through the pcode mailbox.
        if gtt.poll(0x138124, 1 << 31, 0) {
            gtt.write(0x138128, 0x0000_0029);
            gtt.write(0x138124, 0x8000_0004);
            if gtt.poll(0x138124, 1 << 31, 0) {
                gtt.write(0x138124, 0x8000_000a);
            }
            gtt.poll(0x138124, 1 << 31, 0);
        }
        for (reg, value) in [
            // RC control and wake rate limits.
            (0xa090, 0x0000_0000),
            (0xa098, 0x03e8_0000),
            (0xa09c, 0x0028_001e),
            (0xa0a0, 0x0000_001e),
            (0xa0a8, 0x0001_e848),
            (0xa0ac, 0x0000_0019),
            // Render, video and blitter idle max count.
            (0x2054, 0x0000_000a),
            (0x12054, 0x0000_000a),
            (0x22054, 0x0000_000a),
            // RC thresholds.
            (0xa0b0, 0x0000_0000),
            (0xa0b4, 0x0000_03e8),
            (0xa0b8, 0x0000_c350),
            (0xa0bc, 0x0001_86a0),
            (0xa0c0, 0x0000_fa00),
            // RP timeouts, limits and thresholds.
            (0xa010, 0x000f_4240),
            (0xa014, 0x1206_0000),
            (0xa02c, 0x0001_5f90),
            (0xa030, 0x0001_86a0),
            (0xa068, 0x0001_86a0),
            (0xa06c, 0x0004_93e0),
            (0xa070, 0x0000_000a),
            // Render standby (RC6); deep RC6 is left to the OS.
            (0xa090, 0x8804_0000),
        ] {
            gtt.write(reg, value);
        }
        // Normal frequency request from MCHBAR 0x5998[22:16].
        let rpn = (mch.rp_state_cap.get() >> 16) & 0x7f;
        gtt.write(0xa008, rpn << 25);
        // RP control and PM interrupts.
        gtt.write(0xa024, 0x0000_0592);
        gtt.write(0x4402c, 0x0300_0076);
        gtt.modify(0x6c024, 0x0000_01c0, 0);
        // DP buffer translation defaults.
        for (i, value) in [
            0x0100_030c,
            0x00b8_230c,
            0x06f8_930c,
            0x05f8_e38e,
            0x00b8_030c,
            0x0b78_830c,
            0x09f8_d3cf,
            0x01e8_030c,
            0x09f8_63cf,
            0x0ff8_03cf,
        ]
        .into_iter()
        .enumerate()
        {
            gtt.write(0xe4f00 + 4 * i as u32, value);
        }
    }

    /// coreboot `gma_pm_init_post_vbios`.
    fn gma_pm_init_post_vbios(&self, gtt: Gtt) {
        if self.is_sandy() || self.stepping() < 4 {
            gtt.modify(0xa18c, 1, 0);
            gtt.poll(0x130090, 1, 0);
        } else {
            gtt.write(0xa188, 0x1fffe);
            if gtt.poll(0x130040, 1, 0) {
                gtt.modify(0xa188, 0, 1);
            }
        }
        // SW RC control.
        gtt.write(0xa094, 0x0006_0000);
        // Hot-plug, panel power sequencing and the backlight PWMs.
        self.config.igd.init_panel(gtt.0);
    }

    /// coreboot `gma_func0_init` up to the OpRegion and modeset, which
    /// need verified boot media. `resume` skips nothing here: coreboot
    /// reruns the power-management init on S3 resume too.
    pub(super) fn igd_func0_init(&mut self, resume: bool) {
        let _ = resume;
        if !self.igd_enabled() {
            return;
        }
        let igd = self.igd();
        self.igd_bars = crate::igd::assigned_bars(&igd, false);
        let Some(IgdBars { gtt_mmio, .. }) = self.igd_bars else {
            fstart_log::error!("sandybridge: IGD windows unassigned, skipping graphics init");
            return;
        };
        let gtt = Gtt(gtt_mmio);
        self.gma_pm_init_pre_vbios(gtt);
        // Bus master (and memory decode for the MMIO window).
        igd.or16(0x04, 1 << 1 | 1 << 2);
        crate::igd::clear_gtt_table(gtt_mmio + GTT_OFFSET, GTT_SIZE);
        self.gma_pm_init_post_vbios(gtt);
        crate::igd::pch::enable_swsci(self.config.pmbase);
    }

    /// S3 resume: re-point ASLS at the cold boot's OpRegion.
    pub(super) fn igd_restore_opregion(&self, opregion: Option<u32>) {
        if self.igd_bars.is_none() {
            return;
        }
        match opregion {
            Some(address) => {
                crate::igd::register_opregion(&self.igd(), crate::igd::OpRegionSci::Swsci, address)
            }
            None => fstart_log::error!("sandybridge: no OpRegion to restore on resume"),
        }
    }

    /// The OpRegion and, when the board asked for one and legacy VGA decode
    /// is on, the native modeset (coreboot `gma_gfxinit`).
    pub(super) fn igd_publish_and_modeset(&mut self, opregion: crate::igd::OpRegionMemory<'_>) {
        let Some(bars) = self.igd_bars else {
            return;
        };
        let igd = self.igd();
        let vbt = crate::igd::publish_opregion(
            &igd,
            crate::igd::OpRegionSci::Swsci,
            &self.config.igd.vbt,
            opregion,
        );
        if self.config.igd.display.is_none() {
            return;
        }
        if self.host().ggc.is_set(super::regs::GGC::IVD) {
            fstart_log::info!("sandybridge: IGD does not decode VGA, skipping native graphics");
            return;
        }
        let stolen_base = self.host().bdsm.get() & 0xfff0_0000;
        let ggc = self.host().ggc.extract();
        let stolen_size = u32::from(ggc.read(super::regs::GGC::GMS)) * (32 << 20);
        let cpu = if self.is_sandy() {
            crate::gma::types::Cpu::Sandybridge
        } else {
            crate::gma::types::Cpu::Ivybridge
        };
        // libgfxinit reaches the GTT through GTTMMADR; PGETBL is not used
        // on this generation.
        let addresses = crate::igd::IgdAddresses {
            pci_bdf: crate::gma::types::PciAddress::new(0, 0, IGD_DEV, 0),
            gtt_mmio_base: bars.gtt_mmio,
            gtt_mmio_size: GTTMMADR_SIZE,
            gtt_pte_base: None,
            gmadr_base: Some(bars.gmadr),
            gmadr_size: crate::igd::gmadr_size_from_msac(igd.read8(IGD_MSAC)),
            stolen_base: u64::from(stolen_base),
            stolen_size,
            gtt_size: GTT_SIZE,
            gcfgc: None,
        };
        self.display.initialize::<crate::gma::Ironlake>(
            cpu,
            self.config.igd.display.as_ref(),
            &addresses,
            vbt,
        );
    }
}
