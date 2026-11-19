//! Ironlake, Sandy Bridge and Ivy Bridge display (libgfxinit `common/ironlake`).
//!
//! The CPU drives eDP itself (`DP_CTL_A`) and every other output through an
//! FDI link to the PCH, whose transcoder feeds the VGA DAC, LVDS, HDMI or DP
//! port. FDI links are fixed to their pipe: pipe A feeds FDI/transcoder A,
//! pipe B feeds B and, on Ivy Bridge, pipe C feeds C.
//!
//! The sequence is libgfxinit's: `Initialize` (panel sequencer, clean state,
//! reference clocks, raw clock), `Enable_Output` (link preparation, PLL
//! allocation, two tries of `Connectors.Pre_On` / `Display_Controller.On` /
//! `Connectors.Post_On` per link setting) and `Disable_Output`.

mod edp;
mod fdi;
mod pch;
mod pipe;
mod plls;
pub(crate) mod port_detect;

pub(crate) use plls::DpllState;

use crate::gma::config::OutputConfig;
use crate::gma::dp_aux::{AuxRegs, DpLinkRate, ReceiverCaps};
use crate::gma::dp_training::{self, DpLinkConfig};
use crate::gma::error::GmaError;
use crate::gma::framebuffer::SurfaceConfig;
use crate::gma::generation::{GenerationOps, sealed};
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::Mode;
use crate::gma::panel::{PanelPowerRegs, PanelSequencer, PanelSequencerCaps};
use crate::gma::port_detect::PortDetectState;
use crate::gma::regs::{CPU_DP_CTL, PCH_DREF_CONTROL, PCH_RAWCLK_FREQ};
use crate::gma::scaler::ScalingPolicy;
use crate::gma::state::GenerationState;
use crate::gma::types::{Cpu, Generation, Pipe, Port};
use crate::gma::{GmaContext, GmaInitConfig};

/// Ironlake generation marker.
pub struct Ironlake;

impl sealed::Sealed for Ironlake {}

const PCH_DREF_CONTROL_REG: usize = 0xc6200;
const PCH_RAWCLK_FREQ_REG: usize = 0xc6204;
const DP_CTL_A: usize = 0x64000;
/// libgfxinit `Default_RawClk_Freq` for the whole generation.
const RAW_CLOCK_HZ: u64 = 125_000_000;
/// libgfxinit `HDMI_Max_Clock_24bpp` for Ironlake.
const HDMI_MAX_CLOCK_24BPP_KHZ: u32 = 225_000;
/// libgfxinit `LVDS_Dual_Threshold`.
pub(crate) const LVDS_DUAL_THRESHOLD_KHZ: u32 = 95_000;

/// The panel sequencer of the generation (`Up_To_Ironlake`, `Has_PCH_Panel_Power`).
const PANEL: PanelSequencerCaps = PanelSequencerCaps {
    regs: PanelPowerRegs::PCH,
    write_protection: true,
    port_select: true,
    divisor_reg: true,
    vdd_override: true,
};

/// libgfxinit `Config.FDI_Training`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdiTraining {
    /// Ironlake: one pass without voltage/emphasis steps.
    Simple,
    /// Sandy Bridge: manual training over the four voltage/emphasis settings.
    Full,
    /// Ivy Bridge: hardware auto training.
    Auto,
}

/// Per-CPU facts from libgfxinit's `Config` for this generation. The PCH
/// follows the CPU: Ibex Peak with Ironlake, Cougar/Panther Point after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    cpu: Cpu,
}

impl Caps {
    /// Caps of an Ironlake-generation CPU.
    pub(crate) const fn new(cpu: Cpu) -> Result<Self, GmaError> {
        match cpu {
            Cpu::Ironlake | Cpu::Sandybridge | Cpu::Ivybridge => Ok(Self { cpu }),
            _ => Err(GmaError::UnsupportedPlatform),
        }
    }

    const fn ivybridge(self) -> bool {
        matches!(self.cpu, Cpu::Ivybridge)
    }

    /// `Cougar_Point_On`.
    const fn cougar_point(self) -> bool {
        !matches!(self.cpu, Cpu::Ironlake)
    }

    /// `Has_New_FDI_Sink`: training pattern in FDI_RX bits 9:8 and the
    /// two-bit PCH port transcoder select.
    pub(crate) const fn new_fdi_sink(self) -> bool {
        self.cougar_point()
    }

    /// `Has_New_FDI_Source`: training pattern in FDI_TX bits 9:8.
    pub(crate) const fn new_fdi_source(self) -> bool {
        self.ivybridge()
    }

    /// `Has_FDI_Composite_Sel`.
    pub(crate) const fn fdi_composite_sel(self) -> bool {
        self.ivybridge()
    }

    /// `Has_FDI_C`: FDI B and C share four lanes.
    pub(crate) const fn has_fdi_c(self) -> bool {
        self.ivybridge()
    }

    /// `Has_Trans_Timing_Ovrrde`.
    pub(crate) const fn trans_timing_override(self) -> bool {
        self.cougar_point()
    }

    /// `Has_Trans_DP_Ctl`.
    pub(crate) const fn trans_dp_ctl(self) -> bool {
        self.cougar_point()
    }

    /// `Has_PF_Pipe_Select`.
    pub(crate) const fn pf_pipe_select(self) -> bool {
        self.ivybridge()
    }

    /// `Has_Ivybridge_Cursors`.
    pub(crate) const fn ivybridge_cursors(self) -> bool {
        self.ivybridge()
    }

    /// `FDI_Training`.
    pub(crate) const fn fdi_training(self) -> FdiTraining {
        match self.cpu {
            Cpu::Ironlake => FdiTraining::Simple,
            Cpu::Sandybridge => FdiTraining::Full,
            _ => FdiTraining::Auto,
        }
    }

    /// Pipes up to `Max_Pipe` (`Has_Tertiary_Pipe` on Ivy Bridge).
    pub(crate) const fn pipes(self) -> &'static [Pipe] {
        if self.ivybridge() {
            &[Pipe::A, Pipe::B, Pipe::C]
        } else {
            &[Pipe::A, Pipe::B]
        }
    }

    /// FDI links/PCH transcoders that exist next to the pipes.
    pub(crate) const fn fdi_ports(self) -> &'static [FdiPort] {
        if self.ivybridge() {
            &[FdiPort::A, FdiPort::B, FdiPort::C]
        } else {
            &[FdiPort::A, FdiPort::B]
        }
    }

    /// `Default_CDClk_Freq`.
    pub(crate) const fn cdclk_hz(self) -> u64 {
        match self.cpu {
            Cpu::Ironlake => 450_000_000,
            _ => 400_000_000,
        }
    }
}

/// AUX channel of an Ironlake-generation DP or eDP port.
pub(crate) fn aux_regs(cpu: Cpu, port: Port) -> Result<AuxRegs, GmaError> {
    AuxRegs::pch_for_port(port, Caps::new(cpu)?.cdclk_hz(), RAW_CLOCK_HZ)
}

/// PCH FDI receiver and transcoder (libgfxinit `PCH.FDI_Port_Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdiPort {
    /// FDI/transcoder A, fed by pipe A.
    A,
    /// FDI/transcoder B, fed by pipe B.
    B,
    /// FDI/transcoder C, fed by pipe C (Ivy Bridge).
    C,
}

impl FdiPort {
    /// The FDI link fixed to `pipe` (libgfxinit `To_GPU_Port`).
    pub(crate) const fn for_pipe(pipe: Pipe) -> Self {
        match pipe {
            Pipe::A => Self::A,
            Pipe::B => Self::B,
            Pipe::C => Self::C,
        }
    }

    /// Index for per-port register strides.
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// A PCH digital port (libgfxinit `PCH_HDMI_B`..`D`, `PCH_DP_B`..`D`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchDigital {
    /// Port B.
    B,
    /// Port C.
    C,
    /// Port D.
    D,
}

impl PchDigital {
    /// Index for per-port register strides.
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// PCH output of a port (libgfxinit `PCH_Port`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchPort {
    /// The VGA DAC.
    Dac,
    /// LVDS.
    Lvds,
    /// HDMI on a digital port.
    Hdmi(PchDigital),
    /// DisplayPort on a digital port.
    Dp(PchDigital),
}

/// Kind of display behind a port (libgfxinit `Display_Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Display {
    /// LVDS panel.
    Lvds,
    /// DisplayPort or eDP.
    Dp,
    /// HDMI/DVI.
    Hdmi,
    /// Analog VGA.
    Vga,
}

/// Where a port's signal goes: libgfxinit's `Port`/`PCH_Port`/`Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Route {
    /// Board port.
    pub port: Port,
    /// FDI link, or `None` for the CPU eDP port (`DIGI_A`).
    pub fdi: Option<FdiPort>,
    /// PCH output; meaningless for eDP.
    pub pch: PchPort,
    /// Display kind.
    pub display: Display,
}

impl Route {
    /// Route `port` driven by `pipe` (libgfxinit `To_GPU_Port`,
    /// `To_PCH_Port`, `To_Display_Type`).
    pub(crate) fn new(caps: Caps, pipe: Pipe, port: Port) -> Result<Self, GmaError> {
        if !caps.pipes().contains(&pipe) {
            return Err(GmaError::UnsupportedPort);
        }
        let (pch, display) = match port {
            Port::Lvds => (PchPort::Lvds, Display::Lvds),
            Port::Edp => (PchPort::Lvds, Display::Dp),
            Port::Vga => (PchPort::Dac, Display::Vga),
            Port::HdmiA => (PchPort::Hdmi(PchDigital::B), Display::Hdmi),
            Port::HdmiB => (PchPort::Hdmi(PchDigital::C), Display::Hdmi),
            Port::HdmiC => (PchPort::Hdmi(PchDigital::D), Display::Hdmi),
            Port::DpA => (PchPort::Dp(PchDigital::B), Display::Dp),
            Port::DpB => (PchPort::Dp(PchDigital::C), Display::Dp),
            Port::DpC => (PchPort::Dp(PchDigital::D), Display::Dp),
            Port::DpD => return Err(GmaError::UnsupportedPort),
        };
        Ok(Self {
            port,
            fdi: if port == Port::Edp {
                None
            } else {
                Some(FdiPort::for_pipe(pipe))
            },
            pch,
            display,
        })
    }
}

/// libgfxinit `Port_Config` for one enable attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortConfig {
    /// Signal path.
    pub route: Route,
    /// Mode with the dotclock limits applied.
    pub mode: Mode,
    /// Bits per colour on the link (`Mode.BPC`).
    pub bpc: u8,
    /// FDI receiver caps (`Configure_FDI_Link`), possibly narrowed by
    /// `Prepare` on Ivy Bridge.
    pub fdi_caps: ReceiverCaps,
    /// FDI link setting.
    pub fdi_link: DpLinkConfig,
    /// DP sink caps.
    pub dp_caps: ReceiverCaps,
    /// DP link setting.
    pub dp_link: DpLinkConfig,
}

const NO_CAPS: ReceiverCaps = ReceiverCaps {
    dpcd_revision: 0,
    max_link_rate: DpLinkRate::Rbr,
    max_lane_count: 1,
    tps3_supported: false,
    enhanced_framing: false,
    no_aux_handshake: false,
    aux_rd_interval: 0,
};

const NO_LINK: DpLinkConfig = DpLinkConfig {
    link_rate: DpLinkRate::Rbr,
    lane_count: 1,
    enhanced_framing: false,
    tps3_supported: false,
    aux_rd_interval: 0,
};

impl PortConfig {
    /// libgfxinit `Fill_Port_Config`, with `Power_And_Clocks.Limit_Dotclocks`
    /// applied first, as `Update_Outputs` does.
    pub(crate) fn fill(caps: Caps, pipe: Pipe, port: Port, mode: Mode) -> Result<Self, GmaError> {
        let route = Route::new(caps, pipe, port)?;
        let mut mode = mode;
        let max_dotclock_khz = (caps.cdclk_hz() * 90 / 100 / 1000) as u32;
        mode.pixel_clock_khz = mode.pixel_clock_khz.min(max_dotclock_khz);
        // `Connector_Info.Default_BPC`: 6 bpc for LVDS and eDP.
        let bpc = mode
            .bpc
            .unwrap_or(if matches!(port, Port::Lvds | Port::Edp) {
                6
            } else {
                8
            });
        if route.display == Display::Hdmi {
            let max = HDMI_MAX_CLOCK_24BPP_KHZ * 8 / u32::from(bpc);
            mode.pixel_clock_khz = mode.pixel_clock_khz.min(max);
        }
        // `Configure_FDI_Link`: 2.7 Gbit/s, `FDI_Lane_Count`, enhanced framing.
        let fdi_caps = ReceiverCaps {
            max_link_rate: DpLinkRate::Hbr,
            max_lane_count: if route.fdi == Some(FdiPort::C) { 2 } else { 4 },
            enhanced_framing: true,
            ..NO_CAPS
        };
        let fdi_link = match route.fdi {
            Some(_) => dp_training::preferred_link_setting(fdi_caps, mode, bpc)
                .ok_or(GmaError::ModeUnavailable)?,
            None => NO_LINK,
        };
        Ok(Self {
            route,
            mode,
            bpc,
            fdi_caps,
            fdi_link,
            dp_caps: NO_CAPS,
            dp_link: NO_LINK,
        })
    }
}

/// libgfxinit `Connectors.Prepare`: on Ivy Bridge, FDI C needs FDI B to
/// leave it two lanes, and FDI B is limited to two lanes while C runs.
fn prepare(mmio: &Mmio, caps: Caps, cfg: &mut PortConfig) -> Result<(), GmaError> {
    let Some(port) = cfg.route.fdi else {
        return Ok(());
    };
    if caps.has_fdi_c() && port == FdiPort::C && fdi::tx_uses_four_lanes(mmio, FdiPort::B) {
        return Err(GmaError::InvalidConfig);
    }
    if caps.has_fdi_c() && port == FdiPort::B && fdi::tx_enabled(mmio, FdiPort::C) {
        cfg.fdi_caps.max_lane_count = 2;
    }
    cfg.fdi_link = dp_training::preferred_link_setting(cfg.fdi_caps, cfg.mode, cfg.bpc)
        .ok_or(GmaError::ModeUnavailable)?;
    Ok(())
}

/// The panel sequencer, if `port` is the panel (libgfxinit `To_Panel`).
fn panel_of<'a>(mmio: &'a Mmio, state: &GenerationState, port: Port) -> Option<PanelSequencer<'a>> {
    (state.panel.port == Some(port)).then(|| PanelSequencer::new(mmio, PANEL))
}

/// libgfxinit `Connectors.Pre_On`.
fn pre_on(mmio: &Mmio, caps: Caps, pipe: Pipe, cfg: &PortConfig) {
    match cfg.route.fdi {
        None => edp::pre_on(mmio, caps, pipe, cfg),
        Some(port) => fdi::pre_on(mmio, caps, port, cfg),
    }
}

/// libgfxinit `Connectors.Post_On`.
fn post_on(
    mmio: &Mmio,
    caps: Caps,
    cfg: &PortConfig,
    pll: Option<plls::PchPll>,
    state: &mut GenerationState,
) -> Result<(), GmaError> {
    let panel = panel_of(mmio, state, cfg.route.port);
    match cfg.route.fdi {
        None => {
            edp::pre_training(mmio);
            if let Some(panel) = &panel {
                panel.on(&mut state.panel, true);
            }
            edp::post_on(mmio, caps, cfg.dp_link)?;
        }
        Some(port) => {
            fdi::post_on(mmio, caps, port)?;
            pch::transcoder_on(mmio, caps, port, cfg, pll);
            match cfg.route.pch {
                PchPort::Dac => pch::vga_on(mmio, caps, port, cfg.mode),
                PchPort::Lvds => pch::lvds_on(mmio, caps, port, cfg.mode),
                PchPort::Hdmi(digital) => pch::hdmi_on(mmio, caps, port, digital, cfg.mode),
                PchPort::Dp(digital) => {
                    pch::dp_pre_training(mmio, caps, port, digital, cfg.dp_link);
                    if let Some(panel) = &panel {
                        panel.on(&mut state.panel, true);
                    }
                    pch::dp_train(mmio, caps, digital, cfg)?;
                }
            }
        }
    }
    if let Some(panel) = &panel {
        panel.on(&mut state.panel, false);
        panel.backlight_on(&state.panel);
    }
    Ok(())
}

/// libgfxinit `Connectors.Pre_Off`.
fn pre_off(mmio: &Mmio, port: Port, state: &mut GenerationState) {
    if let Some(panel) = panel_of(mmio, state, port) {
        panel.backlight_off(&state.panel);
        panel.off(&mut state.panel);
    }
}

/// libgfxinit `Connectors.Post_Off`.
fn post_off(mmio: &Mmio, caps: Caps, route: Route) {
    let Some(port) = route.fdi else {
        edp::off(mmio);
        return;
    };
    if let PchPort::Dp(digital) = route.pch {
        pch::dp_off(mmio, caps, digital);
    }
    fdi::off(mmio, caps, port, fdi::Off::Link);
    match route.pch {
        PchPort::Dac => pch::vga_off(mmio),
        PchPort::Lvds => pch::lvds_off(mmio),
        PchPort::Hdmi(digital) => pch::hdmi_off(mmio, caps, digital),
        PchPort::Dp(_) => {}
    }
    pch::transcoder_off(mmio, caps, port);
    fdi::off(mmio, caps, port, fdi::Off::Clock);
}

/// libgfxinit `Clean_State`: everything off, from the panel to the PLLs.
fn all_off(mmio: &Mmio, caps: Caps, state: &mut GenerationState) {
    // `Connectors.Pre_All_Off`.
    if let Some(port) = state.panel.port {
        pre_off(mmio, port, state);
    }
    pipe::all_off(mmio, caps);
    // `Connectors.Post_All_Off`.
    edp::off(mmio);
    for port in caps.fdi_ports() {
        fdi::off(mmio, caps, *port, fdi::Off::Link);
    }
    pch::vga_off(mmio);
    pch::lvds_off(mmio);
    for digital in [PchDigital::B, PchDigital::C, PchDigital::D] {
        pch::hdmi_off(mmio, caps, digital);
    }
    for digital in [PchDigital::B, PchDigital::C, PchDigital::D] {
        pch::dp_off(mmio, caps, digital);
    }
    for port in caps.fdi_ports() {
        pch::transcoder_off(mmio, caps, *port);
    }
    for port in caps.fdi_ports() {
        fdi::off(mmio, caps, *port, fdi::Off::Clock);
    }
    plls::all_off(mmio);
    crate::gma::gtt::clear_legacy_fences(mmio, caps.cpu);
}

/// libgfxinit `Power_And_Clocks.Initialize`: run both 120 MHz references and
/// feed the CPU from the SSC one if eDP is strapped.
fn power_and_clocks_initialize(mmio: &Mmio) {
    let dref = PCH_DREF_CONTROL::SSC_ENABLE::Enabled
        + PCH_DREF_CONTROL::NONSSC_ENABLE::Enabled
        + PCH_DREF_CONTROL::SSC_MODULATION_ENABLE::SET;
    mmio.write32(PCH_DREF_CONTROL_REG, dref.value);
    mmio.posting_read(PCH_DREF_CONTROL_REG);
    delay_us(1);
    if mmio.read32(DP_CTL_A) & CPU_DP_CTL::PORT_DETECTED::SET.value != 0 {
        mmio.update32(
            PCH_DREF_CONTROL_REG,
            0,
            PCH_DREF_CONTROL::CPU_OUTPUT::Ssc.value,
        );
        mmio.posting_read(PCH_DREF_CONTROL_REG);
        // DMI latency.
        delay_us(20);
    }
}

/// libgfxinit `Enable_Output` without the failure's `Panel.Off`.
#[allow(clippy::too_many_arguments)]
fn enable_output(
    mmio: &Mmio,
    caps: Caps,
    pipe: Pipe,
    port: Port,
    mode: Mode,
    surface: SurfaceConfig,
    scaling: ScalingPolicy,
    state: &mut GenerationState,
) -> Result<(), GmaError> {
    let mut cfg = PortConfig::fill(caps, pipe, port, mode)?;
    let display = pipe::DisplayPlan::new(caps, pipe, surface, cfg.mode, scaling)?;
    // `Connector_Info.Preferred_Link_Setting`.
    if cfg.route.display == Display::Dp {
        if let Some(panel) = panel_of(mmio, state, port) {
            // `Use_PP_VDD_Override`.
            panel.vdd_override(&mut state.panel, true);
        }
        let aux = aux_regs(caps.cpu, port)?;
        let (dp_caps, dp_link) = dp_training::prepare_sink(mmio, aux, cfg.mode, cfg.bpc)?;
        cfg.dp_caps = dp_caps;
        cfg.dp_link = dp_link;
    }
    prepare(mmio, caps, &mut cfg)?;
    let mut last_error = GmaError::HardwareError;
    loop {
        match state.dplls.alloc(mmio, &cfg) {
            Ok(pll) => {
                for _ in 0..2 {
                    let _ = port_detect::hotplug_detect(mmio, port);
                    pre_on(mmio, caps, pipe, &cfg);
                    pipe::on(mmio, caps, pipe, &cfg, &display);
                    match post_on(mmio, caps, &cfg, pll, state) {
                        Ok(()) => {
                            state.dplls.set_pipe(pipe, pll);
                            return Ok(());
                        }
                        Err(err) => {
                            last_error = err;
                            pipe::off(mmio, caps, pipe);
                            post_off(mmio, caps, cfg.route);
                        }
                    }
                }
                state.dplls.free(mmio, pll);
            }
            Err(err) => last_error = err,
        }
        // `Connector_Info.Next_Link_Setting`.
        if cfg.route.display != Display::Dp {
            return Err(last_error);
        }
        cfg.dp_link = dp_training::next_link_setting(cfg.dp_caps, cfg.dp_link, cfg.mode, cfg.bpc)
            .ok_or(last_error)?;
        prepare(mmio, caps, &mut cfg)?;
    }
}

/// The panel port: the first enabled LVDS or eDP output (libgfxinit
/// `Panel_Ports (Panel_1)`).
fn panel_port(outputs: &[OutputConfig]) -> Option<Port> {
    outputs
        .iter()
        .find(|output| output.enabled && matches!(output.port, Port::Lvds | Port::Edp))
        .map(|output| output.port)
}

impl GenerationOps for Ironlake {
    const GENERATION: Generation = Generation::Ironlake;

    fn initialize(mmio: &Mmio, cpu: Cpu, config: &GmaInitConfig<'_>, state: &mut GenerationState) {
        let Ok(caps) = Caps::new(cpu) else {
            return;
        };
        state.panel.port = panel_port(config.outputs);
        PanelSequencer::new(mmio, PANEL).setup(&mut state.panel);
        all_off(mmio, caps, state);
        power_and_clocks_initialize(mmio);
        // `Port_Detect.Initialize` runs from the update; `Connectors.Initialize`
        // is empty.
        mmio.update32(
            PCH_RAWCLK_FREQ_REG,
            PCH_RAWCLK_FREQ::FREQ.val(0x3ff).value,
            PCH_RAWCLK_FREQ::FREQ
                .val((RAW_CLOCK_HZ / 1_000_000) as u32)
                .value,
        );
    }

    fn port_detect(mmio: &Mmio, _cpu: Cpu, initialize: bool) -> Option<PortDetectState> {
        Some(port_detect::detect(mmio, initialize))
    }

    fn hotplug_detect(mmio: &Mmio, port: Port) -> Result<bool, GmaError> {
        Ok(port_detect::hotplug_detect(mmio, port))
    }

    fn prepare_panel_probe(mmio: &Mmio, config: &GmaInitConfig<'_>, state: &mut GenerationState) {
        // `Scan_Ports`: VDD for every listed panel before probing.
        if let Some(port) = state.panel.port
            && config
                .outputs
                .iter()
                .any(|output| output.enabled && output.port == port)
        {
            PanelSequencer::new(mmio, PANEL).vdd_override(&mut state.panel, false);
        }
    }

    fn before_probe(mmio: &Mmio, port: Port, state: &mut GenerationState) {
        // `Probe_Port`: `Panel.Wait_On (To_Panel (Port), VDD_Only => True)`.
        if let Some(panel) = panel_of(mmio, state, port) {
            panel.wait_on(&state.panel, true);
        }
    }

    fn release_unused_panel(mmio: &Mmio, state: &mut GenerationState) {
        // `Scan_Ports`: `Panel.Off` for a probed panel left unconfigured.
        PanelSequencer::new(mmio, PANEL).off(&mut state.panel);
    }

    fn init_display(ctx: &mut GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        let caps = Caps::new(ctx.config.cpu)?;
        let port = crate::gma::selected_enabled_port(ctx.config.outputs)?;
        let mmio = ctx.mmio();
        let (pipe, surface) = (ctx.pipe, ctx.surface);
        let scaling = ctx.config.framebuffer.scaling;
        let result = enable_output(&mmio, caps, pipe, port, mode, surface, scaling, ctx.state);
        if result.is_err()
            && let Some(panel) = panel_of(&mmio, ctx.state, port)
        {
            panel.off(&mut ctx.state.panel);
        }
        result
    }

    fn disable_output(
        mmio: &Mmio,
        cpu: Cpu,
        pipe: Pipe,
        port: Port,
        state: &mut GenerationState,
    ) -> Result<(), GmaError> {
        let caps = Caps::new(cpu)?;
        let route = Route::new(caps, pipe, port)?;
        pre_off(mmio, port, state);
        pipe::off(mmio, caps, pipe);
        post_off(mmio, caps, route);
        let pll = state.dplls.take_pipe(pipe);
        state.dplls.free(mmio, pll);
        Ok(())
    }

    fn clean(mmio: &Mmio, cpu: Cpu) {
        if let Ok(caps) = Caps::new(cpu) {
            all_off(mmio, caps, &mut GenerationState::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_follow_libgfxinit_config() {
        let ilk = Caps::new(Cpu::Ironlake).unwrap();
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let ivb = Caps::new(Cpu::Ivybridge).unwrap();
        assert_eq!(
            [ilk.fdi_training(), snb.fdi_training(), ivb.fdi_training()],
            [FdiTraining::Simple, FdiTraining::Full, FdiTraining::Auto]
        );
        assert!(!ilk.new_fdi_sink() && snb.new_fdi_sink() && ivb.new_fdi_sink());
        assert!(!snb.new_fdi_source() && ivb.new_fdi_source());
        assert!(!ilk.trans_timing_override() && snb.trans_timing_override());
        assert!(!ilk.trans_dp_ctl() && snb.trans_dp_ctl());
        assert_eq!(snb.pipes(), &[Pipe::A, Pipe::B]);
        assert_eq!(ivb.pipes(), &[Pipe::A, Pipe::B, Pipe::C]);
        assert_eq!(Caps::new(Cpu::Gm45), Err(GmaError::UnsupportedPlatform));
    }

    #[test]
    fn routes_follow_the_pipe_for_fdi_ports() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let lvds = Route::new(snb, Pipe::A, Port::Lvds).unwrap();
        assert_eq!((lvds.fdi, lvds.pch), (Some(FdiPort::A), PchPort::Lvds));
        let dp2 = Route::new(snb, Pipe::B, Port::DpB).unwrap();
        assert_eq!(
            (dp2.fdi, dp2.pch),
            (Some(FdiPort::B), PchPort::Dp(PchDigital::C))
        );
        let edp = Route::new(snb, Pipe::B, Port::Edp).unwrap();
        assert_eq!((edp.fdi, edp.display), (None, Display::Dp));
        assert_eq!(
            Route::new(snb, Pipe::C, Port::Vga),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn port_config_picks_bpc_lanes_and_dotclock_limits() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        // 1366x768 LVDS at 76.3 MHz: 6 bpc fits one 2.7 Gbit/s FDI lane.
        let panel = Mode {
            pixel_clock_khz: 76_300,
            ..Mode::XGA_1024X768_60
        };
        let lvds = PortConfig::fill(snb, Pipe::A, Port::Lvds, panel).unwrap();
        assert_eq!(lvds.bpc, 6);
        assert_eq!(
            (lvds.fdi_link.link_rate, lvds.fdi_link.lane_count),
            (DpLinkRate::Hbr, 1)
        );
        assert!(lvds.fdi_link.enhanced_framing);
        // HDMI at 8 bpc is limited to 225 MHz, every port to 90% of CDClk.
        let fast = Mode {
            pixel_clock_khz: 400_000,
            ..Mode::XGA_1024X768_60
        };
        let hdmi = PortConfig::fill(snb, Pipe::B, Port::HdmiA, fast).unwrap();
        assert_eq!(hdmi.mode.pixel_clock_khz, 225_000);
        assert_eq!(hdmi.fdi_link.lane_count, 4);
        let vga = PortConfig::fill(snb, Pipe::A, Port::Vga, fast).unwrap();
        assert_eq!(vga.mode.pixel_clock_khz, 360_000);
        // An EDID 1.4 depth overrides the default.
        let deep = PortConfig::fill(
            snb,
            Pipe::A,
            Port::Lvds,
            Mode {
                bpc: Some(8),
                ..panel
            },
        )
        .unwrap();
        assert_eq!(deep.bpc, 8);
    }

    #[test]
    fn failed_fdi_training_tears_the_lvds_path_down() {
        use crate::gma::framebuffer::PixelFormat;
        use crate::gma::mode::ModeFlags;

        // Plain memory never reports FDI lock, so both tries fail.
        let mut regs = alloc::vec![0u32; 0x11_0000 / 4];
        // SAFETY: the backing vector covers every register touched here.
        let mmio = unsafe { Mmio::new(fstart_core::typed::mmio32(regs.as_mut_ptr() as u64)) };
        let caps = Caps::new(Cpu::Sandybridge).unwrap();
        let mode = Mode {
            hdisplay: 1366,
            hsync_start: 1414,
            hsync_end: 1446,
            htotal: 1534,
            vdisplay: 768,
            vsync_start: 771,
            vsync_end: 777,
            vtotal: 790,
            pixel_clock_khz: 72_700,
            flags: ModeFlags::NHSYNC | ModeFlags::NVSYNC,
            bpc: None,
        };
        let surface = SurfaceConfig::packed(0, 1366, 768, PixelFormat::Xrgb8888);
        let mut state = GenerationState::default();
        state.panel.port = Some(Port::Lvds);
        let result = enable_output(
            &mmio,
            caps,
            Pipe::A,
            Port::Lvds,
            mode,
            surface,
            ScalingPolicy::PreserveAspect,
            &mut state,
        );
        assert_eq!(result, Err(GmaError::HardwareError));
        let read = |offset: usize| regs[offset / 4];
        // DPLL A was set up for LVDS from the SSC reference, then stopped.
        let dpll = read(0xc6014);
        assert_eq!(dpll & (1 << 31), 0);
        assert_eq!(dpll & ((3 << 26) | (3 << 13)), (2 << 26) | (3 << 13));
        assert_eq!(state.dplls.users(plls::PchPll::A), 0);
        assert_eq!(state.dplls.take_pipe(Pipe::A), None);
        // Pipe A ran at 6 bpc with dithering and one FDI lane; everything
        // is off again.
        assert_eq!(read(0x70008) & (1 << 31), 0);
        assert_eq!(read(0x70008) & ((7 << 5) | (1 << 4)), (2 << 5) | (1 << 4));
        assert_eq!(read(0x60100) & ((1 << 31) | (1 << 14)), 0);
        assert_eq!(read(0x60100) & (7 << 19), 0);
        assert_eq!(read(0xf000c) & ((1 << 31) | (1 << 13)), 0);
        assert_eq!(read(0xf0008), 0);
        assert_eq!(read(0xe1180), 0);
        // The plane scanned the framebuffer unscaled: no fitter.
        assert_eq!(read(0x6001c), (1365 << 16) | 767);
        assert_eq!(read(0x68080) & (1 << 31), 0);
    }

    #[test]
    fn panel_port_is_the_first_enabled_panel_output() {
        let outputs = [
            OutputConfig {
                port: Port::DpA,
                enabled: true,
            },
            OutputConfig {
                port: Port::Edp,
                enabled: false,
            },
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
        ];
        assert_eq!(panel_port(&outputs), Some(Port::Lvds));
        assert_eq!(panel_port(&outputs[..2]), None);
    }
}
