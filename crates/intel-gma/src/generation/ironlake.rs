//! Ironlake Intel GMA generation support.
//!
//! The Ironlake/Sandy Bridge/Ivy Bridge path now executes the split-PCH portions
//! of the libgfxinit-style plan (GTT mapping, framebuffer fill, PCH PLL/port
//! programming, CPU pipe/primary plane programming, transcoder programming, and
//! FDI training).

#![allow(dead_code)]

use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::Mmio;
use crate::mode::{Mode, ModeFlags};
use crate::plane::{PlaneAddressModel, PlaneConfig, primary_for_pipe};
use heapless::Vec;

use crate::port::PortRegisterOp;
use crate::regs::{
    CPU_DP_CTL, CPU_DSPCNTR, CPU_PIPECONF, FDI_RX_CTL, FDI_RX_IIR, FDI_RX_MISC, FDI_RX_TUSIZE1,
    FDI_TX_CTL, IronlakeFdiRxOffsets, IronlakeFdiTxOffsets, PCH_ADPA as PCH_ADPA_REG, PCH_DPLL,
    PCH_DPLL_SEL as PCH_DPLL_SEL_REG, PCH_FP, PCH_HDMI as PCH_HDMI_REG, PCH_LVDS as PCH_LVDS_REG,
    TRANS_CONF,
};
use crate::types::{Cpu, Generation, Pipe, Plane, Port};

/// Ironlake generation marker.
pub struct Ironlake;

impl sealed::Sealed for Ironlake {}

impl GenerationOps for Ironlake {
    const GENERATION: Generation = Generation::Ironlake;

    fn init_display(ctx: &mut crate::GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        let plan = ironlake_init_sequence_for_mode(ctx.config.cpu, port, mode)?;
        map_gtt(ctx)?;
        // SAFETY: the framebuffer surface was selected from validated GMADR
        // aperture/stolen-memory resources and mapped into the GTT immediately
        // above, so the CPU-visible aperture covers this surface.
        unsafe { ctx.surface.fill_bringup_pattern()? };
        let pipe = ironlake_pipeline_plan(ctx.config.cpu, port, mode)?.fdi_pipe();
        let plane = primary_for_pipe(pipe);
        let mut mmio = ctx.mmio();
        execute_ironlake_init_plan_registers(&mut mmio, &plan, mode, ctx.surface, pipe, plane)
    }
}

fn selected_port(ctx: &crate::GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

fn map_gtt(ctx: &crate::GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, &ctx.surface)?;
    gtt::flush_gfx(&ctx.mmio());
    Ok(())
}

/// Split-PCH FDI/transcoder port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdiPort {
    /// FDI/transcoder A.
    A,
    /// FDI/transcoder B.
    B,
    /// FDI/transcoder C, available on later platforms.
    C,
}

/// PCH HDMI register block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchHdmiPort {
    /// PCH HDMI-B.
    B,
    /// PCH HDMI-C.
    C,
    /// PCH HDMI-D.
    D,
}

/// libgfxinit FDI training families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdiTrainingMode {
    /// Ironlake simple training.
    Simple,
    /// Sandy Bridge full training.
    Full,
    /// Ivy Bridge auto training.
    Auto,
}

/// Split-PCH DPLL selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchPll {
    /// PCH DPLL A.
    A,
    /// PCH DPLL B.
    B,
}

/// Display clock mode used by libgfxinit's PCH DPLL encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchDpllMode {
    /// LVDS uses SSC reference and LVDS mode bits.
    Lvds,
    /// DisplayPort uses SSC reference, high-speed, and DAC mode bits.
    Dp,
    /// VGA/HDMI use DREF reference, high-speed, and DAC mode bits.
    DacHdmi,
}

/// Return the FDI training mode selected by libgfxinit for a CPU family.
pub(crate) const fn fdi_training_mode(cpu: Cpu) -> Result<FdiTrainingMode, GmaError> {
    match cpu {
        Cpu::Ironlake => Ok(FdiTrainingMode::Simple),
        Cpu::Sandybridge => Ok(FdiTrainingMode::Full),
        Cpu::Ivybridge => Ok(FdiTrainingMode::Auto),
        _ => Err(GmaError::UnsupportedPlatform),
    }
}

/// Resolve the PCH HDMI register used by a board-level logical port.
pub(crate) const fn pch_hdmi_port(port: Port) -> Result<PchHdmiPort, GmaError> {
    match port {
        Port::HdmiA => Ok(PchHdmiPort::B),
        Port::HdmiB => Ok(PchHdmiPort::C),
        Port::HdmiC => Ok(PchHdmiPort::D),
        _ => Err(GmaError::UnsupportedPort),
    }
}

const PCH_DPLL_A: usize = 0xc6014;
const PCH_DPLL_B: usize = 0xc6018;
const PCH_FPA0: usize = 0xc6040;
const PCH_FPA1: usize = 0xc6044;
const PCH_FPB0: usize = 0xc6048;
const PCH_FPB1: usize = 0xc604c;
const PCH_DPLL_SEL: usize = 0xc7000;
const TRANS_TIMING_A: usize = 0xe0000;
const TRANS_TIMING_B: usize = 0xe1000;
const TRANSACONF: usize = 0xf0008;
const TRANSBCONF: usize = 0xf1008;
const PCH_ADPA: usize = 0xe1100;
const PCH_HDMIB: usize = 0xe1140;
const PCH_HDMIC: usize = 0xe1150;
const PCH_HDMID: usize = 0xe1160;
const PCH_LVDS: usize = 0xe1180;
const DP_CTL_A: usize = 0x64000;

const PCH_TRANSCODER_SELECT_MASK: u32 = PCH_ADPA_REG::TRANSCODER_SELECT::TranscoderB.value;
const PCH_ADPA_DAC_ENABLE: u32 = PCH_ADPA_REG::DAC_ENABLE::SET.value;
const PCH_ADPA_VSYNC_DISABLE: u32 = PCH_ADPA_REG::VSYNC_DISABLE::SET.value;
const PCH_ADPA_HSYNC_DISABLE: u32 = PCH_ADPA_REG::HSYNC_DISABLE::SET.value;
const PCH_ADPA_VSYNC_ACTIVE_HIGH: u32 = PCH_ADPA_REG::VSYNC_ACTIVE_HIGH::SET.value;
const PCH_ADPA_HSYNC_ACTIVE_HIGH: u32 = PCH_ADPA_REG::HSYNC_ACTIVE_HIGH::SET.value;
const PCH_LVDS_ENABLE: u32 = PCH_LVDS_REG::ENABLE::SET.value;
const PCH_LVDS_VSYNC_POLARITY_INVERT: u32 = PCH_LVDS_REG::VSYNC_POLARITY_INVERT::SET.value;
const PCH_LVDS_HSYNC_POLARITY_INVERT: u32 = PCH_LVDS_REG::HSYNC_POLARITY_INVERT::SET.value;
const PCH_LVDS_CLK_A_DATA_A0A2_POWER_UP: u32 = PCH_LVDS_REG::CLK_A_DATA_A0A2_POWER::PowerUp.value;
const PCH_LVDS_CLK_B_POWER_UP: u32 = PCH_LVDS_REG::CLK_B_POWER::PowerUp.value;
const PCH_LVDS_DATA_B0B2_POWER_UP: u32 = PCH_LVDS_REG::DATA_B0B2_POWER::PowerUp.value;
const PCH_HDMI_ENABLE: u32 = PCH_HDMI_REG::ENABLE::SET.value;
const PCH_HDMI_COLOR_FORMAT_MASK: u32 = PCH_HDMI_REG::COLOR_FORMAT.val(7).value;
const PCH_HDMI_SDVO_ENCODING_HDMI: u32 = PCH_HDMI_REG::SDVO_ENCODING::Hdmi.value;
const PCH_HDMI_SDVO_ENCODING_MASK: u32 = PCH_HDMI_REG::SDVO_ENCODING.val(3).value;
const PCH_HDMI_VSYNC_ACTIVE_HIGH: u32 = PCH_HDMI_REG::VSYNC_ACTIVE_HIGH::SET.value;
const PCH_HDMI_HSYNC_ACTIVE_HIGH: u32 = PCH_HDMI_REG::HSYNC_ACTIVE_HIGH::SET.value;
const PCH_DPLL_VCO_ENABLE: u32 = PCH_DPLL::VCO_ENABLE::SET.value;
const PCH_DPLL_HIGH_SPEED: u32 = PCH_DPLL::HIGH_SPEED::SET.value;
const PCH_DPLL_MODE_LVDS: u32 = PCH_DPLL::MODE::Lvds.value;
const PCH_DPLL_MODE_DAC: u32 = PCH_DPLL::MODE::Dac.value;
const PCH_DPLL_P2_5_OR_7: u32 = PCH_DPLL::P2_5_OR_7::SET.value;
const PCH_DPLL_DREFCLK: u32 = PCH_DPLL::REFCLK::Dref.value;
const PCH_DPLL_SSC: u32 = PCH_DPLL::REFCLK::Ssc.value;
const PCH_DPLL_SEL_ENABLE_A: u32 = PCH_DPLL_SEL_REG::TRANSCODER_A_ENABLE::SET.value;
const PCH_DPLL_SEL_ENABLE_B: u32 = PCH_DPLL_SEL_REG::TRANSCODER_B_ENABLE::SET.value;
const PCH_DPLL_SEL_MASK_A: u32 = PCH_DPLL_SEL_REG::TRANSCODER_A_PLL.val(1).value;
const PCH_DPLL_SEL_MASK_B: u32 = PCH_DPLL_SEL_REG::TRANSCODER_B_PLL.val(1).value;
const TRANS_CONF_ENABLE: u32 = TRANS_CONF::ENABLE::SET.value;
const FDI_TX_CTL_A: usize = IronlakeFdiTxOffsets::CTL_A;
const FDI_TX_CTL_B: usize = IronlakeFdiTxOffsets::CTL_B;
const FDI_TX_CTL_C: usize = IronlakeFdiTxOffsets::CTL_C;
const FDI_RXA_CTL: usize = IronlakeFdiRxOffsets::A.ctl;
const FDI_RX_MISC_A: usize = IronlakeFdiRxOffsets::A.misc;
const FDI_RXA_IIR: usize = IronlakeFdiRxOffsets::A.iir;
const FDI_RXA_IMR: usize = IronlakeFdiRxOffsets::A.imr;
const FDI_RXA_TUSIZE1: usize = IronlakeFdiRxOffsets::A.tusize;
const FDI_RXB_CTL: usize = IronlakeFdiRxOffsets::B.ctl;
const FDI_RX_MISC_B: usize = IronlakeFdiRxOffsets::B.misc;
const FDI_RXB_IIR: usize = IronlakeFdiRxOffsets::B.iir;
const FDI_RXB_IMR: usize = IronlakeFdiRxOffsets::B.imr;
const FDI_RXB_TUSIZE1: usize = IronlakeFdiRxOffsets::B.tusize;
const FDI_RXC_CTL: usize = IronlakeFdiRxOffsets::C.ctl;
const FDI_RX_MISC_C: usize = IronlakeFdiRxOffsets::C.misc;
const FDI_RXC_IIR: usize = IronlakeFdiRxOffsets::C.iir;
const FDI_RXC_IMR: usize = IronlakeFdiRxOffsets::C.imr;
const FDI_RXC_TUSIZE1: usize = IronlakeFdiRxOffsets::C.tusize;

const FDI_TX_CTL_FDI_TX_ENABLE: u32 = FDI_TX_CTL::FDI_TX_ENABLE::SET.value;
const FDI_TX_CTL_VP_MASK: u32 = FDI_TX_CTL::VP.val(0x3f).value;
const FDI_TX_CTL_ENHANCED_FRAMING_ENABLE: u32 = FDI_TX_CTL::ENHANCED_FRAMING_ENABLE::SET.value;
const FDI_TX_CTL_FDI_PLL_ENABLE: u32 = FDI_TX_CTL::FDI_PLL_ENABLE::SET.value;
const FDI_TX_CTL_COMPOSITE_SYNC_SELECT: u32 = FDI_TX_CTL::COMPOSITE_SYNC_SELECT::SET.value;
const FDI_TX_CTL_AUTO_TRAIN_ENABLE: u32 = FDI_TX_CTL::AUTO_TRAIN_ENABLE::SET.value;
const FDI_TX_CTL_AUTO_TRAIN_DONE: u32 = FDI_TX_CTL::AUTO_TRAIN_DONE::SET.value;

const FDI_RX_CTL_FDI_RX_ENABLE: u32 = FDI_RX_CTL::FDI_RX_ENABLE::SET.value;
const FDI_RX_CTL_FS_ERROR_CORRECTION_ENABLE: u32 =
    FDI_RX_CTL::FS_ERROR_CORRECTION_ENABLE::SET.value;
const FDI_RX_CTL_FE_ERROR_CORRECTION_ENABLE: u32 =
    FDI_RX_CTL::FE_ERROR_CORRECTION_ENABLE::SET.value;
const FDI_RX_CTL_FDI_PLL_ENABLE: u32 = FDI_RX_CTL::FDI_PLL_ENABLE::SET.value;
const FDI_RX_CTL_COMPOSITE_SYNC_SELECT: u32 = FDI_RX_CTL::COMPOSITE_SYNC_SELECT::SET.value;
const FDI_RX_CTL_FDI_AUTO_TRAIN: u32 = FDI_RX_CTL::FDI_AUTO_TRAIN::SET.value;
const FDI_RX_CTL_ENHANCED_FRAMING_ENABLE: u32 = FDI_RX_CTL::ENHANCED_FRAMING_ENABLE::SET.value;
const FDI_RX_CTL_RAWCLK_TO_PCDCLK_SEL_MASK: u32 = FDI_RX_CTL::RAWCLK_TO_PCDCLK_SEL.val(1).value;
const FDI_RX_CTL_RAWCLK_TO_PCDCLK_SEL_PCDCLK: u32 = FDI_RX_CTL::RAWCLK_TO_PCDCLK_SEL::SET.value;
const FDI_RX_MISC_FDI_RX_PWRDN_LANE1_MASK: u32 = FDI_RX_MISC::FDI_RX_PWRDN_LANE1.val(3).value;
const FDI_RX_MISC_FDI_RX_PWRDN_LANE0_MASK: u32 = FDI_RX_MISC::FDI_RX_PWRDN_LANE0.val(3).value;
const FDI_RX_MISC_TP1_TO_TP2_TIME_48: u32 = FDI_RX_MISC::TP1_TO_TP2_TIME.val(2).value;
const FDI_RX_MISC_FDI_DELAY_90: u32 = FDI_RX_MISC::FDI_DELAY.val(0x90).value;
const FDI_RX_INTERLANE_ALIGNMENT: u32 = FDI_RX_IIR::INTERLANE_ALIGNMENT::SET.value;
const FDI_RX_SYMBOL_LOCK: u32 = FDI_RX_IIR::SYMBOL_LOCK::SET.value;
const FDI_RX_BIT_LOCK: u32 = FDI_RX_IIR::BIT_LOCK::SET.value;

const LVDS_DUAL_CHANNEL_THRESHOLD_KHZ: u32 = 95_000;

const PCH_ADPA_MASK: u32 = PCH_TRANSCODER_SELECT_MASK
    | PCH_ADPA_DAC_ENABLE
    | PCH_ADPA_VSYNC_DISABLE
    | PCH_ADPA_HSYNC_DISABLE
    | PCH_ADPA_VSYNC_ACTIVE_HIGH
    | PCH_ADPA_HSYNC_ACTIVE_HIGH;
const PCH_HDMI_MASK: u32 = PCH_TRANSCODER_SELECT_MASK
    | PCH_HDMI_ENABLE
    | PCH_HDMI_COLOR_FORMAT_MASK
    | PCH_HDMI_SDVO_ENCODING_MASK
    | PCH_HDMI_HSYNC_ACTIVE_HIGH
    | PCH_HDMI_VSYNC_ACTIVE_HIGH;
const PCH_HDMI_DISABLE_VALUE: u32 = PCH_HDMI_HSYNC_ACTIVE_HIGH | PCH_HDMI_VSYNC_ACTIVE_HIGH;
const DP_CTL_DISPLAYPORT_ENABLE: u32 = CPU_DP_CTL::DISPLAYPORT_ENABLE::SET.value;
const DP_CTL_PORT_WIDTH_MASK: u32 = CPU_DP_CTL::PORT_WIDTH.val(7).value;
const DP_CTL_ENHANCED_FRAMING_ENABLE: u32 = CPU_DP_CTL::ENHANCED_FRAMING_ENABLE::SET.value;
const DP_CTL_PLL_FREQUENCY_MASK: u32 = CPU_DP_CTL::PLL_FREQUENCY.val(3).value;
const DP_CTL_PLL_FREQUENCY_270: u32 = CPU_DP_CTL::PLL_FREQUENCY::Rate270MHz.value;
const DP_CTL_PLL_FREQUENCY_162: u32 = CPU_DP_CTL::PLL_FREQUENCY::Rate162MHz.value;
const DP_CTL_PLL_ENABLE: u32 = CPU_DP_CTL::PLL_ENABLE::SET.value;
const DP_CTL_LINK_TRAIN_MASK: u32 = CPU_DP_CTL::LINK_TRAIN.val(3).value;
const DP_CTL_LINK_TRAIN_PAT1: u32 = CPU_DP_CTL::LINK_TRAIN::Pattern1.value;
const DP_CTL_LINK_TRAIN_PAT2: u32 = CPU_DP_CTL::LINK_TRAIN::Pattern2.value;
const DP_CTL_LINK_TRAIN_IDLE: u32 = CPU_DP_CTL::LINK_TRAIN::Idle.value;
const DP_CTL_LINK_TRAIN_NORMAL: u32 = CPU_DP_CTL::LINK_TRAIN::Normal.value;
const DP_CTL_VSYNC_ACTIVE_HIGH: u32 = CPU_DP_CTL::VSYNC_ACTIVE_HIGH::SET.value;
const DP_CTL_HSYNC_ACTIVE_HIGH: u32 = CPU_DP_CTL::HSYNC_ACTIVE_HIGH::SET.value;
const DP_CTL_PIPE_SELECT_B: u32 = CPU_DP_CTL::PIPE_SELECT::PipeB.value;

/// Return a libgfxinit-style PCH VGA enable operation.
pub(crate) const fn pch_vga_enable_op(fdi: FdiPort, mode: Mode) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: PCH_ADPA,
        mask_unset: PCH_ADPA_MASK,
        mask_set: PCH_ADPA_DAC_ENABLE | pch_transcoder_select(fdi) | pch_vga_sync_polarity(mode),
    }
}

/// Return a libgfxinit-style PCH VGA off operation.
pub(crate) const fn pch_vga_off_op(vga_has_sync_disable: bool) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: PCH_ADPA,
        mask_unset: PCH_ADPA_DAC_ENABLE,
        mask_set: if vga_has_sync_disable {
            PCH_ADPA_HSYNC_DISABLE | PCH_ADPA_VSYNC_DISABLE
        } else {
            0
        },
    }
}

/// Return a libgfxinit-style PCH LVDS enable operation.
pub(crate) const fn pch_lvds_enable_op(fdi: FdiPort, mode: Mode) -> PortRegisterOp {
    PortRegisterOp::Write {
        register: PCH_LVDS,
        value: PCH_LVDS_ENABLE
            | pch_transcoder_select(fdi)
            | pch_lvds_sync_polarity(mode)
            | PCH_LVDS_CLK_A_DATA_A0A2_POWER_UP
            | pch_lvds_dual_channel_bits(mode),
    }
}

/// Return a libgfxinit-style PCH LVDS off operation.
pub(crate) const fn pch_lvds_off_op() -> PortRegisterOp {
    PortRegisterOp::Write {
        register: PCH_LVDS,
        value: 0,
    }
}

/// Return a libgfxinit-style PCH HDMI enable operation.
pub(crate) const fn pch_hdmi_enable_op(
    hdmi: PchHdmiPort,
    fdi: FdiPort,
    mode: Mode,
) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: pch_hdmi_register(hdmi),
        mask_unset: PCH_HDMI_MASK,
        mask_set: PCH_HDMI_ENABLE
            | pch_transcoder_select(fdi)
            | PCH_HDMI_SDVO_ENCODING_HDMI
            | pch_hdmi_sync_polarity(mode),
    }
}

/// Return the first libgfxinit-style PCH HDMI off operation.
pub(crate) const fn pch_hdmi_off_op(hdmi: PchHdmiPort) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: pch_hdmi_register(hdmi),
        mask_unset: PCH_HDMI_MASK,
        mask_set: PCH_HDMI_DISABLE_VALUE,
    }
}

/// Link settings needed for Ironlake CPU eDP DP_CTL_A programming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IronlakeEdpLinkConfig {
    /// DisplayPort lane count: 1, 2, or 4.
    pub lane_count: u8,
    /// Link bandwidth code in kHz: 1_620_000 or 2_700_000.
    pub link_rate_khz: u32,
    /// Whether enhanced framing is enabled.
    pub enhanced_framing: bool,
}

impl IronlakeEdpLinkConfig {
    /// Conservative 1-lane RBR config used by planning tests.
    pub const RBR_1_LANE: Self = Self {
        lane_count: 1,
        link_rate_khz: 1_620_000,
        enhanced_framing: true,
    };
}

/// Return libgfxinit-style eDP `Pre_On` DP_CTL_A writes.
pub(crate) const fn edp_pre_on_ops(
    pipe: Pipe,
    mode: Mode,
    link: IronlakeEdpLinkConfig,
) -> Result<[PortRegisterOp; 2], GmaError> {
    match edp_dp_ctl_base(pipe, mode, link) {
        Ok(base) => Ok([
            PortRegisterOp::Write {
                register: DP_CTL_A,
                value: base,
            },
            PortRegisterOp::Write {
                register: DP_CTL_A,
                value: DP_CTL_PLL_ENABLE | base,
            },
        ]),
        Err(err) => Err(err),
    }
}

const fn edp_dp_ctl_base(
    pipe: Pipe,
    mode: Mode,
    link: IronlakeEdpLinkConfig,
) -> Result<u32, GmaError> {
    match (
        edp_pipe_select(pipe),
        edp_port_width(link.lane_count),
        edp_link_rate(link.link_rate_khz),
    ) {
        (Ok(pipe_bits), Ok(width_bits), Ok(rate_bits)) => Ok(pipe_bits
            | width_bits
            | rate_bits
            | if_bool(link.enhanced_framing, DP_CTL_ENHANCED_FRAMING_ENABLE)
            | edp_sync_polarity(mode)),
        (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => Err(err),
    }
}

const fn edp_pipe_select(pipe: Pipe) -> Result<u32, GmaError> {
    match pipe {
        Pipe::A => Ok(0),
        Pipe::B => Ok(DP_CTL_PIPE_SELECT_B),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

const fn edp_port_width(lane_count: u8) -> Result<u32, GmaError> {
    match lane_count {
        1 => Ok(CPU_DP_CTL::PORT_WIDTH.val(0).value),
        2 => Ok(CPU_DP_CTL::PORT_WIDTH.val(1).value),
        4 => Ok(CPU_DP_CTL::PORT_WIDTH.val(3).value),
        _ => Err(GmaError::InvalidConfig),
    }
}

const fn edp_link_rate(link_rate_khz: u32) -> Result<u32, GmaError> {
    match link_rate_khz {
        1_620_000 => Ok(DP_CTL_PLL_FREQUENCY_162),
        2_700_000 => Ok(DP_CTL_PLL_FREQUENCY_270),
        _ => Err(GmaError::InvalidConfig),
    }
}

const fn edp_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(ModeFlags::PHSYNC) {
        DP_CTL_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(ModeFlags::PVSYNC) {
        DP_CTL_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

const fn pch_hdmi_register(hdmi: PchHdmiPort) -> usize {
    match hdmi {
        PchHdmiPort::B => PCH_HDMIB,
        PchHdmiPort::C => PCH_HDMIC,
        PchHdmiPort::D => PCH_HDMID,
    }
}

const fn pch_transcoder_select(fdi: FdiPort) -> u32 {
    match fdi {
        FdiPort::A => 0,
        FdiPort::B => PCH_TRANSCODER_SELECT_MASK,
        // Ivy Bridge has transcoder C in separate registers; this helper models
        // the one-bit PCH port selector used by Ironlake/SNB PCH ports.
        FdiPort::C => PCH_TRANSCODER_SELECT_MASK,
    }
}

const fn pch_vga_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(ModeFlags::PHSYNC) {
        PCH_ADPA_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(ModeFlags::PVSYNC) {
        PCH_ADPA_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

const fn pch_hdmi_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(ModeFlags::PHSYNC) {
        PCH_HDMI_HSYNC_ACTIVE_HIGH
    } else {
        0
    };
    let v = if mode.flags.contains(ModeFlags::PVSYNC) {
        PCH_HDMI_VSYNC_ACTIVE_HIGH
    } else {
        0
    };
    h | v
}

const fn pch_lvds_sync_polarity(mode: Mode) -> u32 {
    let h = if mode.flags.contains(ModeFlags::PHSYNC) {
        0
    } else {
        PCH_LVDS_HSYNC_POLARITY_INVERT
    };
    let v = if mode.flags.contains(ModeFlags::PVSYNC) {
        0
    } else {
        PCH_LVDS_VSYNC_POLARITY_INVERT
    };
    h | v
}

const fn pch_lvds_dual_channel_bits(mode: Mode) -> u32 {
    if mode.pixel_clock_khz >= LVDS_DUAL_CHANNEL_THRESHOLD_KHZ {
        PCH_LVDS_CLK_B_POWER_UP | PCH_LVDS_DATA_B0B2_POWER_UP
    } else {
        0
    }
}

/// Clock tuple selected for a PCH DPLL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PchClock {
    /// DPLL N divider.
    pub n: u32,
    /// DPLL M1 divider.
    pub m1: u32,
    /// DPLL M2 divider.
    pub m2: u32,
    /// DPLL P1 divider.
    pub p1: u32,
    /// DPLL P2 divider.
    pub p2: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PchClockLimits {
    n_lower: u32,
    n_upper: u32,
    m_lower: u32,
    m_upper: u32,
    m1_lower: u32,
    m1_upper: u32,
    m2_lower: u32,
    m2_upper: u32,
    p_lower: u32,
    p_upper: u32,
    p1_lower: u32,
    p1_upper: u32,
    p2_fast: u32,
    p2_slow: u32,
    p2_threshold_khz: u32,
    vco_lower_khz: u64,
    vco_upper_khz: u64,
}

const PCH_REF_CLOCK_KHZ: u64 = 120_000;
const PCH_MAX_DOTCLOCK_KHZ: u32 = 340_000;

const PCH_LVDS_SINGLE_LIMITS: PchClockLimits = PchClockLimits {
    n_lower: 3,
    n_upper: 5,
    m_lower: 79,
    m_upper: 118,
    m1_lower: 14,
    m1_upper: 22,
    m2_lower: 7,
    m2_upper: 11,
    p_lower: 28,
    p_upper: 112,
    p1_lower: 2,
    p1_upper: 8,
    p2_fast: 14,
    p2_slow: 14,
    p2_threshold_khz: 0,
    vco_lower_khz: 1_760_000,
    vco_upper_khz: 3_510_000,
};

const PCH_LVDS_DUAL_LIMITS: PchClockLimits = PchClockLimits {
    n_lower: 3,
    n_upper: 5,
    m_lower: 79,
    m_upper: 127,
    m1_lower: 14,
    m1_upper: 24,
    m2_lower: 7,
    m2_upper: 11,
    p_lower: 14,
    p_upper: 56,
    p1_lower: 2,
    p1_upper: 8,
    p2_fast: 7,
    p2_slow: 7,
    p2_threshold_khz: 0,
    vco_lower_khz: 1_760_000,
    vco_upper_khz: 3_510_000,
};

const PCH_OTHER_LIMITS: PchClockLimits = PchClockLimits {
    n_lower: 3,
    n_upper: 7,
    m_lower: 79,
    m_upper: 127,
    m1_lower: 14,
    m1_upper: 24,
    m2_lower: 7,
    m2_upper: 11,
    p_lower: 5,
    p_upper: 80,
    p1_lower: 1,
    p1_upper: 8,
    p2_fast: 5,
    p2_slow: 10,
    p2_threshold_khz: 225_000,
    vco_lower_khz: 1_760_000,
    vco_upper_khz: 3_510_000,
};

/// PCH DPLL and FP register offsets for one PLL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PchPllRegs {
    /// DPLL control register.
    pub dpll: usize,
    /// FP0 register.
    pub fp0: usize,
    /// FP1 register.
    pub fp1: usize,
}

/// Find a PCH DPLL clock tuple with libgfxinit's Ironlake search limits.
pub(crate) fn find_pch_clock(port: Port, mode: Mode) -> Result<PchClock, GmaError> {
    if mode.pixel_clock_khz > PCH_MAX_DOTCLOCK_KHZ {
        return Err(GmaError::ModeUnavailable);
    }
    let limits = pch_clock_limits(port, mode)?;
    let p2 = if mode.pixel_clock_khz <= limits.p2_threshold_khz {
        limits.p2_slow
    } else {
        limits.p2_fast
    };
    let mut best_clock = None;
    let mut best_delta = u64::MAX;
    let mut n = limits.n_lower;
    while n <= limits.n_upper {
        let mut m1 = limits.m1_upper;
        loop {
            let mut m2 = limits.m2_upper;
            loop {
                let mut p1 = limits.p1_upper;
                loop {
                    if let Some(dotclock) = verify_pch_clock(limits, n, m1, m2, p1, p2) {
                        let delta = dotclock.abs_diff(mode.pixel_clock_khz as u64);
                        if delta < best_delta {
                            best_delta = delta;
                            best_clock = Some(PchClock { n, m1, m2, p1, p2 });
                        }
                    }
                    if p1 == limits.p1_lower {
                        break;
                    }
                    p1 -= 1;
                }
                if m2 == limits.m2_lower {
                    break;
                }
                m2 -= 1;
            }
            if m1 == limits.m1_lower {
                break;
            }
            m1 -= 1;
        }
        n += 1;
    }
    best_clock.ok_or(GmaError::ModeUnavailable)
}

fn pch_clock_limits(port: Port, mode: Mode) -> Result<PchClockLimits, GmaError> {
    match port {
        Port::Lvds if mode.pixel_clock_khz >= LVDS_DUAL_CHANNEL_THRESHOLD_KHZ => {
            Ok(PCH_LVDS_DUAL_LIMITS)
        }
        Port::Lvds => Ok(PCH_LVDS_SINGLE_LIMITS),
        Port::Vga | Port::HdmiA | Port::HdmiB | Port::HdmiC | Port::DpA | Port::DpB | Port::DpC => {
            Ok(PCH_OTHER_LIMITS)
        }
        _ => Err(GmaError::UnsupportedPort),
    }
}

fn verify_pch_clock(
    limits: PchClockLimits,
    n: u32,
    m1: u32,
    m2: u32,
    p1: u32,
    p2: u32,
) -> Option<u64> {
    let m = 5 * m1 + m2;
    let p = p1 * p2;
    let vco = PCH_REF_CLOCK_KHZ * m as u64 / n as u64;
    let dotclock = vco / p as u64;
    if (limits.p2_fast == p2 || limits.p2_slow == p2)
        && (limits.p_lower..=limits.p_upper).contains(&p)
        && (limits.m_lower..=limits.m_upper).contains(&m)
        && (limits.vco_lower_khz..=limits.vco_upper_khz).contains(&vco)
    {
        Some(dotclock)
    } else {
        None
    }
}

/// Return PCH DPLL/FP registers, matching libgfxinit's `DPLL`, `FP0`, and `FP1` arrays.
pub(crate) const fn pch_pll_regs(pll: PchPll) -> PchPllRegs {
    match pll {
        PchPll::A => PchPllRegs {
            dpll: PCH_DPLL_A,
            fp0: PCH_FPA0,
            fp1: PCH_FPA1,
        },
        PchPll::B => PchPllRegs {
            dpll: PCH_DPLL_B,
            fp0: PCH_FPB0,
            fp1: PCH_FPB1,
        },
    }
}

/// Encode a PCH FP register value from a clock tuple.
pub(crate) const fn encode_pch_fp(clock: PchClock) -> u32 {
    PCH_FP::N.val(clock.n - 2).value
        | PCH_FP::M1.val(clock.m1 - 2).value
        | PCH_FP::M2.val(clock.m2 - 2).value
}

/// Encode libgfxinit PCH DPLL mode/divider bits without the VCO-enable bit.
pub(crate) const fn encode_pch_dpll(mode: PchDpllMode, clock: PchClock) -> u32 {
    let mode_bits = match mode {
        PchDpllMode::Lvds => PCH_DPLL_MODE_LVDS | PCH_DPLL_SSC,
        PchDpllMode::Dp => PCH_DPLL_MODE_DAC | PCH_DPLL_SSC | PCH_DPLL_HIGH_SPEED,
        PchDpllMode::DacHdmi => PCH_DPLL_MODE_DAC | PCH_DPLL_DREFCLK | PCH_DPLL_HIGH_SPEED,
    };
    let p2 = if clock.p2 == 5 || clock.p2 == 7 {
        PCH_DPLL_P2_5_OR_7
    } else {
        0
    };
    mode_bits | p2 | PCH_DPLL::P1_DIVIDER.val(1u32 << (clock.p1 - 1)).value
}

/// Data-only PCH DPLL programming plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PchPllPlan {
    /// Register set used by the PLL.
    pub regs: PchPllRegs,
    /// FP0 value.
    pub fp0_value: u32,
    /// FP1 value.
    pub fp1_value: u32,
    /// Initial DPLL write before enabling the VCO.
    pub dpll_value: u32,
    /// DPLL VCO enable bit set after the initial write.
    pub enable_mask: u32,
}

/// Build a libgfxinit-style PCH DPLL programming plan.
pub(crate) const fn pch_pll_plan(pll: PchPll, mode: PchDpllMode, clock: PchClock) -> PchPllPlan {
    let fp = encode_pch_fp(clock);
    PchPllPlan {
        regs: pch_pll_regs(pll),
        fp0_value: fp,
        fp1_value: fp,
        dpll_value: encode_pch_dpll(mode, clock),
        enable_mask: PCH_DPLL_VCO_ENABLE,
    }
}

/// Return the PCH DPLL_SEL update for a transcoder/PLL pair.
pub(crate) const fn pch_dpll_sel_op(fdi: FdiPort, pll: PchPll) -> PortRegisterOp {
    let pll_bit = match pll {
        PchPll::A => 0,
        PchPll::B => 1,
    };
    match fdi {
        FdiPort::A => PortRegisterOp::Update {
            register: PCH_DPLL_SEL,
            mask_unset: PCH_DPLL_SEL_MASK_A,
            mask_set: PCH_DPLL_SEL_ENABLE_A | pll_bit,
        },
        FdiPort::B => PortRegisterOp::Update {
            register: PCH_DPLL_SEL,
            mask_unset: PCH_DPLL_SEL_MASK_B,
            mask_set: PCH_DPLL_SEL_ENABLE_B | (pll_bit << 4),
        },
        FdiPort::C => PortRegisterOp::Update {
            register: PCH_DPLL_SEL,
            mask_unset: 1 << 8,
            mask_set: (1 << 11) | (pll_bit << 8),
        },
    }
}

/// PCH transcoder register block offsets for one FDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PchTranscoderRegs {
    /// Base of the contiguous PCH transcoder timing register block.
    pub timing: usize,
    /// Transcoder config register.
    pub conf: usize,
}

impl PchTranscoderRegs {
    const HTOTAL_OFFSET: usize = 0x00;
    const HBLANK_OFFSET: usize = 0x04;
    const HSYNC_OFFSET: usize = 0x08;
    const VTOTAL_OFFSET: usize = 0x0c;
    const VBLANK_OFFSET: usize = 0x10;
    const VSYNC_OFFSET: usize = 0x14;

    const fn htotal(self) -> usize {
        self.timing + Self::HTOTAL_OFFSET
    }

    const fn hblank(self) -> usize {
        self.timing + Self::HBLANK_OFFSET
    }

    const fn hsync(self) -> usize {
        self.timing + Self::HSYNC_OFFSET
    }

    const fn vtotal(self) -> usize {
        self.timing + Self::VTOTAL_OFFSET
    }

    const fn vblank(self) -> usize {
        self.timing + Self::VBLANK_OFFSET
    }

    const fn vsync(self) -> usize {
        self.timing + Self::VSYNC_OFFSET
    }
}

/// Return split-PCH transcoder timing registers for a port.
pub(crate) const fn pch_transcoder_regs(fdi: FdiPort) -> PchTranscoderRegs {
    match fdi {
        FdiPort::A => PchTranscoderRegs {
            timing: TRANS_TIMING_A,
            conf: TRANSACONF,
        },
        FdiPort::B | FdiPort::C => PchTranscoderRegs {
            timing: TRANS_TIMING_B,
            conf: TRANSBCONF,
        },
    }
}

/// Data-only timing values for a PCH transcoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PchTranscoderTimingPlan {
    /// Register set used by the transcoder.
    pub regs: PchTranscoderRegs,
    /// Encoded HTOTAL value.
    pub htotal: u32,
    /// Encoded HBLANK value.
    pub hblank: u32,
    /// Encoded HSYNC value.
    pub hsync: u32,
    /// Encoded VTOTAL value.
    pub vtotal: u32,
    /// Encoded VBLANK value.
    pub vblank: u32,
    /// Encoded VSYNC value.
    pub vsync: u32,
    /// Transcoder enable value.
    pub conf_enable: u32,
}

/// Build the libgfxinit PCH transcoder timing plan for a mode.
pub(crate) const fn pch_transcoder_timing_plan(
    fdi: FdiPort,
    mode: Mode,
) -> PchTranscoderTimingPlan {
    PchTranscoderTimingPlan {
        regs: pch_transcoder_regs(fdi),
        htotal: encode_transcoder_range(mode.hdisplay, mode.htotal),
        hblank: encode_transcoder_range(mode.hdisplay, mode.htotal),
        hsync: encode_transcoder_range(mode.hsync_start, mode.hsync_end),
        vtotal: encode_transcoder_range(mode.vdisplay, mode.vtotal),
        vblank: encode_transcoder_range(mode.vdisplay, mode.vtotal),
        vsync: encode_transcoder_range(mode.vsync_start, mode.vsync_end),
        conf_enable: TRANS_CONF_ENABLE,
    }
}

const fn encode_transcoder_range(start: u16, end: u16) -> u32 {
    ((start as u32) - 1) | (((end as u32) - 1) << 16)
}

/// FDI training pattern encoded in TX/RX control registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdiTrainingPattern {
    /// Training pattern 1.
    Tp1,
    /// Training pattern 2.
    Tp2,
    /// Idle pattern.
    Idle,
    /// Normal/none pattern.
    None,
}

/// Feature and link settings needed to build FDI training plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiLinkConfig {
    /// FDI lane count: 1, 2, or 4.
    pub lane_count: u8,
    /// Bits per color for FDI RX BPC encoding.
    pub bpc: u8,
    /// Enable enhanced framing.
    pub enhanced_framing: bool,
    /// Enable composite sync select when the platform has the bit.
    pub composite_sync: bool,
    /// Use the newer TP bit position on the CPU-side TX register.
    pub new_source: bool,
    /// Use the newer TP bit position on the PCH-side RX register.
    pub new_sink: bool,
    /// Include RX lane power-down setup/restore bits.
    pub rx_power_down: bool,
    /// Include RX BPC bits.
    pub has_bpc: bool,
}

impl FdiLinkConfig {
    /// Common 4-lane 8bpc enhanced-framing default for tests/planning.
    pub const DEFAULT: Self = Self {
        lane_count: 4,
        bpc: 8,
        enhanced_framing: true,
        composite_sync: false,
        new_source: false,
        new_sink: false,
        rx_power_down: false,
        has_bpc: false,
    };
}

/// PCH FDI RX register block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiRxRegs {
    /// FDI RX control register.
    pub ctl: usize,
    /// FDI RX misc register.
    pub misc: usize,
    /// FDI RX transfer-unit size register.
    pub tusize: usize,
    /// FDI RX interrupt-mask register.
    pub imr: usize,
    /// FDI RX interrupt-identity register.
    pub iir: usize,
}

/// Data-only pre-train setup for both PCH RX and CPU TX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiPreTrainPlan {
    /// PCH RX setup operations, in libgfxinit order.
    pub rx_ops: [PortRegisterOp; 6],
    /// CPU TX initial write after RX setup.
    pub tx_op: PortRegisterOp,
}

/// Data-only operations for one simple/full FDI train pattern transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiTrainStep {
    /// CPU TX pattern operation.
    pub tx_op: PortRegisterOp,
    /// PCH RX pattern operation.
    pub rx_op: PortRegisterOp,
    /// RX IIR lock bit polled by libgfxinit for this step, if any.
    pub lock_bit: Option<u32>,
}

/// Data-only Simple training sequence used by Ironlake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiSimpleTrainingPlan {
    /// TP1, TP2, and normal/none steps.
    pub steps: [FdiTrainStep; 3],
    /// TX cleanup operation used on failure.
    pub failure_tx_off: PortRegisterOp,
    /// TX PLL clear operation used on failure.
    pub failure_tx_clock_off: PortRegisterOp,
}

/// Data-only Full training attempt used by Sandy Bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiFullTrainingAttemptPlan {
    /// TP1 and TP2 attempt steps for one voltage/pre-emphasis setting.
    pub steps: [FdiTrainStep; 2],
    /// Final normal/none step used after a successful attempt.
    pub final_step: FdiTrainStep,
    /// TX cleanup operation used before retry.
    pub retry_tx_off: PortRegisterOp,
}

/// Data-only Auto training attempt used by Ivy Bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdiAutoTrainingAttemptPlan {
    /// CPU TX auto-training operation for one voltage/pre-emphasis setting.
    pub tx_auto_op: PortRegisterOp,
    /// PCH RX auto-training operation.
    pub rx_auto_op: PortRegisterOp,
    /// TX status bit polled by libgfxinit for auto-train completion.
    pub tx_done_bit: u32,
    /// RX error-correction enable operation after successful auto training.
    pub rx_enable_ec_op: PortRegisterOp,
    /// TX cleanup operation used before retry.
    pub retry_tx_off: PortRegisterOp,
}

pub(crate) const fn fdi_tx_ctl_register(fdi: FdiPort) -> usize {
    match fdi {
        FdiPort::A => FDI_TX_CTL_A,
        FdiPort::B => FDI_TX_CTL_B,
        FdiPort::C => FDI_TX_CTL_C,
    }
}

pub(crate) const fn fdi_rx_regs(fdi: FdiPort) -> FdiRxRegs {
    match fdi {
        FdiPort::A => FdiRxRegs {
            ctl: FDI_RXA_CTL,
            misc: FDI_RX_MISC_A,
            tusize: FDI_RXA_TUSIZE1,
            imr: FDI_RXA_IMR,
            iir: FDI_RXA_IIR,
        },
        FdiPort::B => FdiRxRegs {
            ctl: FDI_RXB_CTL,
            misc: FDI_RX_MISC_B,
            tusize: FDI_RXB_TUSIZE1,
            imr: FDI_RXB_IMR,
            iir: FDI_RXB_IIR,
        },
        FdiPort::C => FdiRxRegs {
            ctl: FDI_RXC_CTL,
            misc: FDI_RX_MISC_C,
            tusize: FDI_RXC_TUSIZE1,
            imr: FDI_RXC_IMR,
            iir: FDI_RXC_IIR,
        },
    }
}

pub(crate) const fn fdi_tx_training_pattern_mask(new_source: bool) -> u32 {
    if new_source {
        FDI_TX_CTL::TRAINING_PATTERN_NEW.val(3).value
    } else {
        FDI_TX_CTL::TRAINING_PATTERN_OLD.val(3).value
    }
}

pub(crate) const fn fdi_rx_training_pattern_mask(new_sink: bool) -> u32 {
    if new_sink {
        FDI_RX_CTL::TRAINING_PATTERN_NEW.val(3).value
    } else {
        FDI_RX_CTL::TRAINING_PATTERN_OLD.val(3).value
    }
}

pub(crate) const fn encode_fdi_tx_training_pattern(
    pattern: FdiTrainingPattern,
    new_source: bool,
) -> u32 {
    if new_source {
        encode_fdi_tx_training_pattern_new(pattern)
    } else {
        encode_fdi_tx_training_pattern_old(pattern)
    }
}

pub(crate) const fn encode_fdi_rx_training_pattern(
    pattern: FdiTrainingPattern,
    new_sink: bool,
) -> u32 {
    if new_sink {
        encode_fdi_rx_training_pattern_new(pattern)
    } else {
        encode_fdi_rx_training_pattern_old(pattern)
    }
}

const fn encode_fdi_tx_training_pattern_new(pattern: FdiTrainingPattern) -> u32 {
    match pattern {
        FdiTrainingPattern::Tp1 => FDI_TX_CTL::TRAINING_PATTERN_NEW::Tp1.value,
        FdiTrainingPattern::Tp2 => FDI_TX_CTL::TRAINING_PATTERN_NEW::Tp2.value,
        FdiTrainingPattern::Idle => FDI_TX_CTL::TRAINING_PATTERN_NEW::Idle.value,
        FdiTrainingPattern::None => FDI_TX_CTL::TRAINING_PATTERN_NEW::None.value,
    }
}

const fn encode_fdi_tx_training_pattern_old(pattern: FdiTrainingPattern) -> u32 {
    match pattern {
        FdiTrainingPattern::Tp1 => FDI_TX_CTL::TRAINING_PATTERN_OLD::Tp1.value,
        FdiTrainingPattern::Tp2 => FDI_TX_CTL::TRAINING_PATTERN_OLD::Tp2.value,
        FdiTrainingPattern::Idle => FDI_TX_CTL::TRAINING_PATTERN_OLD::Idle.value,
        FdiTrainingPattern::None => FDI_TX_CTL::TRAINING_PATTERN_OLD::None.value,
    }
}

const fn encode_fdi_rx_training_pattern_new(pattern: FdiTrainingPattern) -> u32 {
    match pattern {
        FdiTrainingPattern::Tp1 => FDI_RX_CTL::TRAINING_PATTERN_NEW::Tp1.value,
        FdiTrainingPattern::Tp2 => FDI_RX_CTL::TRAINING_PATTERN_NEW::Tp2.value,
        FdiTrainingPattern::Idle => FDI_RX_CTL::TRAINING_PATTERN_NEW::Idle.value,
        FdiTrainingPattern::None => FDI_RX_CTL::TRAINING_PATTERN_NEW::None.value,
    }
}

const fn encode_fdi_rx_training_pattern_old(pattern: FdiTrainingPattern) -> u32 {
    match pattern {
        FdiTrainingPattern::Tp1 => FDI_RX_CTL::TRAINING_PATTERN_OLD::Tp1.value,
        FdiTrainingPattern::Tp2 => FDI_RX_CTL::TRAINING_PATTERN_OLD::Tp2.value,
        FdiTrainingPattern::Idle => FDI_RX_CTL::TRAINING_PATTERN_OLD::Idle.value,
        FdiTrainingPattern::None => FDI_RX_CTL::TRAINING_PATTERN_OLD::None.value,
    }
}

pub(crate) const fn fdi_tx_port_width(lane_count: u8) -> u32 {
    FDI_TX_CTL::PORT_WIDTH_SEL
        .val((lane_count as u32) - 1)
        .value
}

pub(crate) const fn fdi_rx_port_width(lane_count: u8) -> u32 {
    FDI_RX_CTL::PORT_WIDTH_SEL
        .val((lane_count as u32) - 1)
        .value
}

const fn fdi_rx_bpc_bits(bpc: u8) -> u32 {
    match bpc {
        6 => FDI_RX_CTL::BPC.val(2).value,
        10 => FDI_RX_CTL::BPC.val(1).value,
        12 => FDI_RX_CTL::BPC.val(3).value,
        _ => 0,
    }
}

const fn fdi_tx_vswing_preemph(vp: u8) -> u32 {
    match vp {
        0 => FDI_TX_CTL::VP.val(0x00).value,
        1 => FDI_TX_CTL::VP.val(0x3a).value,
        2 => FDI_TX_CTL::VP.val(0x39).value,
        _ => FDI_TX_CTL::VP.val(0x38).value,
    }
}

const fn fdi_rx_power_down_lanes(value: u32) -> u32 {
    FDI_RX_MISC::FDI_RX_PWRDN_LANE1.val(value).value
        | FDI_RX_MISC::FDI_RX_PWRDN_LANE0.val(value).value
}

const fn fdi_rx_tusize(value: u32) -> u32 {
    FDI_RX_TUSIZE1::TU_SIZE.val(value - 1).value
}

const fn fdi_lock_bit(pattern: FdiTrainingPattern) -> Option<u32> {
    match pattern {
        FdiTrainingPattern::Tp1 => Some(FDI_RX_BIT_LOCK),
        FdiTrainingPattern::Tp2 => Some(FDI_RX_SYMBOL_LOCK | FDI_RX_INTERLANE_ALIGNMENT),
        FdiTrainingPattern::Idle | FdiTrainingPattern::None => None,
    }
}

/// Build libgfxinit-style FDI pre-training setup operations.
pub(crate) const fn fdi_pre_train_plan(fdi: FdiPort, config: FdiLinkConfig) -> FdiPreTrainPlan {
    let rx = fdi_rx_regs(fdi);
    let locks = FDI_RX_INTERLANE_ALIGNMENT | FDI_RX_SYMBOL_LOCK | FDI_RX_BIT_LOCK;
    let rx_ctl_settings = fdi_rx_port_width(config.lane_count)
        | if_bool(config.has_bpc, fdi_rx_bpc_bits(config.bpc))
        | if_bool(config.composite_sync, FDI_RX_CTL_COMPOSITE_SYNC_SELECT)
        | if_bool(config.enhanced_framing, FDI_RX_CTL_ENHANCED_FRAMING_ENABLE);
    FdiPreTrainPlan {
        rx_ops: [
            PortRegisterOp::Write {
                register: rx.misc,
                value: if_bool(config.rx_power_down, fdi_rx_power_down_lanes(2))
                    | FDI_RX_MISC_TP1_TO_TP2_TIME_48
                    | FDI_RX_MISC_FDI_DELAY_90,
            },
            PortRegisterOp::Write {
                register: rx.tusize,
                value: fdi_rx_tusize(64),
            },
            PortRegisterOp::Update {
                register: rx.imr,
                mask_unset: locks,
                mask_set: 0,
            },
            PortRegisterOp::Write {
                register: rx.iir,
                value: locks,
            },
            PortRegisterOp::Write {
                register: rx.ctl,
                value: FDI_RX_CTL_FDI_PLL_ENABLE | rx_ctl_settings,
            },
            PortRegisterOp::Update {
                register: rx.ctl,
                mask_unset: 0,
                mask_set: FDI_RX_CTL_RAWCLK_TO_PCDCLK_SEL_PCDCLK,
            },
        ],
        tx_op: PortRegisterOp::Write {
            register: fdi_tx_ctl_register(fdi),
            value: fdi_tx_port_width(config.lane_count)
                | FDI_TX_CTL_ENHANCED_FRAMING_ENABLE
                | FDI_TX_CTL_FDI_PLL_ENABLE
                | if_bool(config.composite_sync, FDI_TX_CTL_COMPOSITE_SYNC_SELECT)
                | encode_fdi_tx_training_pattern(FdiTrainingPattern::Tp1, config.new_source),
        },
    }
}

/// Build one libgfxinit-style FDI RX training operation.
pub(crate) const fn fdi_rx_train_op(
    fdi: FdiPort,
    pattern: FdiTrainingPattern,
    config: FdiLinkConfig,
) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: fdi_rx_regs(fdi).ctl,
        mask_unset: fdi_rx_training_pattern_mask(config.new_sink),
        mask_set: FDI_RX_CTL_FDI_RX_ENABLE
            | encode_fdi_rx_training_pattern(pattern, config.new_sink),
    }
}

/// Build one libgfxinit-style FDI TX pattern operation.
pub(crate) const fn fdi_tx_train_op(
    fdi: FdiPort,
    pattern: FdiTrainingPattern,
    config: FdiLinkConfig,
    enable: bool,
    auto: bool,
    vp: u8,
) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: fdi_tx_ctl_register(fdi),
        mask_unset: FDI_TX_CTL_VP_MASK | fdi_tx_training_pattern_mask(config.new_source),
        mask_set: if_bool(enable, FDI_TX_CTL_FDI_TX_ENABLE)
            | fdi_tx_vswing_preemph(vp)
            | if_bool(auto, FDI_TX_CTL_AUTO_TRAIN_ENABLE)
            | encode_fdi_tx_training_pattern(pattern, config.new_source),
    }
}

/// Build the Ironlake Simple FDI training sequence.
pub(crate) const fn fdi_simple_training_plan(
    fdi: FdiPort,
    config: FdiLinkConfig,
) -> FdiSimpleTrainingPlan {
    FdiSimpleTrainingPlan {
        steps: [
            FdiTrainStep {
                tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::Tp1, config, true, false, 0),
                rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::Tp1, config),
                lock_bit: fdi_lock_bit(FdiTrainingPattern::Tp1),
            },
            FdiTrainStep {
                tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::Tp2, config, false, false, 0),
                rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::Tp2, config),
                lock_bit: fdi_lock_bit(FdiTrainingPattern::Tp2),
            },
            FdiTrainStep {
                tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::None, config, false, false, 0),
                rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::None, config),
                lock_bit: fdi_lock_bit(FdiTrainingPattern::None),
            },
        ],
        failure_tx_off: fdi_tx_off_op(fdi, config, false),
        failure_tx_clock_off: fdi_tx_clock_off_op(fdi),
    }
}

/// Build one Sandy Bridge Full FDI training attempt.
pub(crate) const fn fdi_full_training_attempt_plan(
    fdi: FdiPort,
    config: FdiLinkConfig,
    vp: u8,
) -> FdiFullTrainingAttemptPlan {
    FdiFullTrainingAttemptPlan {
        steps: [
            FdiTrainStep {
                tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::Tp1, config, true, false, vp),
                rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::Tp1, config),
                lock_bit: fdi_lock_bit(FdiTrainingPattern::Tp1),
            },
            FdiTrainStep {
                tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::Tp2, config, false, false, vp),
                rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::Tp2, config),
                lock_bit: fdi_lock_bit(FdiTrainingPattern::Tp2),
            },
        ],
        final_step: FdiTrainStep {
            tx_op: fdi_tx_train_op(fdi, FdiTrainingPattern::None, config, false, false, vp),
            rx_op: fdi_rx_train_op(fdi, FdiTrainingPattern::None, config),
            lock_bit: fdi_lock_bit(FdiTrainingPattern::None),
        },
        retry_tx_off: fdi_tx_off_op(fdi, config, false),
    }
}

/// Build one Ivy Bridge Auto FDI training attempt.
pub(crate) const fn fdi_auto_training_attempt_plan(
    fdi: FdiPort,
    config: FdiLinkConfig,
    vp: u8,
) -> FdiAutoTrainingAttemptPlan {
    FdiAutoTrainingAttemptPlan {
        tx_auto_op: fdi_tx_train_op(fdi, FdiTrainingPattern::Tp1, config, true, true, vp),
        rx_auto_op: PortRegisterOp::Update {
            register: fdi_rx_regs(fdi).ctl,
            mask_unset: 0,
            mask_set: FDI_RX_CTL_FDI_RX_ENABLE | FDI_RX_CTL_FDI_AUTO_TRAIN,
        },
        tx_done_bit: FDI_TX_CTL_AUTO_TRAIN_DONE,
        rx_enable_ec_op: PortRegisterOp::Update {
            register: fdi_rx_regs(fdi).ctl,
            mask_unset: 0,
            mask_set: FDI_RX_CTL_FS_ERROR_CORRECTION_ENABLE | FDI_RX_CTL_FE_ERROR_CORRECTION_ENABLE,
        },
        retry_tx_off: fdi_tx_off_op(fdi, config, true),
    }
}

/// Build the libgfxinit TX off operation; `auto` also clears the auto-train bit.
pub(crate) const fn fdi_tx_off_op(
    fdi: FdiPort,
    config: FdiLinkConfig,
    auto: bool,
) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: fdi_tx_ctl_register(fdi),
        mask_unset: FDI_TX_CTL_FDI_TX_ENABLE
            | if_bool(auto, FDI_TX_CTL_AUTO_TRAIN_ENABLE)
            | fdi_tx_training_pattern_mask(config.new_source),
        mask_set: encode_fdi_tx_training_pattern(FdiTrainingPattern::Tp1, config.new_source),
    }
}

/// Build the libgfxinit TX PLL-off operation.
pub(crate) const fn fdi_tx_clock_off_op(fdi: FdiPort) -> PortRegisterOp {
    PortRegisterOp::Update {
        register: fdi_tx_ctl_register(fdi),
        mask_unset: FDI_TX_CTL_FDI_PLL_ENABLE,
        mask_set: 0,
    }
}

const fn if_bool(condition: bool, value: u32) -> u32 {
    if condition { value } else { 0 }
}

/// High-level ordered split-PCH operation used by the pure Ironlake init planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IronlakeInitOp {
    /// Reuse the common GTT framebuffer mapping stage.
    MapGtt,
    /// Fill the selected framebuffer surface before display scanout.
    FillFramebuffer,
    /// Disable existing split-PCH output ports before reprogramming the path.
    DisablePchPorts,
    /// Write PCH DPLL FP0.
    PchPllFp0 { register: usize, value: u32 },
    /// Write PCH DPLL FP1.
    PchPllFp1 { register: usize, value: u32 },
    /// Write initial PCH DPLL control value.
    PchPllControl { register: usize, value: u32 },
    /// Enable the selected PCH DPLL VCO.
    PchPllEnable { register: usize, mask: u32 },
    /// Select the PCH DPLL for the target transcoder.
    PchDpllSelect(PortRegisterOp),
    /// Program CPU pipe timings.
    ProgramCpuPipe,
    /// Program split-PCH transcoder timings.
    ProgramPchTranscoder(PchTranscoderTimingPlan),
    /// Program the primary plane.
    ProgramPrimaryPlane,
    /// PCH FDI RX setup operation.
    FdiPreTrainRx(PortRegisterOp),
    /// CPU FDI TX setup operation.
    FdiPreTrainTx(PortRegisterOp),
    /// Run the generation-specific FDI training sequence.
    FdiTrain {
        /// CPU/PCH FDI path to train.
        fdi: FdiPort,
        /// Training algorithm selected for the CPU family.
        mode: FdiTrainingMode,
        /// Link configuration used by the pre-training register setup.
        config: FdiLinkConfig,
    },
    /// Enable the final PCH output port.
    EnablePchPort(PortRegisterOp),
}

/// Pure ordered plan for one split-PCH Ironlake-family modeset path.
pub(crate) type IronlakeInitPlan = Vec<IronlakeInitOp, 32>;

/// Resolved split-PCH pipeline inputs for LVDS/VGA/HDMI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IronlakePipelinePlan {
    /// Logical board port.
    pub port: Port,
    /// CPU/PCH FDI path.
    pub fdi: FdiPort,
    /// Selected PCH DPLL.
    pub pll: PchPll,
    /// PCH DPLL mode.
    pub dpll_mode: PchDpllMode,
    /// FDI link configuration.
    pub fdi_link: FdiLinkConfig,
    /// Final PCH output operation.
    pub pch_port_enable: PortRegisterOp,
}

impl IronlakePipelinePlan {
    /// Return the CPU pipe paired with the selected FDI/transcoder path.
    pub const fn fdi_pipe(self) -> Pipe {
        match self.fdi {
            FdiPort::A => Pipe::A,
            FdiPort::B => Pipe::B,
            FdiPort::C => Pipe::C,
        }
    }
}

/// Resolve the staged single-output split-PCH pipeline for LVDS/VGA/HDMI.
pub(crate) fn ironlake_pipeline_plan(
    cpu: Cpu,
    port: Port,
    mode: Mode,
) -> Result<IronlakePipelinePlan, GmaError> {
    let training = fdi_training_mode(cpu)?;
    let fdi = match port {
        Port::Lvds => FdiPort::B,
        Port::Vga | Port::HdmiA | Port::HdmiB | Port::HdmiC => FdiPort::A,
        _ => return Err(GmaError::UnsupportedPort),
    };
    let pll = match port {
        Port::Lvds => PchPll::B,
        Port::Vga | Port::HdmiA | Port::HdmiB | Port::HdmiC => PchPll::A,
        _ => return Err(GmaError::UnsupportedPort),
    };
    let dpll_mode = match port {
        Port::Lvds => PchDpllMode::Lvds,
        Port::Vga => PchDpllMode::DacHdmi,
        Port::HdmiA | Port::HdmiB | Port::HdmiC => PchDpllMode::DacHdmi,
        _ => return Err(GmaError::UnsupportedPort),
    };
    let mut fdi_link = FdiLinkConfig::DEFAULT;
    fdi_link.new_source = matches!(training, FdiTrainingMode::Auto);
    fdi_link.new_sink = matches!(training, FdiTrainingMode::Auto);
    fdi_link.rx_power_down = matches!(cpu, Cpu::Ivybridge);
    fdi_link.has_bpc = matches!(cpu, Cpu::Sandybridge | Cpu::Ivybridge);
    let pch_port_enable = match port {
        Port::Lvds => pch_lvds_enable_op(fdi, mode),
        Port::Vga => pch_vga_enable_op(fdi, mode),
        Port::HdmiA | Port::HdmiB | Port::HdmiC => {
            pch_hdmi_enable_op(pch_hdmi_port(port)?, fdi, mode)
        }
        _ => return Err(GmaError::UnsupportedPort),
    };
    Ok(IronlakePipelinePlan {
        port,
        fdi,
        pll,
        dpll_mode,
        fdi_link,
        pch_port_enable,
    })
}

/// Build a pure libgfxinit-ordered Ironlake-family init sequence for LVDS/VGA/HDMI.
pub(crate) fn ironlake_init_sequence_for_mode(
    cpu: Cpu,
    port: Port,
    mode: Mode,
) -> Result<IronlakeInitPlan, GmaError> {
    ironlake_init_sequence_plan(cpu, port, mode, find_pch_clock(port, mode)?)
}

fn ironlake_init_sequence_plan(
    cpu: Cpu,
    port: Port,
    mode: Mode,
    clock: PchClock,
) -> Result<IronlakeInitPlan, GmaError> {
    let pipeline = ironlake_pipeline_plan(cpu, port, mode)?;
    let pll = pch_pll_plan(pipeline.pll, pipeline.dpll_mode, clock);
    let fdi = fdi_pre_train_plan(pipeline.fdi, pipeline.fdi_link);
    let mut ops = IronlakeInitPlan::new();
    push_init_op(&mut ops, IronlakeInitOp::MapGtt)?;
    push_init_op(&mut ops, IronlakeInitOp::FillFramebuffer)?;
    push_init_op(&mut ops, IronlakeInitOp::DisablePchPorts)?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::PchPllFp0 {
            register: pll.regs.fp0,
            value: pll.fp0_value,
        },
    )?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::PchPllFp1 {
            register: pll.regs.fp1,
            value: pll.fp1_value,
        },
    )?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::PchPllControl {
            register: pll.regs.dpll,
            value: pll.dpll_value,
        },
    )?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::PchPllEnable {
            register: pll.regs.dpll,
            mask: pll.enable_mask,
        },
    )?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::PchDpllSelect(pch_dpll_sel_op(pipeline.fdi, pipeline.pll)),
    )?;
    push_init_op(&mut ops, IronlakeInitOp::ProgramCpuPipe)?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::ProgramPchTranscoder(pch_transcoder_timing_plan(pipeline.fdi, mode)),
    )?;
    push_init_op(&mut ops, IronlakeInitOp::ProgramPrimaryPlane)?;
    for op in fdi.rx_ops {
        push_init_op(&mut ops, IronlakeInitOp::FdiPreTrainRx(op))?;
    }
    push_init_op(&mut ops, IronlakeInitOp::FdiPreTrainTx(fdi.tx_op))?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::FdiTrain {
            fdi: pipeline.fdi,
            mode: fdi_training_mode(cpu)?,
            config: pipeline.fdi_link,
        },
    )?;
    push_init_op(
        &mut ops,
        IronlakeInitOp::EnablePchPort(pipeline.pch_port_enable),
    )?;
    Ok(ops)
}

fn push_init_op(plan: &mut IronlakeInitPlan, op: IronlakeInitOp) -> Result<(), GmaError> {
    plan.push(op).map_err(|_| GmaError::InvalidConfig)
}

/// Minimal register sink used by the Ironlake live-executor skeleton.
pub(crate) trait IronlakeMmioSink {
    /// Read a 32-bit register.
    fn read32(&mut self, register: usize) -> u32;
    /// Write a 32-bit register.
    fn write32(&mut self, register: usize, value: u32);
    /// Read a register to flush posted writes.
    fn posting_read(&mut self, register: usize) {
        let _ = self.read32(register);
    }
    /// Clear and set selected bits.
    fn update32(&mut self, register: usize, mask_unset: u32, mask_set: u32) {
        let value = (self.read32(register) & !mask_unset) | mask_set;
        self.write32(register, value);
    }
    /// Set selected bits.
    fn set_bits32(&mut self, register: usize, mask: u32) {
        let value = self.read32(register) | mask;
        self.write32(register, value);
    }
}

impl IronlakeMmioSink for Mmio {
    fn read32(&mut self, register: usize) -> u32 {
        Mmio::read32(self, register)
    }

    fn write32(&mut self, register: usize, value: u32) {
        Mmio::write32(self, register, value);
    }

    fn posting_read(&mut self, register: usize) {
        Mmio::posting_read(self, register);
    }

    fn update32(&mut self, register: usize, mask_unset: u32, mask_set: u32) {
        Mmio::update32(self, register, mask_unset, mask_set);
    }

    fn set_bits32(&mut self, register: usize, mask: u32) {
        Mmio::set_bits32(self, register, mask);
    }
}

/// Execute currently-live register writes from an Ironlake init plan.
///
/// This deliberately leaves common GMCH stages (GTT/framebuffer/CPU pipe/plane)
/// as no-ops owned by the eventual integrated modeset path, applies all planned
/// split-PCH register writes in order, and runs the staged FDI training loops
/// with the same simple/full/auto family split used by libgfxinit.
pub(crate) fn execute_ironlake_init_plan_registers<S: IronlakeMmioSink>(
    sink: &mut S,
    plan: &IronlakeInitPlan,
    mode: Mode,
    surface: crate::framebuffer::SurfaceConfig,
    pipe: Pipe,
    plane: Plane,
) -> Result<(), GmaError> {
    for op in plan {
        match *op {
            IronlakeInitOp::MapGtt | IronlakeInitOp::FillFramebuffer => {}
            IronlakeInitOp::ProgramCpuPipe => program_cpu_pipe_for_executor(sink, pipe, mode)?,
            IronlakeInitOp::ProgramPrimaryPlane => {
                program_primary_plane_for_executor(sink, plane, pipe, surface)?
            }
            IronlakeInitOp::DisablePchPorts => disable_pch_ports_for_executor(sink),
            IronlakeInitOp::PchPllFp0 { register, value }
            | IronlakeInitOp::PchPllFp1 { register, value }
            | IronlakeInitOp::PchPllControl { register, value } => sink.write32(register, value),
            IronlakeInitOp::PchPllEnable { register, mask } => {
                sink.set_bits32(register, mask);
                sink.posting_read(register);
            }
            IronlakeInitOp::PchDpllSelect(port_op)
            | IronlakeInitOp::FdiPreTrainRx(port_op)
            | IronlakeInitOp::FdiPreTrainTx(port_op)
            | IronlakeInitOp::EnablePchPort(port_op) => apply_executor_port_op(sink, port_op),
            IronlakeInitOp::ProgramPchTranscoder(timing) => {
                program_pch_transcoder_timing_for_executor(sink, timing);
            }
            IronlakeInitOp::FdiTrain { fdi, mode, config } => {
                execute_fdi_training(sink, fdi, mode, config)?;
            }
        }
    }
    Ok(())
}

fn program_pch_transcoder_timing_for_executor<S: IronlakeMmioSink>(
    sink: &mut S,
    timing: PchTranscoderTimingPlan,
) {
    let regs = timing.regs;
    sink.write32(regs.htotal(), timing.htotal);
    sink.write32(regs.hblank(), timing.hblank);
    sink.write32(regs.hsync(), timing.hsync);
    sink.write32(regs.vtotal(), timing.vtotal);
    sink.write32(regs.vblank(), timing.vblank);
    sink.write32(regs.vsync(), timing.vsync);
    sink.write32(regs.conf, timing.conf_enable);
    sink.posting_read(regs.conf);
}

fn disable_pch_ports_for_executor<S: IronlakeMmioSink>(sink: &mut S) {
    apply_executor_port_op(sink, pch_vga_off_op(true));
    apply_executor_port_op(sink, pch_lvds_off_op());
    apply_executor_port_op(sink, pch_hdmi_off_op(PchHdmiPort::B));
    apply_executor_port_op(sink, pch_hdmi_off_op(PchHdmiPort::C));
    apply_executor_port_op(sink, pch_hdmi_off_op(PchHdmiPort::D));
}

fn execute_fdi_training<S: IronlakeMmioSink>(
    sink: &mut S,
    fdi: FdiPort,
    mode: FdiTrainingMode,
    config: FdiLinkConfig,
) -> Result<(), GmaError> {
    match mode {
        FdiTrainingMode::Simple => execute_fdi_simple_training(sink, fdi, config),
        FdiTrainingMode::Full => execute_fdi_full_training(sink, fdi, config),
        FdiTrainingMode::Auto => execute_fdi_auto_training(sink, fdi, config),
    }
}

fn execute_fdi_simple_training<S: IronlakeMmioSink>(
    sink: &mut S,
    fdi: FdiPort,
    config: FdiLinkConfig,
) -> Result<(), GmaError> {
    let plan = fdi_simple_training_plan(fdi, config);
    for step in plan.steps {
        if !execute_fdi_train_step(sink, fdi, step) {
            apply_executor_port_op(sink, plan.failure_tx_off);
            apply_executor_port_op(sink, plan.failure_tx_clock_off);
            return Err(GmaError::HardwareError);
        }
    }
    Ok(())
}

fn execute_fdi_full_training<S: IronlakeMmioSink>(
    sink: &mut S,
    fdi: FdiPort,
    config: FdiLinkConfig,
) -> Result<(), GmaError> {
    let mut vp = 0;
    while vp < 4 {
        let plan = fdi_full_training_attempt_plan(fdi, config, vp);
        if execute_fdi_train_step(sink, fdi, plan.steps[0])
            && execute_fdi_train_step(sink, fdi, plan.steps[1])
        {
            apply_executor_port_op(sink, plan.final_step.rx_op);
            apply_executor_port_op(sink, plan.final_step.tx_op);
            return Ok(());
        }
        apply_executor_port_op(sink, plan.retry_tx_off);
        vp += 1;
    }
    Err(GmaError::HardwareError)
}

fn execute_fdi_auto_training<S: IronlakeMmioSink>(
    sink: &mut S,
    fdi: FdiPort,
    config: FdiLinkConfig,
) -> Result<(), GmaError> {
    let mut vp = 0;
    while vp < 4 {
        let plan = fdi_auto_training_attempt_plan(fdi, config, vp);
        apply_executor_port_op(sink, plan.rx_auto_op);
        apply_executor_port_op(sink, plan.tx_auto_op);
        if poll_register_set(sink, fdi_tx_ctl_register(fdi), plan.tx_done_bit) {
            apply_executor_port_op(sink, plan.rx_enable_ec_op);
            return Ok(());
        }
        apply_executor_port_op(sink, plan.retry_tx_off);
        vp += 1;
    }
    Err(GmaError::HardwareError)
}

fn execute_fdi_train_step<S: IronlakeMmioSink>(
    sink: &mut S,
    fdi: FdiPort,
    step: FdiTrainStep,
) -> bool {
    apply_executor_port_op(sink, step.rx_op);
    apply_executor_port_op(sink, step.tx_op);
    match step.lock_bit {
        Some(bit) => poll_register_set(sink, fdi_rx_regs(fdi).iir, bit),
        None => true,
    }
}

fn poll_register_set<S: IronlakeMmioSink>(sink: &mut S, register: usize, mask: u32) -> bool {
    let mut tries = 0;
    while tries < 10 {
        if sink.read32(register) & mask == mask {
            return true;
        }
        tries += 1;
    }
    false
}

fn program_cpu_pipe_for_executor<S: IronlakeMmioSink>(
    sink: &mut S,
    pipe: Pipe,
    mode: Mode,
) -> Result<(), GmaError> {
    let regs = CpuPipeRegs::for_pipe(pipe)?;
    let pipe_config = crate::pipe::PipeConfig::new(pipe, mode);
    sink.write32(regs.htotal, pipe_config.htotal());
    sink.write32(regs.hblank, pipe_config.hblank());
    sink.write32(regs.hsync, pipe_config.hsync());
    sink.write32(regs.vtotal, pipe_config.vtotal());
    sink.write32(regs.vblank, pipe_config.vblank());
    sink.write32(regs.vsync, pipe_config.vsync());
    sink.write32(regs.pipesrc, pipe_config.pipesrc());
    sink.write32(regs.pipeconf, CPU_PIPECONF_ENABLE | CPU_PIPECONF_6BPC);
    sink.posting_read(regs.pipeconf);
    Ok(())
}

fn program_primary_plane_for_executor<S: IronlakeMmioSink>(
    sink: &mut S,
    plane: Plane,
    pipe: Pipe,
    surface: crate::framebuffer::SurfaceConfig,
) -> Result<(), GmaError> {
    let regs = CpuPlaneRegs::for_plane(plane)?;
    let plane_config = PlaneConfig::new(plane, pipe, PlaneAddressModel::Surface, surface);
    sink.write32(regs.stride, plane_config.stride_bytes()?);
    sink.write32(regs.tileoff, 0);
    sink.write32(regs.linoff, 0);
    sink.write32(regs.surf, surface.offset);
    sink.write32(
        regs.cntr,
        CPU_DSPCNTR_ENABLE | CPU_DSPCNTR_FORMAT_XRGB8888 | cpu_dspcntr_pipe_select(pipe),
    );
    sink.write32(regs.surf, surface.offset);
    sink.posting_read(regs.surf);
    Ok(())
}

struct CpuPipeRegs {
    htotal: usize,
    hblank: usize,
    hsync: usize,
    vtotal: usize,
    vblank: usize,
    vsync: usize,
    pipesrc: usize,
    pipeconf: usize,
}

impl CpuPipeRegs {
    const fn for_pipe(pipe: Pipe) -> Result<Self, GmaError> {
        match pipe {
            Pipe::A => Ok(Self {
                htotal: 0x60000,
                hblank: 0x60004,
                hsync: 0x60008,
                vtotal: 0x6000c,
                vblank: 0x60010,
                vsync: 0x60014,
                pipesrc: 0x6001c,
                pipeconf: 0x70008,
            }),
            Pipe::B => Ok(Self {
                htotal: 0x61000,
                hblank: 0x61004,
                hsync: 0x61008,
                vtotal: 0x6100c,
                vblank: 0x61010,
                vsync: 0x61014,
                pipesrc: 0x6101c,
                pipeconf: 0x71008,
            }),
            Pipe::C => Ok(Self {
                htotal: 0x62000,
                hblank: 0x62004,
                hsync: 0x62008,
                vtotal: 0x6200c,
                vblank: 0x62010,
                vsync: 0x62014,
                pipesrc: 0x6201c,
                pipeconf: 0x72008,
            }),
        }
    }
}

struct CpuPlaneRegs {
    cntr: usize,
    linoff: usize,
    stride: usize,
    surf: usize,
    tileoff: usize,
}

impl CpuPlaneRegs {
    const fn for_plane(plane: Plane) -> Result<Self, GmaError> {
        match plane {
            Plane::PrimaryA => Ok(Self {
                cntr: 0x70180,
                linoff: 0x70184,
                stride: 0x70188,
                surf: 0x7019c,
                tileoff: 0x701a4,
            }),
            Plane::PrimaryB => Ok(Self {
                cntr: 0x71180,
                linoff: 0x71184,
                stride: 0x71188,
                surf: 0x7119c,
                tileoff: 0x711a4,
            }),
            Plane::PrimaryC => Ok(Self {
                cntr: 0x72180,
                linoff: 0x72184,
                stride: 0x72188,
                surf: 0x7219c,
                tileoff: 0x721a4,
            }),
        }
    }
}

const CPU_PIPECONF_ENABLE: u32 = CPU_PIPECONF::ENABLE::SET.value;
const CPU_PIPECONF_6BPC: u32 = CPU_PIPECONF::BPC::Bits6.value;
const CPU_DSPCNTR_ENABLE: u32 = CPU_DSPCNTR::ENABLE::SET.value;
const CPU_DSPCNTR_FORMAT_XRGB8888: u32 = CPU_DSPCNTR::FORMAT::Xrgb8888.value;

const fn cpu_dspcntr_pipe_select(pipe: Pipe) -> u32 {
    match pipe {
        Pipe::A => CPU_DSPCNTR::PIPE_SELECT::PipeA.value,
        Pipe::B => CPU_DSPCNTR::PIPE_SELECT::PipeB.value,
        Pipe::C => CPU_DSPCNTR::PIPE_SELECT::PipeC.value,
    }
}

fn apply_executor_port_op<S: IronlakeMmioSink>(sink: &mut S, op: PortRegisterOp) {
    match op {
        PortRegisterOp::Write { register, value } => sink.write32(register, value),
        PortRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => sink.update32(register, mask_unset, mask_set),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdi_training_mode_matches_libgfxinit_cpu_families() {
        assert_eq!(
            fdi_training_mode(Cpu::Ironlake),
            Ok(FdiTrainingMode::Simple)
        );
        assert_eq!(
            fdi_training_mode(Cpu::Sandybridge),
            Ok(FdiTrainingMode::Full)
        );
        assert_eq!(fdi_training_mode(Cpu::Ivybridge), Ok(FdiTrainingMode::Auto));
        assert_eq!(
            fdi_training_mode(Cpu::G45),
            Err(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn pch_vga_plan_matches_libgfxinit_masks_and_polarity() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = ModeFlags::PHSYNC | ModeFlags::PVSYNC;
        assert_eq!(
            pch_vga_enable_op(FdiPort::B, mode),
            PortRegisterOp::Update {
                register: PCH_ADPA,
                mask_unset: PCH_ADPA_MASK,
                mask_set: PCH_ADPA_DAC_ENABLE
                    | PCH_TRANSCODER_SELECT_MASK
                    | PCH_ADPA_HSYNC_ACTIVE_HIGH
                    | PCH_ADPA_VSYNC_ACTIVE_HIGH,
            }
        );
        assert_eq!(
            pch_vga_off_op(true),
            PortRegisterOp::Update {
                register: PCH_ADPA,
                mask_unset: PCH_ADPA_DAC_ENABLE,
                mask_set: PCH_ADPA_HSYNC_DISABLE | PCH_ADPA_VSYNC_DISABLE,
            }
        );
    }

    #[test]
    fn pch_lvds_plan_matches_libgfxinit_threshold_and_inverted_polarity() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = ModeFlags::empty();
        assert_eq!(
            pch_lvds_enable_op(FdiPort::B, mode),
            PortRegisterOp::Write {
                register: PCH_LVDS,
                value: PCH_LVDS_ENABLE
                    | PCH_TRANSCODER_SELECT_MASK
                    | PCH_LVDS_HSYNC_POLARITY_INVERT
                    | PCH_LVDS_VSYNC_POLARITY_INVERT
                    | PCH_LVDS_CLK_A_DATA_A0A2_POWER_UP,
            }
        );
        mode.pixel_clock_khz = LVDS_DUAL_CHANNEL_THRESHOLD_KHZ;
        let PortRegisterOp::Write { value, .. } = pch_lvds_enable_op(FdiPort::A, mode) else {
            panic!("expected write")
        };
        assert_ne!(value & PCH_LVDS_CLK_B_POWER_UP, 0);
        assert_ne!(value & PCH_LVDS_DATA_B0B2_POWER_UP, 0);
        assert_eq!(
            pch_lvds_off_op(),
            PortRegisterOp::Write {
                register: PCH_LVDS,
                value: 0
            }
        );
    }

    #[test]
    fn pch_hdmi_plan_matches_libgfxinit_registers_and_double_enable_base() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = ModeFlags::PHSYNC | ModeFlags::PVSYNC;
        assert_eq!(pch_hdmi_port(Port::HdmiA), Ok(PchHdmiPort::B));
        assert_eq!(pch_hdmi_port(Port::HdmiB), Ok(PchHdmiPort::C));
        assert_eq!(pch_hdmi_port(Port::HdmiC), Ok(PchHdmiPort::D));
        assert_eq!(pch_hdmi_port(Port::Vga), Err(GmaError::UnsupportedPort));
        assert_eq!(
            pch_hdmi_enable_op(PchHdmiPort::D, FdiPort::B, mode),
            PortRegisterOp::Update {
                register: PCH_HDMID,
                mask_unset: PCH_HDMI_MASK,
                mask_set: PCH_HDMI_ENABLE
                    | PCH_TRANSCODER_SELECT_MASK
                    | PCH_HDMI_SDVO_ENCODING_HDMI
                    | PCH_HDMI_HSYNC_ACTIVE_HIGH
                    | PCH_HDMI_VSYNC_ACTIVE_HIGH,
            }
        );
        assert_eq!(
            pch_hdmi_off_op(PchHdmiPort::B),
            PortRegisterOp::Update {
                register: PCH_HDMIB,
                mask_unset: PCH_HDMI_MASK,
                mask_set: PCH_HDMI_DISABLE_VALUE,
            }
        );
    }

    #[test]
    fn edp_pre_on_plan_matches_libgfxinit_dp_ctl_a_sequence() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.flags = ModeFlags::PHSYNC | ModeFlags::PVSYNC;
        let ops = edp_pre_on_ops(
            Pipe::B,
            mode,
            IronlakeEdpLinkConfig {
                lane_count: 4,
                link_rate_khz: 1_620_000,
                enhanced_framing: true,
            },
        )
        .unwrap();
        let base = DP_CTL_PIPE_SELECT_B
            | (3 << 19)
            | DP_CTL_ENHANCED_FRAMING_ENABLE
            | DP_CTL_PLL_FREQUENCY_162
            | DP_CTL_HSYNC_ACTIVE_HIGH
            | DP_CTL_VSYNC_ACTIVE_HIGH;
        assert_eq!(
            ops,
            [
                PortRegisterOp::Write {
                    register: DP_CTL_A,
                    value: base,
                },
                PortRegisterOp::Write {
                    register: DP_CTL_A,
                    value: DP_CTL_PLL_ENABLE | base,
                },
            ]
        );
        assert_eq!(
            edp_pre_on_ops(Pipe::C, mode, IronlakeEdpLinkConfig::RBR_1_LANE),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            edp_pre_on_ops(
                Pipe::A,
                mode,
                IronlakeEdpLinkConfig {
                    lane_count: 3,
                    link_rate_khz: 1_620_000,
                    enhanced_framing: true,
                },
            ),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            edp_pre_on_ops(
                Pipe::A,
                mode,
                IronlakeEdpLinkConfig {
                    lane_count: 1,
                    link_rate_khz: 5_400_000,
                    enhanced_framing: true,
                },
            ),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn fdi_port_a_and_c_selector_model_is_explicit() {
        assert_eq!(pch_transcoder_select(FdiPort::A), 0);
        assert_eq!(
            pch_transcoder_select(FdiPort::B),
            PCH_TRANSCODER_SELECT_MASK
        );
        assert_eq!(
            pch_transcoder_select(FdiPort::C),
            PCH_TRANSCODER_SELECT_MASK
        );
    }

    #[test]
    fn pch_clock_search_matches_libgfxinit_limits() {
        let lvds = find_pch_clock(Port::Lvds, Mode::XGA_1024X768_60).unwrap();
        assert_eq!(
            lvds,
            PchClock {
                n: 4,
                m1: 16,
                m2: 11,
                p1: 3,
                p2: 14,
            }
        );
        let vga = find_pch_clock(Port::Vga, Mode::XGA_1024X768_60).unwrap();
        assert_eq!(
            vga,
            PchClock {
                n: 7,
                m1: 21,
                m2: 9,
                p1: 3,
                p2: 10,
            }
        );
        let mut dual_lvds = Mode::XGA_1024X768_60;
        dual_lvds.pixel_clock_khz = LVDS_DUAL_CHANNEL_THRESHOLD_KHZ;
        assert_eq!(find_pch_clock(Port::Lvds, dual_lvds).unwrap().p2, 7);
        let mut too_high = Mode::XGA_1024X768_60;
        too_high.pixel_clock_khz = PCH_MAX_DOTCLOCK_KHZ + 1;
        assert_eq!(
            find_pch_clock(Port::Vga, too_high),
            Err(GmaError::ModeUnavailable)
        );
        assert_eq!(
            find_pch_clock(Port::Edp, Mode::XGA_1024X768_60),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn pch_dpll_registers_and_encoding_match_libgfxinit() {
        assert_eq!(
            pch_pll_regs(PchPll::A),
            PchPllRegs {
                dpll: PCH_DPLL_A,
                fp0: PCH_FPA0,
                fp1: PCH_FPA1,
            }
        );
        assert_eq!(
            pch_pll_regs(PchPll::B),
            PchPllRegs {
                dpll: PCH_DPLL_B,
                fp0: PCH_FPB0,
                fp1: PCH_FPB1,
            }
        );

        let clock = PchClock {
            n: 3,
            m1: 14,
            m2: 8,
            p1: 2,
            p2: 10,
        };
        assert_eq!(
            encode_pch_fp(clock),
            PCH_FP::N.val(clock.n - 2).value
                | PCH_FP::M1.val(clock.m1 - 2).value
                | PCH_FP::M2.val(clock.m2 - 2).value
        );
        assert_eq!(
            encode_pch_dpll(PchDpllMode::Lvds, clock),
            PCH_DPLL_MODE_LVDS | PCH_DPLL_SSC | PCH_DPLL::P1_DIVIDER.val(2).value
        );
        assert_eq!(
            encode_pch_dpll(PchDpllMode::DacHdmi, clock),
            PCH_DPLL_MODE_DAC
                | PCH_DPLL_DREFCLK
                | PCH_DPLL_HIGH_SPEED
                | PCH_DPLL::P1_DIVIDER.val(2).value
        );
        let mut dp_clock = clock;
        dp_clock.p2 = 7;
        assert_eq!(
            encode_pch_dpll(PchDpllMode::Dp, dp_clock),
            PCH_DPLL_MODE_DAC
                | PCH_DPLL_SSC
                | PCH_DPLL_HIGH_SPEED
                | PCH_DPLL_P2_5_OR_7
                | PCH_DPLL::P1_DIVIDER.val(2).value
        );

        let plan = pch_pll_plan(PchPll::B, PchDpllMode::DacHdmi, clock);
        assert_eq!(plan.regs.dpll, PCH_DPLL_B);
        assert_eq!(plan.fp0_value, encode_pch_fp(clock));
        assert_eq!(plan.fp1_value, encode_pch_fp(clock));
        assert_eq!(plan.enable_mask, PCH_DPLL_VCO_ENABLE);
    }

    #[test]
    fn pch_dpll_sel_ops_match_transcoder_bit_layout() {
        assert_eq!(
            pch_dpll_sel_op(FdiPort::A, PchPll::B),
            PortRegisterOp::Update {
                register: PCH_DPLL_SEL,
                mask_unset: PCH_DPLL_SEL_MASK_A,
                mask_set: PCH_DPLL_SEL_ENABLE_A | 1,
            }
        );
        assert_eq!(
            pch_dpll_sel_op(FdiPort::B, PchPll::A),
            PortRegisterOp::Update {
                register: PCH_DPLL_SEL,
                mask_unset: PCH_DPLL_SEL_MASK_B,
                mask_set: PCH_DPLL_SEL_ENABLE_B,
            }
        );
        assert_eq!(
            pch_dpll_sel_op(FdiPort::C, PchPll::B),
            PortRegisterOp::Update {
                register: PCH_DPLL_SEL,
                mask_unset: 1 << 8,
                mask_set: (1 << 11) | (1 << 8),
            }
        );
    }

    #[test]
    fn pch_transcoder_timing_plan_matches_libgfxinit_ranges() {
        let mode = Mode::XGA_1024X768_60;
        let plan = pch_transcoder_timing_plan(FdiPort::B, mode);
        assert_eq!(plan.regs.htotal(), TRANS_TIMING_B);
        assert_eq!(plan.regs.hblank(), TRANS_TIMING_B + 0x04);
        assert_eq!(plan.regs.hsync(), TRANS_TIMING_B + 0x08);
        assert_eq!(plan.regs.vtotal(), TRANS_TIMING_B + 0x0c);
        assert_eq!(plan.regs.vblank(), TRANS_TIMING_B + 0x10);
        assert_eq!(plan.regs.vsync(), TRANS_TIMING_B + 0x14);
        assert_eq!(plan.regs.conf, TRANSBCONF);
        assert_eq!(
            plan.htotal,
            (mode.hdisplay as u32 - 1) | ((mode.htotal as u32 - 1) << 16)
        );
        assert_eq!(plan.hblank, plan.htotal);
        assert_eq!(
            plan.hsync,
            (mode.hsync_start as u32 - 1) | ((mode.hsync_end as u32 - 1) << 16)
        );
        assert_eq!(
            plan.vtotal,
            (mode.vdisplay as u32 - 1) | ((mode.vtotal as u32 - 1) << 16)
        );
        assert_eq!(plan.vblank, plan.vtotal);
        assert_eq!(
            plan.vsync,
            (mode.vsync_start as u32 - 1) | ((mode.vsync_end as u32 - 1) << 16)
        );
        assert_eq!(plan.conf_enable, TRANS_CONF_ENABLE);

        let a = pch_transcoder_timing_plan(FdiPort::A, mode);
        assert_eq!(a.regs.htotal(), TRANS_TIMING_A);
        assert_eq!(a.regs.conf, TRANSACONF);
    }

    #[test]
    fn fdi_register_offsets_and_pattern_bits_match_libgfxinit() {
        assert_eq!(fdi_tx_ctl_register(FdiPort::A), FDI_TX_CTL_A);
        assert_eq!(fdi_tx_ctl_register(FdiPort::B), FDI_TX_CTL_B);
        assert_eq!(fdi_tx_ctl_register(FdiPort::C), FDI_TX_CTL_C);
        assert_eq!(fdi_rx_regs(FdiPort::A).ctl, FDI_RXA_CTL);
        assert_eq!(fdi_rx_regs(FdiPort::B).misc, FDI_RX_MISC_B);
        assert_eq!(fdi_rx_regs(FdiPort::C).tusize, FDI_RXC_TUSIZE1);

        assert_eq!(fdi_tx_training_pattern_mask(false), 3 << 28);
        assert_eq!(fdi_tx_training_pattern_mask(true), 3 << 8);
        assert_eq!(
            encode_fdi_tx_training_pattern(FdiTrainingPattern::Tp2, false),
            1 << 28
        );
        assert_eq!(
            encode_fdi_rx_training_pattern(FdiTrainingPattern::None, true),
            3 << 8
        );
        assert_eq!(
            fdi_tx_port_width(4),
            FDI_TX_CTL::PORT_WIDTH_SEL.val(3).value
        );
        assert_eq!(
            fdi_rx_port_width(2),
            FDI_RX_CTL::PORT_WIDTH_SEL.val(1).value
        );
    }

    #[test]
    fn fdi_pre_train_plan_matches_libgfxinit_order() {
        let config = FdiLinkConfig {
            lane_count: 2,
            bpc: 10,
            enhanced_framing: true,
            composite_sync: true,
            new_source: false,
            new_sink: false,
            rx_power_down: true,
            has_bpc: true,
        };
        let plan = fdi_pre_train_plan(FdiPort::B, config);
        assert_eq!(
            plan.rx_ops[0],
            PortRegisterOp::Write {
                register: FDI_RX_MISC_B,
                value: fdi_rx_power_down_lanes(2)
                    | FDI_RX_MISC_TP1_TO_TP2_TIME_48
                    | FDI_RX_MISC_FDI_DELAY_90,
            }
        );
        assert_eq!(
            plan.rx_ops[1],
            PortRegisterOp::Write {
                register: FDI_RXB_TUSIZE1,
                value: FDI_RX_TUSIZE1::TU_SIZE.val(63).value,
            }
        );
        assert_eq!(
            plan.rx_ops[2],
            PortRegisterOp::Update {
                register: FDI_RXB_IMR,
                mask_unset: FDI_RX_INTERLANE_ALIGNMENT | FDI_RX_SYMBOL_LOCK | FDI_RX_BIT_LOCK,
                mask_set: 0,
            }
        );
        assert_eq!(
            plan.rx_ops[3],
            PortRegisterOp::Write {
                register: FDI_RXB_IIR,
                value: FDI_RX_INTERLANE_ALIGNMENT | FDI_RX_SYMBOL_LOCK | FDI_RX_BIT_LOCK,
            }
        );
        assert_eq!(
            plan.rx_ops[4],
            PortRegisterOp::Write {
                register: FDI_RXB_CTL,
                value: FDI_RX_CTL_FDI_PLL_ENABLE
                    | (FDI_RX_CTL::PORT_WIDTH_SEL.val(1).value)
                    | FDI_RX_CTL::BPC.val(1).value
                    | FDI_RX_CTL_COMPOSITE_SYNC_SELECT
                    | FDI_RX_CTL_ENHANCED_FRAMING_ENABLE,
            }
        );
        assert_eq!(
            plan.rx_ops[5],
            PortRegisterOp::Update {
                register: FDI_RXB_CTL,
                mask_unset: 0,
                mask_set: FDI_RX_CTL_RAWCLK_TO_PCDCLK_SEL_PCDCLK,
            }
        );
        assert_eq!(
            plan.tx_op,
            PortRegisterOp::Write {
                register: FDI_TX_CTL_B,
                value: (FDI_TX_CTL::PORT_WIDTH_SEL.val(1).value)
                    | FDI_TX_CTL_ENHANCED_FRAMING_ENABLE
                    | FDI_TX_CTL_FDI_PLL_ENABLE
                    | FDI_TX_CTL_COMPOSITE_SYNC_SELECT,
            }
        );
    }

    #[test]
    fn ironlake_init_sequence_for_mode_uses_pch_clock_search() {
        let mode = Mode::XGA_1024X768_60;
        let plan = ironlake_init_sequence_for_mode(Cpu::Ironlake, Port::Lvds, mode).unwrap();
        assert_eq!(
            plan[3],
            IronlakeInitOp::PchPllFp0 {
                register: PCH_FPB0,
                value: encode_pch_fp(find_pch_clock(Port::Lvds, mode).unwrap()),
            }
        );
        assert_eq!(
            ironlake_init_sequence_for_mode(Cpu::Ironlake, Port::Edp, mode),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn ironlake_init_sequence_orders_lvds_like_libgfxinit_split_pch_path() {
        let mode = Mode::XGA_1024X768_60;
        let clock = PchClock {
            n: 3,
            m1: 14,
            m2: 8,
            p1: 2,
            p2: 10,
        };
        let plan = ironlake_init_sequence_plan(Cpu::Ironlake, Port::Lvds, mode, clock).unwrap();
        assert_eq!(plan[0], IronlakeInitOp::MapGtt);
        assert_eq!(plan[1], IronlakeInitOp::FillFramebuffer);
        assert_eq!(plan[2], IronlakeInitOp::DisablePchPorts);
        assert!(matches!(
            plan[3],
            IronlakeInitOp::PchPllFp0 {
                register: PCH_FPB0,
                ..
            }
        ));
        assert!(matches!(
            plan[5],
            IronlakeInitOp::PchPllControl {
                register: PCH_DPLL_B,
                ..
            }
        ));
        assert_eq!(
            plan[7],
            IronlakeInitOp::PchDpllSelect(pch_dpll_sel_op(FdiPort::B, PchPll::B))
        );
        assert_eq!(plan[8], IronlakeInitOp::ProgramCpuPipe);
        assert!(matches!(
            plan[9],
            IronlakeInitOp::ProgramPchTranscoder(PchTranscoderTimingPlan {
                regs: PchTranscoderRegs {
                    timing: TRANS_TIMING_B,
                    ..
                },
                ..
            })
        ));
        assert_eq!(plan[10], IronlakeInitOp::ProgramPrimaryPlane);
        assert!(matches!(plan[11], IronlakeInitOp::FdiPreTrainRx(_)));
        assert!(matches!(plan[16], IronlakeInitOp::FdiPreTrainRx(_)));
        assert!(matches!(
            plan[17],
            IronlakeInitOp::FdiPreTrainTx(PortRegisterOp::Write {
                register: FDI_TX_CTL_B,
                ..
            })
        ));
        assert_eq!(
            plan[18],
            IronlakeInitOp::FdiTrain {
                fdi: FdiPort::B,
                mode: FdiTrainingMode::Simple,
                config: FdiLinkConfig::DEFAULT,
            }
        );
        assert_eq!(
            plan[19],
            IronlakeInitOp::EnablePchPort(pch_lvds_enable_op(FdiPort::B, mode))
        );
        assert_eq!(plan.len(), 20);
    }

    #[test]
    fn ironlake_init_sequence_resolves_vga_and_hdmi_ports() {
        let mode = Mode::XGA_1024X768_60;
        let clock = PchClock {
            n: 3,
            m1: 14,
            m2: 8,
            p1: 2,
            p2: 10,
        };
        let vga = ironlake_init_sequence_plan(Cpu::Sandybridge, Port::Vga, mode, clock).unwrap();
        assert_eq!(
            vga[7],
            IronlakeInitOp::PchDpllSelect(pch_dpll_sel_op(FdiPort::A, PchPll::A))
        );
        assert!(matches!(
            vga[18],
            IronlakeInitOp::FdiTrain {
                fdi: FdiPort::A,
                mode: FdiTrainingMode::Full,
                ..
            }
        ));
        assert_eq!(
            vga[19],
            IronlakeInitOp::EnablePchPort(pch_vga_enable_op(FdiPort::A, mode))
        );

        let hdmi = ironlake_init_sequence_plan(Cpu::Ivybridge, Port::HdmiB, mode, clock).unwrap();
        assert!(matches!(
            hdmi[18],
            IronlakeInitOp::FdiTrain {
                fdi: FdiPort::A,
                mode: FdiTrainingMode::Auto,
                ..
            }
        ));
        assert_eq!(
            hdmi[19],
            IronlakeInitOp::EnablePchPort(pch_hdmi_enable_op(PchHdmiPort::C, FdiPort::A, mode))
        );
        assert_eq!(
            ironlake_init_sequence_plan(Cpu::G45, Port::Vga, mode, clock),
            Err(GmaError::UnsupportedPlatform)
        );
        assert_eq!(
            ironlake_init_sequence_plan(Cpu::Ironlake, Port::DpA, mode, clock),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn ironlake_executor_applies_register_ops_and_fdi_training() {
        let mode = Mode::XGA_1024X768_60;
        let clock = PchClock {
            n: 3,
            m1: 14,
            m2: 8,
            p1: 2,
            p2: 10,
        };
        let plan = ironlake_init_sequence_plan(Cpu::Ironlake, Port::Lvds, mode, clock).unwrap();
        let mut sink = MockSink::default();
        assert_eq!(
            execute_ironlake_init_plan_registers(
                &mut sink,
                &plan,
                mode,
                crate::framebuffer::SurfaceConfig::packed(
                    crate::types::PhysAddr(0xd000_0000),
                    1024,
                    768,
                    crate::framebuffer::PixelFormat::Xrgb8888,
                ),
                Pipe::B,
                Plane::PrimaryB,
            ),
            Ok(())
        );
        assert!(sink.wrote(PCH_LVDS, 0));
        assert!(sink.wrote(PCH_FPB0, encode_pch_fp(clock)));
        assert!(sink.wrote(PCH_DPLL_B, encode_pch_dpll(PchDpllMode::Lvds, clock)));
        assert_eq!(
            sink.latest(PCH_DPLL_B) & PCH_DPLL_VCO_ENABLE,
            PCH_DPLL_VCO_ENABLE
        );
        assert!(sink.wrote(
            pch_transcoder_regs(FdiPort::B).htotal(),
            pch_transcoder_timing_plan(FdiPort::B, mode).htotal
        ));
        assert!(sink.wrote(
            FDI_RX_MISC_B,
            fdi_pre_train_plan(FdiPort::B, FdiLinkConfig::DEFAULT).rx_ops[0].value_for_test()
        ));
        assert!(sink.wrote(PCH_LVDS, pch_lvds_enable_value_for_test(FdiPort::B, mode)));
    }

    #[test]
    fn ironlake_executor_can_complete_plan_without_training_marker() {
        let mode = Mode::XGA_1024X768_60;
        let timing = pch_transcoder_timing_plan(FdiPort::A, mode);
        let mut plan = IronlakeInitPlan::new();
        plan.push(IronlakeInitOp::PchPllFp0 {
            register: PCH_FPA0,
            value: 0x1234,
        })
        .unwrap();
        plan.push(IronlakeInitOp::PchDpllSelect(pch_dpll_sel_op(
            FdiPort::A,
            PchPll::A,
        )))
        .unwrap();
        plan.push(IronlakeInitOp::ProgramPchTranscoder(timing))
            .unwrap();
        plan.push(IronlakeInitOp::EnablePchPort(pch_vga_enable_op(
            FdiPort::A,
            mode,
        )))
        .unwrap();
        let mut sink = MockSink::default();
        assert_eq!(
            execute_ironlake_init_plan_registers(
                &mut sink,
                &plan,
                mode,
                crate::framebuffer::SurfaceConfig::packed(
                    crate::types::PhysAddr(0xd000_0000),
                    1024,
                    768,
                    crate::framebuffer::PixelFormat::Xrgb8888,
                ),
                Pipe::A,
                Plane::PrimaryA,
            ),
            Ok(())
        );
        assert!(sink.wrote(PCH_FPA0, 0x1234));
        assert!(sink.wrote(timing.regs.conf, TRANS_CONF_ENABLE));
        assert_ne!(sink.latest(PCH_ADPA) & PCH_ADPA_DAC_ENABLE, 0);
    }

    #[derive(Default)]
    struct MockSink {
        writes: std::vec::Vec<(usize, u32)>,
    }

    impl MockSink {
        fn latest(&self, register: usize) -> u32 {
            self.writes
                .iter()
                .rev()
                .find(|(written_register, _)| *written_register == register)
                .map(|(_, value)| *value)
                .unwrap_or(0)
        }

        fn wrote(&self, register: usize, value: u32) -> bool {
            self.writes.iter().any(|(written_register, written_value)| {
                *written_register == register && *written_value == value
            })
        }
    }

    impl IronlakeMmioSink for MockSink {
        fn read32(&mut self, register: usize) -> u32 {
            self.latest(register)
        }

        fn write32(&mut self, register: usize, value: u32) {
            self.writes.push((register, value));
        }
    }

    trait PortRegisterOpTestExt {
        fn value_for_test(self) -> u32;
    }

    impl PortRegisterOpTestExt for PortRegisterOp {
        fn value_for_test(self) -> u32 {
            match self {
                PortRegisterOp::Write { value, .. } => value,
                PortRegisterOp::Update { mask_set, .. } => mask_set,
            }
        }
    }

    const fn pch_lvds_enable_value_for_test(fdi: FdiPort, mode: Mode) -> u32 {
        match pch_lvds_enable_op(fdi, mode) {
            PortRegisterOp::Write { value, .. } => value,
            PortRegisterOp::Update { .. } => 0,
        }
    }

    #[test]
    fn fdi_simple_full_and_auto_training_plans_match_libgfxinit() {
        let config = FdiLinkConfig::DEFAULT;
        let simple = fdi_simple_training_plan(FdiPort::A, config);
        assert_eq!(
            simple.steps[0].tx_op,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_A,
                mask_unset: FDI_TX_CTL_VP_MASK | (3 << 28),
                mask_set: FDI_TX_CTL_FDI_TX_ENABLE,
            }
        );
        assert_eq!(
            simple.steps[0].rx_op,
            PortRegisterOp::Update {
                register: FDI_RXA_CTL,
                mask_unset: 3 << 28,
                mask_set: FDI_RX_CTL_FDI_RX_ENABLE,
            }
        );
        assert_eq!(simple.steps[0].lock_bit, Some(FDI_RX_BIT_LOCK));
        assert_eq!(
            simple.steps[1].tx_op,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_A,
                mask_unset: FDI_TX_CTL_VP_MASK | (3 << 28),
                mask_set: 1 << 28,
            }
        );
        assert_eq!(
            simple.steps[2].tx_op,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_A,
                mask_unset: FDI_TX_CTL_VP_MASK | (3 << 28),
                mask_set: 3 << 28,
            }
        );
        assert_eq!(
            simple.failure_tx_clock_off,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_A,
                mask_unset: FDI_TX_CTL_FDI_PLL_ENABLE,
                mask_set: 0,
            }
        );

        let full = fdi_full_training_attempt_plan(FdiPort::B, config, 2);
        assert_eq!(
            full.steps[0].tx_op,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_B,
                mask_unset: FDI_TX_CTL_VP_MASK | (3 << 28),
                mask_set: FDI_TX_CTL_FDI_TX_ENABLE | (0x39 << 22),
            }
        );
        assert_eq!(full.final_step.lock_bit, None);
        assert_eq!(
            full.retry_tx_off,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_B,
                mask_unset: FDI_TX_CTL_FDI_TX_ENABLE | (3 << 28),
                mask_set: 0,
            }
        );

        let mut ivy = config;
        ivy.new_source = true;
        ivy.new_sink = true;
        let auto = fdi_auto_training_attempt_plan(FdiPort::C, ivy, 1);
        assert_eq!(
            auto.tx_auto_op,
            PortRegisterOp::Update {
                register: FDI_TX_CTL_C,
                mask_unset: FDI_TX_CTL_VP_MASK | (3 << 8),
                mask_set: FDI_TX_CTL_FDI_TX_ENABLE | (0x3a << 22) | FDI_TX_CTL_AUTO_TRAIN_ENABLE,
            }
        );
        assert_eq!(
            auto.rx_auto_op,
            PortRegisterOp::Update {
                register: FDI_RXC_CTL,
                mask_unset: 0,
                mask_set: FDI_RX_CTL_FDI_RX_ENABLE | FDI_RX_CTL_FDI_AUTO_TRAIN,
            }
        );
        assert_eq!(auto.tx_done_bit, FDI_TX_CTL_AUTO_TRAIN_DONE);
        assert_eq!(
            auto.rx_enable_ec_op,
            PortRegisterOp::Update {
                register: FDI_RXC_CTL,
                mask_unset: 0,
                mask_set: FDI_RX_CTL_FS_ERROR_CORRECTION_ENABLE
                    | FDI_RX_CTL_FE_ERROR_CORRECTION_ENABLE,
            }
        );
    }
}
