//! Panel metadata helpers for Intel GMA display initialization.
//!
//! This module intentionally models VBT/libgfxinit panel data as pure values.
//! Hardware sequencing still lives in generation code and is not changed by
//! these helpers.

use serde::{Deserialize, Serialize};

use crate::mode::Mode;
use crate::regs::{
    BXT_BLC_PWM_CTL, CPU_BLC_PWM_CTL, PP_CONTROL, PP_DIVISOR, PP_OFF_DELAYS, PP_ON_DELAYS,
};
use crate::types::Port;

/// LVDS panel options from VBT block 40 (`BDB_LFP_OPTIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        div_round_up(self.power_cycle_us, 100_000) + 1
    }

    /// Encode Broxton-style PP_CONTROL power-cycle delay bits.
    pub const fn encode_bxt_control_cycle_delay(self) -> u32 {
        self.encode_divisor_cycle_delay() << BXT_PP_CONTROL_PWR_CYC_DELAY_SHIFT
    }
}

/// Panel power port-select values used in PP_ON_DELAYS on PCH platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
            Port::Edp | Port::DpA | Port::HdmiA => Self::DpA,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelPowerSequencerPlan {
    /// Effective delay values after applying defaults.
    pub delays: PanelPowerDelays,
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
    let effective = delays.with_defaults();
    PanelPowerSequencerPlan {
        delays: effective,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

const fn div_round_up(num: u32, denom: u32) -> u32 {
    num.div_ceil(denom)
}

const fn pp_delay_100us(us: u32, shift: u32) -> u32 {
    div_round_up(us, 100) << shift
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
    fn panel_power_sequencer_plan_matches_libgfxinit_masks() {
        let plan = panel_power_sequencer_plan(
            PanelPowerRegs::PCH,
            PanelPowerDelays::DEFAULT_EDP,
            PanelPowerPortSelect::DpA,
            true,
            true,
        );
        assert_eq!(
            plan.on_delays,
            PanelRegisterOp::Update {
                register: 0xc7208,
                mask_unset: PP_ON_DELAYS_PORT_SELECT_MASK
                    | PP_ON_DELAYS_PWR_UP_MASK
                    | PP_ON_DELAYS_PWR_UP_BL_ON_MASK,
                mask_set: (1 << 30) | (2100 << 16) | 1,
            }
        );
        assert_eq!(
            plan.off_delays,
            PanelRegisterOp::Update {
                register: 0xc720c,
                mask_unset: PP_OFF_DELAYS_PWR_DOWN_MASK | PP_OFF_DELAYS_BL_OFF_PWR_DOWN_MASK,
                mask_set: (5000 << 16) | 500,
            }
        );
        assert_eq!(
            plan.cycle_delay,
            PanelRegisterOp::Update {
                register: 0xc7210,
                mask_unset: PP_DIVISOR_PWR_CYC_DELAY_MASK,
                mask_set: 7,
            }
        );
        assert_eq!(
            plan.control,
            PanelRegisterOp::Update {
                register: 0xc7204,
                mask_unset: PP_CONTROL_WRITE_PROTECT_MASK,
                mask_set: PP_CONTROL_WRITE_PROTECT_KEY | PP_CONTROL_POWER_DOWN_ON_RESET,
            }
        );
    }

    #[test]
    fn panel_power_port_select_maps_libgfxinit_ports() {
        assert_eq!(PanelPowerPortSelect::from_port(Port::Lvds).bits(), 0);
        assert_eq!(PanelPowerPortSelect::from_port(Port::Edp).bits(), 1 << 30);
        assert_eq!(PanelPowerPortSelect::from_port(Port::DpB).bits(), 2 << 30);
        assert_eq!(PanelPowerPortSelect::from_port(Port::HdmiC).bits(), 3 << 30);
        assert_eq!(PanelPowerPortSelect::from_port(Port::Vga).bits(), 0);
    }

    #[test]
    fn panel_power_control_plan_matches_libgfxinit_on_off_backlight() {
        let plan = panel_power_control_plan(PanelPowerRegs::GMCH);
        assert_eq!(
            plan.panel_on,
            PanelRegisterOp::Set {
                register: 0x61204,
                mask: PP_CONTROL_TARGET_ON,
            }
        );
        assert_eq!(
            plan.panel_off,
            PanelRegisterOp::Clear {
                register: 0x61204,
                mask: PP_CONTROL_TARGET_ON | PP_CONTROL_VDD_OVERRIDE,
            }
        );
        assert_eq!(plan.vdd_override, plan.panel_on);
        assert_eq!(
            plan.backlight_on,
            PanelRegisterOp::Set {
                register: 0x61204,
                mask: PP_CONTROL_BACKLIGHT_ENABLE,
            }
        );
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
