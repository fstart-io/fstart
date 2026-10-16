//! GM45 IGD function-0 init and display hand-off (coreboot `gm45/gma.c`).
//!
//! Mainstage-only: clears the GTT, programs the panel power sequencer and
//! backlight PWM for the OS, publishes the OpRegion and hands the display to
//! the shared GMA library when the board asked for a framebuffer.

use super::{Gm45GraphicsPolicy, IntelGm45};
use crate::igd::{IgdBars, IgdDisplayPolicy, VbtSource};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::Readable;

const IGD_DEV: u8 = 2;
const IGD_BSM: u16 = 0x5c;
const IGD_MSAC: u16 = 0x62;
const IGD_GCFGC: u16 = 0xf0;
const GCFGC_CD_SELECT: u16 = 1 << 12;
/// GTTMMADR: 512 KiB of display MMIO, the 2 MiB GTT window at +2 MiB.
const GTTMMADR_SIZE: u32 = 4 << 20;
const GTT_OFFSET: u64 = 2 << 20;
const GTT_SIZE: u32 = 2 << 20;

const PP_ON_DELAYS: u32 = 0x61208;
const PP_OFF_DELAYS: u32 = 0x6120c;
const PP_DIVISOR: u32 = 0x61210;
const BLC_PWM_CTL2: u32 = 0x61250;
const BLC_PWM_CTL: u32 = 0x61254;

/// Backlight PWM frequency for one panel, keyed by its EDID ASCII string
/// (coreboot `blc_pwm_t`, mainboard `blc.c`).
#[derive(Debug, Clone, Copy)]
pub struct BacklightPwm {
    pub panel: &'static str,
    pub hz: u16,
}

/// Board IGD policy: panel sequencing (devicetree `gpu_panel_*`), backlight
/// table and display bring-up.
#[derive(Debug, Clone, Copy)]
pub struct Gm45IgdConfig {
    /// VBT for the OpRegion, chosen from the panel's table PWM frequency
    /// (0 when unknown), like coreboot `mainboard_vbt_filename()`.
    pub vbt: fn(u16) -> VbtSource,
    /// Panel power-up delay in 100 us units.
    pub panel_power_up_delay: u16,
    /// Panel power-down delay in 100 us units.
    pub panel_power_down_delay: u16,
    /// Backlight-on delay in 100 us units.
    pub panel_backlight_on_delay: u16,
    /// Backlight-off delay in 100 us units.
    pub panel_backlight_off_delay: u16,
    /// Panel power-cycle delay in 100 ms units; zero leaves PP_DIVISOR.
    pub panel_power_cycle_delay: u8,
    /// Known panels' backlight PWM frequencies.
    pub backlight_pwm: &'static [BacklightPwm],
    /// PWM frequency for panels missing from the table; zero keeps coreboot's
    /// fixed `0x06100610` fallback.
    pub default_pwm_freq: u16,
    /// Initial backlight duty cycle in percent; zero means 100.
    pub duty_cycle: u8,
    /// Outputs and framebuffer for the native modeset; `None` stays headless.
    pub display: Option<IgdDisplayPolicy>,
}

const fn legacy_vbt(_: u16) -> VbtSource {
    VbtSource::LEGACY
}

impl Gm45IgdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vbt: legacy_vbt,
            panel_power_up_delay: 0,
            panel_power_down_delay: 0,
            panel_backlight_on_delay: 0,
            panel_backlight_off_delay: 0,
            panel_power_cycle_delay: 0,
            backlight_pwm: &[],
            default_pwm_freq: 0,
            duty_cycle: 0,
            display: None,
        }
    }
}

impl Default for Gm45IgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Coreboot `gm45/gma.c get_cdclk()`: VCO and GCFGC display-clock select.
fn cdclk_hz(vco: u8, gcfgc: u16) -> u32 {
    let high = gcfgc & GCFGC_CD_SELECT != 0;
    match (vco & 7, high) {
        (1, true) => 320_000_000,
        (1, false) => 228_571_429,
        (_, true) => 333_333_333,
        (_, false) => 222_222_222,
    }
}

fn blc_pwm_ctl(cdclk: u32, pwm_hz: u16, duty: u8) -> u32 {
    let modulation = cdclk / (128 * u32::from(pwm_hz));
    (modulation << 16) | (modulation * u32::from(duty.min(100)) / 100)
}

#[cfg(any(feature = "display", test))]
fn lookup_pwm(table: &[BacklightPwm], panel: &[u8]) -> Option<u16> {
    table
        .iter()
        .find(|entry| entry.panel.as_bytes() == panel)
        .map(|entry| entry.hz)
}

impl IntelGm45 {
    fn igd(&self) -> EcamDevice {
        EcamDevice::new(0, IGD_DEV, 0)
    }

    /// Coreboot only runs `gma_func0_init` for a decoded IGD; native
    /// graphics further requires GGC to keep legacy VGA decode.
    pub(super) fn igd_enabled(&self) -> bool {
        self.graphics != Gm45GraphicsPolicy::Discrete
            && self.hostbridge().deven.get() & super::DEVEN_IGD != 0
            && self.hostbridge().ggc.get() & 2 == 0
            && self.igd().is_present()
    }

    fn cdclk(&self) -> u32 {
        cdclk_hz(self.mch_vco(), self.igd().read16(IGD_GCFGC))
    }

    /// LVDS EDID ASCII string through GMBUS, as coreboot reads it before
    /// the modeset to pick the backlight PWM frequency.
    #[cfg(feature = "display")]
    fn panel_pwm_hz(&self, gtt_mmio: u64) -> Option<u16> {
        use fstart_intel_gma::gmbus::{HardwareGmbus, read_base_edid};
        let mut raw = [0; 128];
        // SAFETY: GTTMMADR was assigned by enumeration and decodes memory.
        let mut bus = unsafe {
            HardwareGmbus::gmch(
                fstart_core::typed::mmio32(gtt_mmio),
                fstart_intel_gma::GmbusPin::Panel,
            )
        };
        let edid = read_base_edid(&mut bus, &mut raw).ok()?;
        let panel = edid.ascii_string()?;
        let hz = lookup_pwm(self.config.igd.backlight_pwm, panel);
        if hz.is_none() {
            // Coreboot asks for the panel string and vendor BLC_PWM_CTL here.
            fstart_log::info!("gm45: panel EDID string not in the backlight PWM table");
        }
        hz
    }

    #[cfg(not(feature = "display"))]
    fn panel_pwm_hz(&self, _gtt_mmio: u64) -> Option<u16> {
        None
    }

    /// Coreboot `gma_pm_init_post_vbios()`.
    fn panel_power_and_backlight(&self, gtt_mmio: u64, pwm_hz: u16) {
        let conf = &self.config.igd;
        let read = |offset| crate::igd::mmio_read32(gtt_mmio, offset);
        let write = |offset, value| crate::igd::mmio_write32(gtt_mmio, offset, value);
        if read(PP_ON_DELAYS) == 0 {
            write(
                PP_ON_DELAYS,
                (u32::from(conf.panel_power_up_delay & 0x1fff) << 16)
                    | u32::from(conf.panel_backlight_on_delay & 0x1fff),
            );
        }
        if read(PP_OFF_DELAYS) == 0 {
            write(
                PP_OFF_DELAYS,
                (u32::from(conf.panel_power_down_delay & 0x1fff) << 16)
                    | u32::from(conf.panel_backlight_off_delay & 0x1fff),
            );
        }
        if conf.panel_power_cycle_delay != 0 {
            write(
                PP_DIVISOR,
                ((self.cdclk() / 20_000 - 1) << 8) | u32::from(conf.panel_power_cycle_delay & 0x1f),
            );
        }
        write(BLC_PWM_CTL2, 1 << 31);
        let duty = if conf.duty_cycle == 0 {
            100
        } else {
            conf.duty_cycle
        };
        write(
            BLC_PWM_CTL,
            if pwm_hz == 0 {
                0x0610_0610
            } else {
                blc_pwm_ctl(self.cdclk(), pwm_hz, duty)
            },
        );
    }

    /// Coreboot `gma_func0_init()` up to the OpRegion: bus master, clear the
    /// 2 MiB GTT window and program the panel sequencer and backlight.
    pub(super) fn igd_func0_init(&mut self) {
        if !self.igd_enabled() {
            return;
        }
        let igd = self.igd();
        self.igd_bars = crate::igd::assigned_bars(&igd, false);
        let Some(IgdBars { gtt_mmio, .. }) = self.igd_bars else {
            fstart_log::error!("gm45: IGD windows unassigned, skipping graphics init");
            return;
        };
        igd.or16(0x04, (1 << 1) | (1 << 2));
        crate::igd::clear_gtt_table(gtt_mmio + GTT_OFFSET, GTT_SIZE);
        // The VBT choice uses only the panel lookup; the PWM falls back further.
        self.panel_pwm_hz = self.panel_pwm_hz(gtt_mmio).unwrap_or(0);
        let pwm_hz = match self.panel_pwm_hz {
            0 => self.config.igd.default_pwm_freq,
            hz => hz,
        };
        self.panel_power_and_backlight(gtt_mmio, pwm_hz);
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
            None => fstart_log::error!("gm45: no OpRegion to restore on resume"),
        }
    }

    /// OpRegion (VBT chosen by backlight type) and the optional modeset.
    pub(super) fn igd_publish_and_modeset(&mut self, opregion: crate::igd::OpRegionMemory<'_>) {
        let Some(bars) = self.igd_bars else {
            return;
        };
        let igd = self.igd();
        let source = (self.config.igd.vbt)(self.panel_pwm_hz);
        let vbt =
            crate::igd::publish_opregion(&igd, crate::igd::OpRegionSci::Swsci, &source, opregion);
        if self.config.igd.display.is_none() {
            return;
        }
        let Ok((_, tolud, _)) = self.memory_map() else {
            return;
        };
        let stolen_base = igd.read32(IGD_BSM) & 0xfff0_0000;
        let addresses = crate::igd::IgdAddresses {
            pci_bdf: fstart_intel_gma::types::PciAddress::new(0, 0, IGD_DEV, 0),
            gtt_mmio_base: bars.gtt_mmio,
            gtt_mmio_size: GTTMMADR_SIZE,
            gtt_pte_base: None,
            gmadr_base: Some(bars.gmadr),
            gmadr_size: crate::igd::gmadr_size_from_msac(igd.read8(IGD_MSAC)),
            stolen_base: u64::from(stolen_base),
            stolen_size: tolud.saturating_sub(stolen_base),
            gtt_size: GTT_SIZE,
            gcfgc: Some(igd.read16(IGD_GCFGC)),
        };
        self.display.initialize(
            fstart_intel_gma::types::Cpu::Gm45,
            self.config.igd.display.as_ref(),
            &addresses,
            vbt.as_deref(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm45_cdclk_and_backlight_match_coreboot() {
        assert_eq!(cdclk_hz(4, GCFGC_CD_SELECT), 333_333_333);
        assert_eq!(cdclk_hz(0, 0), 222_222_222);
        assert_eq!(cdclk_hz(1, GCFGC_CD_SELECT), 320_000_000);
        assert_eq!(cdclk_hz(1, 0), 228_571_429);
        // X200 LTD121EWVB at 165 Hz on a 320 MHz CDCLK, 100 % duty.
        assert_eq!(blc_pwm_ctl(320_000_000, 165, 100), (15151 << 16) | 15151);
        const TABLE: &[BacklightPwm] = &[BacklightPwm {
            panel: "B121EW09 V3 ",
            hz: 400,
        }];
        assert_eq!(lookup_pwm(TABLE, b"B121EW09 V3 "), Some(400));
        assert_eq!(lookup_pwm(TABLE, b"B121EW09 V3"), None);
    }
}
