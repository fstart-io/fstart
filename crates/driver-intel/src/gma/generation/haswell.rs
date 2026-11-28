//! Intel Haswell / Haswell-ULT / Haswell-ULX GMA display initialization.
//!
//! Implements the native Haswell DDI modesetting executor (eDP, DisplayPort,
//! and HDMI/DVI) alongside the existing Haswell/Broadwell DDI planning helpers.
//! Hardware register writes flow through [`HaswellMmio`] and [`HaswellAux`] so
//! host unit tests can record and assert the exact register and AUX sequences.

#![allow(dead_code)]

use crate::gma::config::OutputConfig;
use crate::gma::ddi::{
    DDI_BUF_CTL_IDLE_STATUS, DdiClockRouting, DdiDpInitStep, DdiLaneCount, DdiPort, DdiPortRegs,
    HSW_PORT_CLK_SEL_BASE, HSW_WRPLL_BASE, HSW_WRPLL_CTL_REG, HswDdiPllPlan, HswDpInitParams,
    HswDpInitSequencePlan, HswHdmiInitParams, HswHdmiInitSequencePlan, HswPllSelect,
    HswPortClockSelectRegs, HswWrpllRegs, SKL_DPLL_CTRL2_REG, calculate_hsw_wrpll,
    ddi_buf_ctl_dp_rmw, ddi_buf_ctl_hdmi_rmw, hsw_ddi_buffer_translations,
    hsw_dp_init_sequence_plan, hsw_hdmi_init_sequence_plan,
};
use crate::gma::dp_aux::{
    DPCD_DOWNSPREAD_CTRL, DPCD_LINK_BW_SET, DPCD_LINK_STATUS, DPCD_RECEIVER_CAPS,
    DPCD_TRAINING_PATTERN_SET, DpLinkRate, LinkStatus, PreEmphasis, ReceiverCaps, TrainSet,
    TrainingPattern, VoltageSwing, haswell_native_aux_read, haswell_native_aux_write,
};
use crate::gma::dp_training::{DpLinkConfig, link_config_candidates, sink_link_config_writes};
use crate::gma::error::GmaError;
use crate::gma::framebuffer::SurfaceConfig;
use crate::gma::generation::{GenerationOps, sealed};
use crate::gma::gtt;
use crate::gma::mmio::Mmio;
use crate::gma::mode::{Mode, ModeFlags};
use crate::gma::panel::{
    BLC_PWM_PCH_CTL1, BLM_PCH_PWM_ENABLE, HaswellBacklightPwmPlan, PanelPowerDelays,
    PanelPowerPortSelect, PanelPowerRegs, PanelRegisterOp, panel_power_control_plan,
    panel_power_sequencer_plan,
};
use crate::gma::pipe::PipeConfig;
use crate::gma::plane::{PlaneAddressModel, PlaneConfig, primary_for_pipe};
use crate::gma::regs::{PP_CONTROL, PP_STATUS};
use crate::gma::scaler::{self, IronlakePfitRegs, ScalingPolicy};
use crate::gma::types::{Cpu, Generation, Pipe, Port};
use crate::gma::vbt::Vbt;
use tock_registers::interfaces::{Readable, Writeable};

// ---------------------------------------------------------------------------
// Haswell display register offsets and bit definitions
// ---------------------------------------------------------------------------

const CPU_VGACNTRL: usize = 0x41000;
const VGA_DISPLAY_DISABLE: u32 = 1 << 31;

const IPS_CTL: usize = 0x43408;
const IPS_CTL_ENABLE: u32 = 1 << 31;

const HOTPLUG_CTL: usize = 0x44030;
const SHOTPLUG_CTL: usize = 0xc4030;
const SFUSE_STRAP: usize = 0xc2014;
const SFUSE_STRAP_DDI_B: u32 = 1 << 2;
const SFUSE_STRAP_DDI_C: u32 = 1 << 1;
const SFUSE_STRAP_DDI_D: u32 = 1 << 0;
const SFUSE_STRAP_CRT_DAC_DISABLE: u32 = 1 << 6;

const WM_LINETIME_A: usize = 0x45270;
const WM_LINETIME_B: usize = 0x45274;
const WM_LINETIME_C: usize = 0x45278;

const PWR_WELL_CTL_BIOS: usize = 0x45400;
const PWR_WELL_CTL_DRIVER: usize = 0x45404;
const PWR_WELL_CTL_KVMR: usize = 0x45408;
const PWR_WELL_CTL_DEBUG: usize = 0x4540c;
const PWR_WELL_CTL_ENABLE_REQUEST: u32 = 1 << 31;
const PWR_WELL_CTL_STATE_ENABLED: u32 = 1 << 30;

const SPLL_CTL: usize = 0x46020;
const SPLL_CTL_ENABLE: u32 = 1 << 31;
const WRPLL_CTL_1: usize = 0x46040;
const WRPLL_CTL_2: usize = 0x46060;
const WRPLL_CTL_ENABLE: u32 = 1 << 31;

const PORT_CLK_SEL_LCPLL2700: u32 = 0 << 29;
const PORT_CLK_SEL_LCPLL1350: u32 = 1 << 29;
const PORT_CLK_SEL_LCPLL810: u32 = 2 << 29;
const PORT_CLK_SEL_WRPLL1: u32 = 4 << 29;
const PORT_CLK_SEL_WRPLL2: u32 = 5 << 29;
const PORT_CLK_SEL_NONE: u32 = 7 << 29;
const PORT_CLK_SEL_MASK: u32 = 7 << 29;

const TRANSA_CLK_SEL: usize = 0x46140;
const TRANSB_CLK_SEL: usize = 0x46144;
const TRANSC_CLK_SEL: usize = 0x46148;
const TRANS_CLK_SEL_NONE: u32 = 0;
const TRANS_CLK_SEL_DDIB: u32 = 2 << 29;
const TRANS_CLK_SEL_DDIC: u32 = 3 << 29;
const TRANS_CLK_SEL_DDID: u32 = 4 << 29;

const NDE_RSTWRN_OPT: usize = 0x46408;
const RST_PCH_HANDSHAKE_EN: u32 = 1 << 4;

const PIPESRC_A: usize = 0x6001c;
const PIPESRC_B: usize = 0x6101c;
const PIPESRC_C: usize = 0x6201c;

const SRD_CTL: usize = 0x64800;
const SRD_STATUS: usize = 0x64840;
const SRD_CTL_ENABLE: u32 = 1 << 31;
const SRD_STATUS_STATE_MASK: u32 = 7 << 29;

const DDI_BUF_CTL_A: usize = 0x64000;
const DDI_BUF_CTL_ENABLE: u32 = 1 << 31;
const DDI_BUF_CTL_A_4_LANES: u32 = 1 << 4;
const DDI_BUF_CTL_INIT_DISPLAY_DETECTED: u32 = 1 << 0;

const DP_TP_CTL_TRANSPORT_ENABLE: u32 = 1 << 31;
const DP_TP_CTL_ENHANCED_FRAME_ENABLE: u32 = 1 << 18;
const DP_TP_CTL_LINK_TRAIN_MASK: u32 = 7 << 8;
const DP_TP_CTL_LINK_TRAIN_PAT1: u32 = 0 << 8;
const DP_TP_CTL_LINK_TRAIN_PAT2: u32 = 1 << 8;
const DP_TP_CTL_LINK_TRAIN_IDLE: u32 = 2 << 8;
const DP_TP_CTL_LINK_TRAIN_NORMAL: u32 = 3 << 8;
const DP_TP_CTL_LINK_TRAIN_PAT3: u32 = 4 << 8;
const DP_TP_STATUS_MIN_IDLES_SENT: u32 = 1 << 25;

const DDI_FUNC_CTL_ENABLE: u32 = 1 << 31;
const DDI_FUNC_CTL_DDI_SELECT_B: u32 = 1 << 28;
const DDI_FUNC_CTL_DDI_SELECT_C: u32 = 2 << 28;
const DDI_FUNC_CTL_DDI_SELECT_D: u32 = 3 << 28;
const DDI_FUNC_CTL_MODE_DVI: u32 = 1 << 24;
const DDI_FUNC_CTL_MODE_DP_SST: u32 = 2 << 24;
const DDI_FUNC_CTL_BPC_8: u32 = 0 << 20;
const DDI_FUNC_CTL_VSYNC_HIGH: u32 = 1 << 17;
const DDI_FUNC_CTL_HSYNC_HIGH: u32 = 1 << 16;
const DDI_FUNC_CTL_EDP_SELECT_MASK: u32 = 7 << 12;
const DDI_FUNC_CTL_EDP_SELECT_ALWAYS_ON: u32 = 0 << 12;
const DDI_FUNC_CTL_EDP_SELECT_PIPE_A: u32 = 4 << 12;
const DDI_FUNC_CTL_EDP_SELECT_PIPE_B: u32 = 5 << 12;
const DDI_FUNC_CTL_EDP_SELECT_PIPE_C: u32 = 6 << 12;

const TRANS_DATA_M_TU_64: u32 = 63 << 25;
const TRANS_MSA_MISC_SYNC_CLK: u32 = 1 << 0;
const TRANS_MSA_MISC_8BPC: u32 = 1 << 5;
const TRANS_CONF_ENABLE: u32 = 1 << 31;
const TRANS_CONF_ENABLED_STATUS: u32 = 1 << 30;

const DSPCNTR_ENABLE: u32 = 1 << 31;
const DSPCNTR_FORMAT_BGRX8888: u32 = 6 << 26;

const PCH_RAWCLK_FREQ: usize = 0xc6204;

const LCPLL_CTL: usize = 0x130040;
const LCPLL_CTL_CD_FREQ_SEL_MASK: u32 = 3 << 26;
const LCPLL_CTL_CD_FREQ_SEL_HSW_ALT: u32 = 1 << 26;

const DEFAULT_HASWELL_HDMI_TRANSLATION: u8 = 6;
const DEFAULT_BACKLIGHT_PWM_HZ: u32 = 200;
const DEFAULT_BACKLIGHT_DUTY_PERCENT: u8 = 100;

const RECEIVER_CAPS_LEN: usize = 15;
const LINK_STATUS_LEN: usize = 6;
const CLOCK_RECOVERY_MAX_TRIES: u8 = 32;
const CLOCK_RECOVERY_MAX_SAME_VS_TRIES: u8 = 5;
const CHANNEL_EQ_MAX_TRIES: u8 = 6;

const POLL_TIMEOUT_PSR_US: u32 = 40_000;
const POLL_TIMEOUT_PDW_US: u32 = 20_000;
const POLL_TIMEOUT_PIPE_OFF_US: u32 = 40_000;
const POLL_TIMEOUT_DDI_IDLE_US: u32 = 10_000;
const POLL_TIMEOUT_DP_IDLE_US: u32 = 10_000;
const POLL_TIMEOUT_PANEL_ON_US: u32 = 1_000_000;

// ---------------------------------------------------------------------------
// Hardware/test sink traits
// ---------------------------------------------------------------------------

/// MMIO + delay/poll interface for Haswell modesetting.
///
/// Implemented directly for [`Mmio`] on hardware and by recording mocks in
/// host unit tests.
pub(crate) trait HaswellMmio {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);
    fn delay_us(&mut self, us: u32);

    fn posting_read(&self, offset: usize) {
        let _ = self.read32(offset);
    }

    fn update32(&mut self, offset: usize, mask_unset: u32, mask_set: u32) {
        let next = (self.read32(offset) & !mask_unset) | mask_set;
        self.write32(offset, next);
    }

    /// Poll until `(read32(offset) & mask) == expected` or `timeout_us` elapses.
    fn poll_mask(&mut self, offset: usize, mask: u32, expected: u32, timeout_us: u32) -> bool {
        let _ = timeout_us;
        (self.read32(offset) & mask) == expected
    }
}

impl HaswellMmio for Mmio {
    fn read32(&self, offset: usize) -> u32 {
        Mmio::read32(self, offset)
    }

    fn write32(&mut self, offset: usize, value: u32) {
        Mmio::write32(self, offset, value);
    }

    fn delay_us(&mut self, us: u32) {
        crate::gma::mmio::delay_us(us);
    }

    fn posting_read(&self, offset: usize) {
        Mmio::posting_read(self, offset);
    }

    fn poll_mask(&mut self, offset: usize, mask: u32, expected: u32, timeout_us: u32) -> bool {
        let deadline = fstart_arch::x86::timestamp_us().saturating_add(u64::from(timeout_us));
        loop {
            if (Mmio::read32(self, offset) & mask) == expected {
                return true;
            }
            if fstart_arch::x86::timestamp_us() >= deadline {
                return false;
            }
            core::hint::spin_loop();
        }
    }
}

/// DP AUX native read/write interface for Haswell link training.
pub(crate) trait HaswellAux {
    fn aux_read(&mut self, port: Port, address: u32, out: &mut [u8]) -> Result<(), GmaError>;
    fn aux_write(&mut self, port: Port, address: u32, data: &[u8]) -> Result<(), GmaError>;
}

struct LiveHaswellAux<'a> {
    mmio: &'a Mmio,
    cdclk_hz: u64,
    rawclk_hz: u64,
}

impl HaswellAux for LiveHaswellAux<'_> {
    fn aux_read(&mut self, port: Port, address: u32, out: &mut [u8]) -> Result<(), GmaError> {
        haswell_native_aux_read(self.mmio, port, self.cdclk_hz, self.rawclk_hz, address, out)
    }

    fn aux_write(&mut self, port: Port, address: u32, data: &[u8]) -> Result<(), GmaError> {
        haswell_native_aux_write(
            self.mmio,
            port,
            self.cdclk_hz,
            self.rawclk_hz,
            address,
            data,
        )
    }
}

// ---------------------------------------------------------------------------
// Generation marker & GenerationOps implementation
// ---------------------------------------------------------------------------

/// Haswell generation marker.
pub struct Haswell;

impl sealed::Sealed for Haswell {}

impl GenerationOps for Haswell {
    const GENERATION: Generation = Generation::Haswell;

    fn init_display(ctx: &mut crate::gma::GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        let mut mmio = ctx.mmio();
        let live_mmio = ctx.mmio();
        let cdclk_hz = detect_cdclk_hz(&live_mmio, ctx.config.cpu);
        let rawclk = rawclk_hz(ctx.config.cpu);
        let mut aux = LiveHaswellAux {
            mmio: &live_mmio,
            cdclk_hz,
            rawclk_hz: rawclk,
        };
        init_output(
            &mut mmio,
            &mut aux,
            ctx.config.cpu,
            port,
            ctx.pipe,
            mode,
            ctx.surface,
            ctx.config.framebuffer.scaling,
            ctx.config.vbt,
        )
    }

    fn disable_output(
        mmio: &Mmio,
        cpu: Cpu,
        pipe: Pipe,
        port: Port,
        _state: &mut crate::gma::state::GenerationState,
    ) -> Result<(), GmaError> {
        let mut sink = *mmio;
        disable_output_on(&mut sink, cpu, pipe, port)
    }

    fn clean(mmio: &Mmio, cpu: Cpu) {
        let mut sink = *mmio;
        clean_state(&mut sink, cpu);
    }

    fn prepare_port_probe(mmio: &Mmio, cpu: Cpu, outputs: &[OutputConfig], vbt: Option<&[u8]>) {
        let mut sink = *mmio;
        prepare_port_probe_on(&mut sink, cpu, outputs, vbt);
    }

    fn cleanup_unused_panel_probe(
        mmio: &Mmio,
        cpu: Cpu,
        outputs: &[OutputConfig],
        resolved_ports: &[Option<Port>],
    ) {
        let mut sink = *mmio;
        cleanup_unused_panel_probe_on(&mut sink, cpu, outputs, resolved_ports);
    }
}

// ---------------------------------------------------------------------------
// Clocks, power wells, and port detection
// ---------------------------------------------------------------------------

/// PCH raw clock frequency in Hz (`24 MHz` on Haswell-ULT/ULX, `125 MHz` on Traditional).
pub(crate) const fn rawclk_hz(cpu: Cpu) -> u64 {
    if cpu.is_haswell_lp() {
        24_000_000
    } else {
        125_000_000
    }
}

/// Decode Haswell CDCLK in Hz from `LCPLL_CTL` (`0x130040`) and CPU SKU.
///
/// Mirrors libgfxinit `HW.GFX.GMA.Power_And_Clocks.Get_Cur_CDClk` for Haswell:
/// when `LCPLL_CTL` bits 27:26 equal `1` (`HSW_ALTERNATE`), ULX runs at
/// 337.5 MHz, ULT at 450 MHz, and Traditional at 540 MHz; otherwise 450 MHz.
pub(crate) const fn decode_cdclk_hz(cpu: Cpu, lcpll_ctl: u32) -> u64 {
    if (lcpll_ctl & LCPLL_CTL_CD_FREQ_SEL_MASK) == LCPLL_CTL_CD_FREQ_SEL_HSW_ALT {
        match cpu {
            Cpu::HaswellUlx => 337_500_000,
            Cpu::HaswellUlt => 450_000_000,
            _ => 540_000_000,
        }
    } else {
        450_000_000
    }
}

/// Read `LCPLL_CTL` from MMIO and decode the active Haswell CDCLK in Hz.
pub(crate) fn detect_cdclk_hz<M: HaswellMmio>(mmio: &M, cpu: Cpu) -> u64 {
    decode_cdclk_hz(cpu, mmio.read32(LCPLL_CTL))
}

/// Whether the Haswell Power Down Well (PDW) is required for `(port, pipe, scaled)`.
///
/// Matches libgfxinit `Need_PDW`: eDP on Pipe A without scaling runs purely in
/// the Always-On well; any other port, pipe B/C, or scaled eDP requires PDW.
pub(crate) const fn need_pdw(port: Port, pipe: Pipe, scaled: bool) -> bool {
    !matches!(port, Port::Edp) || !matches!(pipe, Pipe::A) || scaled
}

/// Detected Haswell digital/analog port straps from `DDI_BUF_CTL_A` and `SFUSE_STRAP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HaswellDetectedPorts {
    /// eDP detected via `DDI_BUF_CTL_A` bit 0.
    pub edp: bool,
    /// DDI-A configured for 4 lanes (`DDI_BUF_CTL_A` bit 4).
    pub ddi_a_4_lanes: bool,
    /// DDI-B strapped present (`SFUSE_STRAP` bit 2).
    pub ddi_b: bool,
    /// DDI-C strapped present (`SFUSE_STRAP` bit 1).
    pub ddi_c: bool,
    /// DDI-D strapped present (`SFUSE_STRAP` bit 0; always false on ULT/ULX).
    pub ddi_d: bool,
    /// CRT/VGA DAC present (`SFUSE_STRAP` bit 6 clear; always false on ULT/ULX).
    pub crt: bool,
}

/// Decode Haswell hardware port straps from `DDI_BUF_CTL_A` and `SFUSE_STRAP`.
pub const fn decode_detected_ports(
    cpu: Cpu,
    ddi_buf_ctl_a: u32,
    sfuse_strap: u32,
) -> HaswellDetectedPorts {
    let is_lp = cpu.is_haswell_lp();
    HaswellDetectedPorts {
        edp: (ddi_buf_ctl_a & DDI_BUF_CTL_INIT_DISPLAY_DETECTED) != 0,
        ddi_a_4_lanes: (ddi_buf_ctl_a & DDI_BUF_CTL_A_4_LANES) != 0,
        ddi_b: (sfuse_strap & SFUSE_STRAP_DDI_B) != 0,
        ddi_c: (sfuse_strap & SFUSE_STRAP_DDI_C) != 0,
        ddi_d: !is_lp && (sfuse_strap & SFUSE_STRAP_DDI_D) != 0,
        crt: !is_lp && (sfuse_strap & SFUSE_STRAP_CRT_DAC_DISABLE) == 0,
    }
}

/// Read `DDI_BUF_CTL_A` and `SFUSE_STRAP` to detect present Haswell ports.
pub(crate) fn detect_ports<M: HaswellMmio>(mmio: &M, cpu: Cpu) -> HaswellDetectedPorts {
    decode_detected_ports(cpu, mmio.read32(DDI_BUF_CTL_A), mmio.read32(SFUSE_STRAP))
}

/// Disable PSR (`SRD_CTL`) and, on ULT/ULX, IPS (`IPS_CTL` + 20 ms wait).
pub(crate) fn pre_all_off<M: HaswellMmio>(mmio: &mut M, cpu: Cpu) {
    if (mmio.read32(SRD_CTL) & SRD_CTL_ENABLE) != 0 {
        mmio.update32(SRD_CTL, SRD_CTL_ENABLE, 0);
        let _ = mmio.poll_mask(SRD_STATUS, SRD_STATUS_STATE_MASK, 0, POLL_TIMEOUT_PSR_US);
    }
    if cpu.is_haswell_lp() && (mmio.read32(IPS_CTL) & IPS_CTL_ENABLE) != 0 {
        mmio.update32(IPS_CTL, IPS_CTL_ENABLE, 0);
        mmio.delay_us(20_000);
    }
}

/// Request Haswell Power Down Well (PDW) on via `PWR_WELL_CTL_DRIVER`.
pub(crate) fn pdw_on<M: HaswellMmio>(mmio: &mut M) {
    let ctl1 = mmio.read32(PWR_WELL_CTL_BIOS);
    let ctl2 = mmio.read32(PWR_WELL_CTL_DRIVER);
    let ctl3 = mmio.read32(PWR_WELL_CTL_KVMR);
    let ctl4 = mmio.read32(PWR_WELL_CTL_DEBUG);

    if ((ctl1 | ctl2 | ctl3 | ctl4) & PWR_WELL_CTL_ENABLE_REQUEST) == 0 {
        let _ = mmio.poll_mask(
            PWR_WELL_CTL_DRIVER,
            PWR_WELL_CTL_STATE_ENABLED,
            0,
            POLL_TIMEOUT_PDW_US,
        );
    }
    if (ctl2 & PWR_WELL_CTL_ENABLE_REQUEST) == 0 {
        mmio.write32(PWR_WELL_CTL_DRIVER, PWR_WELL_CTL_ENABLE_REQUEST);
        let _ = mmio.poll_mask(
            PWR_WELL_CTL_DRIVER,
            PWR_WELL_CTL_STATE_ENABLED,
            PWR_WELL_CTL_STATE_ENABLED,
            POLL_TIMEOUT_PDW_US,
        );
    }
}

/// Release BIOS and driver requests on the Haswell Power Down Well (PDW).
pub(crate) fn pdw_off<M: HaswellMmio>(mmio: &mut M) {
    let ctl1 = mmio.read32(PWR_WELL_CTL_BIOS);
    let ctl2 = mmio.read32(PWR_WELL_CTL_DRIVER);
    let ctl3 = mmio.read32(PWR_WELL_CTL_KVMR);
    let ctl4 = mmio.read32(PWR_WELL_CTL_DEBUG);

    if ((ctl1 | ctl2 | ctl3 | ctl4) & PWR_WELL_CTL_ENABLE_REQUEST) != 0 {
        let _ = mmio.poll_mask(
            PWR_WELL_CTL_DRIVER,
            PWR_WELL_CTL_STATE_ENABLED,
            PWR_WELL_CTL_STATE_ENABLED,
            POLL_TIMEOUT_PDW_US,
        );
    }
    if (ctl1 & PWR_WELL_CTL_ENABLE_REQUEST) != 0 {
        mmio.write32(PWR_WELL_CTL_BIOS, 0);
    }
    if (ctl2 & PWR_WELL_CTL_ENABLE_REQUEST) != 0 {
        mmio.write32(PWR_WELL_CTL_DRIVER, 0);
    }
}

// ---------------------------------------------------------------------------
// Panel power & backlight helpers (no write-protect key on Haswell)
// ---------------------------------------------------------------------------

fn apply_panel_op<M: HaswellMmio>(mmio: &mut M, op: PanelRegisterOp) {
    match op {
        PanelRegisterOp::Write { register, value } => mmio.write32(register, value),
        PanelRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => mmio.update32(register, mask_unset, mask_set),
        PanelRegisterOp::Set { register, mask } => mmio.update32(register, 0, mask),
        PanelRegisterOp::Clear { register, mask } => mmio.update32(register, mask, 0),
    }
}

/// Configure PCH panel power sequencer delays for Haswell eDP without any
/// pre-Haswell write-protect key.
pub(crate) fn setup_edp_panel_sequencer<M: HaswellMmio>(mmio: &mut M, cpu: Cpu) {
    let regs = PanelPowerRegs::PCH;
    let delays = PanelPowerDelays::from_registers(
        mmio.read32(regs.on_delays),
        mmio.read32(regs.off_delays),
        mmio.read32(regs.divisor),
    );
    let plan = panel_power_sequencer_plan(
        regs,
        delays,
        PanelPowerPortSelect::DpA,
        true,
        false, // Has_PP_Write_Protection = false on Haswell
    );
    apply_panel_op(mmio, plan.control);
    if plan.override_delays {
        apply_panel_op(mmio, plan.on_delays);
        apply_panel_op(mmio, plan.off_delays);
        apply_panel_op(mmio, plan.cycle_delay);
    }
    // Ensure PP_DIVISOR reference clock divider (bits 31:8 = rawclk / 20 kHz - 1)
    // is populated when reset left it zero.
    let divisor = mmio.read32(regs.divisor);
    if (divisor & 0xffff_ff00) == 0 {
        let ref_div = ((rawclk_hz(cpu) / 20_000) - 1) as u32;
        mmio.write32(regs.divisor, (ref_div << 8) | (divisor & 0x1f));
    }
}

/// Turn on Haswell eDP panel power without writing the pre-Haswell `0xabcd` key.
pub(crate) fn panel_power_on<M: HaswellMmio>(mmio: &mut M) -> Result<(), GmaError> {
    let regs = PanelPowerRegs::PCH;
    let control = mmio.read32(regs.control);
    let was_on = (control & PP_CONTROL::TARGET_ON::SET.value) != 0;
    let next = control | PP_CONTROL::TARGET_ON::SET.value;
    mmio.write32(regs.control, next);
    mmio.posting_read(regs.control);
    if !was_on {
        mmio.delay_us(210_000);
    }
    let ready_mask = PP_STATUS::SEQUENCE.val(3).value | PP_STATUS::ON::SET.value;
    let ready_val = PP_STATUS::ON::SET.value;
    if mmio.poll_mask(regs.status, ready_mask, ready_val, POLL_TIMEOUT_PANEL_ON_US) {
        Ok(())
    } else {
        Err(GmaError::Timeout)
    }
}

/// Turn off Haswell eDP backlight and panel power (no write-protect key).
pub(crate) fn panel_off<M: HaswellMmio>(mmio: &mut M) {
    let ctrl = panel_power_control_plan(PanelPowerRegs::PCH);
    apply_panel_op(mmio, ctrl.backlight_off);
    if (mmio.read32(BLC_PWM_PCH_CTL1) & BLM_PCH_PWM_ENABLE) != 0 {
        mmio.update32(BLC_PWM_PCH_CTL1, BLM_PCH_PWM_ENABLE, 0);
    }
    apply_panel_op(mmio, ctrl.panel_off);
    mmio.posting_read(PanelPowerRegs::PCH.control);
}

/// Configure Haswell PCH backlight PWM and enable panel backlight gate.
pub(crate) fn backlight_on<M: HaswellMmio>(mmio: &mut M, cpu: Cpu, vbt: Option<&[u8]>) {
    let (pwm_hz, active_low) = vbt
        .and_then(|bytes| Vbt::parse(bytes).ok())
        .and_then(|parsed| parsed.lfp_panel_metadata().ok())
        .and_then(|meta| meta.backlight)
        .filter(|bl| bl.pwm_freq_hz > 0)
        .map(|bl| (u32::from(bl.pwm_freq_hz), bl.active_low_pwm))
        .unwrap_or((DEFAULT_BACKLIGHT_PWM_HZ, false));

    if let Some(plan) = HaswellBacklightPwmPlan::new(
        cpu.is_haswell_lp(),
        pwm_hz,
        active_low,
        DEFAULT_BACKLIGHT_DUTY_PERCENT,
    ) {
        apply_panel_op(mmio, plan.south_chicken2);
        apply_panel_op(mmio, plan.pch_ctl2);
        apply_panel_op(mmio, plan.pch_ctl1);
    }
    let ctrl = panel_power_control_plan(PanelPowerRegs::PCH);
    apply_panel_op(mmio, ctrl.backlight_on);
    mmio.posting_read(PanelPowerRegs::PCH.control);
}

// ---------------------------------------------------------------------------
// Transcoder, pipe, plane, scaler, and DDI register helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transcoder {
    A,
    B,
    C,
    Edp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscoderRegs {
    htotal: usize,
    hblank: usize,
    hsync: usize,
    vtotal: usize,
    vblank: usize,
    vsync: usize,
    data_m1: usize,
    data_n1: usize,
    link_m1: usize,
    link_n1: usize,
    ddi_func_ctl: usize,
    msa_misc: usize,
    conf: usize,
    clk_sel: Option<usize>,
}

impl Transcoder {
    const fn for_pipe(pipe: Pipe) -> Self {
        match pipe {
            Pipe::A => Self::A,
            Pipe::B => Self::B,
            Pipe::C => Self::C,
        }
    }

    const fn for_output(pipe: Pipe, ddi: DdiPort) -> Self {
        match ddi {
            DdiPort::A => Self::Edp,
            _ => Self::for_pipe(pipe),
        }
    }

    const fn regs(self) -> TranscoderRegs {
        let (timing_base, conf, clk_sel) = match self {
            Self::A => (0x60000, 0x70008, Some(TRANSA_CLK_SEL)),
            Self::B => (0x61000, 0x71008, Some(TRANSB_CLK_SEL)),
            Self::C => (0x62000, 0x72008, Some(TRANSC_CLK_SEL)),
            Self::Edp => (0x6f000, 0x7f008, None),
        };
        TranscoderRegs {
            htotal: timing_base,
            hblank: timing_base + 0x04,
            hsync: timing_base + 0x08,
            vtotal: timing_base + 0x0c,
            vblank: timing_base + 0x10,
            vsync: timing_base + 0x14,
            data_m1: timing_base + 0x30,
            data_n1: timing_base + 0x34,
            link_m1: timing_base + 0x40,
            link_n1: timing_base + 0x44,
            ddi_func_ctl: timing_base + 0x400,
            msa_misc: timing_base + 0x410,
            conf,
            clk_sel,
        }
    }
}

const fn pipe_base(pipe: Pipe) -> usize {
    match pipe {
        Pipe::A => 0x70000,
        Pipe::B => 0x71000,
        Pipe::C => 0x72000,
    }
}

const fn pipesrc_reg(pipe: Pipe) -> usize {
    match pipe {
        Pipe::A => PIPESRC_A,
        Pipe::B => PIPESRC_B,
        Pipe::C => PIPESRC_C,
    }
}

const fn wm_linetime_reg(pipe: Pipe) -> usize {
    match pipe {
        Pipe::A => WM_LINETIME_A,
        Pipe::B => WM_LINETIME_B,
        Pipe::C => WM_LINETIME_C,
    }
}

/// Compute Haswell `WM_LINETIME` (in units of 1/8 µs, masked to 9 bits).
pub(crate) const fn wm_linetime(mode: Mode) -> u32 {
    if mode.pixel_clock_khz == 0 {
        return 0;
    }
    let numerator = (mode.htotal as u64) * 8_000 + (mode.pixel_clock_khz as u64) / 2;
    ((numerator / mode.pixel_clock_khz as u64) as u32) & 0x1ff
}

const fn ddi_regs_offsets(ddi: DdiPort) -> (usize, usize, Option<usize>, usize) {
    let idx = match ddi {
        DdiPort::A => 0,
        DdiPort::B => 1,
        DdiPort::C => 2,
        DdiPort::D => 3,
        DdiPort::E => 4,
    };
    let buf_ctl = 0x64000 + idx * 0x100;
    let dp_tp_ctl = 0x64040 + idx * 0x100;
    let dp_tp_status = match ddi {
        DdiPort::A => None,
        _ => Some(0x64044 + idx * 0x100),
    };
    let port_clk_sel = HSW_PORT_CLK_SEL_BASE + idx * 4;
    (buf_ctl, dp_tp_ctl, dp_tp_status, port_clk_sel)
}

const fn trans_clk_sel_for_ddi(ddi: DdiPort) -> u32 {
    match ddi {
        DdiPort::B => TRANS_CLK_SEL_DDIB,
        DdiPort::C => TRANS_CLK_SEL_DDIC,
        DdiPort::D => TRANS_CLK_SEL_DDID,
        _ => TRANS_CLK_SEL_NONE,
    }
}

const fn edp_select_bits(pipe: Pipe, scaled: bool) -> u32 {
    match pipe {
        Pipe::A if !scaled => DDI_FUNC_CTL_EDP_SELECT_ALWAYS_ON,
        Pipe::A => DDI_FUNC_CTL_EDP_SELECT_PIPE_A,
        Pipe::B => DDI_FUNC_CTL_EDP_SELECT_PIPE_B,
        Pipe::C => DDI_FUNC_CTL_EDP_SELECT_PIPE_C,
    }
}

const fn port_clk_sel_for_dp_rate(rate: DpLinkRate) -> u32 {
    match rate {
        DpLinkRate::Hbr2 => PORT_CLK_SEL_LCPLL2700,
        DpLinkRate::Hbr => PORT_CLK_SEL_LCPLL1350,
        _ => PORT_CLK_SEL_LCPLL810,
    }
}

/// Program the 20-dword `DDI_BUF_TRANS` table for one Haswell DDI port.
pub(crate) fn init_ddi_buf_trans<M: HaswellMmio>(
    mmio: &mut M,
    cpu: Cpu,
    ddi: DdiPort,
) -> Result<(), GmaError> {
    let table = hsw_ddi_buffer_translations(cpu, ddi, false, DEFAULT_HASWELL_HDMI_TRANSLATION)?;
    let base = ddi.buf_trans_base();
    for (index, &word) in table.iter().enumerate() {
        mmio.write32(base + index * 4, word);
    }
    Ok(())
}

/// Disable one DDI port (`DDI_BUF_CTL`, `DP_TP_CTL`, idle wait, `PORT_CLK_SEL = NONE`).
///
/// Preserves bit 4 (`DDI_A_4_LANES`) and bit 0 (`INIT_DISPLAY_DETECTED`) in `DDI_BUF_CTL`.
pub(crate) fn digital_off<M: HaswellMmio>(mmio: &mut M, ddi: DdiPort) {
    let (buf_ctl_reg, dp_tp_ctl_reg, _, port_clk_sel_reg) = ddi_regs_offsets(ddi);
    let was_enabled = (mmio.read32(buf_ctl_reg) & DDI_BUF_CTL_ENABLE) != 0;
    if was_enabled {
        mmio.update32(buf_ctl_reg, DDI_BUF_CTL_ENABLE, 0);
    }
    mmio.update32(dp_tp_ctl_reg, DP_TP_CTL_TRANSPORT_ENABLE, 0);
    if was_enabled {
        let _ = mmio.poll_mask(
            buf_ctl_reg,
            DDI_BUF_CTL_IDLE_STATUS,
            DDI_BUF_CTL_IDLE_STATUS,
            POLL_TIMEOUT_DDI_IDLE_US,
        );
    }
    mmio.write32(port_clk_sel_reg, PORT_CLK_SEL_NONE);
}

fn disable_transcoder<M: HaswellMmio>(mmio: &mut M, trans: Transcoder) {
    let regs = trans.regs();
    let enabled = (mmio.read32(regs.conf) & TRANS_CONF_ENABLE) != 0;
    if enabled {
        mmio.update32(regs.conf, TRANS_CONF_ENABLE, 0);
    }
    let _ = mmio.poll_mask(
        regs.conf,
        TRANS_CONF_ENABLED_STATUS,
        0,
        POLL_TIMEOUT_PIPE_OFF_US,
    );
    if (mmio.read32(regs.ddi_func_ctl) & DDI_FUNC_CTL_ENABLE) != 0 {
        mmio.write32(regs.ddi_func_ctl, 0);
    }
}

fn disable_pipe_transcoders<M: HaswellMmio>(mmio: &mut M, pipe: Pipe) {
    let edp_func = mmio.read32(Transcoder::Edp.regs().ddi_func_ctl) & DDI_FUNC_CTL_EDP_SELECT_MASK;
    let edp_bound_to_pipe = match pipe {
        Pipe::A => {
            edp_func == DDI_FUNC_CTL_EDP_SELECT_ALWAYS_ON
                || edp_func == DDI_FUNC_CTL_EDP_SELECT_PIPE_A
        }
        Pipe::B => edp_func == DDI_FUNC_CTL_EDP_SELECT_PIPE_B,
        Pipe::C => edp_func == DDI_FUNC_CTL_EDP_SELECT_PIPE_C,
    };
    if edp_bound_to_pipe {
        disable_transcoder(mmio, Transcoder::Edp);
    }
    disable_transcoder(mmio, Transcoder::for_pipe(pipe));
    if let Some(clk_sel) = Transcoder::for_pipe(pipe).regs().clk_sel {
        mmio.write32(clk_sel, TRANS_CLK_SEL_NONE);
    }
}

fn disable_pipe_planes_and_pfit<M: HaswellMmio>(mmio: &mut M, pipe: Pipe) {
    let base = pipe_base(pipe);
    // Disable cursor and primary plane.
    mmio.write32(base + 0x80, 0); // CUR_CTL
    mmio.write32(base + 0xa0, 0); // CUR_FBC_CTL
    if (mmio.read32(base + 0x180) & DSPCNTR_ENABLE) != 0 {
        mmio.update32(base + 0x180, DSPCNTR_ENABLE, 0);
        mmio.write32(base + 0x19c, 0); // DSPSURF arm
    }
    // Disable panel fitter.
    let pfit = IronlakePfitRegs::for_pipe(pipe);
    mmio.update32(pfit.control, 1 << 31, 0);
    mmio.write32(pfit.win_size, 0);
}

// ---------------------------------------------------------------------------
// Haswell DP / eDP link training (End_EDP_Training_Late)
// ---------------------------------------------------------------------------

const fn haswell_max_voltage_swing() -> VoltageSwing {
    VoltageSwing::Level2
}

const fn haswell_max_pre_emphasis(vs: VoltageSwing) -> PreEmphasis {
    match vs {
        VoltageSwing::Level0 => PreEmphasis::Level3,
        VoltageSwing::Level1 => PreEmphasis::Level2,
        VoltageSwing::Level2 => PreEmphasis::Level1,
        VoltageSwing::Level3 => PreEmphasis::Level0,
    }
}

const fn clamp_haswell_train_set(set: TrainSet) -> TrainSet {
    let vs = if (set.voltage_swing as u8) > (haswell_max_voltage_swing() as u8) {
        haswell_max_voltage_swing()
    } else {
        set.voltage_swing
    };
    let max_pe = haswell_max_pre_emphasis(vs);
    let pe = if (set.pre_emphasis as u8) > (max_pe as u8) {
        max_pe
    } else {
        set.pre_emphasis
    };
    TrainSet {
        voltage_swing: vs,
        pre_emphasis: pe,
    }
}

/// Map `(VoltageSwing, PreEmphasis)` to Haswell `DDI_BUF_CTL` `TRANS_SELECT` index (0..=8).
pub(crate) const fn haswell_trans_select(train_set: TrainSet) -> u8 {
    match (train_set.voltage_swing, train_set.pre_emphasis) {
        (VoltageSwing::Level0, PreEmphasis::Level0) => 0,
        (VoltageSwing::Level0, PreEmphasis::Level1) => 1,
        (VoltageSwing::Level0, PreEmphasis::Level2) => 2,
        (VoltageSwing::Level0, PreEmphasis::Level3) => 3,
        (VoltageSwing::Level1, PreEmphasis::Level0) => 4,
        (VoltageSwing::Level1, PreEmphasis::Level1) => 5,
        (VoltageSwing::Level1, PreEmphasis::Level2) => 6,
        (VoltageSwing::Level2, PreEmphasis::Level0) => 7,
        (VoltageSwing::Level2, PreEmphasis::Level1) => 8,
        (VoltageSwing::Level3, PreEmphasis::Level0) => 9,
        _ => 0,
    }
}

const fn haswell_dp_tp_pattern(pattern: TrainingPattern) -> u32 {
    match pattern {
        TrainingPattern::Pattern1 => DP_TP_CTL_LINK_TRAIN_PAT1,
        TrainingPattern::Pattern2 => DP_TP_CTL_LINK_TRAIN_PAT2,
        TrainingPattern::Pattern3 => DP_TP_CTL_LINK_TRAIN_PAT3,
        TrainingPattern::None => DP_TP_CTL_LINK_TRAIN_NORMAL,
    }
}

fn write_dp_tp_ctl<M: HaswellMmio>(
    mmio: &mut M,
    ddi: DdiPort,
    link: DpLinkConfig,
    pattern_bits: u32,
) {
    let (_, dp_tp_ctl_reg, _, _) = ddi_regs_offsets(ddi);
    let enhanced = if link.enhanced_framing {
        DP_TP_CTL_ENHANCED_FRAME_ENABLE
    } else {
        0
    };
    mmio.write32(
        dp_tp_ctl_reg,
        DP_TP_CTL_TRANSPORT_ENABLE | enhanced | pattern_bits,
    );
}

fn set_ddi_signal_levels<M: HaswellMmio>(
    mmio: &mut M,
    ddi: DdiPort,
    link: DpLinkConfig,
    train_set: TrainSet,
) -> Result<(), GmaError> {
    let (buf_ctl_reg, _, _, _) = ddi_regs_offsets(ddi);
    let lanes = DdiLaneCount::new(link.lane_count)?;
    let current = mmio.read32(buf_ctl_reg);
    let was_enabled = (current & DDI_BUF_CTL_ENABLE) != 0;
    let next = ddi_buf_ctl_dp_rmw(current, haswell_trans_select(train_set), lanes, true);
    mmio.write32(buf_ctl_reg, next);
    mmio.posting_read(buf_ctl_reg);
    if !was_enabled {
        // Intel Haswell PRM / libgfxinit: wait >= 518 us after first DDI buffer enable.
        mmio.delay_us(600);
    }
    Ok(())
}

fn haswell_sink_training_payload(
    pattern: TrainingPattern,
    train_set: TrainSet,
    lane_count: u8,
    out: &mut [u8; 5],
) -> Result<usize, GmaError> {
    if !matches!(lane_count, 1 | 2 | 4) {
        return Err(GmaError::InvalidConfig);
    }
    let clamped = clamp_haswell_train_set(train_set);
    out[0] = pattern.encode();
    let lane_byte = clamped.encode(
        haswell_max_voltage_swing(),
        haswell_max_pre_emphasis(clamped.voltage_swing),
    );
    for slot in out.iter_mut().skip(1).take(lane_count as usize) {
        *slot = lane_byte;
    }
    Ok(1 + lane_count as usize)
}

fn haswell_adjusted_train_set(status: LinkStatus, lane_count: u8) -> TrainSet {
    let mut vs = VoltageSwing::Level0;
    let mut pe = PreEmphasis::Level0;
    for lane in 0..lane_count {
        let req = status.requested_train_set(lane);
        if (req.voltage_swing as u8) > (vs as u8) {
            vs = req.voltage_swing;
        }
        if (req.pre_emphasis as u8) > (pe as u8) {
            pe = req.pre_emphasis;
        }
    }
    clamp_haswell_train_set(TrainSet {
        voltage_swing: vs,
        pre_emphasis: pe,
    })
}

const fn cr_delay_us(link: DpLinkConfig) -> u32 {
    if matches!(link.link_rate, DpLinkRate::Hbr2) && link.aux_rd_interval != 0 {
        (link.aux_rd_interval as u32) * 4_000
    } else {
        100
    }
}

const fn eq_delay_us(link: DpLinkConfig) -> u32 {
    if link.aux_rd_interval != 0 {
        (link.aux_rd_interval as u32) * 4_000
    } else {
        400
    }
}

fn train_haswell_dp_once<M: HaswellMmio, A: HaswellAux>(
    mmio: &mut M,
    aux: &mut A,
    port: Port,
    ddi: DdiPort,
    link: DpLinkConfig,
) -> Result<(), GmaError> {
    let writes = sink_link_config_writes(link);
    aux.aux_write(
        port,
        DPCD_LINK_BW_SET,
        &[writes.link_bw_set, writes.lane_count_set],
    )?;
    aux.aux_write(
        port,
        DPCD_DOWNSPREAD_CTRL,
        &[writes.downspread_ctrl, writes.main_link_channel_coding],
    )?;

    // Stage 1: Clock recovery (Training Pattern 1).
    let mut train_set = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    let mut same_vs_tries = 0u8;
    let mut last_vs = train_set.voltage_swing;
    let mut cr_ok = false;

    for _ in 0..CLOCK_RECOVERY_MAX_TRIES {
        write_dp_tp_ctl(mmio, ddi, link, DP_TP_CTL_LINK_TRAIN_PAT1);
        set_ddi_signal_levels(mmio, ddi, link, train_set)?;
        let mut payload = [0u8; 5];
        let len = haswell_sink_training_payload(
            TrainingPattern::Pattern1,
            train_set,
            link.lane_count,
            &mut payload,
        )?;
        aux.aux_write(port, DPCD_TRAINING_PATTERN_SET, &payload[..len])?;

        mmio.delay_us(cr_delay_us(link));
        let mut status_bytes = [0u8; LINK_STATUS_LEN];
        aux.aux_read(port, DPCD_LINK_STATUS, &mut status_bytes)?;
        let status = LinkStatus::parse(&status_bytes)?;
        if status.clock_recovery_done(link.lane_count) {
            cr_ok = true;
            break;
        }
        let next = haswell_adjusted_train_set(status, link.lane_count);
        if next.voltage_swing == last_vs {
            same_vs_tries += 1;
        } else {
            same_vs_tries = 0;
            last_vs = next.voltage_swing;
        }
        if same_vs_tries >= CLOCK_RECOVERY_MAX_SAME_VS_TRIES
            || train_set.voltage_swing == haswell_max_voltage_swing()
        {
            break;
        }
        train_set = next;
    }
    if !cr_ok {
        return Err(GmaError::HardwareError);
    }

    // Stage 2: Channel equalization (TPS3 when supported, else TPS2).
    let eq_pattern = if link.tps3_supported {
        TrainingPattern::Pattern3
    } else {
        TrainingPattern::Pattern2
    };
    let mut eq_ok = false;
    for _ in 0..CHANNEL_EQ_MAX_TRIES {
        write_dp_tp_ctl(mmio, ddi, link, haswell_dp_tp_pattern(eq_pattern));
        set_ddi_signal_levels(mmio, ddi, link, train_set)?;
        let mut payload = [0u8; 5];
        let len =
            haswell_sink_training_payload(eq_pattern, train_set, link.lane_count, &mut payload)?;
        aux.aux_write(port, DPCD_TRAINING_PATTERN_SET, &payload[..len])?;

        mmio.delay_us(eq_delay_us(link));
        let mut status_bytes = [0u8; LINK_STATUS_LEN];
        aux.aux_read(port, DPCD_LINK_STATUS, &mut status_bytes)?;
        let status = LinkStatus::parse(&status_bytes)?;
        if status.channel_eq_done(link.lane_count) {
            eq_ok = true;
            break;
        }
        if !status.clock_recovery_done(link.lane_count) {
            break;
        }
        train_set = haswell_adjusted_train_set(status, link.lane_count);
    }
    if !eq_ok {
        return Err(GmaError::HardwareError);
    }

    // Stage 3: End training on sink first, then send IDLE on source.
    let mut end_payload = [0u8; 5];
    let end_len = haswell_sink_training_payload(
        TrainingPattern::None,
        TrainSet {
            voltage_swing: VoltageSwing::Level0,
            pre_emphasis: PreEmphasis::Level0,
        },
        link.lane_count,
        &mut end_payload,
    )?;
    aux.aux_write(port, DPCD_TRAINING_PATTERN_SET, &end_payload[..end_len])?;

    write_dp_tp_ctl(mmio, ddi, link, DP_TP_CTL_LINK_TRAIN_IDLE);
    // Haswell `End_EDP_Training_Late`: DDI-A stays in IDLE until Post_On (after
    // PIPE_EDP_CONF enable). Non-eDP ports wait for MIN_IDLES_SENT and enter NORMAL now.
    if ddi != DdiPort::A {
        let (_, _, dp_tp_status, _) = ddi_regs_offsets(ddi);
        if let Some(status_reg) = dp_tp_status {
            let _ = mmio.poll_mask(
                status_reg,
                DP_TP_STATUS_MIN_IDLES_SENT,
                DP_TP_STATUS_MIN_IDLES_SENT,
                POLL_TIMEOUT_DP_IDLE_US,
            );
        }
        write_dp_tp_ctl(mmio, ddi, link, DP_TP_CTL_LINK_TRAIN_NORMAL);
    }
    Ok(())
}

fn train_haswell_dp<M: HaswellMmio, A: HaswellAux>(
    mmio: &mut M,
    aux: &mut A,
    cpu: Cpu,
    port: Port,
    ddi: DdiPort,
    mode: Mode,
) -> Result<DpLinkConfig, GmaError> {
    let mut caps_bytes = [0u8; RECEIVER_CAPS_LEN];
    aux.aux_read(port, DPCD_RECEIVER_CAPS, &mut caps_bytes)?;
    let mut caps = ReceiverCaps::parse(&caps_bytes)?;
    // libgfxinit `DP_Max_2_7_GHz := CPU_Haswell and Is_ULX`: ULX LCPLL has no 2.7 GHz (5.4 Gbps) tap.
    if cpu == Cpu::HaswellUlx && matches!(caps.max_link_rate, DpLinkRate::Hbr2) {
        caps.max_link_rate = DpLinkRate::Hbr;
    }
    // Haswell DDI supports Training Pattern 3 (`TPS3_Supported => True`).
    let candidates = link_config_candidates(caps, mode, true)?;
    let (_, _, _, port_clk_sel_reg) = ddi_regs_offsets(ddi);
    let mut last_error = GmaError::ModeUnavailable;
    let mut index = 0usize;
    while let Some(link) = candidates.get(index) {
        for _attempt in 0..2 {
            mmio.write32(port_clk_sel_reg, port_clk_sel_for_dp_rate(link.link_rate));
            match train_haswell_dp_once(mmio, aux, port, ddi, link) {
                Ok(()) => return Ok(link),
                Err(err) => {
                    last_error = err;
                    let _ = aux.aux_write(port, DPCD_TRAINING_PATTERN_SET, &[0]);
                    digital_off(mmio, ddi);
                }
            }
        }
        index += 1;
    }
    Err(last_error)
}

// ---------------------------------------------------------------------------
// Top-level Haswell sequences (clean, probe, init_output, disable_output)
// ---------------------------------------------------------------------------

/// Return Haswell display hardware to libgfxinit's `Clean_State` and initialize
/// PCH rawclk, reset handshake, and DDI buffer translation tables.
pub(crate) fn clean_state<M: HaswellMmio>(mmio: &mut M, cpu: Cpu) {
    pre_all_off(mmio, cpu);
    panel_off(mmio);
    mmio.write32(CPU_VGACNTRL, VGA_DISPLAY_DISABLE);

    for pipe in [Pipe::A, Pipe::B, Pipe::C] {
        disable_pipe_planes_and_pfit(mmio, pipe);
        disable_pipe_transcoders(mmio, pipe);
    }

    let ddis: &[DdiPort] = if cpu.is_haswell_lp() {
        &[DdiPort::A, DdiPort::B, DdiPort::C]
    } else {
        &[DdiPort::A, DdiPort::B, DdiPort::C, DdiPort::D]
    };
    for &ddi in ddis {
        digital_off(mmio, ddi);
    }

    // Disable WRPLLs and SPLL.
    mmio.update32(WRPLL_CTL_1, WRPLL_CTL_ENABLE, 0);
    mmio.update32(WRPLL_CTL_2, WRPLL_CTL_ENABLE, 0);
    mmio.update32(SPLL_CTL, SPLL_CTL_ENABLE, 0);

    pdw_off(mmio);

    // Initialize PCH rawclk and display/PCH reset handshake.
    mmio.write32(PCH_RAWCLK_FREQ, (rawclk_hz(cpu) / 1_000_000) as u32);
    mmio.update32(NDE_RSTWRN_OPT, 0, RST_PCH_HANDSHAKE_EN);

    // Program DDI buffer translation tables for all available DDI ports.
    for &ddi in ddis {
        let _ = init_ddi_buf_trans(mmio, cpu, ddi);
    }
}

/// Prepare Haswell connectors before probing EDID/AUX: enable PDW if any
/// non-eDP connector is enabled, and power on the eDP panel (without the
/// pre-Haswell write-protect key) if `Port::Edp` is enabled.
pub(crate) fn prepare_port_probe_on<M: HaswellMmio>(
    mmio: &mut M,
    cpu: Cpu,
    outputs: &[OutputConfig],
    _vbt: Option<&[u8]>,
) {
    if !cpu.is_haswell() {
        return;
    }
    let wants_non_edp = outputs
        .iter()
        .any(|output| output.enabled && output.port != Port::Edp);
    if wants_non_edp {
        pdw_on(mmio);
    }
    let wants_edp = outputs
        .iter()
        .any(|output| output.enabled && output.port == Port::Edp);
    if wants_edp {
        setup_edp_panel_sequencer(mmio, cpu);
        let _ = panel_power_on(mmio);
    }
}

/// Tear down probe-only panel power if `Port::Edp` was enabled for probing but
/// not selected for output, and power down PDW if no outputs were resolved.
pub(crate) fn cleanup_unused_panel_probe_on<M: HaswellMmio>(
    mmio: &mut M,
    cpu: Cpu,
    outputs: &[OutputConfig],
    resolved_ports: &[Option<Port>],
) {
    if !cpu.is_haswell() {
        return;
    }
    let wants_edp = outputs
        .iter()
        .any(|output| output.enabled && output.port == Port::Edp);
    let resolved_edp = resolved_ports.contains(&Some(Port::Edp));
    if wants_edp && !resolved_edp {
        panel_off(mmio);
    }
    let any_resolved = resolved_ports.iter().any(Option::is_some);
    if !any_resolved {
        pdw_off(mmio);
    }
}

fn allocate_wrpll<M: HaswellMmio>(mmio: &M, pipe: Pipe) -> (usize, u32) {
    let wrpll1_in_use = (mmio.read32(WRPLL_CTL_1) & WRPLL_CTL_ENABLE) != 0;
    let wrpll2_in_use = (mmio.read32(WRPLL_CTL_2) & WRPLL_CTL_ENABLE) != 0;
    match (wrpll1_in_use, wrpll2_in_use) {
        (false, _) => (WRPLL_CTL_1, PORT_CLK_SEL_WRPLL1),
        (true, false) => (WRPLL_CTL_2, PORT_CLK_SEL_WRPLL2),
        (true, true) => match pipe {
            Pipe::B => (WRPLL_CTL_2, PORT_CLK_SEL_WRPLL2),
            _ => (WRPLL_CTL_1, PORT_CLK_SEL_WRPLL1),
        },
    }
}

/// Execute a full Haswell modeset for one output (`eDP`, `DP`, or `HDMI/DVI`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn init_output<M: HaswellMmio, A: HaswellAux>(
    mmio: &mut M,
    aux: &mut A,
    cpu: Cpu,
    port: Port,
    pipe: Pipe,
    mode: Mode,
    surface: SurfaceConfig,
    scaling: ScalingPolicy,
    vbt: Option<&[u8]>,
) -> Result<(), GmaError> {
    if !cpu.is_haswell() {
        return Err(GmaError::UnsupportedPlatform);
    }
    let ddi = DdiPort::from_port(port)?;
    if cpu.is_haswell_lp() && ddi == DdiPort::D {
        return Err(GmaError::UnsupportedPort);
    }
    let cdclk_hz = detect_cdclk_hz(mmio, cpu);
    if u64::from(mode.pixel_clock_khz) * 1_000 > cdclk_hz {
        return Err(GmaError::ModeUnavailable);
    }

    let scaled = scaler::requires_scaling(surface, mode);
    if need_pdw(port, pipe, scaled) {
        pdw_on(mmio);
    }

    init_ddi_buf_trans(mmio, cpu, ddi)?;

    let is_dp = matches!(
        port,
        Port::Edp | Port::DpA | Port::DpB | Port::DpC | Port::DpD
    );
    let is_hdmi = matches!(port, Port::HdmiA | Port::HdmiB | Port::HdmiC);
    if !is_dp && !is_hdmi {
        return Err(GmaError::UnsupportedPort);
    }

    // 1. Pre_On: clock & link bring-up.
    let trained_link = if is_dp {
        if port == Port::Edp {
            setup_edp_panel_sequencer(mmio, cpu);
            panel_power_on(mmio)?;
        }
        Some(train_haswell_dp(mmio, aux, cpu, port, ddi, mode)?)
    } else {
        let wrpll = calculate_hsw_wrpll(u64::from(mode.pixel_clock_khz) * 1_000)?;
        let (wrpll_reg, port_clk_sel_val) = allocate_wrpll(mmio, pipe);
        mmio.write32(wrpll_reg, wrpll.encode_ctl());
        mmio.posting_read(wrpll_reg);
        mmio.delay_us(20);
        let (_, _, _, port_clk_sel_reg) = ddi_regs_offsets(ddi);
        mmio.write32(port_clk_sel_reg, port_clk_sel_val);
        None
    };

    // 2. Pipe / transcoder / primary plane / panel fitter setup.
    mmio.write32(CPU_VGACNTRL, VGA_DISPLAY_DISABLE);
    let base = pipe_base(pipe);
    mmio.write32(base + 0x80, 0); // CUR_CTL
    mmio.write32(base + 0xa0, 0); // CUR_FBC_CTL
    mmio.write32(wm_linetime_reg(pipe), wm_linetime(mode));

    let trans = Transcoder::for_output(pipe, ddi);
    let tregs = trans.regs();
    if let Some(clk_sel_reg) = tregs.clk_sel {
        mmio.write32(clk_sel_reg, trans_clk_sel_for_ddi(ddi));
    }

    if let Some(link) = trained_link {
        let mn = link.m_n(mode, 8);
        mmio.write32(tregs.data_m1, TRANS_DATA_M_TU_64 | mn.data_m);
        mmio.write32(tregs.data_n1, mn.data_n);
        mmio.write32(tregs.link_m1, mn.link_m);
        mmio.write32(tregs.link_n1, mn.link_n);
        mmio.write32(
            tregs.msa_misc,
            TRANS_MSA_MISC_SYNC_CLK | TRANS_MSA_MISC_8BPC,
        );
    }

    let pipe_cfg = PipeConfig::new(pipe, mode);
    mmio.write32(tregs.htotal, pipe_cfg.htotal());
    mmio.write32(tregs.hblank, pipe_cfg.hblank());
    mmio.write32(tregs.hsync, pipe_cfg.hsync());
    mmio.write32(tregs.vtotal, pipe_cfg.vtotal());
    mmio.write32(tregs.vblank, pipe_cfg.vblank());
    mmio.write32(tregs.vsync, pipe_cfg.vsync());
    mmio.write32(pipesrc_reg(pipe), PipeConfig::surface_source(surface)?);

    let pfit = IronlakePfitRegs::for_pipe(pipe);
    if scaled {
        let enc = scaler::encode_ironlake_pfit(
            pipe,
            surface.width,
            surface.height,
            mode.hdisplay,
            mode.vdisplay,
            scaling,
            true,
        )?;
        mmio.write32(pfit.control, enc.control);
        mmio.write32(pfit.win_pos, enc.win_pos);
        mmio.write32(pfit.win_size, enc.win_size);
    } else {
        mmio.update32(pfit.control, 1 << 31, 0);
        mmio.write32(pfit.win_size, 0);
    }

    let plane = PlaneConfig::new(
        primary_for_pipe(pipe),
        pipe,
        PlaneAddressModel::Surface,
        surface,
    );
    let plane_fmt = DSPCNTR_FORMAT_BGRX8888 | plane.legacy_tiling_bits();
    mmio.write32(base + 0x180, plane_fmt); // DSPCNTR (disabled while configuring)
    mmio.write32(base + 0x188, plane.stride_bytes()?); // DSPSTRIDE
    mmio.write32(base + 0x184, 0); // DSPLINOFF
    mmio.write32(base + 0x1a4, plane.tile_offset()); // DSPTILEOFF
    mmio.write32(base + 0x19c, surface.plane_surface_offset()); // DSPSURF
    mmio.write32(base + 0x180, DSPCNTR_ENABLE | plane_fmt);
    mmio.write32(base + 0x19c, surface.plane_surface_offset());

    // Configure & enable TRANS_DDI_FUNC_CTL, then TRANS_CONF (no BPC in PIPECONF).
    let ddi_select = match ddi {
        DdiPort::A => 0,
        DdiPort::B => DDI_FUNC_CTL_DDI_SELECT_B,
        DdiPort::C => DDI_FUNC_CTL_DDI_SELECT_C,
        DdiPort::D => DDI_FUNC_CTL_DDI_SELECT_D,
        DdiPort::E => return Err(GmaError::UnsupportedPort),
    };
    let mode_select = if is_dp {
        DDI_FUNC_CTL_MODE_DP_SST
    } else {
        DDI_FUNC_CTL_MODE_DVI
    };
    let sync_bits = (if mode.flags.contains(ModeFlags::PVSYNC) {
        DDI_FUNC_CTL_VSYNC_HIGH
    } else {
        0
    }) | (if mode.flags.contains(ModeFlags::PHSYNC) {
        DDI_FUNC_CTL_HSYNC_HIGH
    } else {
        0
    });
    let edp_sel = if trans == Transcoder::Edp {
        edp_select_bits(pipe, scaled)
    } else {
        0
    };
    let port_width = if let Some(link) = trained_link {
        u32::from(link.lane_count.saturating_sub(1)) << 1
    } else {
        0
    };
    let ddi_func = ddi_select | mode_select | DDI_FUNC_CTL_BPC_8 | sync_bits | edp_sel | port_width;
    mmio.write32(tregs.ddi_func_ctl, ddi_func);
    mmio.write32(tregs.ddi_func_ctl, ddi_func | DDI_FUNC_CTL_ENABLE);

    mmio.write32(tregs.conf, TRANS_CONF_ENABLE);
    mmio.posting_read(tregs.conf);

    // 3. Post_On: late eDP NORMAL transition, HDMI buffer enable, eDP backlight.
    if ddi == DdiPort::A {
        let (_, dp_tp_ctl_reg, _, _) = ddi_regs_offsets(DdiPort::A);
        mmio.update32(
            dp_tp_ctl_reg,
            DP_TP_CTL_LINK_TRAIN_MASK,
            DP_TP_CTL_LINK_TRAIN_NORMAL,
        );
    }
    if is_hdmi {
        let (buf_ctl_reg, _, _, _) = ddi_regs_offsets(ddi);
        let cur = mmio.read32(buf_ctl_reg);
        mmio.write32(buf_ctl_reg, ddi_buf_ctl_hdmi_rmw(cur, true));
        mmio.posting_read(buf_ctl_reg);
        mmio.delay_us(600);
    }
    if port == Port::Edp {
        backlight_on(mmio, cpu, vbt);
    }
    Ok(())
}

/// Disable one Haswell output (plane, scaler, transcoder, DDI port, and any
/// unused WRPLL / panel power).
pub(crate) fn disable_output_on<M: HaswellMmio>(
    mmio: &mut M,
    cpu: Cpu,
    pipe: Pipe,
    port: Port,
) -> Result<(), GmaError> {
    let ddi = DdiPort::from_port(port)?;
    if port == Port::Edp {
        panel_off(mmio);
    }
    disable_pipe_planes_and_pfit(mmio, pipe);
    disable_pipe_transcoders(mmio, pipe);
    digital_off(mmio, ddi);

    // Disable WRPLLs that are no longer referenced by any active DDI port.
    if matches!(port, Port::HdmiA | Port::HdmiB | Port::HdmiC) {
        let ddis: &[DdiPort] = if cpu.is_haswell_lp() {
            &[DdiPort::A, DdiPort::B, DdiPort::C]
        } else {
            &[DdiPort::A, DdiPort::B, DdiPort::C, DdiPort::D]
        };
        let mut wrpll1_used = false;
        let mut wrpll2_used = false;
        for &other in ddis {
            let (_, _, _, clk_sel_reg) = ddi_regs_offsets(other);
            match mmio.read32(clk_sel_reg) & PORT_CLK_SEL_MASK {
                PORT_CLK_SEL_WRPLL1 => wrpll1_used = true,
                PORT_CLK_SEL_WRPLL2 => wrpll2_used = true,
                _ => {}
            }
        }
        if !wrpll1_used {
            mmio.update32(WRPLL_CTL_1, WRPLL_CTL_ENABLE, 0);
        }
        if !wrpll2_used {
            mmio.update32(WRPLL_CTL_2, WRPLL_CTL_ENABLE, 0);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Existing DDI planning helpers (preserved for Haswell/Broadwell planning)
// ---------------------------------------------------------------------------

fn selected_port(ctx: &crate::gma::GmaContext<'_>) -> Result<Port, GmaError> {
    crate::gma::selected_enabled_port(ctx.config.outputs)
}

fn ddi_pipe_for_port(port: Port) -> Pipe {
    match port {
        Port::Edp | Port::DpA | Port::HdmiA => Pipe::A,
        Port::DpB | Port::HdmiB => Pipe::B,
        _ => Pipe::C,
    }
}

fn map_gtt(ctx: &crate::gma::GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, ctx.config.cpu, &ctx.surface)?;
    gtt::flush_gfx_for_cpu(&ctx.mmio(), ctx.config.cpu);
    Ok(())
}

fn execute_hsw_modeset_plan(mmio: &mut Mmio, plan: HswDdiModesetPlan) -> Result<(), GmaError> {
    match plan.protocol {
        HswDdiProtocolPlan::Hdmi(hdmi) => {
            program_hsw_pll(mmio, hdmi.routed.pll);
            apply_routing(mmio, hdmi.routed.routing);
            write_ddi_port(
                mmio,
                hdmi.routed.port.regs.buf_ctl,
                hdmi.routed.port.buf_ctl,
                None,
            );
        }
        HswDdiProtocolPlan::Dp(dp) => {
            program_hsw_pll(mmio, dp.routed.pll);
            apply_routing(mmio, dp.routed.routing);
            write_ddi_port(
                mmio,
                dp.routed.port.regs.buf_ctl,
                dp.routed.port.buf_ctl,
                dp.routed.port.dp_tp_ctl,
            );
        }
    }
    Ok(())
}

fn write_ddi_port(mmio: &Mmio, buf_ctl_register: usize, buf_ctl: u32, dp_tp_ctl: Option<u32>) {
    // SAFETY: DDI plans only carry `buf_ctl` offsets returned by
    // `DdiRegisters::for_port`, whose local block layout matches `DdiPortRegs`.
    let regs = unsafe { mmio.reg_block::<DdiPortRegs>(buf_ctl_register) };
    regs.buf_ctl.set(buf_ctl);
    if let Some(value) = dp_tp_ctl {
        regs.dp_tp_ctl.set(value);
    }
}

fn program_hsw_pll(mmio: &Mmio, pll: HswDdiPllPlan) {
    if let HswDdiPllPlan::Wrpll { pll, plan } = pll {
        // SAFETY: `HSW_WRPLL_BASE` is the WRPLL0 register, and `HswWrpllRegs`
        // models the WRPLL0/WRPLL1 spacing used by HSW/BDW.
        let regs = unsafe { mmio.reg_block::<HswWrpllRegs>(HSW_WRPLL_BASE) };
        let value = HSW_WRPLL_CTL_REG::RAW.val(plan.encode_ctl());
        match pll {
            HswPllSelect::Wrpll0 => {
                regs.wrpll0.write(value);
                let _ = regs.wrpll0.get();
            }
            HswPllSelect::Wrpll1 => {
                regs.wrpll1.write(value);
                let _ = regs.wrpll1.get();
            }
            _ => {}
        }
        crate::gma::mmio::delay_us(20);
    }
}

fn apply_routing(mmio: &Mmio, routing: DdiClockRouting) {
    let op = match routing {
        DdiClockRouting::PortClkSel { op } | DdiClockRouting::DpllCtrl2 { op } => op,
    };
    if let Some(reg) = port_clock_select_reg(mmio, op.register) {
        let value = (reg.get() & !op.mask_unset) | op.mask_set;
        reg.set(value);
        return;
    }
    // SAFETY: DDI routing operations carry MMIO offsets from typed DDI planning
    // helpers. This fallback covers the shared DPLL_CTRL2-style route register.
    let reg = unsafe {
        mmio.reg_block::<fstart_core::mmio::MmioReadWrite<u32, SKL_DPLL_CTRL2_REG::Register>>(
            op.register,
        )
    };
    reg.set((reg.get() & !op.mask_unset) | op.mask_set);
}

fn port_clock_select_reg(
    mmio: &Mmio,
    register: usize,
) -> Option<
    &'static fstart_core::mmio::MmioReadWrite<u32, crate::gma::ddi::PORT_CLK_SEL_REG::Register>,
> {
    if register < HSW_PORT_CLK_SEL_BASE {
        return None;
    }
    let offset = register - HSW_PORT_CLK_SEL_BASE;
    // SAFETY: `HSW_PORT_CLK_SEL_BASE` is DDI A's port-clock select register;
    // the following fields cover the contiguous DDI A-E selector registers.
    let regs = unsafe { mmio.reg_block::<HswPortClockSelectRegs>(HSW_PORT_CLK_SEL_BASE) };
    match offset {
        0x00 => Some(&regs.ddi_a),
        0x04 => Some(&regs.ddi_b),
        0x08 => Some(&regs.ddi_c),
        0x0c => Some(&regs.ddi_d),
        0x10 => Some(&regs.ddi_e),
        _ => None,
    }
}

/// Display power and clock preparation stage for HSW/BDW DDI modesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswPowerClockPlan {
    /// CPU family being planned.
    pub cpu: Cpu,
    /// Logical port that requires power wells and display clocks.
    pub port: Port,
    /// Whether the port uses AUX/DDI DP-style power sequencing.
    pub uses_aux_power: bool,
    /// Whether this path must keep PCH split resources available.
    pub requires_pch_power: bool,
}

/// HSW/BDW output protocol planned for one DDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HswDdiProtocolPlan {
    /// HDMI/DVI DDI sequence.
    Hdmi(HswHdmiInitSequencePlan),
    /// DisplayPort/eDP DDI sequence.
    Dp(HswDpInitSequencePlan),
}

/// Ordered generation-level steps surrounding the shared DDI sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HswDdiModesetStep {
    /// Enable required display power wells and clocks.
    PreparePowerAndClocks,
    /// Execute the shared DDI protocol sequence.
    ProgramDdiProtocol,
    /// Commit the pipe/transcoder/port state.
    CommitDisplay,
}

/// Complete HSW/BDW DDI modeset plan for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswDdiModesetPlan {
    /// Power/clock preparation requirements.
    pub power: HswPowerClockPlan,
    /// Protocol-specific DDI sequence.
    pub protocol: HswDdiProtocolPlan,
    /// Generation-level ordering around the shared protocol sequence.
    pub steps: [HswDdiModesetStep; 3],
}

impl HswDdiModesetPlan {
    /// Return the pipe selected by the protocol sequence.
    pub const fn pipe(self) -> Pipe {
        match self.protocol {
            HswDdiProtocolPlan::Hdmi(plan) => plan.pipe.pipe,
            HswDdiProtocolPlan::Dp(plan) => plan.pipe.pipe,
        }
    }

    /// Return whether the protocol sequence includes DP AUX link training.
    pub const fn trains_dp_link(self) -> bool {
        match self.protocol {
            HswDdiProtocolPlan::Hdmi(_) => false,
            HswDdiProtocolPlan::Dp(plan) => has_dp_training_step(plan.steps),
        }
    }
}

/// Parameters for a Haswell/Broadwell HDMI/DVI DDI modeset plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswHdmiModesetParams {
    pub cpu: Cpu,
    pub port: Port,
    pub pipe: Pipe,
    pub mode: Mode,
    pub pll: HswPllSelect,
    pub per_ddi_clock_sel: bool,
    pub hdmi_translation: u8,
}

/// Build a Haswell/Broadwell HDMI/DVI DDI modeset plan.
pub(crate) fn hdmi_modeset_plan(
    params: HswHdmiModesetParams,
) -> Result<HswDdiModesetPlan, GmaError> {
    let HswHdmiModesetParams {
        cpu,
        port,
        pipe,
        mode,
        pll,
        per_ddi_clock_sel,
        hdmi_translation,
    } = params;
    let protocol = hsw_hdmi_init_sequence_plan(HswHdmiInitParams {
        cpu,
        port,
        pipe,
        dotclock_hz: mode.pixel_clock_khz as u64 * 1_000,
        pll,
        per_ddi_clock_sel,
        hdmi_translation,
    })?;
    Ok(HswDdiModesetPlan {
        power: power_clock_plan(cpu, port)?,
        protocol: HswDdiProtocolPlan::Hdmi(protocol),
        steps: modeset_steps(),
    })
}

/// Build a Haswell/Broadwell DP/eDP DDI modeset plan.
pub(crate) fn dp_modeset_plan(params: HswDpInitParams) -> Result<HswDdiModesetPlan, GmaError> {
    let protocol = hsw_dp_init_sequence_plan(params)?;
    let HswDpInitParams { cpu, port, .. } = params;
    Ok(HswDdiModesetPlan {
        power: power_clock_plan(cpu, port)?,
        protocol: HswDdiProtocolPlan::Dp(protocol),
        steps: modeset_steps(),
    })
}

const fn power_clock_plan(cpu: Cpu, port: Port) -> Result<HswPowerClockPlan, GmaError> {
    match cpu {
        Cpu::Haswell | Cpu::HaswellUlt | Cpu::HaswellUlx | Cpu::Broadwell => {
            Ok(HswPowerClockPlan {
                cpu,
                port,
                uses_aux_power: matches!(
                    port,
                    Port::Edp | Port::DpA | Port::DpB | Port::DpC | Port::DpD
                ),
                requires_pch_power: matches!(
                    port,
                    Port::HdmiA | Port::HdmiB | Port::HdmiC | Port::DpB | Port::DpC | Port::DpD
                ),
            })
        }
        _ => Err(GmaError::UnsupportedPlatform),
    }
}

const fn modeset_steps() -> [HswDdiModesetStep; 3] {
    [
        HswDdiModesetStep::PreparePowerAndClocks,
        HswDdiModesetStep::ProgramDdiProtocol,
        HswDdiModesetStep::CommitDisplay,
    ]
}

const fn has_dp_training_step(
    steps: [DdiDpInitStep; crate::gma::ddi::DDI_DP_INIT_STEP_COUNT],
) -> bool {
    matches!(steps[6], DdiDpInitStep::TrainDpLink)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::collections::BTreeMap;
    use std::vec::Vec;

    use super::*;
    use crate::gma::ddi::{
        DdiClockRouting, DdiLaneCount, DdiPort, DdiRegisterOp, DpTrainingPattern, HswDdiPllPlan,
        HswHdmiInitStep, HswWrpllPlan,
    };
    use crate::gma::dp_aux::{AuxRegs, haswell_aux_clock_div, haswell_aux_ctl_start_value};
    use crate::gma::framebuffer::PixelFormat;
    use crate::gma::panel::{BLC_PWM_PCH_CTL2, LPT_PWM_GRANULARITY, SOUTH_CHICKEN2};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MmioOp {
        Write(usize, u32),
        DelayUs(u32),
    }

    struct MockMmio {
        regs: BTreeMap<usize, u32>,
        ops: Vec<MmioOp>,
        auto_power_well: bool,
        auto_panel_status: bool,
    }

    impl MockMmio {
        fn new() -> Self {
            Self {
                regs: BTreeMap::new(),
                ops: Vec::new(),
                auto_power_well: true,
                auto_panel_status: true,
            }
        }

        fn set(&mut self, offset: usize, value: u32) {
            self.regs.insert(offset, value);
        }

        fn get(&self, offset: usize) -> u32 {
            self.regs.get(&offset).copied().unwrap_or(0)
        }

        fn write_index(&self, offset: usize, pred: impl Fn(u32) -> bool) -> Option<usize> {
            self.ops.iter().position(|op| match *op {
                MmioOp::Write(reg, val) => reg == offset && pred(val),
                MmioOp::DelayUs(_) => false,
            })
        }
    }

    impl HaswellMmio for MockMmio {
        fn read32(&self, offset: usize) -> u32 {
            self.get(offset)
        }

        fn write32(&mut self, offset: usize, value: u32) {
            self.ops.push(MmioOp::Write(offset, value));
            self.regs.insert(offset, value);
            if self.auto_power_well && offset == PWR_WELL_CTL_DRIVER {
                let state = if (value & PWR_WELL_CTL_ENABLE_REQUEST) != 0 {
                    PWR_WELL_CTL_STATE_ENABLED
                } else {
                    0
                };
                self.regs.insert(offset, value | state);
            }
            if self.auto_panel_status && offset == PanelPowerRegs::PCH.control {
                let on = if (value & PP_CONTROL::TARGET_ON::SET.value) != 0 {
                    PP_STATUS::ON::SET.value
                } else {
                    0
                };
                self.regs.insert(PanelPowerRegs::PCH.status, on);
            }
            if offset == Transcoder::A.regs().conf
                || offset == Transcoder::B.regs().conf
                || offset == Transcoder::C.regs().conf
                || offset == Transcoder::Edp.regs().conf
            {
                let status = if (value & TRANS_CONF_ENABLE) != 0 {
                    TRANS_CONF_ENABLED_STATUS
                } else {
                    0
                };
                self.regs.insert(offset, value | status);
            }
            if offset == DDI_BUF_CTL_A
                || offset == 0x64100
                || offset == 0x64200
                || offset == 0x64300
            {
                let idle = if (value & DDI_BUF_CTL_ENABLE) == 0 {
                    DDI_BUF_CTL_IDLE_STATUS
                } else {
                    0
                };
                self.regs
                    .insert(offset, (value & !DDI_BUF_CTL_IDLE_STATUS) | idle);
            }
            if offset == SRD_CTL && (value & SRD_CTL_ENABLE) == 0 {
                self.regs.insert(SRD_STATUS, 0);
            }
            if (offset == 0x64140 || offset == 0x64240 || offset == 0x64340)
                && (value & DP_TP_CTL_LINK_TRAIN_MASK) == DP_TP_CTL_LINK_TRAIN_IDLE
            {
                self.regs.insert(offset + 4, DP_TP_STATUS_MIN_IDLES_SENT);
            }
        }

        fn delay_us(&mut self, us: u32) {
            self.ops.push(MmioOp::DelayUs(us));
        }

        fn poll_mask(&mut self, offset: usize, mask: u32, expected: u32, _timeout_us: u32) -> bool {
            (self.read32(offset) & mask) == expected
        }
    }

    struct MockAux {
        caps: [u8; RECEIVER_CAPS_LEN],
        cr_fail_steps: u8,
        eq_fail_steps: u8,
        requested_vs: u8,
        requested_pe: u8,
        writes: Vec<(Port, u32, Vec<u8>)>,
    }

    impl MockAux {
        fn new(max_rate_code: u8, max_lanes: u8, tps3: bool) -> Self {
            let mut caps = [0u8; RECEIVER_CAPS_LEN];
            caps[0] = 0x12; // DP 1.2
            caps[1] = max_rate_code;
            caps[2] = max_lanes | 0x80 | if tps3 { 0x40 } else { 0 };
            Self {
                caps,
                cr_fail_steps: 0,
                eq_fail_steps: 0,
                requested_vs: 0,
                requested_pe: 0,
                writes: Vec::new(),
            }
        }
    }

    impl HaswellAux for MockAux {
        fn aux_read(&mut self, _port: Port, address: u32, out: &mut [u8]) -> Result<(), GmaError> {
            match address {
                DPCD_RECEIVER_CAPS => {
                    out.copy_from_slice(&self.caps[..out.len()]);
                    Ok(())
                }
                DPCD_LINK_STATUS => {
                    let mut st = [0u8; LINK_STATUS_LEN];
                    if self.cr_fail_steps > 0 {
                        self.cr_fail_steps -= 1;
                        let adj = (self.requested_vs & 3) | ((self.requested_pe & 3) << 2);
                        st[4] = adj | (adj << 4);
                        st[5] = adj | (adj << 4);
                    } else if self.eq_fail_steps > 0 {
                        self.eq_fail_steps -= 1;
                        // CR done on all 4 lanes, EQ not yet done.
                        st[0] = 0x11;
                        st[1] = 0x11;
                        let adj = (self.requested_vs & 3) | ((self.requested_pe & 3) << 2);
                        st[4] = adj | (adj << 4);
                        st[5] = adj | (adj << 4);
                    } else {
                        // CR + Channel EQ + Symbol lock + Interlane align done.
                        st[0] = 0x77;
                        st[1] = 0x77;
                        st[2] = 0x01;
                    }
                    out.copy_from_slice(&st[..out.len()]);
                    Ok(())
                }
                _ => Err(GmaError::HardwareError),
            }
        }

        fn aux_write(&mut self, port: Port, address: u32, data: &[u8]) -> Result<(), GmaError> {
            self.writes.push((port, address, data.to_vec()));
            Ok(())
        }
    }

    fn mode_1080p60() -> Mode {
        Mode {
            hdisplay: 1920,
            hsync_start: 2008,
            hsync_end: 2052,
            htotal: 2200,
            vdisplay: 1080,
            vsync_start: 1084,
            vsync_end: 1089,
            vtotal: 1125,
            pixel_clock_khz: 148_500,
            flags: ModeFlags::PHSYNC | ModeFlags::PVSYNC,
            bpc: None,
        }
    }

    // -----------------------------------------------------------------------
    // 1. PCI ID & CPU variant detection + caps
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_pci_id_mapping_distinguishes_trad_ult_ulx() {
        assert_eq!(Cpu::from_haswell_pci_id(0x0412), Some(Cpu::Haswell));
        assert_eq!(Cpu::from_haswell_pci_id(0x0416), Some(Cpu::Haswell));
        assert_eq!(Cpu::from_haswell_pci_id(0x0d26), Some(Cpu::Haswell));
        assert_eq!(Cpu::from_haswell_pci_id(0x0a06), Some(Cpu::HaswellUlt));
        assert_eq!(Cpu::from_haswell_pci_id(0x0a16), Some(Cpu::HaswellUlt));
        assert_eq!(Cpu::from_haswell_pci_id(0x0a26), Some(Cpu::HaswellUlt));
        assert_eq!(Cpu::from_haswell_pci_id(0x0a0e), Some(Cpu::HaswellUlx));
        assert_eq!(Cpu::from_haswell_pci_id(0x0a1e), Some(Cpu::HaswellUlx));
        assert_eq!(Cpu::from_haswell_pci_id(0x1616), None);
        assert!(Cpu::Haswell.is_haswell() && !Cpu::Haswell.is_haswell_lp());
        assert!(Cpu::HaswellUlt.is_haswell() && Cpu::HaswellUlt.is_haswell_lp());
        assert!(Cpu::HaswellUlx.is_haswell() && Cpu::HaswellUlx.is_haswell_lp());
    }

    // -----------------------------------------------------------------------
    // 2. CDCLK, RAWCLK, AUX clock divider, and port detection
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_cdclk_rawclk_and_aux_dividers_match_libgfxinit() {
        // Default selector (0 << 26) -> 450 MHz on all Haswell variants.
        assert_eq!(decode_cdclk_hz(Cpu::Haswell, 0), 450_000_000);
        assert_eq!(decode_cdclk_hz(Cpu::HaswellUlt, 0), 450_000_000);
        assert_eq!(decode_cdclk_hz(Cpu::HaswellUlx, 0), 450_000_000);
        // Alternate selector (1 << 26) -> 540 MHz Trad, 450 MHz ULT, 337.5 MHz ULX.
        assert_eq!(
            decode_cdclk_hz(Cpu::Haswell, LCPLL_CTL_CD_FREQ_SEL_HSW_ALT),
            540_000_000
        );
        assert_eq!(
            decode_cdclk_hz(Cpu::HaswellUlt, LCPLL_CTL_CD_FREQ_SEL_HSW_ALT),
            450_000_000
        );
        assert_eq!(
            decode_cdclk_hz(Cpu::HaswellUlx, LCPLL_CTL_CD_FREQ_SEL_HSW_ALT),
            337_500_000
        );
        assert_eq!(rawclk_hz(Cpu::Haswell), 125_000_000);
        assert_eq!(rawclk_hz(Cpu::HaswellUlt), 24_000_000);
        assert_eq!(rawclk_hz(Cpu::HaswellUlx), 24_000_000);

        // Haswell AUX register offsets: eDP/DP-A on CPU (0x64010), DP-B/C/D on PCH (0xe4110/210/310).
        assert_eq!(AuxRegs::haswell_for_port(Port::Edp).unwrap().ctl, 0x64010);
        assert_eq!(AuxRegs::haswell_for_port(Port::DpB).unwrap().ctl, 0xe4110);
        assert_eq!(AuxRegs::haswell_for_port(Port::DpC).unwrap().ctl, 0xe4210);
        assert_eq!(AuxRegs::haswell_for_port(Port::DpD).unwrap().ctl, 0xe4310);

        // AUX clock divider: CPU AUX uses CDCLK (450 MHz -> 225, 337.5 MHz -> 169),
        // PCH AUX uses RAWCLK (24 MHz -> 12 on LP, 125 MHz -> 63 on Trad).
        assert_eq!(
            haswell_aux_clock_div(Port::Edp, 450_000_000, 24_000_000),
            Ok(225)
        );
        assert_eq!(
            haswell_aux_clock_div(Port::Edp, 337_500_000, 24_000_000),
            Ok(169)
        );
        assert_eq!(
            haswell_aux_clock_div(Port::DpB, 450_000_000, 24_000_000),
            Ok(12)
        );
        assert_eq!(
            haswell_aux_clock_div(Port::DpB, 540_000_000, 125_000_000),
            Ok(63)
        );
        let ctl = haswell_aux_ctl_start_value(4, 225).unwrap();
        // Timeout = 3 (bits 27:26), precharge = 5 (bits 19:16), clock_div = 225 (bits 10:0).
        assert_eq!((ctl >> 26) & 3, 3);
        assert_eq!((ctl >> 16) & 0xf, 5);
        assert_eq!(ctl & 0x7ff, 225);
    }

    #[test]
    fn haswell_port_detection_masks_ddi_d_and_crt_on_lp() {
        let trad = decode_detected_ports(Cpu::Haswell, 0x11, 0x07);
        assert!(
            trad.edp && trad.ddi_a_4_lanes && trad.ddi_b && trad.ddi_c && trad.ddi_d && trad.crt
        );

        let ult = decode_detected_ports(Cpu::HaswellUlt, 0x11, 0x07);
        assert!(ult.edp && ult.ddi_a_4_lanes && ult.ddi_b && ult.ddi_c);
        assert!(!ult.ddi_d);
        assert!(!ult.crt);
    }

    // -----------------------------------------------------------------------
    // 3. Power Down Well (PDW), PSR, and IPS (ULT/ULX only)
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_need_pdw_matches_libgfxinit() {
        assert!(!need_pdw(Port::Edp, Pipe::A, false));
        assert!(need_pdw(Port::Edp, Pipe::A, true));
        assert!(need_pdw(Port::Edp, Pipe::B, false));
        assert!(need_pdw(Port::HdmiA, Pipe::A, false));
        assert!(need_pdw(Port::DpB, Pipe::A, false));
    }

    #[test]
    fn haswell_pre_all_off_disables_psr_and_lp_ips() {
        // Traditional Haswell: clears PSR, does NOT touch IPS_CTL.
        let mut trad = MockMmio::new();
        trad.set(SRD_CTL, SRD_CTL_ENABLE);
        trad.set(SRD_STATUS, SRD_STATUS_STATE_MASK);
        trad.set(IPS_CTL, IPS_CTL_ENABLE);
        pre_all_off(&mut trad, Cpu::Haswell);
        assert_eq!(trad.get(SRD_CTL) & SRD_CTL_ENABLE, 0);
        assert_eq!(trad.get(IPS_CTL) & IPS_CTL_ENABLE, IPS_CTL_ENABLE);
        assert!(!trad.ops.contains(&MmioOp::DelayUs(20_000)));

        // Haswell-ULT: clears PSR and IPS + 20 ms delay.
        let mut ult = MockMmio::new();
        ult.set(SRD_CTL, SRD_CTL_ENABLE);
        ult.set(SRD_STATUS, SRD_STATUS_STATE_MASK);
        ult.set(IPS_CTL, IPS_CTL_ENABLE);
        pre_all_off(&mut ult, Cpu::HaswellUlt);
        assert_eq!(ult.get(SRD_CTL) & SRD_CTL_ENABLE, 0);
        assert_eq!(ult.get(IPS_CTL) & IPS_CTL_ENABLE, 0);
        assert!(ult.ops.contains(&MmioOp::DelayUs(20_000)));
    }

    #[test]
    fn haswell_pdw_on_and_off_update_power_well_registers() {
        let mut mmio = MockMmio::new();
        pdw_on(&mut mmio);
        assert_ne!(
            mmio.get(PWR_WELL_CTL_DRIVER) & PWR_WELL_CTL_ENABLE_REQUEST,
            0
        );
        assert_ne!(
            mmio.get(PWR_WELL_CTL_DRIVER) & PWR_WELL_CTL_STATE_ENABLED,
            0
        );

        mmio.set(PWR_WELL_CTL_BIOS, PWR_WELL_CTL_ENABLE_REQUEST);
        pdw_off(&mut mmio);
        assert_eq!(mmio.get(PWR_WELL_CTL_BIOS) & PWR_WELL_CTL_ENABLE_REQUEST, 0);
        assert_eq!(
            mmio.get(PWR_WELL_CTL_DRIVER) & PWR_WELL_CTL_ENABLE_REQUEST,
            0
        );
    }

    // -----------------------------------------------------------------------
    // 4. End-to-end eDP modeset: no WP key, DDI_A_4_LANES preserved,
    //    600 us first-enable delay, TPS3, End_EDP_Training_Late, backlight PWM,
    //    and PDW kept OFF for unscaled Pipe A eDP
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_ult_edp_modeset_preserves_4_lanes_and_ends_training_late() {
        let mut mmio = MockMmio::new();
        // Set DDI_A_4_LANES (bit 4) and INIT_DISPLAY_DETECTED (bit 0) from firmware/strap.
        mmio.set(
            DDI_BUF_CTL_A,
            DDI_BUF_CTL_A_4_LANES | DDI_BUF_CTL_INIT_DISPLAY_DETECTED | DDI_BUF_CTL_IDLE_STATUS,
        );
        mmio.set(LCPLL_CTL, 0); // 450 MHz CDCLK

        // Receiver supports HBR2, 2 lanes, TPS3; request VS=1 PE=1 during CR once.
        let mut aux = MockAux::new(0x14, 2, true);
        aux.cr_fail_steps = 1;
        aux.requested_vs = 1;
        aux.requested_pe = 1;

        let mode = mode_1080p60();
        let surface = SurfaceConfig::packed(0xd000_0000, 1920, 1080, PixelFormat::Xrgb8888);

        init_output(
            &mut mmio,
            &mut aux,
            Cpu::HaswellUlt,
            Port::Edp,
            Pipe::A,
            mode,
            surface,
            ScalingPolicy::None,
            None,
        )
        .unwrap();

        // 1. PDW was NOT enabled for unscaled Pipe A eDP.
        assert_eq!(mmio.write_index(PWR_WELL_CTL_DRIVER, |_| true), None);

        // 2. PCH_PP_CONTROL never received the pre-Haswell 0xabcd write-protect key.
        for op in &mmio.ops {
            if let MmioOp::Write(reg, val) = *op
                && reg == PanelPowerRegs::PCH.control
            {
                assert_eq!(val >> 16, 0, "Haswell PP_CONTROL must not write 0xabcd key");
            }
        }

        // 3. DDI_BUF_CTL_A preserved bit 4 (DDI_A_4_LANES) and bit 0 on every write,
        //    and had a 600 us delay after first enable.
        let first_ddi_enable = mmio
            .write_index(DDI_BUF_CTL_A, |v| (v & DDI_BUF_CTL_ENABLE) != 0)
            .expect("DDI_BUF_CTL_A must be enabled");
        assert!(
            matches!(
                mmio.ops.get(first_ddi_enable + 1),
                Some(MmioOp::DelayUs(600))
            ),
            "expected 600 us delay after first DDI_BUF_CTL enable"
        );
        assert_ne!(mmio.get(DDI_BUF_CTL_A) & DDI_BUF_CTL_A_4_LANES, 0);
        assert_ne!(
            mmio.get(DDI_BUF_CTL_A) & DDI_BUF_CTL_INIT_DISPLAY_DETECTED,
            0
        );
        // After CR adjust to VS=1 PE=1, TRANS_SELECT should be 5 (bits 27:24).
        assert_eq!((mmio.get(DDI_BUF_CTL_A) >> 24) & 0xf, 5);

        // 4. End_EDP_Training_Late: DP_TP_CTL_A enters IDLE before PIPE_EDP_CONF enable,
        //    and only enters NORMAL AFTER PIPE_EDP_CONF is enabled!
        let tp_idle = mmio
            .write_index(0x64040, |v| {
                (v & DP_TP_CTL_LINK_TRAIN_MASK) == DP_TP_CTL_LINK_TRAIN_IDLE
            })
            .expect("DP_TP_CTL_A must enter IDLE");
        let pipe_edp_on = mmio
            .write_index(Transcoder::Edp.regs().conf, |v| {
                (v & TRANS_CONF_ENABLE) != 0
            })
            .expect("PIPE_EDP_CONF must be enabled");
        let tp_normal = mmio
            .write_index(0x64040, |v| {
                (v & DP_TP_CTL_LINK_TRAIN_MASK) == DP_TP_CTL_LINK_TRAIN_NORMAL
            })
            .expect("DP_TP_CTL_A must enter NORMAL in Post_On");
        assert!(
            tp_idle < pipe_edp_on,
            "DP_TP_CTL_A IDLE must precede PIPE_EDP_CONF"
        );
        assert!(
            pipe_edp_on < tp_normal,
            "DP_TP_CTL_A NORMAL must follow PIPE_EDP_CONF"
        );

        // 5. TPS3 was used for Channel Equalization (DPCD TRAINING_PATTERN_SET = 0x23).
        assert!(
            aux.writes
                .iter()
                .any(|(_, addr, data)| *addr == DPCD_TRAINING_PATTERN_SET && data[0] == 0x23),
            "Haswell DP training should use TPS3 when sink supports it"
        );

        // 6. PIPE_EDP_DDI_FUNC_CTL uses ALWAYS_ON (0 << 12) for unscaled Pipe A,
        //    DP SST mode, and PIPE_EDP_CONF has NO BPC bits set.
        let func = mmio.get(Transcoder::Edp.regs().ddi_func_ctl);
        assert_eq!(
            func & DDI_FUNC_CTL_EDP_SELECT_MASK,
            DDI_FUNC_CTL_EDP_SELECT_ALWAYS_ON
        );
        assert_eq!(func & (7 << 24), DDI_FUNC_CTL_MODE_DP_SST);
        assert_eq!(mmio.get(Transcoder::Edp.regs().conf) & (7 << 5), 0);

        // 7. Backlight PWM configured on Lynx Point-LP (24 MHz, 200 Hz -> 128-step, period 937,
        //    SOUTH_CHICKEN2 bit 5 cleared, BLC_PWM_PCH_CTL1 enabled, PCH_PP_CONTROL bit 2 set).
        assert_eq!(mmio.get(SOUTH_CHICKEN2) & LPT_PWM_GRANULARITY, 0);
        assert_eq!(mmio.get(BLC_PWM_PCH_CTL2), (937 << 16) | 937);
        assert_eq!(
            mmio.get(BLC_PWM_PCH_CTL1),
            crate::gma::panel::BLM_PCH_OVERRIDE_ENABLE | BLM_PCH_PWM_ENABLE
        );
        assert_ne!(
            mmio.get(PanelPowerRegs::PCH.control) & PP_CONTROL::BACKLIGHT_ENABLE::SET.value,
            0
        );
    }

    #[test]
    fn haswell_ulx_caps_dp_link_rate_to_hbr_2_7_ghz() {
        let mut mmio = MockMmio::new();
        mmio.set(LCPLL_CTL, LCPLL_CTL_CD_FREQ_SEL_HSW_ALT); // 337.5 MHz ULX CDCLK
        // Sink advertises HBR2 (0x14), 4 lanes; ULX must never select HBR2 (0x14 / LCPLL2700).
        let mut aux = MockAux::new(0x14, 4, true);
        let mode = mode_1080p60();
        let surface = SurfaceConfig::packed(0xd000_0000, 1920, 1080, PixelFormat::Xrgb8888);

        init_output(
            &mut mmio,
            &mut aux,
            Cpu::HaswellUlx,
            Port::Edp,
            Pipe::A,
            mode,
            surface,
            ScalingPolicy::None,
            None,
        )
        .unwrap();

        let bw_write = aux
            .writes
            .iter()
            .find(|(_, addr, _)| *addr == DPCD_LINK_BW_SET)
            .expect("LINK_BW_SET write");
        assert_ne!(
            bw_write.2[0], 0x14,
            "Haswell-ULX must cap DP link rate at 2.7 GHz"
        );
        assert_ne!(mmio.get(HSW_PORT_CLK_SEL_BASE), PORT_CLK_SEL_LCPLL2700);
    }

    // -----------------------------------------------------------------------
    // 5. End-to-end HDMI modeset on Haswell (WRPLL + 20 us, PORT_CLK_SEL,
    //    TRANS_CLK_SEL, DDI_FUNC_CTL DVI, PIPECONF, Post_On DDI_BUF_CTL + 600 us)
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_hdmi_modeset_orders_wrpll_transcoder_and_post_on_ddi_buf() {
        let mut mmio = MockMmio::new();
        mmio.set(LCPLL_CTL, 0);
        let mut aux = MockAux::new(0x0a, 4, false);
        let mode = mode_1080p60();
        let surface = SurfaceConfig::packed(0xd000_0000, 1920, 1080, PixelFormat::Xrgb8888);

        init_output(
            &mut mmio,
            &mut aux,
            Cpu::Haswell,
            Port::HdmiA, // DDI-B
            Pipe::B,
            mode,
            surface,
            ScalingPolicy::None,
            None,
        )
        .unwrap();

        // PDW enabled for non-eDP HDMI output.
        assert_ne!(
            mmio.get(PWR_WELL_CTL_DRIVER) & PWR_WELL_CTL_ENABLE_REQUEST,
            0
        );

        let wrpll_idx = mmio
            .write_index(WRPLL_CTL_1, |v| (v & WRPLL_CTL_ENABLE) != 0)
            .expect("WRPLL_CTL_1 must be enabled");
        assert!(matches!(
            mmio.ops.get(wrpll_idx + 1),
            Some(MmioOp::DelayUs(20))
        ));

        let port_clk_idx = mmio
            .write_index(HSW_PORT_CLK_SEL_BASE + 4, |v| v == PORT_CLK_SEL_WRPLL1)
            .expect("PORT_CLK_SEL_DDIB must select WRPLL1");
        let trans_clk_idx = mmio
            .write_index(TRANSB_CLK_SEL, |v| v == TRANS_CLK_SEL_DDIB)
            .expect("TRANSB_CLK_SEL must select DDI-B");
        let pipeconf_idx = mmio
            .write_index(Transcoder::B.regs().conf, |v| (v & TRANS_CONF_ENABLE) != 0)
            .expect("PIPEBCONF must be enabled");
        let buf_ctl_idx = mmio
            .write_index(0x64100, |v| (v & DDI_BUF_CTL_ENABLE) != 0)
            .expect("DDI_BUF_CTL_B must be enabled in Post_On");
        assert!(matches!(
            mmio.ops.get(buf_ctl_idx + 1),
            Some(MmioOp::DelayUs(600))
        ));

        assert!(wrpll_idx < port_clk_idx);
        assert!(port_clk_idx < trans_clk_idx);
        assert!(trans_clk_idx < pipeconf_idx);
        assert!(pipeconf_idx < buf_ctl_idx);

        // PIPEB_DDI_FUNC_CTL selects DDI-B, DVI mode, 8 bpc, positive H/V sync, enabled.
        let func = mmio.get(Transcoder::B.regs().ddi_func_ctl);
        assert_eq!(
            func,
            DDI_FUNC_CTL_ENABLE
                | DDI_FUNC_CTL_DDI_SELECT_B
                | DDI_FUNC_CTL_MODE_DVI
                | DDI_FUNC_CTL_BPC_8
                | DDI_FUNC_CTL_HSYNC_HIGH
                | DDI_FUNC_CTL_VSYNC_HIGH
        );
        // WM_LINETIME_B programmed for 1080p60: round(2200 * 8000 / 148500) = 119.
        assert_eq!(mmio.get(WM_LINETIME_B), 119);

        // Disable output tears down plane, pipe, DDI-B, and unreferenced WRPLL1.
        disable_output_on(&mut mmio, Cpu::Haswell, Pipe::B, Port::HdmiA).unwrap();
        assert_eq!(mmio.get(0x71180) & DSPCNTR_ENABLE, 0);
        assert_eq!(mmio.get(Transcoder::B.regs().conf) & TRANS_CONF_ENABLE, 0);
        assert_eq!(mmio.get(0x64100) & DDI_BUF_CTL_ENABLE, 0);
        assert_eq!(mmio.get(HSW_PORT_CLK_SEL_BASE + 4), PORT_CLK_SEL_NONE);
        assert_eq!(mmio.get(WRPLL_CTL_1) & WRPLL_CTL_ENABLE, 0);
    }

    #[test]
    fn haswell_lp_rejects_ddi_d_outputs() {
        let mut mmio = MockMmio::new();
        let mut aux = MockAux::new(0x0a, 4, false);
        let surface = SurfaceConfig::packed(0xd000_0000, 1024, 768, PixelFormat::Xrgb8888);
        assert_eq!(
            init_output(
                &mut mmio,
                &mut aux,
                Cpu::HaswellUlt,
                Port::HdmiC,
                Pipe::C,
                Mode::XGA_1024X768_60,
                surface,
                ScalingPolicy::None,
                None,
            ),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn haswell_clean_state_disables_all_and_loads_ddi_buf_trans() {
        let mut mmio = MockMmio::new();
        mmio.set(SRD_CTL, SRD_CTL_ENABLE);
        mmio.set(IPS_CTL, IPS_CTL_ENABLE);
        mmio.set(DDI_BUF_CTL_A, DDI_BUF_CTL_ENABLE | DDI_BUF_CTL_A_4_LANES);
        mmio.set(WRPLL_CTL_1, WRPLL_CTL_ENABLE);
        mmio.set(SPLL_CTL, SPLL_CTL_ENABLE);
        mmio.set(
            PWR_WELL_CTL_DRIVER,
            PWR_WELL_CTL_ENABLE_REQUEST | PWR_WELL_CTL_STATE_ENABLED,
        );

        clean_state(&mut mmio, Cpu::HaswellUlt);

        assert_eq!(mmio.get(SRD_CTL) & SRD_CTL_ENABLE, 0);
        assert_eq!(mmio.get(IPS_CTL) & IPS_CTL_ENABLE, 0);
        assert_eq!(mmio.get(CPU_VGACNTRL), VGA_DISPLAY_DISABLE);
        // DDI-A disabled but bit 4 (DDI_A_4_LANES) preserved.
        assert_eq!(mmio.get(DDI_BUF_CTL_A) & DDI_BUF_CTL_ENABLE, 0);
        assert_ne!(mmio.get(DDI_BUF_CTL_A) & DDI_BUF_CTL_A_4_LANES, 0);
        assert_eq!(mmio.get(WRPLL_CTL_1) & WRPLL_CTL_ENABLE, 0);
        assert_eq!(mmio.get(SPLL_CTL) & SPLL_CTL_ENABLE, 0);
        assert_eq!(
            mmio.get(PWR_WELL_CTL_DRIVER) & PWR_WELL_CTL_ENABLE_REQUEST,
            0
        );
        assert_eq!(mmio.get(PCH_RAWCLK_FREQ), 24);
        assert_ne!(mmio.get(NDE_RSTWRN_OPT) & RST_PCH_HANDSHAKE_EN, 0);
        // DDI_BUF_TRANS_A entry 0 loaded (0x00ff_ffff, 0x0006_000e).
        assert_eq!(mmio.get(DdiPort::A.buf_trans_base()), 0x00ff_ffff);
        assert_eq!(mmio.get(DdiPort::A.buf_trans_base() + 4), 0x0006_000e);
    }

    #[test]
    fn haswell_external_dp_enters_normal_before_pipeconf_unlike_edp() {
        let mut mmio = MockMmio::new();
        mmio.set(LCPLL_CTL, 0);
        let mut aux = MockAux::new(0x14, 4, true);
        let mode = mode_1080p60();
        let surface = SurfaceConfig::packed(0xd000_0000, 1920, 1080, PixelFormat::Xrgb8888);

        init_output(
            &mut mmio,
            &mut aux,
            Cpu::Haswell,
            Port::DpB,
            Pipe::B,
            mode,
            surface,
            ScalingPolicy::None,
            None,
        )
        .unwrap();

        let tp_normal = mmio
            .write_index(0x64140, |v| {
                (v & DP_TP_CTL_LINK_TRAIN_MASK) == DP_TP_CTL_LINK_TRAIN_NORMAL
            })
            .expect("DP_TP_CTL_B must enter NORMAL");
        let pipe_b_on = mmio
            .write_index(Transcoder::B.regs().conf, |v| (v & TRANS_CONF_ENABLE) != 0)
            .expect("PIPEBCONF must be enabled");
        assert!(
            tp_normal < pipe_b_on,
            "external DP-B must enter NORMAL before PIPEBCONF enable"
        );
        assert_eq!(mmio.get(TRANSB_CLK_SEL), TRANS_CLK_SEL_DDIB);
    }

    #[test]
    fn haswell_probe_prepares_and_cleans_up_unused_edp_panel() {
        let mut mmio = MockMmio::new();
        let outputs = [OutputConfig {
            port: Port::Edp,
            enabled: true,
        }];
        prepare_port_probe_on(&mut mmio, Cpu::HaswellUlt, &outputs, None);
        assert_ne!(
            mmio.get(PanelPowerRegs::PCH.control) & PP_CONTROL::TARGET_ON::SET.value,
            0
        );
        assert_eq!(mmio.get(PanelPowerRegs::PCH.control) >> 16, 0);
        // PP_DIVISOR reference divider populated for 24 MHz LP rawclk: (24_000_000 / 20_000) - 1 = 1199.
        assert_eq!(mmio.get(PanelPowerRegs::PCH.divisor) >> 8, 1199);

        // If eDP did not resolve, cleanup turns panel power back off.
        cleanup_unused_panel_probe_on(&mut mmio, Cpu::HaswellUlt, &outputs, &[None, None, None]);
        assert_eq!(
            mmio.get(PanelPowerRegs::PCH.control) & PP_CONTROL::TARGET_ON::SET.value,
            0
        );
    }

    #[test]
    fn haswell_backlight_pwm_plan_handles_lp_and_trad_granularity_and_polarity() {
        // Lynx Point-LP (24 MHz): hz_limit = 24_000_000 / 128 / 100 = 1875 Hz.
        let lp_low = HaswellBacklightPwmPlan::new(true, 200, false, 50).unwrap();
        assert!(!lp_low.use_16_granularity);
        assert_eq!(lp_low.pwm_period, 937);
        assert_eq!(lp_low.pwm_duty, 468);
        assert_eq!(
            lp_low.south_chicken2,
            PanelRegisterOp::Clear {
                register: SOUTH_CHICKEN2,
                mask: LPT_PWM_GRANULARITY,
            }
        );

        // Above 1875 Hz on LP -> 16-step granularity (SOUTH_CHICKEN2 bit 5 set) + active-low bit 29.
        let lp_high = HaswellBacklightPwmPlan::new(true, 2000, true, 100).unwrap();
        assert!(lp_high.use_16_granularity);
        assert_eq!(lp_high.pwm_period, 750);
        assert_eq!(lp_high.pwm_duty, 750);
        assert_eq!(
            lp_high.south_chicken2,
            PanelRegisterOp::Set {
                register: SOUTH_CHICKEN2,
                mask: LPT_PWM_GRANULARITY,
            }
        );
        assert_eq!(
            lp_high.pch_ctl1,
            PanelRegisterOp::Write {
                register: BLC_PWM_PCH_CTL1,
                value: crate::gma::panel::BLM_PCH_POLARITY_LOW
                    | crate::gma::panel::BLM_PCH_OVERRIDE_ENABLE
                    | BLM_PCH_PWM_ENABLE,
            }
        );

        // Traditional Lynx Point-H (135 MHz): 1000 Hz uses 128-step (135e6 / 128 / 1000 = 1054).
        let trad = HaswellBacklightPwmPlan::new(false, 1000, false, 100).unwrap();
        assert!(!trad.use_16_granularity);
        assert_eq!(trad.pwm_period, 1054);
        assert_eq!(trad.pwm_duty, 1054);
    }

    #[test]
    fn haswell_gtt_uses_32bit_ptes_at_2mib_offset_with_39bit_phys() {
        for cpu in [Cpu::Haswell, Cpu::HaswellUlt, Cpu::HaswellUlx] {
            assert_eq!(
                gtt::gtt_pte_encoding(cpu),
                Some(gtt::GttPteEncoding::Bits32)
            );
            assert_eq!(gtt::gtt_pte_offset(cpu), Some(0x0020_0000));
        }
        // Physical address bit 36 (0x10_0000_0000) maps to PTE bit 8 (1 << 8 = 0x100).
        let (low, high) = gtt::encode_gtt_pte(0x10_7f00_0000, gtt::GttPteEncoding::Bits32);
        assert_eq!(low, 0x7f00_0101);
        assert_eq!(high, 0);
    }

    // -----------------------------------------------------------------------
    // 6. Existing planning tests (updated for fixed HdmiB -> DdiPort::C)
    // -----------------------------------------------------------------------

    #[test]
    fn haswell_hdmi_modeset_plan_wraps_shared_ddi_sequence() {
        let plan = hdmi_modeset_plan(HswHdmiModesetParams {
            cpu: Cpu::Haswell,
            port: Port::HdmiB,
            pipe: Pipe::B,
            mode: Mode::XGA_1024X768_60,
            pll: HswPllSelect::Wrpll0,
            per_ddi_clock_sel: true,
            hdmi_translation: 7,
        })
        .unwrap();
        assert_eq!(plan.power.cpu, Cpu::Haswell);
        assert_eq!(plan.power.port, Port::HdmiB);
        assert!(!plan.power.uses_aux_power);
        assert!(plan.power.requires_pch_power);
        assert_eq!(plan.pipe(), Pipe::B);
        assert!(!plan.trains_dp_link());
        assert_eq!(plan.steps, modeset_steps());
        let HswDdiProtocolPlan::Hdmi(hdmi) = plan.protocol else {
            panic!("expected HDMI protocol plan");
        };
        assert_eq!(hdmi.cpu, Cpu::Haswell);
        assert_eq!(hdmi.routed.port.port, DdiPort::C);
        assert_eq!(hdmi.routed.port.dp_tp_ctl, None);
        assert!(matches!(
            hdmi.routed.pll,
            HswDdiPllPlan::Wrpll {
                pll: HswPllSelect::Wrpll0,
                plan: HswWrpllPlan { .. },
            }
        ));
        assert_eq!(
            hdmi.steps,
            [
                HswHdmiInitStep::ProgramWrpll,
                HswHdmiInitStep::LoadBufferTranslations,
                HswHdmiInitStep::RouteDdiClock,
                HswHdmiInitStep::ProgramPipe,
                HswHdmiInitStep::ProgramTranscoder,
                HswHdmiInitStep::ProgramDdiPort,
            ]
        );
    }

    #[test]
    fn broadwell_edp_modeset_plan_includes_aux_training_and_low_vswing_table() {
        let plan = dp_modeset_plan(HswDpInitParams {
            cpu: Cpu::Broadwell,
            port: Port::Edp,
            pipe: Pipe::A,
            lanes: DdiLaneCount::Two,
            pll: HswPllSelect::Lcpll0,
            pattern: DpTrainingPattern::Pattern1,
            enhanced_framing: true,
            per_ddi_clock_sel: true,
            iboost_enabled: true,
        })
        .unwrap();
        assert_eq!(plan.power.cpu, Cpu::Broadwell);
        assert!(plan.power.uses_aux_power);
        assert!(!plan.power.requires_pch_power);
        assert_eq!(plan.pipe(), Pipe::A);
        assert!(plan.trains_dp_link());
        let HswDdiProtocolPlan::Dp(dp) = plan.protocol else {
            panic!("expected DP protocol plan");
        };
        assert_eq!(dp.routed.port.port, DdiPort::A);
        assert!(dp.routed.port.dp_tp_ctl.is_some());
        assert_eq!(dp.routed.pll, HswDdiPllPlan::Fixed(HswPllSelect::Lcpll0));
        assert_eq!(dp.buffer_translations[0], 0x00ff_ffff);
        assert_eq!(dp.buffer_translations[9], 0x0002_0011);
        assert_eq!(
            dp.steps,
            [
                DdiDpInitStep::ProgramDpPll,
                DdiDpInitStep::LoadBufferTranslations,
                DdiDpInitStep::RouteDdiClock,
                DdiDpInitStep::ProgramPipe,
                DdiDpInitStep::ProgramTranscoder,
                DdiDpInitStep::ProgramDdiPort,
                DdiDpInitStep::TrainDpLink,
            ]
        );
    }

    #[test]
    fn haswell_dp_modeset_plan_routes_pch_ddi_clock() {
        let plan = dp_modeset_plan(HswDpInitParams {
            cpu: Cpu::Haswell,
            port: Port::DpC,
            pipe: Pipe::C,
            lanes: DdiLaneCount::Four,
            pll: HswPllSelect::Spll,
            pattern: DpTrainingPattern::Pattern2,
            enhanced_framing: true,
            per_ddi_clock_sel: true,
            iboost_enabled: false,
        })
        .unwrap();
        assert!(plan.power.uses_aux_power);
        assert!(plan.power.requires_pch_power);
        let HswDdiProtocolPlan::Dp(dp) = plan.protocol else {
            panic!("expected DP protocol plan");
        };
        assert_eq!(
            dp.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46108,
                    mask_unset: u32::MAX,
                    mask_set: 3 << 29,
                }
            }
        );
    }

    #[test]
    fn haswell_modeset_plans_reject_wrong_cpu_or_legacy_port() {
        assert_eq!(
            hdmi_modeset_plan(HswHdmiModesetParams {
                cpu: Cpu::Skylake,
                port: Port::HdmiB,
                pipe: Pipe::B,
                mode: Mode::XGA_1024X768_60,
                pll: HswPllSelect::Wrpll0,
                per_ddi_clock_sel: true,
                hdmi_translation: 7,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
        assert_eq!(
            dp_modeset_plan(HswDpInitParams {
                cpu: Cpu::Haswell,
                port: Port::Vga,
                pipe: Pipe::A,
                lanes: DdiLaneCount::Four,
                pll: HswPllSelect::Spll,
                pattern: DpTrainingPattern::Pattern1,
                enhanced_framing: true,
                per_ddi_clock_sel: true,
                iboost_enabled: false,
            }),
            Err(GmaError::UnsupportedPort)
        );
    }
}
