//! Lenovo switchable / hybrid graphics policy (coreboot
//! `drivers/lenovo/hybrid_graphics`).
//!
//! ThinkPads with a discrete GPU (e.g. T400/T500/R400/W500) strap a
//! presence GPIO low when the dGPU is fitted, power it through a GPIO in
//! early boot, and route the internal panel and backlight muxes in ramstage.

use embedded_hal::digital::{InputPin, OutputPin, PinState};
use fstart_options::{Choice, Kind, Knob, Meta};

/// User-selectable graphics routing (coreboot `hybrid_graphics_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HybridGraphicsMode {
    IntegratedOnly = 0,
    DiscreteOnly = 1,
    DualGraphics = 2,
}

impl HybridGraphicsMode {
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::DiscreteOnly,
            2 => Self::DualGraphics,
            _ => Self::IntegratedOnly,
        }
    }
}

/// Board option container for `hybrid_graphics_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HybridGraphicsOptions {
    pub mode: HybridGraphicsMode,
}

impl HybridGraphicsOptions {
    /// Coreboot `HYBRID_GRAPHICS_DEFAULT_GPU`: integrated only.
    pub const DEFAULT: Self = Self {
        mode: HybridGraphicsMode::IntegratedOnly,
    };
}

/// Coreboot `hybrid_graphics_mode` CMOS option.
pub const HYBRID_GRAPHICS_MODE: Knob<HybridGraphicsOptions> = Knob::new(
    Meta {
        key: "hybrid_graphics_mode",
        form: "Graphics",
        label: "Graphics device",
        help: "Select integrated, discrete or switchable graphics when a discrete GPU is fitted.",
        kind: Kind::Enum(&[
            Choice::new(0, "Integrated only"),
            Choice::new(1, "Discrete only"),
            Choice::new(2, "Dual graphics"),
        ]),
    },
    |o: &HybridGraphicsOptions| (o.mode as u8).into(),
    |o: &mut HybridGraphicsOptions, v| o.mode = HybridGraphicsMode::from_u8(v as u8),
);

/// Active GPUs selected by [`early_select`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsSelection {
    pub igd: bool,
    pub peg: bool,
}

/// Early bootblock hybrid graphics selection (coreboot
/// `early_hybrid_graphics()`): probe `detect` (low = dGPU installed), choose
/// `(igd, peg)` from `mode`, and drive `dgpu_power` accordingly.
pub fn early_select(
    detect: &mut impl InputPin,
    dgpu_power: &mut impl OutputPin,
    dgpu_power_off_high: bool,
    mode: HybridGraphicsMode,
) -> GraphicsSelection {
    // DGPU_NOT_INSTALLED = 1 (high); default to integrated if read fails.
    if !detect.is_low().unwrap_or(false) {
        return GraphicsSelection {
            igd: true,
            peg: false,
        };
    }
    let selection = match mode {
        HybridGraphicsMode::IntegratedOnly => GraphicsSelection {
            igd: true,
            peg: false,
        },
        HybridGraphicsMode::DiscreteOnly => GraphicsSelection {
            igd: false,
            peg: true,
        },
        HybridGraphicsMode::DualGraphics => GraphicsSelection {
            igd: true,
            peg: true,
        },
    };
    let power_high = if selection.peg {
        !dgpu_power_off_high
    } else {
        dgpu_power_off_high
    };
    let _ = dgpu_power.set_state(PinState::from(power_high));
    selection
}

/// Ramstage panel and backlight mux selection (coreboot
/// `lenovo_hybrid_graphics_enable()`). In dual-graphics mode the panel stays
/// on the integrated GPU.
pub fn mux_panel(
    detect: &mut impl InputPin,
    panel_mux: &mut impl OutputPin,
    panel_integrated_high: bool,
    backlight_mux: &mut impl OutputPin,
    backlight_integrated_high: bool,
    mode: HybridGraphicsMode,
) {
    if !detect.is_low().unwrap_or(false) {
        return;
    }
    let discrete = mode == HybridGraphicsMode::DiscreteOnly;
    let _ = panel_mux.set_state(PinState::from(if discrete {
        !panel_integrated_high
    } else {
        panel_integrated_high
    }));
    let _ = backlight_mux.set_state(PinState::from(if discrete {
        !backlight_integrated_high
    } else {
        backlight_integrated_high
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pin::Pin;

    #[derive(Default)]
    struct OutPin(Option<bool>);

    impl embedded_hal::digital::ErrorType for OutPin {
        type Error = core::convert::Infallible;
    }

    impl OutputPin for OutPin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0 = Some(false);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0 = Some(true);
            Ok(())
        }
    }

    #[test]
    fn early_select_leaves_integrated_only_when_no_dgpu_installed() {
        let mut detect = Pin(Ok(true)); // high = DGPU_NOT_INSTALLED
        let mut power = OutPin::default();
        let sel = early_select(
            &mut detect,
            &mut power,
            false,
            HybridGraphicsMode::DiscreteOnly,
        );
        assert_eq!(
            sel,
            GraphicsSelection {
                igd: true,
                peg: false
            }
        );
        assert_eq!(power.0, None);
    }

    #[test]
    fn early_select_powers_dgpu_for_discrete_and_dual_modes() {
        for (mode, expected_sel, expected_power) in [
            (
                HybridGraphicsMode::IntegratedOnly,
                GraphicsSelection {
                    igd: true,
                    peg: false,
                },
                Some(false),
            ),
            (
                HybridGraphicsMode::DiscreteOnly,
                GraphicsSelection {
                    igd: false,
                    peg: true,
                },
                Some(true),
            ),
            (
                HybridGraphicsMode::DualGraphics,
                GraphicsSelection {
                    igd: true,
                    peg: true,
                },
                Some(true),
            ),
        ] {
            let mut detect = Pin(Ok(false)); // low = DGPU_INSTALLED
            let mut power = OutPin::default();
            assert_eq!(
                early_select(&mut detect, &mut power, false, mode),
                expected_sel
            );
            assert_eq!(power.0, expected_power);
        }
    }

    #[test]
    fn mux_panel_routes_discrete_only_in_discrete_mode() {
        let mut detect = Pin(Ok(false));
        let mut panel = OutPin::default();
        let mut bl = OutPin::default();
        mux_panel(
            &mut detect,
            &mut panel,
            false,
            &mut bl,
            false,
            HybridGraphicsMode::DiscreteOnly,
        );
        assert_eq!((panel.0, bl.0), (Some(true), Some(true)));

        mux_panel(
            &mut detect,
            &mut panel,
            false,
            &mut bl,
            false,
            HybridGraphicsMode::DualGraphics,
        );
        assert_eq!((panel.0, bl.0), (Some(false), Some(false)));
    }

    #[test]
    fn hybrid_graphics_key_has_no_cmos_hash_collision() {
        let hash = fstart_options::cmos::hash(HYBRID_GRAPHICS_MODE.meta.key);
        for other in [
            "wlan",
            "wwan",
            "bluetooth",
            "uwb",
            "trackpoint",
            "touchpad",
            "sticky_fn",
            "fn_ctrl_swap",
            "usb_always_on",
            "power_management_beeps",
            "low_battery_beep",
            "volume",
            "first_battery",
            "debug_level",
            "power_on_after_fail",
            "sata_mode",
            "nmi",
            "gfx_uma_size",
            "tft_brightness",
        ] {
            assert_ne!(hash, fstart_options::cmos::hash(other), "{other}");
        }
    }
}
