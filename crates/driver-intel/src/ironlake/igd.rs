//! Ironlake IGD (coreboot `northbridge/intel/ironlake/gma.c`): bus master,
//! the OpRegion, the native modeset on cold boots, the post-VBIOS panel
//! and backlight programming and the software SCI.

use super::{DEVEN, IntelIronlake};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::Readable;

const IGD_DEV: u8 = 2;
const IGD_MSAC: u16 = 0x62;
/// GTTMMADR: 2 MiB of display MMIO with the GTT in the upper 2 MiB.
const GTTMMADR_SIZE: u32 = 4 << 20;
const GTT_SIZE: u32 = 2 << 20;

pub use crate::igd::pch::PanelPort;

/// Board IGD policy (coreboot `northbridge_intel_ironlake_config` and the
/// mainboard's libgfxinit port list).
pub type IronlakeIgdConfig = crate::igd::pch::PchIgdConfig;

impl IntelIronlake {
    fn igd(&self) -> EcamDevice {
        EcamDevice::new(0, IGD_DEV, 0)
    }

    pub(super) fn igd_enabled(&self) -> bool {
        self.config.igd_enabled
            && self.hostbridge().deven.is_set(DEVEN::IGD)
            && self.igd().is_present()
    }

    /// The start of `gma_func0_init()`: bus master on the assigned windows.
    pub(super) fn igd_func0_init(&mut self) {
        if !self.igd_enabled() {
            return;
        }
        let igd = self.igd();
        self.igd_bars = crate::igd::assigned_bars(&igd, false);
        if self.igd_bars.is_none() {
            fstart_log::error!("ironlake: IGD windows unassigned, skipping graphics init");
            return;
        }
        igd.or16(0x04, (1 << 1) | (1 << 2));
    }

    /// coreboot `gma_pm_init_post_vbios()`.
    fn igd_pm_init_post_vbios(&self, gtt_mmio: u64) {
        self.config.igd.init_panel(gtt_mmio);
    }

    /// coreboot `gma_enable_swsci()`.
    fn igd_enable_swsci(&self) {
        crate::igd::pch::enable_swsci(self.config.pmbase);
    }

    /// The rest of `gma_func0_init()` on a cold boot: the OpRegion, the
    /// native modeset, the post-VBIOS power state and the software SCI.
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
        // The native modeset needs the board's panel delays and PWM setup
        // before it asks the PCH sequencer to power the panel.
        self.igd_pm_init_post_vbios(bars.gtt_mmio);
        if self.config.igd.display.is_some()
            && let Ok(map) = self.read_memory_map()
        {
            let stolen_base = self.hostbridge().igd_base.get() & 0xfff0_0000;
            let addresses = crate::igd::IgdAddresses {
                pci_bdf: crate::gma::types::PciAddress::new(0, 0, IGD_DEV, 0),
                gtt_mmio_base: bars.gtt_mmio,
                gtt_mmio_size: GTTMMADR_SIZE,
                gtt_pte_base: None,
                gmadr_base: Some(bars.gmadr),
                gmadr_size: crate::igd::gmadr_size_from_msac(igd.read8(IGD_MSAC)),
                stolen_base: u64::from(stolen_base),
                stolen_size: map.tolud.saturating_sub(stolen_base),
                gtt_size: GTT_SIZE,
                gcfgc: None,
            };
            self.display.initialize::<crate::gma::Ironlake>(
                crate::gma::types::Cpu::Ironlake,
                self.config.igd.display.as_ref(),
                &addresses,
                vbt,
            );
        }
        self.igd_enable_swsci();
    }

    /// `gma_func0_init()` on S3 resume: no modeset; the OS restores the
    /// screen. ASLS points back at the cold boot's OpRegion.
    pub(super) fn igd_resume(&mut self, opregion: Option<u32>) {
        self.igd_func0_init();
        let Some(bars) = self.igd_bars else {
            return;
        };
        match opregion {
            Some(address) => {
                crate::igd::register_opregion(&self.igd(), crate::igd::OpRegionSci::Swsci, address)
            }
            None => fstart_log::error!("ironlake: no OpRegion to restore on resume"),
        }
        self.igd_pm_init_post_vbios(bars.gtt_mmio);
        self.igd_enable_swsci();
    }
}
