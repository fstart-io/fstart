//! Ironlake IGD (coreboot `northbridge/intel/ironlake/gma.c`): bus master,
//! the OpRegion, the native modeset on cold boots, the post-VBIOS panel
//! and backlight programming and the software SCI.

use super::{DEVEN, IntelIronlake};
use crate::igd::{IgdDisplayPolicy, VbtSource};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::Readable;

const IGD_DEV: u8 = 2;
const IGD_MSAC: u16 = 0x62;
/// GTTMMADR: 2 MiB of display MMIO with the GTT in the upper 2 MiB.
const GTTMMADR_SIZE: u32 = 4 << 20;
const GTT_SIZE: u32 = 2 << 20;

/// PCH display registers `gma_pm_init_post_vbios()` writes.
const PCH_PORT_HOTPLUG: u32 = 0xc4030;
const PCH_PP_ON_DELAYS: u32 = 0xc7208;
const PCH_PP_OFF_DELAYS: u32 = 0xc720c;
const PCH_PP_DIVISOR: u32 = 0xc7210;
const BLC_PWM_CPU_CTL2: u32 = 0x48250;
const BLC_PWM_CPU_CTL: u32 = 0x48254;
const BLC_PWM_PCH_CTL1: u32 = 0xc8250;
const BLC_PWM_PCH_CTL2: u32 = 0xc8254;

/// Panel connection (coreboot `gpu_panel_port_select`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPort {
    Lvds = 0,
    /// Embedded DisplayPort on port A.
    DpA = 1,
    DpC = 2,
    DpD = 3,
}

/// Board IGD policy (coreboot `northbridge_intel_ironlake_config` and the
/// mainboard's libgfxinit port list).
#[derive(Debug, Clone, Copy)]
pub struct IronlakeIgdConfig {
    /// Digital port hot-plug: 0 off, 4 = 2 ms, 5 = 4.5 ms, 6 = 6 ms,
    /// 7 = 100 ms pulse.
    pub dp_b_hotplug: u8,
    pub dp_c_hotplug: u8,
    pub dp_d_hotplug: u8,
    pub panel_port: PanelPort,
    /// T4, in 100 ms units; zero leaves PP_DIVISOR.
    pub panel_power_cycle_delay: u8,
    /// T1+T2, in 100 us units.
    pub panel_power_up_delay: u16,
    /// T3.
    pub panel_power_down_delay: u16,
    /// T5.
    pub panel_backlight_on_delay: u16,
    /// Tx.
    pub panel_backlight_off_delay: u16,
    /// CPU backlight PWM value; zero leaves it.
    pub cpu_backlight: u32,
    /// PCH backlight PWM value; zero leaves it.
    pub pch_backlight: u32,
    /// VBT for the OpRegion.
    pub vbt: VbtSource,
    /// Outputs and framebuffer for the native modeset; `None` stays headless.
    pub display: Option<IgdDisplayPolicy>,
}

impl IronlakeIgdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dp_b_hotplug: 0,
            dp_c_hotplug: 0,
            dp_d_hotplug: 0,
            panel_port: PanelPort::Lvds,
            panel_power_cycle_delay: 0,
            panel_power_up_delay: 0,
            panel_power_down_delay: 0,
            panel_backlight_on_delay: 0,
            panel_backlight_off_delay: 0,
            cpu_backlight: 0,
            pch_backlight: 0,
            vbt: VbtSource::LEGACY,
            display: None,
        }
    }
}

impl Default for IronlakeIgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The hot-plug value `gma_pm_init_post_vbios()` writes.
const fn hotplug(config: &IronlakeIgdConfig) -> u32 {
    ((config.dp_b_hotplug as u32 & 7) << 2)
        | ((config.dp_c_hotplug as u32 & 7) << 10)
        | ((config.dp_d_hotplug as u32 & 7) << 18)
}

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
        let conf = &self.config.igd;
        let read = |offset| crate::igd::mmio_read32(gtt_mmio, offset);
        let write = |offset, value| crate::igd::mmio_write32(gtt_mmio, offset, value);
        if read(PCH_PORT_HOTPLUG) == 0 {
            write(PCH_PORT_HOTPLUG, hotplug(conf));
        }
        if read(PCH_PP_ON_DELAYS) == 0 {
            write(
                PCH_PP_ON_DELAYS,
                ((conf.panel_port as u32 & 3) << 30)
                    | (u32::from(conf.panel_power_up_delay & 0x1fff) << 16)
                    | u32::from(conf.panel_backlight_on_delay & 0x1fff),
            );
        }
        if read(PCH_PP_OFF_DELAYS) == 0 {
            write(
                PCH_PP_OFF_DELAYS,
                (u32::from(conf.panel_power_down_delay & 0x1fff) << 16)
                    | u32::from(conf.panel_backlight_off_delay & 0x1fff),
            );
        }
        if conf.panel_power_cycle_delay != 0 {
            write(
                PCH_PP_DIVISOR,
                (read(PCH_PP_DIVISOR) & !0xff) | u32::from(conf.panel_power_cycle_delay),
            );
        }
        if conf.cpu_backlight != 0 {
            write(BLC_PWM_CPU_CTL2, 1 << 31);
            write(BLC_PWM_CPU_CTL, conf.cpu_backlight);
        }
        if conf.pch_backlight != 0 {
            write(BLC_PWM_PCH_CTL1, 1 << 31);
            write(BLC_PWM_PCH_CTL2, conf.pch_backlight);
        }
    }

    /// coreboot `gma_enable_swsci()`: the OpRegion software SCI raises
    /// `\_GPE._L06` through the TCO SCI.
    fn igd_enable_swsci(&self) {
        use crate::ibexpeak::regs::pm;
        use crate::southbridge::pmio_ich::{self as pmio, PmIo};
        let pm_io = PmIo::new(self.config.pmbase);
        let tco = pm_io.tco();
        let dmisci = tco.read16(pmio::TCO1_STS) & pmio::DMISCI_STS as u16;
        tco.write16(pmio::TCO1_STS, dmisci);
        pm_io.write32(pm::GPE0_STS, pm::TCOSCI);
        pm_io.write16(pm::GPE0_EN, pm_io.read16(pm::GPE0_EN) | pm::TCOSCI as u16);
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
        if self.config.igd.display.is_some()
            && let Ok(map) = self.read_memory_map()
        {
            let stolen_base = self.hostbridge().igd_base.get() & 0xfff0_0000;
            let addresses = crate::igd::IgdAddresses {
                pci_bdf: fstart_intel_gma::types::PciAddress::new(0, 0, IGD_DEV, 0),
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
            self.display.initialize(
                fstart_intel_gma::types::Cpu::Ironlake,
                self.config.igd.display.as_ref(),
                &addresses,
                vbt.as_deref(),
            );
        }
        self.igd_pm_init_post_vbios(bars.gtt_mmio);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotplug_matches_coreboot() {
        let config = IronlakeIgdConfig {
            dp_d_hotplug: 6,
            ..IronlakeIgdConfig::new()
        };
        assert_eq!(hotplug(&config), 6 << 18);
    }
}
