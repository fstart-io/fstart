//! The IGD of the CPUs whose display ports sit in the PCH (Ironlake, Sandy
//! Bridge, Ivy Bridge): the devicetree GPU policy coreboot's
//! `northbridge/intel/{ironlake,sandybridge}` share, the panel-power and
//! backlight part of `gma_pm_init_post_vbios()` and `gma_enable_swsci()`.

use crate::southbridge::pmio_ich::{self as pmio, PmIo};
use crate::southbridge::smi::ICH8_GPE0;

use super::{IgdDisplayPolicy, VbtSource};

/// PCH display registers `gma_pm_init_post_vbios()` writes.
const PCH_PORT_HOTPLUG: u32 = 0xc4030;
const PCH_PP_ON_DELAYS: u32 = 0xc7208;
const PCH_PP_OFF_DELAYS: u32 = 0xc720c;
const PCH_PP_DIVISOR: u32 = 0xc7210;
const BLC_PWM_CPU_CTL2: u32 = 0x48250;
const BLC_PWM_CPU_CTL: u32 = 0x48254;
const BLC_PWM_PCH_CTL1: u32 = 0xc8250;
const BLC_PWM_PCH_CTL2: u32 = 0xc8254;
/// GPE0 TCO SCI, `\_GPE._L06`.
const TCOSCI: u32 = 1 << 6;

/// Panel connection (coreboot `gpu_panel_port_select`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPort {
    Lvds = 0,
    /// Embedded DisplayPort on port A.
    DpA = 1,
    DpC = 2,
    DpD = 3,
}

/// Board IGD policy (the GPU registers of coreboot's
/// `northbridge_intel_{ironlake,sandybridge}_config` and the mainboard's
/// libgfxinit port list).
#[derive(Debug, Clone, Copy)]
pub struct PchIgdConfig {
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

impl PchIgdConfig {
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

    /// The hot-plug value `gma_pm_init_post_vbios()` writes.
    #[must_use]
    pub const fn hotplug(&self) -> u32 {
        ((self.dp_b_hotplug as u32 & 7) << 2)
            | ((self.dp_c_hotplug as u32 & 7) << 10)
            | ((self.dp_d_hotplug as u32 & 7) << 18)
    }

    /// The panel and backlight half of coreboot `gma_pm_init_post_vbios()`:
    /// hot-plug, panel power sequencing and the PWMs, each left alone when
    /// already programmed or not configured. The native modeset needs them
    /// before it asks the PCH sequencer to power the panel.
    pub fn init_panel(&self, gtt_mmio: u64) {
        let read = |offset| super::mmio_read32(gtt_mmio, offset);
        let write = |offset, value| super::mmio_write32(gtt_mmio, offset, value);
        if read(PCH_PORT_HOTPLUG) == 0 {
            write(PCH_PORT_HOTPLUG, self.hotplug());
        }
        if read(PCH_PP_ON_DELAYS) == 0 {
            write(
                PCH_PP_ON_DELAYS,
                ((self.panel_port as u32 & 3) << 30)
                    | (u32::from(self.panel_power_up_delay & 0x1fff) << 16)
                    | u32::from(self.panel_backlight_on_delay & 0x1fff),
            );
        }
        if read(PCH_PP_OFF_DELAYS) == 0 {
            write(
                PCH_PP_OFF_DELAYS,
                (u32::from(self.panel_power_down_delay & 0x1fff) << 16)
                    | u32::from(self.panel_backlight_off_delay & 0x1fff),
            );
        }
        if self.panel_power_cycle_delay != 0 {
            write(
                PCH_PP_DIVISOR,
                (read(PCH_PP_DIVISOR) & !0xff) | u32::from(self.panel_power_cycle_delay),
            );
        }
        if self.cpu_backlight != 0 {
            write(BLC_PWM_CPU_CTL2, 1 << 31);
            write(BLC_PWM_CPU_CTL, self.cpu_backlight);
        }
        if self.pch_backlight != 0 {
            write(BLC_PWM_PCH_CTL1, 1 << 31);
            write(BLC_PWM_PCH_CTL2, self.pch_backlight);
        }
    }
}

impl Default for PchIgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// coreboot `gma_enable_swsci()`: the OpRegion software SCI (DMISCI)
/// raises `\_GPE._L06` through the TCO SCI of the PCH at `pmbase`.
pub fn enable_swsci(pmbase: u16) {
    let pm = PmIo::new(pmbase);
    let tco = pm.tco();
    let dmisci = tco.read16(pmio::TCO1_STS) & pmio::DMISCI_STS as u16;
    tco.write16(pmio::TCO1_STS, dmisci);
    pm.write32(ICH8_GPE0.sts_offset, TCOSCI);
    pm.write16(
        ICH8_GPE0.en_offset,
        pm.read16(ICH8_GPE0.en_offset) | TCOSCI as u16,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotplug_matches_coreboot() {
        let config = PchIgdConfig {
            dp_d_hotplug: 6,
            ..PchIgdConfig::new()
        };
        assert_eq!(config.hotplug(), 6 << 18);
    }
}
