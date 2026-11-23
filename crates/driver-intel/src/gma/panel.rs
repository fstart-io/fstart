//! Panel metadata helpers for Intel GMA display initialization.
//!
//! This module intentionally models VBT/libgfxinit panel data as pure values.
//! Hardware sequencing still lives in generation code and is not changed by
//! these helpers.

use crate::gma::mode::Mode;
use crate::gma::regs::{
    BXT_BLC_PWM_CTL, CPU_BLC_PWM_CTL, PP_CONTROL, PP_DIVISOR, PP_OFF_DELAYS, PP_ON_DELAYS,
};
use crate::gma::types::Port;

/// LVDS panel options from VBT block 40 (`BDB_LFP_OPTIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LvdsPanelOptions {
    /// Selected panel type from the VBT, when it names one of the 16 LFP slots.
    pub panel_type: Option<u8>,
    /// Raw VBT panel type byte. Linux treats `0xff` as an unknown/PNP-selected panel.
    pub raw_panel_type: u8,
    /// Panel fitter mode bits.
    pub pfit_mode: u8,
    /// Enhanced text-mode fitting requested by the VBT.
    pub pfit_text_mode_enhanced: bool,
    /// Enhanced graphics-mode fitting requested by the VBT.
    pub pfit_gfx_mode_enhanced: bool,
    /// Automatic panel-fitter ratio requested by the VBT.
    pub pfit_ratio_auto: bool,
    /// VBT requests pixel dithering.
    pub pixel_dither: bool,
    /// VBT indicates EDID is available for the LVDS panel.
    pub lvds_edid: bool,
    /// Per-panel LVDS channel bitfield, when present in the block.
    pub lvds_panel_channel_bits: Option<u32>,
    /// Per-panel spread-spectrum clock bitfield, when present.
    pub ssc_bits: Option<u16>,
    /// Spread-spectrum clock frequency field, when present.
    pub ssc_freq: Option<u16>,
    /// Panel color-depth field, when present.
    pub panel_color_depth: Option<u16>,
    /// Per-panel DRRS/DPS panel-type bitfield, when present.
    pub dps_panel_type_bits: Option<u32>,
    /// Per-panel backlight-control type bitfield, when present.
    pub backlight_control_type_bits: Option<u32>,
}

/// Register values embedded in one VBT LFP FP-timing record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfpFpTiming {
    /// FP timing X resolution.
    pub x_res: u16,
    /// FP timing Y resolution.
    pub y_res: u16,
    /// LVDS register address named by the VBT.
    pub lvds_reg: u32,
    /// LVDS register value named by the VBT.
    pub lvds_reg_val: u32,
    /// Panel-power on-delay register address named by the VBT.
    pub pp_on_reg: u32,
    /// Panel-power on-delay register value named by the VBT.
    pub pp_on_reg_val: u32,
    /// Panel-power off-delay register address named by the VBT.
    pub pp_off_reg: u32,
    /// Panel-power off-delay register value named by the VBT.
    pub pp_off_reg_val: u32,
    /// Panel-power cycle-delay register address named by the VBT.
    pub pp_cycle_reg: u32,
    /// Panel-power cycle-delay register value named by the VBT.
    pub pp_cycle_reg_val: u32,
    /// Panel-fitter register address named by the VBT.
    pub pfit_reg: u32,
    /// Panel-fitter register value named by the VBT.
    pub pfit_reg_val: u32,
}

/// Backlight control data from VBT block 43 for one panel slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfpBacklightInfo {
    /// Raw backlight type from the legacy data entry (`2` is PWM in Linux).
    pub backlight_type: u8,
    /// PWM polarity bit from the VBT.
    pub active_low_pwm: bool,
    /// Legacy I2C pin field, retained only as parsed metadata.
    pub i2c_pin: u8,
    /// Legacy I2C speed field, retained only as parsed metadata.
    pub i2c_speed: u8,
    /// PWM frequency in Hz from the VBT.
    pub pwm_freq_hz: u16,
    /// Minimum brightness byte from the VBT.
    pub min_brightness: u8,
    /// Legacy I2C address field, retained only as parsed metadata.
    pub i2c_address: u8,
    /// Legacy I2C command field, retained only as parsed metadata.
    pub i2c_command: u8,
}

impl LfpBacklightInfo {
    /// Return true when the parsed backlight entry describes PWM control.
    pub const fn is_pwm(self) -> bool {
        self.backlight_type == 2
    }
}

/// Power-conservation feature bits from VBT block 44.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfpPowerFeatures {
    /// Display Power Saving Technology support bit.
    pub dpst_supported: bool,
    /// VBT power-conservation preference field.
    pub power_conservation_preference: u8,
    /// Local adaptive contrast enhancement enabled-status bit.
    pub lace_enabled_status: bool,
    /// Local adaptive contrast enhancement support bit.
    pub lace_supported: bool,
    /// Ambient light sensor enable bit.
    pub als_enabled: bool,
}

/// Selected panel metadata assembled from safe VBT/LFP blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfpPanelMetadata {
    /// Selected LFP panel type/slot.
    pub panel_type: u8,
    /// Fixed panel mode from the selected DVO timing descriptor.
    pub fixed_mode: Mode,
    /// LVDS options from block 40.
    pub options: LvdsPanelOptions,
    /// FP timing/register metadata from block 42.
    pub fp_timing: Option<LfpFpTiming>,
    /// Optional backlight metadata from block 43.
    pub backlight: Option<LfpBacklightInfo>,
    /// Optional power-conservation metadata from block 44.
    pub power: Option<LfpPowerFeatures>,
}

/// Panel power-sequencer delay set in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPowerDelays {
    /// Delay from panel power target-on to usable panel power.
    pub power_up_us: u32,
    /// Delay from panel power-up to backlight enable.
    pub power_up_to_backlight_on_us: u32,
    /// Delay from panel target-off to panel power-down completion.
    pub power_down_us: u32,
    /// Delay from backlight disable to panel power-down.
    pub backlight_off_to_power_down_us: u32,
    /// Minimum delay between power cycles.
    pub power_cycle_us: u32,
}

impl PanelPowerDelays {
    /// libgfxinit's default eDP delays.
    pub const DEFAULT_EDP: Self = Self {
        power_up_us: 210_000,
        power_up_to_backlight_on_us: 50_000,
        power_down_us: 500_000,
        backlight_off_to_power_down_us: 50_000,
        power_cycle_us: 510_000,
    };

    /// Decode panel delays from PP_ON_DELAYS, PP_OFF_DELAYS, and PP_DIVISOR-style registers.
    pub const fn from_registers(on_delays: u32, off_delays: u32, divisor: u32) -> Self {
        let cycle = divisor & PP_DIVISOR_PWR_CYC_DELAY_MASK;
        Self {
            power_up_us: ((on_delays & PP_ON_DELAYS_PWR_UP_MASK) >> 16) * 100,
            power_up_to_backlight_on_us: (on_delays & PP_ON_DELAYS_PWR_UP_BL_ON_MASK) * 100,
            power_down_us: ((off_delays & PP_OFF_DELAYS_PWR_DOWN_MASK) >> 16) * 100,
            backlight_off_to_power_down_us: (off_delays & PP_OFF_DELAYS_BL_OFF_PWR_DOWN_MASK) * 100,
            power_cycle_us: if cycle > 1 { (cycle - 1) * 100_000 } else { 0 },
        }
    }

    /// True when any decoded field is zero, so libgfxinit substitutes the
    /// default set and writes the delay registers (`Check_PP_Delays`).
    pub const fn needs_override(self) -> bool {
        self.power_up_us == 0
            || self.power_up_to_backlight_on_us == 0
            || self.power_down_us == 0
            || self.backlight_off_to_power_down_us == 0
            || self.power_cycle_us == 0
    }

    /// Return delays with libgfxinit defaults substituted for zero fields.
    pub const fn with_defaults(self) -> Self {
        Self {
            power_up_us: default_if_zero(self.power_up_us, Self::DEFAULT_EDP.power_up_us),
            power_up_to_backlight_on_us: default_if_zero(
                self.power_up_to_backlight_on_us,
                Self::DEFAULT_EDP.power_up_to_backlight_on_us,
            ),
            power_down_us: default_if_zero(self.power_down_us, Self::DEFAULT_EDP.power_down_us),
            backlight_off_to_power_down_us: default_if_zero(
                self.backlight_off_to_power_down_us,
                Self::DEFAULT_EDP.backlight_off_to_power_down_us,
            ),
            power_cycle_us: default_if_zero(self.power_cycle_us, Self::DEFAULT_EDP.power_cycle_us),
        }
    }

    /// Encode PP_ON_DELAYS, forcing libgfxinit's PRM-recommended 100us power-up-to-backlight delay.
    pub const fn encode_on_delays(self, port_select: PanelPowerPortSelect) -> u32 {
        port_select.bits() | pp_delay_100us(self.power_up_us, 16) | pp_delay_100us(100, 0)
    }

    /// Encode PP_OFF_DELAYS.
    pub const fn encode_off_delays(self) -> u32 {
        pp_delay_100us(self.power_down_us, 16)
            | pp_delay_100us(self.backlight_off_to_power_down_us, 0)
    }

    /// Encode the PP_DIVISOR power-cycle delay field.
    pub const fn encode_divisor_cycle_delay(self) -> u32 {
        self.power_cycle_us.div_ceil(100_000) + 1
    }

    /// Encode Broxton-style PP_CONTROL power-cycle delay bits.
    pub const fn encode_bxt_control_cycle_delay(self) -> u32 {
        self.encode_divisor_cycle_delay() << BXT_PP_CONTROL_PWR_CYC_DELAY_SHIFT
    }
}

/// Panel power port-select values used in PP_ON_DELAYS on PCH platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPowerPortSelect {
    /// LVDS panel power port select.
    Lvds,
    /// eDP/DP-A panel power port select.
    DpA,
    /// DP-C/HDMI-C panel power port select.
    DpC,
    /// DP-D/HDMI-D panel power port select.
    DpD,
    /// No port-select bits.
    None,
}

impl PanelPowerPortSelect {
    /// Map fstart ports to libgfxinit's PCH PP_ON_DELAYS port select field.
    pub const fn from_port(port: Port) -> Self {
        match port {
            Port::Lvds => Self::Lvds,
            Port::Edp => Self::DpA,
            Port::DpB | Port::HdmiB => Self::DpC,
            Port::DpC | Port::HdmiC => Self::DpD,
            _ => Self::None,
        }
    }

    /// Encoded PP_ON_DELAYS port-select bits.
    pub const fn bits(self) -> u32 {
        match self {
            Self::Lvds | Self::None => 0,
            Self::DpA => 1 << 30,
            Self::DpC => 2 << 30,
            Self::DpD => 3 << 30,
        }
    }
}

/// Pure register operation used by panel/backlight sequence planners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRegisterOp {
    /// Write a full 32-bit register value.
    Write { register: usize, value: u32 },
    /// Clear a mask and set selected bits.
    Update {
        register: usize,
        mask_unset: u32,
        mask_set: u32,
    },
    /// Set selected bits.
    Set { register: usize, mask: u32 },
    /// Clear selected bits.
    Clear { register: usize, mask: u32 },
}

/// Register block for a panel power sequencer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPowerRegs {
    /// PP_STATUS register.
    pub status: usize,
    /// PP_CONTROL register.
    pub control: usize,
    /// PP_ON_DELAYS register.
    pub on_delays: usize,
    /// PP_OFF_DELAYS register.
    pub off_delays: usize,
    /// PP_DIVISOR register.
    pub divisor: usize,
}

impl PanelPowerRegs {
    /// Legacy GMCH panel power register block.
    pub const GMCH: Self = Self {
        status: 0x61200,
        control: 0x61204,
        on_delays: 0x61208,
        off_delays: 0x6120c,
        divisor: 0x61210,
    };

    /// PCH panel power register block.
    pub const PCH: Self = Self {
        status: 0xc7200,
        control: 0xc7204,
        on_delays: 0xc7208,
        off_delays: 0xc720c,
        divisor: 0xc7210,
    };
}

/// Plan for libgfxinit-style PP sequencer setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPowerSequencerPlan {
    /// Effective delay values after applying defaults.
    pub delays: PanelPowerDelays,
    /// Whether the delay registers should be written at all. libgfxinit only
    /// writes them when it had to substitute a default (`Override_Delays`).
    pub override_delays: bool,
    /// PP_ON_DELAYS operation.
    pub on_delays: PanelRegisterOp,
    /// PP_OFF_DELAYS operation.
    pub off_delays: PanelRegisterOp,
    /// PP_DIVISOR or PP_CONTROL cycle-delay operation.
    pub cycle_delay: PanelRegisterOp,
    /// Final PP_CONTROL operation that sets power-down-on-reset and optional write-protect key.
    pub control: PanelRegisterOp,
}

/// Build a libgfxinit-style PP sequencer setup plan.
pub const fn panel_power_sequencer_plan(
    regs: PanelPowerRegs,
    delays: PanelPowerDelays,
    port_select: PanelPowerPortSelect,
    has_divisor_reg: bool,
    has_write_protection: bool,
) -> PanelPowerSequencerPlan {
    let override_delays = delays.needs_override();
    let effective = delays.with_defaults();
    PanelPowerSequencerPlan {
        delays: effective,
        override_delays,
        on_delays: PanelRegisterOp::Update {
            register: regs.on_delays,
            mask_unset: PP_ON_DELAYS_PORT_SELECT_MASK
                | PP_ON_DELAYS_PWR_UP_MASK
                | PP_ON_DELAYS_PWR_UP_BL_ON_MASK,
            mask_set: effective.encode_on_delays(port_select),
        },
        off_delays: PanelRegisterOp::Update {
            register: regs.off_delays,
            mask_unset: PP_OFF_DELAYS_PWR_DOWN_MASK | PP_OFF_DELAYS_BL_OFF_PWR_DOWN_MASK,
            mask_set: effective.encode_off_delays(),
        },
        cycle_delay: if has_divisor_reg {
            PanelRegisterOp::Update {
                register: regs.divisor,
                mask_unset: PP_DIVISOR_PWR_CYC_DELAY_MASK,
                mask_set: effective.encode_divisor_cycle_delay(),
            }
        } else {
            PanelRegisterOp::Update {
                register: regs.control,
                mask_unset: BXT_PP_CONTROL_PWR_CYC_DELAY_MASK,
                mask_set: effective.encode_bxt_control_cycle_delay(),
            }
        },
        control: if has_write_protection {
            PanelRegisterOp::Update {
                register: regs.control,
                mask_unset: PP_CONTROL_WRITE_PROTECT_MASK,
                mask_set: PP_CONTROL_WRITE_PROTECT_KEY | PP_CONTROL_POWER_DOWN_ON_RESET,
            }
        } else {
            PanelRegisterOp::Set {
                register: regs.control,
                mask: PP_CONTROL_POWER_DOWN_ON_RESET,
            }
        },
    }
}

/// Plan for panel target on/off, VDD override, and backlight operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPowerControlPlan {
    /// Operation to request panel power on.
    pub panel_on: PanelRegisterOp,
    /// Operation to clear target power/VDD override.
    pub panel_off: PanelRegisterOp,
    /// Operation to enable panel-power-sequencer backlight gate.
    pub backlight_on: PanelRegisterOp,
    /// Operation to disable panel-power-sequencer backlight gate.
    pub backlight_off: PanelRegisterOp,
    /// Operation used for VDD override; libgfxinit aliases this to panel on.
    pub vdd_override: PanelRegisterOp,
}

/// Build a libgfxinit-style panel power-control plan.
pub const fn panel_power_control_plan(regs: PanelPowerRegs) -> PanelPowerControlPlan {
    PanelPowerControlPlan {
        panel_on: PanelRegisterOp::Set {
            register: regs.control,
            mask: PP_CONTROL_TARGET_ON,
        },
        panel_off: PanelRegisterOp::Clear {
            register: regs.control,
            mask: PP_CONTROL_TARGET_ON | PP_CONTROL_VDD_OVERRIDE,
        },
        backlight_on: PanelRegisterOp::Set {
            register: regs.control,
            mask: PP_CONTROL_BACKLIGHT_ENABLE,
        },
        backlight_off: PanelRegisterOp::Clear {
            register: regs.control,
            mask: PP_CONTROL_BACKLIGHT_ENABLE,
        },
        vdd_override: PanelRegisterOp::Set {
            register: regs.control,
            mask: PP_CONTROL_TARGET_ON,
        },
    }
}

/// Backlight PWM register layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklightRegisterModel {
    /// Legacy split PWM control: `duty_ctl` holds the low-16-bit duty cycle,
    /// `freq_ctl` the modulation frequency. On GMCH (i965/G45) these are
    /// `BLC_PWM_CTL` (0x61254) and `BLC_PWM_CTL2` (0x61250); on PCH-era parts
    /// they are the CPU-side `BLC_PWM_CPU_CTL` and `BLC_PWM_PCH_CTL2`.
    Legacy { duty_ctl: usize, freq_ctl: usize },
    /// Newer Broxton-style PWM control/frequency/duty registers.
    New {
        ctl: usize,
        freq: usize,
        duty: usize,
    },
}

/// Build a libgfxinit-style backlight duty operation.
pub const fn set_backlight_op(model: BacklightRegisterModel, level: u32) -> PanelRegisterOp {
    match model {
        BacklightRegisterModel::Legacy { duty_ctl, .. } => PanelRegisterOp::Update {
            register: duty_ctl,
            mask_unset: CPU_BLC_PWM_DATA_BL_DUTY_CYC_MASK,
            mask_set: level & CPU_BLC_PWM_DATA_BL_DUTY_CYC_MASK,
        },
        BacklightRegisterModel::New { duty, .. } => PanelRegisterOp::Write {
            register: duty,
            value: level,
        },
    }
}

/// Build the i945/Pineview backlight duty operation.
///
/// GNU/Linux `i9xx_set_backlight` uses bits 15:1 with a `0xfffe` mask on these
/// parts (`BACKLIGHT_DUTY_CYCLE_MASK_PNV`), unlike the bit-0 16-bit field on
/// i965/G45.
pub const fn set_pnv_backlight_op(duty_ctl: usize, level: u32) -> PanelRegisterOp {
    const PNV_DUTY_MASK: u32 = 0xfffe;
    PanelRegisterOp::Update {
        register: duty_ctl,
        mask_unset: PNV_DUTY_MASK,
        mask_set: (level << 1) & PNV_DUTY_MASK,
    }
}

/// Build the libgfxinit-style PWM-controller enable operation, if the model has one.
pub const fn backlight_pwm_enable_op(model: BacklightRegisterModel) -> Option<PanelRegisterOp> {
    match model {
        BacklightRegisterModel::Legacy { .. } => None,
        BacklightRegisterModel::New { ctl, .. } => Some(PanelRegisterOp::Set {
            register: ctl,
            mask: BXT_BLC_PWM_CTL_ENABLE,
        }),
    }
}

/// Build the libgfxinit-style PWM-controller disable operation, if the model has one.
pub const fn backlight_pwm_disable_op(model: BacklightRegisterModel) -> Option<PanelRegisterOp> {
    match model {
        BacklightRegisterModel::Legacy { .. } => None,
        BacklightRegisterModel::New { ctl, .. } => Some(PanelRegisterOp::Clear {
            register: ctl,
            mask: BXT_BLC_PWM_CTL_ENABLE,
        }),
    }
}

/// Haswell/Lynx Point `SOUTH_CHICKEN2` register offset.
pub const SOUTH_CHICKEN2: usize = 0xc2004;
/// Lynx Point PWM granularity select bit (`SOUTH_CHICKEN2` bit 5): 1 = 16-step, 0 = 128-step.
pub const LPT_PWM_GRANULARITY: u32 = 1 << 5;
/// PCH backlight PWM control 1 register (`BLC_PWM_PCH_CTL1`).
pub const BLC_PWM_PCH_CTL1: usize = 0xc8250;
/// PCH backlight PWM control 2 register (`BLC_PWM_PCH_CTL2`).
pub const BLC_PWM_PCH_CTL2: usize = 0xc8254;
/// CPU backlight PWM control 2 register (`BLC_PWM_CPU_CTL2`).
pub const BLC_PWM_CPU_CTL2: usize = 0x48250;
/// CPU backlight PWM control/duty register (`BLC_PWM_CPU_CTL`).
pub const BLC_PWM_CPU_CTL: usize = 0x48254;
/// `BLC_PWM_PCH_CTL1` PWM enable bit (bit 31).
pub const BLM_PCH_PWM_ENABLE: u32 = 1 << 31;
/// `BLC_PWM_PCH_CTL1` PCH override enable bit (bit 30).
pub const BLM_PCH_OVERRIDE_ENABLE: u32 = 1 << 30;
/// `BLC_PWM_PCH_CTL1` active-low polarity bit (bit 29).
pub const BLM_PCH_POLARITY_LOW: u32 = 1 << 29;

/// Pure Haswell / Lynx Point PCH backlight PWM register plan.
///
/// Mirrors coreboot `src/northbridge/intel/haswell/gma.c` (`gma_setup_panel`):
/// selects 16-step vs 128-step granularity in `SOUTH_CHICKEN2` bit 5, programs
/// `BLC_PWM_PCH_CTL2` (modulation frequency in bits 31:16, duty cycle in bits
/// 15:0), and enables PCH override in `BLC_PWM_PCH_CTL1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HaswellBacklightPwmPlan {
    /// Whether `SOUTH_CHICKEN2` bit 5 (`LPT_PWM_GRANULARITY`) should be set.
    pub use_16_granularity: bool,
    /// Computed PWM modulation period (16-bit).
    pub pwm_period: u16,
    /// Computed PWM duty cycle (16-bit).
    pub pwm_duty: u16,
    /// Operation to update `SOUTH_CHICKEN2` bit 5.
    pub south_chicken2: PanelRegisterOp,
    /// Value for `BLC_PWM_PCH_CTL2`.
    pub pch_ctl2: PanelRegisterOp,
    /// Value for `BLC_PWM_PCH_CTL1`.
    pub pch_ctl1: PanelRegisterOp,
}

impl HaswellBacklightPwmPlan {
    /// Compute a Haswell backlight PWM plan for `pwm_hz` at `duty_percent` (0..=100).
    ///
    /// Reference clock is 24 MHz on Lynx Point-LP (ULT/ULX) and 135 MHz on
    /// Traditional Lynx Point-H. Returns `None` if `pwm_hz == 0`.
    #[must_use]
    pub const fn new(is_lp: bool, pwm_hz: u32, active_low: bool, duty_percent: u8) -> Option<Self> {
        if pwm_hz == 0 {
            return None;
        }
        let refclock: u32 = if is_lp { 24_000_000 } else { 135_000_000 };
        let hz_limit = refclock / 128 / 100;
        let use_16_granularity = pwm_hz > hz_limit;
        let pwm_increment: u32 = if use_16_granularity { 16 } else { 128 };
        let raw_period = refclock / pwm_increment / pwm_hz;
        let clamped_period = if raw_period == 0 {
            1
        } else if raw_period > 0xffff {
            0xffff
        } else {
            raw_period
        };
        let clamped_duty_pct = if duty_percent > 100 {
            100
        } else {
            duty_percent
        };
        let pwm_duty = (clamped_period * clamped_duty_pct as u32) / 100;
        let south_chicken2 = if use_16_granularity {
            PanelRegisterOp::Set {
                register: SOUTH_CHICKEN2,
                mask: LPT_PWM_GRANULARITY,
            }
        } else {
            PanelRegisterOp::Clear {
                register: SOUTH_CHICKEN2,
                mask: LPT_PWM_GRANULARITY,
            }
        };
        let pch_ctl2_val = (clamped_period << 16) | (pwm_duty & 0xffff);
        let pch_ctl1_val = (if active_low { BLM_PCH_POLARITY_LOW } else { 0 })
            | BLM_PCH_OVERRIDE_ENABLE
            | BLM_PCH_PWM_ENABLE;
        Some(Self {
            use_16_granularity,
            pwm_period: clamped_period as u16,
            pwm_duty: pwm_duty as u16,
            south_chicken2,
            pch_ctl2: PanelRegisterOp::Write {
                register: BLC_PWM_PCH_CTL2,
                value: pch_ctl2_val,
            },
            pch_ctl1: PanelRegisterOp::Write {
                register: BLC_PWM_PCH_CTL1,
                value: pch_ctl1_val,
            },
        })
    }
}

const PP_CONTROL_WRITE_PROTECT_MASK: u32 = PP_CONTROL::UNLOCK_KEY.val(0xffff).value;
const PP_CONTROL_WRITE_PROTECT_KEY: u32 = PP_CONTROL::UNLOCK_KEY.val(0xabcd).value;
const PP_CONTROL_VDD_OVERRIDE: u32 = PP_CONTROL::VDD_OVERRIDE::SET.value;
const PP_CONTROL_BACKLIGHT_ENABLE: u32 = PP_CONTROL::BACKLIGHT_ENABLE::SET.value;
const PP_CONTROL_POWER_DOWN_ON_RESET: u32 = PP_CONTROL::POWER_DOWN_ON_RESET::SET.value;
const PP_CONTROL_TARGET_ON: u32 = PP_CONTROL::TARGET_ON::SET.value;
const BXT_PP_CONTROL_PWR_CYC_DELAY_SHIFT: u32 = 4;
const BXT_PP_CONTROL_PWR_CYC_DELAY_MASK: u32 = PP_CONTROL::PWR_CYC_DELAY.val(0x1f).value;
const PP_ON_DELAYS_PORT_SELECT_MASK: u32 = PP_ON_DELAYS::PORT_SELECT.val(3).value;
const PP_ON_DELAYS_PWR_UP_MASK: u32 = PP_ON_DELAYS::PWR_UP.val(0x1fff).value;
const PP_ON_DELAYS_PWR_UP_BL_ON_MASK: u32 = PP_ON_DELAYS::PWR_UP_TO_BL_ON.val(0x1fff).value;
const PP_OFF_DELAYS_PWR_DOWN_MASK: u32 = PP_OFF_DELAYS::PWR_DOWN.val(0x1fff).value;
const PP_OFF_DELAYS_BL_OFF_PWR_DOWN_MASK: u32 = PP_OFF_DELAYS::BL_OFF_TO_PWR_DOWN.val(0x1fff).value;
const PP_DIVISOR_PWR_CYC_DELAY_MASK: u32 = PP_DIVISOR::PWR_CYC_DELAY.val(0x1f).value;
const CPU_BLC_PWM_DATA_BL_DUTY_CYC_MASK: u32 = CPU_BLC_PWM_CTL::BL_DUTY_CYCLE.val(0xffff).value;
const BXT_BLC_PWM_CTL_ENABLE: u32 = BXT_BLC_PWM_CTL::ENABLE::SET.value;

const fn default_if_zero(value: u32, default: u32) -> u32 {
    if value == 0 { default } else { value }
}

const fn pp_delay_100us(us: u32, shift: u32) -> u32 {
    us.div_ceil(100) << shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_power_delays_decode_and_default_like_libgfxinit() {
        let delays = PanelPowerDelays::from_registers(0x000a_0014, 0x001e_0028, 6);
        assert_eq!(
            delays,
            PanelPowerDelays {
                power_up_us: 1_000,
                power_up_to_backlight_on_us: 2_000,
                power_down_us: 3_000,
                backlight_off_to_power_down_us: 4_000,
                power_cycle_us: 500_000,
            }
        );
        assert_eq!(
            PanelPowerDelays::from_registers(0, 0, 0).with_defaults(),
            PanelPowerDelays::DEFAULT_EDP
        );
    }

    #[test]
    fn only_edp_selects_the_dp_a_panel_port() {
        assert_eq!(
            PanelPowerPortSelect::from_port(Port::Edp),
            PanelPowerPortSelect::DpA
        );
        // DP1/HDMI1 are not DP-A; libgfxinit maps only eDP there.
        assert_eq!(
            PanelPowerPortSelect::from_port(Port::DpA),
            PanelPowerPortSelect::None
        );
        assert_eq!(
            PanelPowerPortSelect::from_port(Port::HdmiA),
            PanelPowerPortSelect::None
        );
        assert_eq!(
            PanelPowerPortSelect::from_port(Port::DpB),
            PanelPowerPortSelect::DpC
        );
        assert_eq!(
            PanelPowerPortSelect::from_port(Port::HdmiC),
            PanelPowerPortSelect::DpD
        );
    }

    #[test]
    fn delay_override_is_only_requested_for_zero_fields() {
        assert!(!PanelPowerDelays::DEFAULT_EDP.needs_override());
        assert!(PanelPowerDelays::from_registers(0, 0, 0).needs_override());
        // A single zero field is enough (libgfxinit `Check_PP_Delays`).
        let mut partial = PanelPowerDelays::DEFAULT_EDP;
        partial.power_cycle_us = 0;
        assert!(partial.needs_override());
    }

    #[test]
    fn backlight_ops_match_legacy_and_new_models() {
        assert_eq!(
            set_backlight_op(
                BacklightRegisterModel::Legacy {
                    duty_ctl: 0x48254,
                    freq_ctl: 0xc8254,
                },
                0x1_2345,
            ),
            PanelRegisterOp::Update {
                register: 0x48254,
                mask_unset: CPU_BLC_PWM_DATA_BL_DUTY_CYC_MASK,
                mask_set: 0x2345,
            }
        );
        let new = BacklightRegisterModel::New {
            ctl: 0xc8250,
            freq: 0xc8254,
            duty: 0xc8258,
        };
        assert_eq!(
            set_backlight_op(new, 0x1234),
            PanelRegisterOp::Write {
                register: 0xc8258,
                value: 0x1234,
            }
        );
        assert_eq!(
            backlight_pwm_enable_op(new),
            Some(PanelRegisterOp::Set {
                register: 0xc8250,
                mask: BXT_BLC_PWM_CTL_ENABLE,
            })
        );
        assert_eq!(
            backlight_pwm_disable_op(BacklightRegisterModel::Legacy {
                duty_ctl: 1,
                freq_ctl: 2
            }),
            None
        );
    }
}
