//! Output/connector pipeline helpers for Intel GMA display initialization.
//!
//! libgfxinit resolves a `Port_Config` before programming PLLs, pipes, planes,
//! and connectors.  This module captures the current legacy-GMCH subset of that
//! resolution so unsupported connector families remain explicit.

use crate::error::GmaError;
use crate::framebuffer::SurfaceConfig;
use crate::mode::Mode;
use crate::pipe::PipeConfig;
use crate::plane::{PlaneAddressModel, PlaneConfig, primary_for_pipe};
use crate::pll::LegacyPll;
use crate::regs::{ADPA, GMCH_DP, GMCH_HDMI, LVDS};
use crate::types::{Cpu, Pipe, Port};

/// Legacy GMCH ADPA register offset used by VGA/CRT output.
pub(crate) const GMCH_ADPA: usize = 0x61100;
/// Legacy GMCH LVDS port control register offset.
pub(crate) const GMCH_LVDS: usize = 0x61180;
/// Legacy GMCH HDMI/SDVO-B port control register offset.
pub(crate) const GMCH_HDMIB: usize = 0x61140;
/// Legacy GMCH HDMI/SDVO-C port control register offset.
pub(crate) const GMCH_HDMIC: usize = 0x61160;
/// Legacy GMCH DisplayPort-B control register offset.
pub(crate) const GMCH_DPB: usize = 0x64100;
/// Legacy GMCH DisplayPort-C control register offset.
pub(crate) const GMCH_DPC: usize = 0x64200;
/// Legacy GMCH DisplayPort-D control register offset.
pub(crate) const GMCH_DPD: usize = 0x64300;

const GMCH_PORT_PIPE_SELECT_MASK: u32 = ADPA::PIPE_SELECT::PipeB.value;
const ADPA_DAC_ENABLE: u32 = ADPA::DAC_ENABLE::SET.value;
const ADPA_USE_VGA_HVPOLARITY: u32 = ADPA::USE_VGA_HVPOLARITY::SET.value;
const ADPA_VSYNC_DISABLE: u32 = ADPA::VSYNC_DISABLE::SET.value;
const ADPA_HSYNC_DISABLE: u32 = ADPA::HSYNC_DISABLE::SET.value;
const ADPA_VSYNC_ACTIVE_HIGH: u32 = ADPA::VSYNC_ACTIVE_HIGH::SET.value;
const ADPA_HSYNC_ACTIVE_HIGH: u32 = ADPA::HSYNC_ACTIVE_HIGH::SET.value;
const LVDS_ENABLE: u32 = LVDS::ENABLE::SET.value;
const LVDS_DITHER_EN: u32 = LVDS::DITHER_EN::SET.value;
const LVDS_VSYNC_POLARITY_INVERT: u32 = LVDS::VSYNC_POLARITY_INVERT::SET.value;
const LVDS_HSYNC_POLARITY_INVERT: u32 = LVDS::HSYNC_POLARITY_INVERT::SET.value;
const LVDS_CLK_A_DATA_A0A2_POWER_UP: u32 = LVDS::CLK_A_DATA_A0A2_POWER::PowerUp.value;
const LVDS_CLK_B_POWER_UP: u32 = LVDS::CLK_B_POWER::PowerUp.value;
const LVDS_DATA_B0B2_POWER_UP: u32 = LVDS::DATA_B0B2_POWER::PowerUp.value;
const GMCH_HDMI_ENABLE: u32 = GMCH_HDMI::ENABLE::SET.value;
const GMCH_HDMI_COLOR_FORMAT_MASK: u32 = GMCH_HDMI::COLOR_FORMAT.val(7).value;
const GMCH_HDMI_SDVO_ENCODING_HDMI: u32 = GMCH_HDMI::SDVO_ENCODING::Hdmi.value;
const GMCH_HDMI_SDVO_ENCODING_MASK: u32 = GMCH_HDMI::SDVO_ENCODING.val(3).value;
const GMCH_HDMI_MODE_SELECT_HDMI: u32 = GMCH_HDMI::MODE_SELECT_HDMI::SET.value;
const GMCH_HDMI_VSYNC_ACTIVE_HIGH: u32 = GMCH_HDMI::VSYNC_ACTIVE_HIGH::SET.value;
const GMCH_HDMI_HSYNC_ACTIVE_HIGH: u32 = GMCH_HDMI::HSYNC_ACTIVE_HIGH::SET.value;
#[allow(dead_code)]
const GMCH_DP_DISPLAY_PORT_ENABLE: u32 = GMCH_DP::DISPLAY_PORT_ENABLE::SET.value;
#[allow(dead_code)]
const GMCH_DP_LINK_TRAIN_PAT1: u32 = GMCH_DP::LINK_TRAIN::Pattern1.value;
#[allow(dead_code)]
const GMCH_DP_LINK_TRAIN_PAT2: u32 = GMCH_DP::LINK_TRAIN::Pattern2.value;
const GMCH_DP_LINK_TRAIN_IDLE: u32 = GMCH_DP::LINK_TRAIN::Idle.value;
#[allow(dead_code)]
const GMCH_DP_LINK_TRAIN_NORMAL: u32 = GMCH_DP::LINK_TRAIN::Normal.value;
const GMCH_DP_LINK_TRAIN_MASK: u32 = GMCH_DP::LINK_TRAIN.val(3).value;
#[allow(dead_code)]
const GMCH_DP_VSWING_LEVEL_SET_MASK: u32 = GMCH_DP::VSWING_LEVEL_SET.val(7).value;
#[allow(dead_code)]
const GMCH_DP_PREEMPH_LEVEL_SET_MASK: u32 = GMCH_DP::PREEMPH_LEVEL_SET.val(7).value;
#[allow(dead_code)]
const GMCH_DP_ENHANCED_FRAMING_ENABLE: u32 = GMCH_DP::ENHANCED_FRAMING_ENABLE::SET.value;
#[allow(dead_code)]
const GMCH_DP_COLOR_RANGE_16_235: u32 = GMCH_DP::COLOR_RANGE_16_235::SET.value;
#[allow(dead_code)]
const GMCH_DP_VSYNC_ACTIVE_HIGH: u32 = GMCH_DP::VSYNC_ACTIVE_HIGH::SET.value;
#[allow(dead_code)]
const GMCH_DP_HSYNC_ACTIVE_HIGH: u32 = GMCH_DP::HSYNC_ACTIVE_HIGH::SET.value;

/// Mask of ADPA bits that libgfxinit clears before enabling VGA.
pub(crate) const ADPA_ENABLE_MASK: u32 = GMCH_PORT_PIPE_SELECT_MASK
    | ADPA_DAC_ENABLE
    | ADPA_VSYNC_DISABLE
    | ADPA_HSYNC_DISABLE
    | ADPA_VSYNC_ACTIVE_HIGH
    | ADPA_HSYNC_ACTIVE_HIGH
    | ADPA_USE_VGA_HVPOLARITY;

/// ADPA value used to disable VGA sync/DAC output without enabling the DAC.
pub(crate) const ADPA_DISABLE_VALUE: u32 = ADPA_HSYNC_DISABLE | ADPA_VSYNC_DISABLE;

/// LVDS value used by libgfxinit's Off path: all lane power fields down.
pub(crate) const LVDS_DISABLE_VALUE: u32 = 0;

/// Mask of HDMI bits that libgfxinit clears before enabling/disabling GMCH HDMI.
pub(crate) const GMCH_HDMI_MASK: u32 = GMCH_PORT_PIPE_SELECT_MASK
    | GMCH_HDMI_ENABLE
    | GMCH_HDMI_COLOR_FORMAT_MASK
    | GMCH_HDMI_SDVO_ENCODING_MASK
    | GMCH_HDMI_MODE_SELECT_HDMI
    | GMCH_HDMI_HSYNC_ACTIVE_HIGH
    | GMCH_HDMI_VSYNC_ACTIVE_HIGH;

/// HDMI disable value used by libgfxinit's Off path.
pub(crate) const GMCH_HDMI_DISABLE_VALUE: u32 =
    GMCH_HDMI_HSYNC_ACTIVE_HIGH | GMCH_HDMI_VSYNC_ACTIVE_HIGH;

/// Data-only operation needed to program one legacy GMCH port register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortRegisterOp {
    /// Write a full register value.
    Write {
        /// MMIO register offset.
        register: usize,
        /// Full value to write.
        value: u32,
    },
    /// Clear a mask and set selected bits, matching libgfxinit's `Unset_And_Set_Mask`.
    Update {
        /// MMIO register offset.
        register: usize,
        /// Bits to clear before setting new bits.
        mask_unset: u32,
        /// Bits to set after clearing the mask.
        mask_set: u32,
    },
}

/// Link parameters needed to program a legacy GMCH DP control register.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GmchDpLinkConfig {
    /// Number of DisplayPort lanes, encoded as 1, 2, or 4.
    pub lane_count: u8,
    /// Whether enhanced framing is enabled for the link.
    pub enhanced_framing: bool,
}

#[allow(dead_code)]
impl GmchDpLinkConfig {
    /// Conservative single-lane config used by planning tests.
    pub const SINGLE_LANE: Self = Self {
        lane_count: 1,
        enhanced_framing: true,
    };

    /// Validate the lane count against DP's supported lane-count values.
    pub const fn validate(self) -> Result<(), GmaError> {
        match self.lane_count {
            1 | 2 | 4 => Ok(()),
            _ => Err(GmaError::InvalidConfig),
        }
    }
}

/// Data-only legacy GMCH port programming plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyPortPlan {
    /// Port this plan controls.
    pub port: Port,
    /// Pipe selected by the port register, when the port is enabled.
    pub pipe: Pipe,
    /// Operation used before DPLL enable. LVDS requires this; VGA has none.
    pub pre_pll: Option<PortRegisterOp>,
    /// Operation used to enable the port after pipe/plane programming.
    pub enable: PortRegisterOp,
    /// Operation used to disable this port.
    pub disable: PortRegisterOp,
}

impl LegacyPortPlan {
    /// Build a libgfxinit-style legacy GMCH port plan without touching MMIO.
    pub const fn for_port(port: Port, pipe: Pipe, mode: Mode) -> Result<Self, GmaError> {
        match port {
            Port::Lvds => match lvds_port_value(pipe, mode) {
                Ok(value) => Ok(Self {
                    port,
                    pipe,
                    pre_pll: Some(PortRegisterOp::Write {
                        register: GMCH_LVDS,
                        value,
                    }),
                    enable: PortRegisterOp::Write {
                        register: GMCH_LVDS,
                        value,
                    },
                    disable: PortRegisterOp::Write {
                        register: GMCH_LVDS,
                        value: LVDS_DISABLE_VALUE,
                    },
                }),
                Err(err) => Err(err),
            },
            Port::Vga => match adpa_enable_value(pipe, mode) {
                Ok(mask_set) => Ok(Self {
                    port,
                    pipe,
                    pre_pll: None,
                    enable: PortRegisterOp::Update {
                        register: GMCH_ADPA,
                        mask_unset: ADPA_ENABLE_MASK,
                        mask_set,
                    },
                    disable: PortRegisterOp::Update {
                        register: GMCH_ADPA,
                        mask_unset: ADPA_DAC_ENABLE,
                        mask_set: ADPA_DISABLE_VALUE,
                    },
                }),
                Err(err) => Err(err),
            },
            Port::HdmiA | Port::HdmiB => {
                match (gmch_hdmi_register(port), hdmi_enable_value(pipe, mode)) {
                    (Ok(register), Ok(mask_set)) => Ok(Self {
                        port,
                        pipe,
                        pre_pll: None,
                        enable: PortRegisterOp::Update {
                            register,
                            mask_unset: GMCH_HDMI_MASK,
                            mask_set,
                        },
                        disable: PortRegisterOp::Update {
                            register,
                            mask_unset: GMCH_HDMI_MASK,
                            mask_set: GMCH_HDMI_DISABLE_VALUE,
                        },
                    }),
                    (Err(err), _) | (_, Err(err)) => Err(err),
                }
            }
            _ => Err(GmaError::UnsupportedPort),
        }
    }
}

/// Ordered libgfxinit-style off operations for one legacy GMCH port.
///
/// The live G45 cleanup path uses generation-local disable helpers today; this
/// data-only off plan is retained for parity tests and future shared sequencing.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyPortOffPlan {
    /// Port this plan disables.
    pub port: Port,
    /// Ordered register operations. DP uses idle then off; other ports use one op.
    pub ops: [Option<PortRegisterOp>; 2],
}

impl LegacyPortOffPlan {
    /// Build a GMCH connector off plan without touching MMIO.
    ///
    /// Test/staged helper until live connector-off sequencing consumes
    /// `LegacyPortOffPlan` directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn for_port(port: Port) -> Result<Self, GmaError> {
        match port {
            Port::Lvds => Ok(Self {
                port,
                ops: [Some(lvds_disable_op()), None],
            }),
            Port::Vga => Ok(Self {
                port,
                ops: [Some(vga_disable_op()), None],
            }),
            Port::HdmiA | Port::HdmiB => match hdmi_disable_op(port) {
                Ok(op) => Ok(Self {
                    port,
                    ops: [Some(op), None],
                }),
                Err(err) => Err(err),
            },
            Port::DpA | Port::DpB | Port::DpC => match (dp_idle_op(port), dp_off_op(port)) {
                (Ok(idle), Ok(off)) => Ok(Self {
                    port,
                    ops: [Some(idle), Some(off)],
                }),
                (Err(err), _) | (_, Err(err)) => Err(err),
            },
            _ => Err(GmaError::UnsupportedPort),
        }
    }
}

/// Resolved pipeline for one currently-supported legacy GMCH output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputPipeline {
    /// Active output port.
    pub port: Port,
    /// Pipe timing configuration.
    pub pipe: PipeConfig,
    /// Primary plane configuration.
    pub plane: PlaneConfig,
    /// Legacy DPLL selected for the pipe.
    pub pll: LegacyPll,
}

impl OutputPipeline {
    /// Resolve the current single-output legacy GMCH pipeline.
    pub fn legacy_gmch(
        cpu: Cpu,
        port: Port,
        mode: Mode,
        surface: SurfaceConfig,
    ) -> Result<Self, GmaError> {
        validate_current_board_port(cpu, port)?;
        let pipe = pipe_for_legacy_gmch_port(port)?;
        let address_model = legacy_plane_address_model(cpu);
        Ok(Self {
            port,
            pipe: PipeConfig::new(pipe, mode),
            plane: PlaneConfig::new(primary_for_pipe(pipe), pipe, address_model, surface),
            pll: legacy_pll_for_pipe(pipe)?,
        })
    }
}

/// Reject legacy-GMCH ports outside the currently hardware-enabled board scope.
pub(crate) const fn validate_current_board_port(cpu: Cpu, port: Port) -> Result<(), GmaError> {
    match (cpu, port) {
        (Cpu::Pineview, Port::Vga)
        | (Cpu::Gm965, Port::Lvds | Port::Vga)
        | (
            Cpu::G45 | Cpu::Gm45,
            Port::Lvds | Port::Vga | Port::HdmiA | Port::HdmiB | Port::DpA | Port::DpB | Port::DpC,
        ) => Ok(()),
        // Ports outside this generation connector implementation are rejected.
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Return the pipe used by the implemented legacy GMCH output path.
pub(crate) const fn pipe_for_legacy_gmch_port(port: Port) -> Result<Pipe, GmaError> {
    match port {
        Port::Lvds => Ok(Pipe::B),
        Port::Vga | Port::HdmiA | Port::HdmiB | Port::DpA | Port::DpB | Port::DpC => Ok(Pipe::A),
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Return the legacy DPLL paired with a pipe.
pub(crate) const fn legacy_pll_for_pipe(pipe: Pipe) -> Result<LegacyPll, GmaError> {
    match pipe {
        Pipe::A => Ok(LegacyPll::A),
        Pipe::B => Ok(LegacyPll::B),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

/// Return the libgfxinit-style VGA disable operation.
pub(crate) const fn vga_disable_op() -> PortRegisterOp {
    PortRegisterOp::Update {
        register: GMCH_ADPA,
        mask_unset: ADPA_DAC_ENABLE,
        mask_set: ADPA_DISABLE_VALUE,
    }
}

/// Return the libgfxinit-style HDMI disable operation for a GMCH HDMI port.
pub(crate) const fn hdmi_disable_op(port: Port) -> Result<PortRegisterOp, GmaError> {
    match gmch_hdmi_register(port) {
        Ok(register) => Ok(PortRegisterOp::Update {
            register,
            mask_unset: GMCH_HDMI_MASK,
            mask_set: GMCH_HDMI_DISABLE_VALUE,
        }),
        Err(err) => Err(err),
    }
}

/// Return the libgfxinit-style initial DP control write for a GMCH DP port.
#[allow(dead_code)]
pub(crate) const fn dp_enable_op(
    port: Port,
    pipe: Pipe,
    mode: Mode,
    link: GmchDpLinkConfig,
) -> Result<PortRegisterOp, GmaError> {
    match (
        gmch_dp_register(port),
        gmch_port_pipe_select(pipe),
        link.validate(),
    ) {
        (Ok(register), Ok(pipe_select), Ok(())) => Ok(PortRegisterOp::Write {
            register,
            value: GMCH_DP_DISPLAY_PORT_ENABLE
                | dp_port_width(link.lane_count)
                | GMCH_DP_LINK_TRAIN_PAT1
                | if_bool(link.enhanced_framing, GMCH_DP_ENHANCED_FRAMING_ENABLE)
                | pipe_select
                | dp_sync_polarity(mode)
                | GMCH_DP_COLOR_RANGE_16_235,
        }),
        (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => Err(err),
    }
}

/// Return the libgfxinit-style DP training-pattern operation for a GMCH DP port.
#[allow(dead_code)]
pub(crate) const fn dp_training_pattern_op(
    port: Port,
    pattern: crate::dp_aux::TrainingPattern,
) -> Result<PortRegisterOp, GmaError> {
    match gmch_dp_register(port) {
        Ok(register) => Ok(PortRegisterOp::Update {
            register,
            mask_unset: GMCH_DP_LINK_TRAIN_MASK,
            mask_set: gmch_dp_training_pattern_bits(pattern),
        }),
        Err(err) => Err(err),
    }
}

/// Return the libgfxinit-style DP signal-level operation for a GMCH DP port.
#[allow(dead_code)]
pub(crate) const fn dp_signal_levels_op(
    port: Port,
    train_set: crate::dp_aux::TrainSet,
) -> Result<PortRegisterOp, GmaError> {
    match gmch_dp_register(port) {
        Ok(register) => Ok(PortRegisterOp::Update {
            register,
            mask_unset: GMCH_DP_VSWING_LEVEL_SET_MASK | GMCH_DP_PREEMPH_LEVEL_SET_MASK,
            mask_set: gmch_dp_signal_level_bits(train_set),
        }),
        Err(err) => Err(err),
    }
}

/// Return the libgfxinit-style DP idle-training operation for a GMCH DP port.
pub(crate) const fn dp_idle_op(port: Port) -> Result<PortRegisterOp, GmaError> {
    match gmch_dp_register(port) {
        Ok(register) => Ok(PortRegisterOp::Update {
            register,
            mask_unset: GMCH_DP_LINK_TRAIN_MASK,
            mask_set: GMCH_DP_LINK_TRAIN_IDLE,
        }),
        Err(err) => Err(err),
    }
}

/// Return the libgfxinit-style DP off write for a GMCH DP port.
pub(crate) const fn dp_off_op(port: Port) -> Result<PortRegisterOp, GmaError> {
    match gmch_dp_register(port) {
        Ok(register) => Ok(PortRegisterOp::Write { register, value: 0 }),
        Err(err) => Err(err),
    }
}

/// Return the libgfxinit-style LVDS disable operation.
pub(crate) const fn lvds_disable_op() -> PortRegisterOp {
    PortRegisterOp::Write {
        register: GMCH_LVDS,
        value: LVDS_DISABLE_VALUE,
    }
}

/// Return the plane address model for the implemented legacy GMCH CPUs.
pub(crate) const fn legacy_plane_address_model(cpu: Cpu) -> PlaneAddressModel {
    match cpu {
        Cpu::Pineview => PlaneAddressModel::Address,
        _ => PlaneAddressModel::Surface,
    }
}

/// Return the GMCH HDMI register for libgfxinit HDMI1/HDMI2 (DIGI_B/DIGI_C).
pub(crate) const fn gmch_hdmi_register(port: Port) -> Result<usize, GmaError> {
    match port {
        Port::HdmiA => Ok(GMCH_HDMIB),
        Port::HdmiB => Ok(GMCH_HDMIC),
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Return the GMCH DP register for libgfxinit DP1/DP2/DP3 (DIGI_B/DIGI_C/DIGI_D).
pub(crate) const fn gmch_dp_register(port: Port) -> Result<usize, GmaError> {
    match port {
        Port::DpA => Ok(GMCH_DPB),
        Port::DpB => Ok(GMCH_DPC),
        Port::DpC => Ok(GMCH_DPD),
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Return legacy GMCH one-bit pipe select encoding used by ADPA/LVDS/HDMI.
pub(crate) const fn gmch_port_pipe_select(pipe: Pipe) -> Result<u32, GmaError> {
    match pipe {
        Pipe::A => Ok(0),
        Pipe::B => Ok(GMCH_PORT_PIPE_SELECT_MASK),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

/// Encode libgfxinit-style ADPA enable bits for VGA/CRT.
pub(crate) const fn adpa_enable_value(pipe: Pipe, mode: Mode) -> Result<u32, GmaError> {
    match gmch_port_pipe_select(pipe) {
        Ok(pipe_select) => Ok(ADPA_DAC_ENABLE | pipe_select | adpa_sync_polarity(mode)),
        Err(err) => Err(err),
    }
}

/// Encode libgfxinit/Linux ADPA sync polarity bits.
pub(crate) const fn adpa_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(crate::mode::ModeFlags::PHSYNC) {
        ADPA_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(crate::mode::ModeFlags::PVSYNC) {
        ADPA_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

/// VBT/libgfxinit policy bits used to encode GMCH LVDS port control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LvdsPortConfig {
    /// Force dual-channel lane power when VBT per-panel channel metadata is available.
    pub force_dual_channel: Option<bool>,
    /// Enable pixel dithering.
    pub enable_dither: bool,
}

impl LvdsPortConfig {
    /// Historical fstart/libgfxinit-safe defaults used before VBT metadata is applied.
    pub const DEFAULT: Self = Self {
        force_dual_channel: None,
        enable_dither: true,
    };
}

/// Encode libgfxinit-style LVDS port control value.
pub(crate) const fn lvds_port_value(pipe: Pipe, mode: Mode) -> Result<u32, GmaError> {
    lvds_port_value_with_config(pipe, mode, LvdsPortConfig::DEFAULT)
}

/// Encode libgfxinit-style LVDS port control value with parsed VBT policy.
pub(crate) const fn lvds_port_value_with_config(
    pipe: Pipe,
    mode: Mode,
    config: LvdsPortConfig,
) -> Result<u32, GmaError> {
    match gmch_port_pipe_select(pipe) {
        Ok(pipe_select) => Ok(LVDS_ENABLE
            | pipe_select
            | lvds_sync_polarity(mode)
            | LVDS_CLK_A_DATA_A0A2_POWER_UP
            | lvds_dual_channel_bits_with_config(mode, config)
            | if_bool(config.enable_dither, LVDS_DITHER_EN)),
        Err(err) => Err(err),
    }
}

/// Encode libgfxinit-style DP training-pattern bits.
#[allow(dead_code)]
pub(crate) const fn gmch_dp_training_pattern_bits(pattern: crate::dp_aux::TrainingPattern) -> u32 {
    match pattern {
        crate::dp_aux::TrainingPattern::Pattern1 => GMCH_DP_LINK_TRAIN_PAT1,
        crate::dp_aux::TrainingPattern::Pattern2 | crate::dp_aux::TrainingPattern::Pattern3 => {
            GMCH_DP_LINK_TRAIN_PAT2
        }
        crate::dp_aux::TrainingPattern::None => GMCH_DP_LINK_TRAIN_NORMAL,
    }
}

/// Encode libgfxinit-style DP signal-level bits.
#[allow(dead_code)]
pub(crate) const fn gmch_dp_signal_level_bits(train_set: crate::dp_aux::TrainSet) -> u32 {
    GMCH_DP::VSWING_LEVEL_SET
        .val(train_set.voltage_swing as u32)
        .value
        | GMCH_DP::PREEMPH_LEVEL_SET
            .val(train_set.pre_emphasis as u32)
            .value
}

/// Encode libgfxinit-style DP port-width bits.
#[allow(dead_code)]
pub(crate) const fn dp_port_width(lane_count: u8) -> u32 {
    GMCH_DP::PORT_WIDTH.val((lane_count as u32) - 1).value
}

/// Encode libgfxinit/Linux GMCH DP sync polarity bits.
#[allow(dead_code)]
pub(crate) const fn dp_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(crate::mode::ModeFlags::PHSYNC) {
        GMCH_DP_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(crate::mode::ModeFlags::PVSYNC) {
        GMCH_DP_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

#[allow(dead_code)]
const fn if_bool(condition: bool, value: u32) -> u32 {
    if condition { value } else { 0 }
}

/// Encode libgfxinit-style GMCH HDMI enable bits.
pub(crate) const fn hdmi_enable_value(pipe: Pipe, mode: Mode) -> Result<u32, GmaError> {
    match gmch_port_pipe_select(pipe) {
        Ok(pipe_select) => Ok(GMCH_HDMI_ENABLE
            | pipe_select
            | GMCH_HDMI_SDVO_ENCODING_HDMI
            | hdmi_sync_polarity(mode)),
        Err(err) => Err(err),
    }
}

/// Encode libgfxinit/Linux GMCH HDMI sync polarity bits.
pub(crate) const fn hdmi_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(crate::mode::ModeFlags::PHSYNC) {
        GMCH_HDMI_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(crate::mode::ModeFlags::PVSYNC) {
        GMCH_HDMI_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

/// Encode LVDS sync polarity inversion bits.
pub(crate) const fn lvds_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(crate::mode::ModeFlags::PHSYNC) {
        0
    } else {
        LVDS_HSYNC_POLARITY_INVERT
    };
    let v = if mode.flags.contains(crate::mode::ModeFlags::PVSYNC) {
        0
    } else {
        LVDS_VSYNC_POLARITY_INVERT
    };
    h | v
}

/// libgfxinit threshold for powering dual-channel LVDS B lanes when VBT does
/// not provide per-panel channel metadata.
const LVDS_DUAL_CHANNEL_THRESHOLD_KHZ: u32 = 95_000;

/// Return LVDS dual-channel lane power bits for high pixel clocks.
#[allow(dead_code)]
pub(crate) const fn lvds_dual_channel_bits(mode: Mode) -> u32 {
    lvds_dual_channel_bits_with_config(mode, LvdsPortConfig::DEFAULT)
}

const fn lvds_dual_channel_bits_with_config(mode: Mode, config: LvdsPortConfig) -> u32 {
    let dual_channel = match config.force_dual_channel {
        Some(value) => value,
        None => mode.pixel_clock_khz >= LVDS_DUAL_CHANNEL_THRESHOLD_KHZ,
    };
    if dual_channel {
        LVDS_CLK_B_POWER_UP | LVDS_DATA_B0B2_POWER_UP
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::{PixelFormat, SurfaceConfig};
    use crate::types::{PhysAddr, Plane};

    #[test]
    fn resolves_enabled_board_pipelines() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let lvds =
            OutputPipeline::legacy_gmch(Cpu::Gm965, Port::Lvds, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(lvds.pipe.pipe, Pipe::B);
        assert_eq!(lvds.plane.plane, Plane::PrimaryB);
        assert_eq!(lvds.plane.address_model, PlaneAddressModel::Surface);

        let pineview_vga =
            OutputPipeline::legacy_gmch(Cpu::Pineview, Port::Vga, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(pineview_vga.pipe.pipe, Pipe::A);
        assert_eq!(pineview_vga.pll, LegacyPll::A);
        assert_eq!(pineview_vga.plane.address_model, PlaneAddressModel::Address);

        let gm965_vga =
            OutputPipeline::legacy_gmch(Cpu::Gm965, Port::Vga, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(gm965_vga.pipe.pipe, Pipe::A);
        assert_eq!(gm965_vga.plane.plane, Plane::PrimaryA);
        assert_eq!(gm965_vga.plane.address_model, PlaneAddressModel::Surface);

        let g45_lvds =
            OutputPipeline::legacy_gmch(Cpu::G45, Port::Lvds, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(g45_lvds.pipe.pipe, Pipe::B);
        assert_eq!(g45_lvds.pll, LegacyPll::B);

        let g45_dp =
            OutputPipeline::legacy_gmch(Cpu::G45, Port::DpA, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(g45_dp.pipe.pipe, Pipe::A);
        assert_eq!(g45_dp.pll, LegacyPll::A);
        assert_eq!(g45_dp.plane.plane, Plane::PrimaryA);
    }

    #[test]
    fn rejects_unimplemented_connector_pipelines() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1024, 768, PixelFormat::Xrgb8888);
        for (cpu, port) in [(Cpu::Gm965, Port::DpB), (Cpu::Pineview, Port::Lvds)] {
            assert!(matches!(
                OutputPipeline::legacy_gmch(cpu, port, Mode::XGA_1024X768_60, surface),
                Err(GmaError::UnsupportedPort)
            ));
        }
    }

    #[test]
    fn encodes_libgfxinit_vga_plan_for_pineview_path() {
        let plan = LegacyPortPlan::for_port(Port::Vga, Pipe::A, Mode::XGA_1024X768_60).unwrap();
        assert_eq!(plan.pre_pll, None);
        assert_eq!(
            plan.enable,
            PortRegisterOp::Update {
                register: GMCH_ADPA,
                mask_unset: ADPA_ENABLE_MASK,
                mask_set: ADPA_DAC_ENABLE,
            }
        );
        assert_eq!(
            plan.disable,
            PortRegisterOp::Update {
                register: GMCH_ADPA,
                mask_unset: ADPA_DAC_ENABLE,
                mask_set: ADPA_HSYNC_DISABLE | ADPA_VSYNC_DISABLE,
            }
        );
    }

    #[test]
    fn encodes_libgfxinit_lvds_plan_for_gm965_path() {
        let plan = LegacyPortPlan::for_port(Port::Lvds, Pipe::B, Mode::XGA_1024X768_60).unwrap();
        let expected = LVDS_ENABLE
            | GMCH_PORT_PIPE_SELECT_MASK
            | LVDS_HSYNC_POLARITY_INVERT
            | LVDS_VSYNC_POLARITY_INVERT
            | LVDS_CLK_A_DATA_A0A2_POWER_UP
            | LVDS_DITHER_EN;
        assert_eq!(
            plan.pre_pll,
            Some(PortRegisterOp::Write {
                register: GMCH_LVDS,
                value: expected,
            })
        );
        assert_eq!(
            plan.enable,
            PortRegisterOp::Write {
                register: GMCH_LVDS,
                value: expected,
            }
        );
        assert_eq!(
            plan.disable,
            PortRegisterOp::Write {
                register: GMCH_LVDS,
                value: LVDS_DISABLE_VALUE,
            }
        );
    }

    #[test]
    fn lvds_dual_channel_threshold_matches_libgfxinit() {
        let mut below = Mode::XGA_1024X768_60;
        below.pixel_clock_khz = LVDS_DUAL_CHANNEL_THRESHOLD_KHZ - 1;
        let below_value = lvds_port_value(Pipe::B, below).unwrap();
        assert_eq!(below_value & LVDS_CLK_B_POWER_UP, 0);
        assert_eq!(below_value & LVDS_DATA_B0B2_POWER_UP, 0);

        let mut at_threshold = Mode::XGA_1024X768_60;
        at_threshold.pixel_clock_khz = LVDS_DUAL_CHANNEL_THRESHOLD_KHZ;
        let at_value = lvds_port_value(Pipe::B, at_threshold).unwrap();
        assert_ne!(at_value & LVDS_CLK_B_POWER_UP, 0);
        assert_ne!(at_value & LVDS_DATA_B0B2_POWER_UP, 0);

        let mut above = Mode::XGA_1024X768_60;
        above.pixel_clock_khz = LVDS_DUAL_CHANNEL_THRESHOLD_KHZ + 1;
        let above_value = lvds_port_value(Pipe::B, above).unwrap();
        assert_ne!(above_value & LVDS_CLK_B_POWER_UP, 0);
        assert_ne!(above_value & LVDS_DATA_B0B2_POWER_UP, 0);
    }

    #[test]
    fn legacy_gmch_port_helpers_reject_pipe_c() {
        assert_eq!(gmch_port_pipe_select(Pipe::C), Err(GmaError::InvalidConfig));
        assert_eq!(legacy_pll_for_pipe(Pipe::C), Err(GmaError::InvalidConfig));
        assert_eq!(
            adpa_enable_value(Pipe::C, Mode::XGA_1024X768_60),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            lvds_port_value(Pipe::C, Mode::XGA_1024X768_60),
            Err(GmaError::InvalidConfig)
        );
        assert!(matches!(
            LegacyPortPlan::for_port(Port::Vga, Pipe::C, Mode::XGA_1024X768_60),
            Err(GmaError::InvalidConfig)
        ));
        assert!(matches!(
            LegacyPortPlan::for_port(Port::Lvds, Pipe::C, Mode::XGA_1024X768_60),
            Err(GmaError::InvalidConfig)
        ));
        assert!(matches!(
            LegacyPortPlan::for_port(Port::HdmiA, Pipe::C, Mode::XGA_1024X768_60),
            Err(GmaError::InvalidConfig)
        ));
    }

    #[test]
    fn encodes_libgfxinit_g45_hdmi_plan() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = crate::mode::ModeFlags::PHSYNC | crate::mode::ModeFlags::PVSYNC;
        let plan = LegacyPortPlan::for_port(Port::HdmiA, Pipe::B, mode).unwrap();
        let expected = GMCH_HDMI_ENABLE
            | GMCH_PORT_PIPE_SELECT_MASK
            | GMCH_HDMI_SDVO_ENCODING_HDMI
            | GMCH_HDMI_HSYNC_ACTIVE_HIGH
            | GMCH_HDMI_VSYNC_ACTIVE_HIGH;
        assert_eq!(plan.pre_pll, None);
        assert_eq!(
            plan.enable,
            PortRegisterOp::Update {
                register: GMCH_HDMIB,
                mask_unset: GMCH_HDMI_MASK,
                mask_set: expected,
            }
        );
        assert_eq!(
            plan.disable,
            PortRegisterOp::Update {
                register: GMCH_HDMIB,
                mask_unset: GMCH_HDMI_MASK,
                mask_set: GMCH_HDMI_DISABLE_VALUE,
            }
        );
        assert_eq!(gmch_hdmi_register(Port::HdmiB), Ok(GMCH_HDMIC));
        assert_eq!(
            gmch_hdmi_register(Port::HdmiC),
            Err(GmaError::UnsupportedPort)
        );
        assert_eq!(
            hdmi_disable_op(Port::HdmiB).unwrap().clone(),
            PortRegisterOp::Update {
                register: GMCH_HDMIC,
                mask_unset: GMCH_HDMI_MASK,
                mask_set: GMCH_HDMI_DISABLE_VALUE,
            }
        );
    }

    #[test]
    fn encodes_libgfxinit_g45_dp_enable_and_disable_sequence() {
        assert_eq!(gmch_dp_register(Port::DpA), Ok(GMCH_DPB));
        assert_eq!(gmch_dp_register(Port::DpB), Ok(GMCH_DPC));
        assert_eq!(gmch_dp_register(Port::DpC), Ok(GMCH_DPD));
        assert_eq!(gmch_dp_register(Port::DpD), Err(GmaError::UnsupportedPort));
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = crate::mode::ModeFlags::PHSYNC | crate::mode::ModeFlags::PVSYNC;
        assert_eq!(
            dp_enable_op(
                Port::DpA,
                Pipe::B,
                mode,
                GmchDpLinkConfig {
                    lane_count: 4,
                    enhanced_framing: true,
                },
            )
            .unwrap()
            .clone(),
            PortRegisterOp::Write {
                register: GMCH_DPB,
                value: GMCH_DP_DISPLAY_PORT_ENABLE
                    | GMCH_PORT_PIPE_SELECT_MASK
                    | GMCH_DP::PORT_WIDTH.val(3).value
                    | GMCH_DP_ENHANCED_FRAMING_ENABLE
                    | GMCH_DP_COLOR_RANGE_16_235
                    | GMCH_DP_HSYNC_ACTIVE_HIGH
                    | GMCH_DP_VSYNC_ACTIVE_HIGH,
            }
        );
        assert_eq!(
            dp_enable_op(
                Port::DpA,
                Pipe::A,
                mode,
                GmchDpLinkConfig {
                    lane_count: 3,
                    enhanced_framing: true,
                }
            ),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            dp_idle_op(Port::DpA).unwrap().clone(),
            PortRegisterOp::Update {
                register: GMCH_DPB,
                mask_unset: GMCH_DP_LINK_TRAIN_MASK,
                mask_set: GMCH_DP_LINK_TRAIN_IDLE,
            }
        );
        assert_eq!(
            dp_training_pattern_op(Port::DpA, crate::dp_aux::TrainingPattern::Pattern2)
                .unwrap()
                .clone(),
            PortRegisterOp::Update {
                register: GMCH_DPB,
                mask_unset: GMCH_DP_LINK_TRAIN_MASK,
                mask_set: GMCH_DP_LINK_TRAIN_PAT2,
            }
        );
        assert_eq!(
            dp_signal_levels_op(
                Port::DpA,
                crate::dp_aux::TrainSet {
                    voltage_swing: crate::dp_aux::VoltageSwing::Level2,
                    pre_emphasis: crate::dp_aux::PreEmphasis::Level1,
                }
            )
            .unwrap()
            .clone(),
            PortRegisterOp::Update {
                register: GMCH_DPB,
                mask_unset: GMCH_DP_VSWING_LEVEL_SET_MASK | GMCH_DP_PREEMPH_LEVEL_SET_MASK,
                mask_set: GMCH_DP::VSWING_LEVEL_SET.val(2).value
                    | GMCH_DP::PREEMPH_LEVEL_SET.val(1).value,
            }
        );
        assert_eq!(
            dp_off_op(Port::DpC).unwrap().clone(),
            PortRegisterOp::Write {
                register: GMCH_DPD,
                value: 0,
            }
        );
    }

    #[test]
    fn legacy_port_off_plan_orders_libgfxinit_disable_sequences() {
        assert_eq!(
            LegacyPortOffPlan::for_port(Port::Vga).unwrap(),
            LegacyPortOffPlan {
                port: Port::Vga,
                ops: [Some(vga_disable_op()), None],
            }
        );
        assert_eq!(
            LegacyPortOffPlan::for_port(Port::Lvds).unwrap(),
            LegacyPortOffPlan {
                port: Port::Lvds,
                ops: [Some(lvds_disable_op()), None],
            }
        );
        assert_eq!(
            LegacyPortOffPlan::for_port(Port::HdmiB).unwrap(),
            LegacyPortOffPlan {
                port: Port::HdmiB,
                ops: [Some(hdmi_disable_op(Port::HdmiB).unwrap()), None],
            }
        );
        assert_eq!(
            LegacyPortOffPlan::for_port(Port::DpA).unwrap(),
            LegacyPortOffPlan {
                port: Port::DpA,
                ops: [
                    Some(dp_idle_op(Port::DpA).unwrap()),
                    Some(dp_off_op(Port::DpA).unwrap())
                ],
            }
        );
        assert_eq!(
            LegacyPortOffPlan::for_port(Port::HdmiC),
            Err(GmaError::UnsupportedPort)
        );
    }
}
