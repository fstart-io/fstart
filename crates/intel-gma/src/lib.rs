//! Shared Intel GMA display initialization support.
//!
//! This is a reusable `no_std` library, not a fstart `Device`. Board-facing
//! northbridge drivers own the `Device` and call [`init`] once the chipset has
//! programmed the display BARs, stolen memory and OpRegion.
//!
//! Layers, from the bottom up:
//!
//! - [`types`], [`error`], [`mode`]: small copy types and structured errors.
//! - [`regs`], [`mmio`], [`pci`]: typed `tock-registers` definitions and the
//!   single unsafe MMIO boundary.
//! - Pure encoders: [`pipe`], [`plane`], [`port`], [`pll`], [`panel`],
//!   [`scaler`], [`gtt`], [`framebuffer`].
//! - [`vbt`], [`edid`]: bounded zero-copy parsers.
//! - [`gmbus`], [`dp_aux`], [`dp_training`]: DDC and DisplayPort link bring-up.
//! - [`ddi`] and the per-generation modules in [`generation`] build register
//!   plans and execute them through small sink traits, so host tests can record
//!   the exact write sequence.

#![cfg_attr(not(test), no_std)]

pub mod config;
pub mod ddi;
pub mod dp_aux;
pub mod dp_training;
pub mod error;
pub mod framebuffer;
pub mod generation;
pub mod gtt;
pub mod mode;
pub mod opregion;
pub mod pci;
pub mod vbt;

pub mod edid;
pub mod gmbus;
pub mod mmio;
pub mod panel;
pub mod pipe;
pub mod plane;
pub mod pll;
pub mod port;
pub mod port_detect;
pub mod power;
pub mod regs;
pub mod scaler;
pub mod state;
pub mod types;

use core::marker::PhantomData;

use fstart_core::services::FramebufferInfo;

pub use config::{OutputConfig, PreferredMode};
pub use error::GmaError;
pub use framebuffer::{FramebufferConfig, PixelFormat, SurfaceConfig};
pub use gmbus::GmbusPin;
pub use mode::{FallbackMode, Mode, ModeFlags};
pub use panel::{
    LfpBacklightInfo, LfpFpTiming, LfpPanelMetadata, LfpPowerFeatures, LvdsPanelOptions,
};
pub use pci::GmaResources;
pub use scaler::{DestinationKind, DestinationRect, ScalerKind, ScalingAspect, ScalingPolicy};
pub use state::{GmaDisplayState, PipeOutputConfig, UpdateOutputsResult};
pub use types::{Cpu, Generation, KHz, PciBdf, PhysAddr, Pipe, Plane, Port};
pub use vbt::GeneralDefinitionsMetadata;

use crate::generation::GenerationOps;

/// Capabilities for one supported CPU/platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformCaps {
    /// Display generation.
    pub generation: Generation,
    /// CPU/platform id.
    pub cpu: Cpu,
    /// Supported pipes.
    pub pipes: &'static [Pipe],
    /// Hardware ports exposed by this CPU family.
    pub ports: &'static [Port],
    /// Platform has split CPU/PCH display hardware.
    pub has_pch_split: bool,
    /// Platform uses FDI links.
    pub has_fdi: bool,
    /// Platform has Intel GMBUS.
    pub has_gmbus: bool,
    /// Platform supports LVDS.
    pub has_lvds: bool,
    /// Platform uses DDI ports.
    pub has_ddi: bool,
    /// Platform supports DisplayPort outputs.
    pub supports_displayport: bool,
    /// Pineview GPIO bit-bang GMBUS clock-gating workaround is required.
    pub requires_pineview_gmbus_clock_wa: bool,
}

const PIPES_AB: &[Pipe] = &[Pipe::A, Pipe::B];
const PIPES_ABC: &[Pipe] = &[Pipe::A, Pipe::B, Pipe::C];
const PORTS_I945G: &[Port] = &[Port::Vga];
const PORTS_I945GM: &[Port] = &[Port::Lvds, Port::Vga];
const PORTS_PINEVIEW: &[Port] = &[Port::Vga];
const PORTS_GM965: &[Port] = &[Port::Lvds, Port::Vga];
const PORTS_G45: &[Port] = &[
    Port::Lvds,
    Port::Vga,
    Port::HdmiA,
    Port::HdmiB,
    Port::DpA,
    Port::DpB,
    Port::DpC,
];
const PORTS_SPLIT_PCH: &[Port] = &[
    Port::Lvds,
    Port::Edp,
    Port::Vga,
    Port::HdmiA,
    Port::HdmiB,
    Port::HdmiC,
    Port::DpA,
    Port::DpB,
    Port::DpC,
];
const PORTS_DDI: &[Port] = &[
    Port::Edp,
    Port::HdmiA,
    Port::HdmiB,
    Port::HdmiC,
    Port::DpA,
    Port::DpB,
    Port::DpC,
    Port::DpD,
];

/// Return static platform capabilities for a CPU.
///
/// Haswell and newer report their port set for planning purposes, but their
/// modeset executors are not implemented: `init_display` returns
/// [`GmaError::UnsupportedPlatform`] rather than half-applying a modeset. Only
/// the I945 and G45 generations and Ironlake currently drive hardware.
pub const fn caps_for(cpu: Cpu) -> PlatformCaps {
    match cpu {
        Cpu::I945G => PlatformCaps {
            generation: Generation::I945,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_I945G,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: false,
            supports_displayport: false,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::I945GM => PlatformCaps {
            generation: Generation::I945,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_I945GM,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: true,
            has_ddi: false,
            supports_displayport: false,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Pineview => PlatformCaps {
            generation: Generation::I945,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_PINEVIEW,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: false,
            supports_displayport: false,
            requires_pineview_gmbus_clock_wa: true,
        },
        Cpu::PineviewM => PlatformCaps {
            generation: Generation::I945,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_I945GM,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: true,
            has_ddi: false,
            supports_displayport: false,
            requires_pineview_gmbus_clock_wa: true,
        },
        Cpu::Gm965 => PlatformCaps {
            generation: Generation::G45,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_GM965,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: true,
            has_ddi: false,
            supports_displayport: false,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::G45 | Cpu::Gm45 => PlatformCaps {
            generation: Generation::G45,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_G45,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: true,
            has_ddi: false,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Ironlake | Cpu::Sandybridge | Cpu::Ivybridge => PlatformCaps {
            generation: Generation::Ironlake,
            cpu,
            pipes: PIPES_AB,
            ports: PORTS_SPLIT_PCH,
            has_pch_split: true,
            has_fdi: true,
            has_gmbus: true,
            has_lvds: true,
            has_ddi: false,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Haswell | Cpu::Broadwell => PlatformCaps {
            generation: Generation::Haswell,
            cpu,
            pipes: PIPES_ABC,
            ports: PORTS_DDI,
            has_pch_split: true,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: true,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Broxton => PlatformCaps {
            generation: Generation::Broxton,
            cpu,
            pipes: PIPES_ABC,
            ports: PORTS_DDI,
            has_pch_split: false,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: true,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Skylake | Cpu::Kabylake => PlatformCaps {
            generation: Generation::Skylake,
            cpu,
            pipes: PIPES_ABC,
            ports: PORTS_DDI,
            has_pch_split: true,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: true,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
        Cpu::Tigerlake | Cpu::Alderlake => PlatformCaps {
            generation: Generation::Tigerlake,
            cpu,
            pipes: PIPES_ABC,
            ports: PORTS_DDI,
            has_pch_split: true,
            has_fdi: false,
            has_gmbus: true,
            has_lvds: false,
            has_ddi: true,
            supports_displayport: true,
            requires_pineview_gmbus_clock_wa: false,
        },
    }
}

/// Input configuration for shared GMA initialization.
pub struct GmaInitConfig<'a> {
    /// CPU/platform selector.
    pub cpu: Cpu,
    /// Board output policy.
    pub outputs: &'a [OutputConfig],
    /// Framebuffer policy.
    pub framebuffer: FramebufferConfig,
    /// Optional VBT bytes.
    pub vbt: Option<&'a [u8]>,
}

/// Result returned to the northbridge driver for framebuffer handoff.
#[derive(Debug, Clone, Copy)]
pub struct GmaInitResult {
    /// Generic framebuffer information for payload handoff.
    pub framebuffer: FramebufferInfo,
}

/// Internal init context shared by generation-specific code.
#[allow(dead_code)]
pub(crate) struct GmaContext<'a> {
    resources: &'a GmaResources,
    config: &'a GmaInitConfig<'a>,
    surface: SurfaceConfig,
}

impl GmaContext<'_> {
    /// Return a volatile MMIO view for the validated display BAR.
    ///
    /// `GmaContext` is constructed only by `init_candidate`, after
    /// `GmaResources::validate()` has accepted the GTTMMADR BAR. Board/chipset
    /// code is responsible for programming and decoding that BAR before entering
    /// this shared display path. Keeping the conversion here centralizes the
    /// physical-address-to-MMIO-window unsafe boundary for generation code.
    pub(crate) fn mmio(&self) -> mmio::Mmio {
        mmio_from_validated_resources(self.resources)
    }
}

/// Statically dispatched generation controller.
pub(crate) struct GmaController<'a, G: GenerationOps> {
    ctx: GmaContext<'a>,
    generation: PhantomData<G>,
}

impl<'a, G: GenerationOps> GmaController<'a, G> {
    fn new(ctx: GmaContext<'a>) -> Self {
        Self {
            ctx,
            generation: PhantomData,
        }
    }

    fn init(mut self, mode: Mode) -> Result<GmaInitResult, GmaError> {
        G::init_display(&mut self.ctx, mode)?;
        Ok(GmaInitResult {
            framebuffer: self.ctx.surface.to_framebuffer_info(),
        })
    }
}

/// Initialize Intel GMA display hardware and return framebuffer handoff state.
pub fn init(
    resources: &GmaResources,
    config: &GmaInitConfig<'_>,
) -> Result<GmaInitResult, GmaError> {
    resources.validate()?;
    validate_outputs(config.cpu, config.outputs)?;
    let mut state = GmaDisplayState::new();
    state
        .update_outputs(resources, config)?
        .primary
        .ok_or(GmaError::UnsupportedPort)
}

pub(crate) fn init_candidate(
    resources: &GmaResources,
    config: &GmaInitConfig<'_>,
) -> Result<GmaInitResult, GmaError> {
    let port = selected_enabled_port(config.outputs)?;
    let mode = clamp_hdmi_dotclock(
        config.cpu,
        port,
        choose_mode(resources, config)?,
    );
    let surface = gtt::choose_framebuffer_surface(resources, &config.framebuffer)?;
    let scaler_pipe = port::pipe_for_legacy_gmch_port(selected_enabled_port(config.outputs)?)?;
    scaler::ScalerPlan::resolve(
        config.cpu,
        scaler_pipe,
        surface,
        mode,
        config.framebuffer.scaling,
    )
    .validate_current()?;
    let ctx = GmaContext {
        resources,
        config,
        surface,
    };

    match caps_for(config.cpu).generation {
        Generation::I945 => GmaController::<generation::i945::I945>::new(ctx).init(mode),
        Generation::G45 => GmaController::<generation::g45::G45>::new(ctx).init(mode),
        Generation::Ironlake => {
            GmaController::<generation::ironlake::Ironlake>::new(ctx).init(mode)
        }
        Generation::Haswell => GmaController::<generation::haswell::Haswell>::new(ctx).init(mode),
        Generation::Broxton => GmaController::<generation::broxton::Broxton>::new(ctx).init(mode),
        Generation::Skylake => GmaController::<generation::skylake::Skylake>::new(ctx).init(mode),
        Generation::Tigerlake => {
            GmaController::<generation::tigerlake::Tigerlake>::new(ctx).init(mode)
        }
    }
}

/// Disable one pipe's display controller and its output port.
///
/// This is the per-pipe half of libgfxinit's `Update_Outputs`, which disables
/// only the outputs whose configuration changed instead of tearing down every
/// pipe. Enabling a second output must not disturb the first.
pub(crate) fn disable_generation_output(
    resources: &GmaResources,
    cpu: Cpu,
    pipe: Pipe,
    port: Port,
) {
    let mmio = mmio_from_validated_resources(resources);
    let _ = match caps_for(cpu).generation {
        Generation::I945 => generation::i945::I945::disable_output(&mmio, cpu, pipe, port),
        Generation::G45 => generation::g45::G45::disable_output(&mmio, cpu, pipe, port),
        Generation::Ironlake => generation::ironlake::Ironlake::disable_output(&mmio, cpu, pipe, port),
        _ => Ok(()),
    };
}

/// Return all pipes, ports and PLLs to libgfxinit's `Clean_State`.
///
/// Runs once before the first modeset, so unknown firmware state cannot leak
/// into the display output. It is deliberately not part of the per-output
/// enable path.
pub(crate) fn clean_generation_state(resources: &GmaResources, cpu: Cpu) {
    let mmio = mmio_from_validated_resources(resources);
    match caps_for(cpu).generation {
        Generation::I945 => generation::i945::I945::clean(&mmio, cpu),
        Generation::G45 => generation::g45::G45::clean(&mmio, cpu),
        Generation::Ironlake => generation::ironlake::Ironlake::clean(&mmio, cpu),
        _ => {}
    }
}

/// Return an MMIO view for resources that have already passed validation.
///
/// This helper is for legacy paths that do not have a `GmaContext` yet, such as
/// output probing and failure cleanup. Callers must be on the shared init path
/// after `resources.validate()` and after chipset code has decoded GTTMMADR.
pub(crate) fn mmio_from_validated_resources(resources: &GmaResources) -> mmio::Mmio {
    // SAFETY: all callers are internal shared-display paths reached after the
    // top-level `init()` validation. The wrapper itself only performs volatile
    // access; typed register block offsets document their own layout invariants.
    unsafe { mmio::Mmio::new(resources.gtt_mmio_base) }
}

fn validate_outputs(cpu: Cpu, outputs: &[OutputConfig]) -> Result<(), GmaError> {
    let caps = caps_for(cpu);
    let mut enabled_count = 0usize;
    for output in outputs.iter().filter(|output| output.enabled) {
        if !caps.ports.contains(&output.port) {
            return Err(GmaError::UnsupportedPort);
        }
        enabled_count += 1;
    }
    match enabled_count {
        0 => Err(GmaError::UnsupportedPort),
        _ => Ok(()),
    }
}

/// Maximum HDMI dot clock at 24 bpp, per libgfxinit `HDMI_Max_Clock_24bpp`.
///
/// libgfxinit scales this by `8 / Mode.BPC`; the framebuffer is 8 bpc here, so
/// the factor is 1.
const fn hdmi_max_dotclock_khz(cpu: Cpu) -> u32 {
    match cpu {
        Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM | Cpu::Gm965 | Cpu::G45
        | Cpu::Gm45 => 165_000,
        Cpu::Ironlake | Cpu::Sandybridge | Cpu::Ivybridge => 225_000,
        Cpu::Haswell | Cpu::Broadwell | Cpu::Broxton | Cpu::Skylake | Cpu::Kabylake => 300_000,
        Cpu::Tigerlake | Cpu::Alderlake => 600_000,
    }
}

/// Clamp an HDMI mode's dot clock to the platform maximum.
fn clamp_hdmi_dotclock(cpu: Cpu, port: Port, mut mode: Mode) -> Mode {
    if matches!(port, Port::HdmiA | Port::HdmiB | Port::HdmiC) {
        let max = hdmi_max_dotclock_khz(cpu);
        if mode.pixel_clock_khz > max {
            mode.pixel_clock_khz = max;
        }
    }
    mode
}

/// Return the first enabled output port in board-policy order.
pub(crate) fn selected_enabled_port(outputs: &[OutputConfig]) -> Result<Port, GmaError> {
    outputs
        .iter()
        .find(|output| output.enabled)
        .map(|output| output.port)
        .ok_or(GmaError::UnsupportedPort)
}

pub(crate) fn initialize_port_detect(
    resources: &GmaResources,
    cpu: Cpu,
) -> Option<port_detect::LegacyPortDetectState> {
    if matches!(caps_for(cpu).generation, Generation::I945 | Generation::G45) {
        let mmio = mmio_from_validated_resources(resources);
        Some(port_detect::initialize_legacy_gmch(&mmio, cpu))
    } else {
        None
    }
}

pub(crate) fn choose_mode(
    resources: &GmaResources,
    config: &GmaInitConfig<'_>,
) -> Result<Mode, GmaError> {
    match config.framebuffer.preferred_mode {
        PreferredMode::VbtPanel => config
            .vbt
            .and_then(|bytes| vbt::Vbt::parse(bytes).ok())
            .and_then(|vbt| vbt.lfp_fixed_mode().ok())
            .or_else(|| fallback_mode(&config.framebuffer).ok())
            .ok_or(GmaError::ModeUnavailable),
        PreferredMode::Fixed => fallback_mode(&config.framebuffer),
        PreferredMode::Edid => edid_mode(resources, config),
    }
}

fn edid_mode(resources: &GmaResources, config: &GmaInitConfig<'_>) -> Result<Mode, GmaError> {
    let port = selected_enabled_port(config.outputs)?;
    let caps = caps_for(config.cpu);
    if let Some(detect) = initialize_port_detect(resources, config.cpu)
        && !detect.is_valid(port)
    {
        return Err(GmaError::ModeUnavailable);
    }
    let mut storage = [0u8; edid::EDID_BLOCK_LEN];
    let mut extension_storage = [[0u8; edid::EDID_BLOCK_LEN]; edid::MAX_EXTENSION_BLOCKS];
    let modes = if uses_aux_ddc(port) {
        let mut bus = aux_ddc_bus(resources, config.cpu, port)?;
        gmbus::read_edid_modes(&mut bus, port, &mut storage, &mut extension_storage)?
    } else {
        let pin = ddc_pin_for_config(port, config).ok_or(GmaError::UnsupportedPort)?;
        if !caps.has_gmbus {
            return Err(GmaError::UnsupportedPlatform);
        }
        // SAFETY: `resources.validate()` has accepted the display MMIO BAR before
        // mode selection, and the shared init path only runs after chipset code has
        // mapped/decoded that BAR. Split-PCH platforms use the PCH GMBUS register
        // block; GMCH/SoC display engines use the GMCH-compatible block.
        let mut bus = unsafe {
            if caps.has_pch_split {
                gmbus::HardwareGmbus::pch(resources.gtt_mmio_base, pin)
            } else {
                gmbus::HardwareGmbus::gmch(resources.gtt_mmio_base, pin)
            }
        };
        gmbus::read_edid_modes(&mut bus, port, &mut storage, &mut extension_storage)?
    };
    modes.first().copied().ok_or(GmaError::ModeUnavailable)
}

fn uses_aux_ddc(port: Port) -> bool {
    matches!(
        port,
        Port::Edp | Port::DpA | Port::DpB | Port::DpC | Port::DpD
    )
}

fn aux_ddc_bus(
    resources: &GmaResources,
    cpu: Cpu,
    port: Port,
) -> Result<dp_aux::HardwareDpAuxDdc, GmaError> {
    match caps_for(cpu).generation {
        // SAFETY: `resources.validate()` has accepted the display MMIO BAR
        // before mode selection, and GMCH DP AUX registers are part of that BAR.
        Generation::G45 => unsafe { dp_aux::HardwareDpAuxDdc::gmch(resources.gtt_mmio_base, port) },
        // SAFETY: split-PCH DP AUX registers are in the decoded display MMIO BAR.
        Generation::Ironlake => unsafe {
            dp_aux::HardwareDpAuxDdc::pch(resources.gtt_mmio_base, port)
        },
        // SAFETY: DDI AUX registers are in the decoded display MMIO BAR.
        Generation::Haswell | Generation::Broxton | Generation::Skylake | Generation::Tigerlake => unsafe {
            dp_aux::HardwareDpAuxDdc::ddi(resources.gtt_mmio_base, port)
        },
        Generation::I945 => Err(GmaError::UnsupportedPort),
    }
}

fn ddc_pin_for_config(port: Port, config: &GmaInitConfig<'_>) -> Option<gmbus::GmbusPin> {
    if port == Port::Vga
        && let Some(pin) = config
            .vbt
            .and_then(|bytes| vbt::Vbt::parse(bytes).ok())
            .and_then(|vbt| vbt.general_definitions_metadata())
            .and_then(|metadata| metadata.crt_ddc_pin)
    {
        return Some(pin);
    }
    gmbus::ddc_pin_for_port(port)
}

fn fallback_mode(framebuffer: &FramebufferConfig) -> Result<Mode, GmaError> {
    framebuffer
        .fallback_mode
        .ok_or(GmaError::ModeUnavailable)?
        .to_mode()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_resources() -> GmaResources {
        GmaResources {
            pci_bdf: PciBdf {
                bus: 0,
                dev: 2,
                func: 0,
            },
            gtt_mmio_base: PhysAddr(0x1000),
            gtt_mmio_size: 0x80000,
            gtt_pte_base: None,
            gmadr_base: Some(PhysAddr(0x8000_0000)),
            gmadr_size: 0x1000_0000,
            stolen_base: PhysAddr(0x7f00_0000),
            stolen_size: 0x800000,
            gtt_size: 0x10000,
            gcfgc: None,
        }
    }

    #[test]
    fn pineview_caps_match_plan() {
        let caps = caps_for(Cpu::Pineview);
        assert_eq!(caps.generation, Generation::I945);
        assert_eq!(caps.ports, &[Port::Vga]);
        assert!(caps.requires_pineview_gmbus_clock_wa);
    }

    #[test]
    fn i945_caps_cover_desktop_and_mobile_parts() {
        let desktop = caps_for(Cpu::I945G);
        assert_eq!(desktop.generation, Generation::I945);
        assert_eq!(desktop.ports, &[Port::Vga]);
        assert!(!desktop.has_lvds);
        for mobile in [Cpu::I945GM, Cpu::PineviewM] {
            let caps = caps_for(mobile);
            assert_eq!(caps.generation, Generation::I945);
            assert_eq!(caps.ports, &[Port::Lvds, Port::Vga]);
            assert!(caps.has_lvds);
        }
        // Pineview desktop has no LVDS either.
        assert_eq!(caps_for(Cpu::Pineview).ports, &[Port::Vga]);
    }

    #[test]
    fn g45_family_advertises_displayport() {
        for cpu in [Cpu::G45, Cpu::Gm45] {
            assert!(caps_for(cpu).supports_displayport);
            assert!(caps_for(cpu).ports.contains(&Port::DpA));
        }
        // GM965 has no DisplayPort.
        assert!(!caps_for(Cpu::Gm965).supports_displayport);
    }

    #[test]
    fn gm965_caps_use_g45_family() {
        let caps = caps_for(Cpu::Gm965);
        assert_eq!(caps.generation, Generation::G45);
        assert_eq!(caps.ports, &[Port::Lvds, Port::Vga]);
        assert!(!caps.supports_displayport);
    }

    #[test]
    fn xga_dmt_60_uses_negative_sync_polarity() {
        let mode = Mode::XGA_1024X768_60;
        assert!(mode.flags.contains(ModeFlags::NHSYNC));
        assert!(mode.flags.contains(ModeFlags::NVSYNC));
        assert!(!mode.flags.contains(ModeFlags::PHSYNC));
        assert!(!mode.flags.contains(ModeFlags::PVSYNC));
    }

    #[test]
    fn framebuffer_info_uses_pixel_stride() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0xe000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let info = surface.to_framebuffer_info();
        assert_eq!(info.stride, 1024);
        assert_eq!(surface.required_bytes(), Ok(1024 * 768 * 4));
    }

    #[test]
    fn gm965_framebuffer_surface_may_upscale_with_policy() {
        let surface = SurfaceConfig::packed(PhysAddr(0xe000_0000), 800, 600, PixelFormat::Xrgb8888);
        let plan = scaler::ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            surface,
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert_eq!(plan.validate_current(), Ok(()));
    }

    #[test]
    fn xga_fallback_mode_is_valid() {
        let mode = FallbackMode {
            width: 1024,
            height: 768,
            refresh_hz: 60,
        }
        .to_mode()
        .unwrap();
        assert!(mode.is_valid());
        assert_eq!(mode.pixel_clock_khz, 65_000);
    }

    #[test]
    fn parses_real_board_vbts() {
        for bytes in [
            include_bytes!("../../../boards/lenovo/x61/data.vbt").as_slice(),
            include_bytes!("../../../boards/foxconn/d41s/data.vbt").as_slice(),
        ] {
            let vbt = vbt::Vbt::parse(bytes).unwrap();
            assert_eq!(vbt::declared_vbt_size(bytes), Some(bytes.len() as u32));
            assert!(vbt.blocks().count() > 4);
            let mode = vbt.lfp_fixed_mode().unwrap();
            assert_eq!((mode.hdisplay, mode.vdisplay), (1024, 768));
            assert!(mode.is_valid());
        }
    }

    #[test]
    fn malformed_bdb_header_size_is_rejected() {
        let mut bytes = *include_bytes!("../../../boards/lenovo/x61/data.vbt");
        let bdb_offset = u16::from_le_bytes([bytes[0x16], bytes[0x17]]) as usize;
        bytes[bdb_offset + 0x12] = 0xff;
        bytes[bdb_offset + 0x13] = 0xff;
        assert!(matches!(vbt::Vbt::parse(&bytes), Err(GmaError::VbtInvalid)));
    }

    #[test]
    fn output_validation_rejects_mixed_unsupported_ports() {
        let outputs = [
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
            OutputConfig {
                port: Port::DpA,
                enabled: true,
            },
        ];
        assert_eq!(
            validate_outputs(Cpu::Pineview, &outputs),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn output_validation_requires_enabled_supported_port() {
        let outputs = [OutputConfig {
            port: Port::Vga,
            enabled: false,
        }];
        assert_eq!(
            validate_outputs(Cpu::Pineview, &outputs),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn output_validation_accepts_multiple_enabled_outputs_for_fallback() {
        let outputs = [
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
        ];
        assert_eq!(validate_outputs(Cpu::Gm965, &outputs), Ok(()));
    }

    #[test]
    fn output_validation_accepts_g45_hdmi_and_dp_without_feature_gates() {
        for cpu in [Cpu::G45, Cpu::Gm45] {
            assert_eq!(caps_for(cpu).ports, PORTS_G45);
            for port in [
                Port::Vga,
                Port::HdmiA,
                Port::HdmiB,
                Port::DpA,
                Port::DpB,
                Port::DpC,
            ] {
                let outputs = [OutputConfig {
                    port,
                    enabled: true,
                }];
                assert_eq!(validate_outputs(cpu, &outputs), Ok(()));
            }
            assert_eq!(
                validate_outputs(
                    cpu,
                    &[OutputConfig {
                        port: Port::HdmiC,
                        enabled: true,
                    }],
                ),
                Err(GmaError::UnsupportedPort)
            );
        }
    }

    #[test]
    fn newer_generations_advertise_hardware_ports() {
        for cpu in [Cpu::Ironlake, Cpu::Sandybridge, Cpu::Ivybridge] {
            assert!(caps_for(cpu).ports.contains(&Port::Lvds));
            assert!(caps_for(cpu).ports.contains(&Port::Edp));
            assert!(caps_for(cpu).ports.contains(&Port::DpA));
            assert_eq!(
                validate_outputs(
                    cpu,
                    &[OutputConfig {
                        port: Port::DpA,
                        enabled: true,
                    }],
                ),
                Ok(())
            );
        }
        for cpu in [
            Cpu::Haswell,
            Cpu::Broadwell,
            Cpu::Broxton,
            Cpu::Skylake,
            Cpu::Kabylake,
            Cpu::Tigerlake,
            Cpu::Alderlake,
        ] {
            assert!(caps_for(cpu).ports.contains(&Port::Edp));
            assert!(caps_for(cpu).ports.contains(&Port::DpD));
            assert_eq!(
                validate_outputs(
                    cpu,
                    &[OutputConfig {
                        port: Port::DpA,
                        enabled: true,
                    }],
                ),
                Ok(())
            );
        }
    }

    #[test]
    fn output_validation_rejects_ports_outside_current_board_scope() {
        assert_eq!(
            validate_outputs(
                Cpu::Pineview,
                &[OutputConfig {
                    port: Port::Lvds,
                    enabled: true,
                }],
            ),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn representative_enabled_board_display_configs_validate() {
        let x61_outputs = [OutputConfig {
            port: Port::Lvds,
            enabled: true,
        }];
        let foxconn_outputs = [OutputConfig {
            port: Port::Vga,
            enabled: true,
        }];
        let framebuffer = FramebufferConfig {
            width: 1024,
            height: 768,
            bits_per_pixel: 32,
            stride: None,
            v_stride: None,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: Default::default(),
            rotation: Default::default(),
            preferred_mode: PreferredMode::Fixed,
            fallback_mode: Some(FallbackMode {
                width: 1024,
                height: 768,
                refresh_hz: 60,
            }),
            scaling: ScalingPolicy::None,
        };

        validate_outputs(Cpu::Gm965, &x61_outputs).unwrap();
        validate_outputs(Cpu::Pineview, &foxconn_outputs).unwrap();
        let mode = fallback_mode(&framebuffer).unwrap();
        assert!(mode.is_valid());
        assert_eq!((mode.hdisplay, mode.vdisplay), (1024, 768));
    }

    #[test]
    fn edid_preferred_mode_requires_an_enabled_output() {
        let config = GmaInitConfig {
            cpu: Cpu::Pineview,
            outputs: &[],
            framebuffer: FramebufferConfig {
                width: 1024,
                height: 768,
                bits_per_pixel: 32,
                stride: None,
                v_stride: None,
                start_x: 0,
                start_y: 0,
                offset: 0,
                tiling: Default::default(),
                rotation: Default::default(),
                preferred_mode: PreferredMode::Edid,
                fallback_mode: Some(FallbackMode {
                    width: 1024,
                    height: 768,
                    refresh_hz: 60,
                }),
                scaling: ScalingPolicy::None,
            },
            vbt: None,
        };
        assert_eq!(
            edid_mode(&dummy_resources(), &config),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn vbt_panel_mode_uses_fallback_when_vbt_unavailable() {
        let config = GmaInitConfig {
            cpu: Cpu::Gm965,
            outputs: &[],
            framebuffer: FramebufferConfig {
                width: 1024,
                height: 768,
                bits_per_pixel: 32,
                stride: None,
                v_stride: None,
                start_x: 0,
                start_y: 0,
                offset: 0,
                tiling: Default::default(),
                rotation: Default::default(),
                preferred_mode: PreferredMode::VbtPanel,
                fallback_mode: Some(FallbackMode {
                    width: 1024,
                    height: 768,
                    refresh_hz: 60,
                }),
                scaling: ScalingPolicy::None,
            },
            vbt: None,
        };
        let mode = choose_mode(&dummy_resources(), &config).unwrap();
        assert_eq!(
            (mode.hdisplay, mode.vdisplay, mode.pixel_clock_khz),
            (1024, 768, 65_000)
        );
    }

    #[test]
    fn vga_ddc_pin_prefers_vbt_general_definitions() {
        let config = GmaInitConfig {
            cpu: Cpu::Gm965,
            outputs: &[OutputConfig {
                port: Port::Vga,
                enabled: true,
            }],
            framebuffer: FramebufferConfig {
                width: 1024,
                height: 768,
                bits_per_pixel: 32,
                stride: None,
                v_stride: None,
                start_x: 0,
                start_y: 0,
                offset: 0,
                tiling: Default::default(),
                rotation: Default::default(),
                preferred_mode: PreferredMode::Edid,
                fallback_mode: None,
                scaling: ScalingPolicy::None,
            },
            vbt: Some(include_bytes!("../../../boards/lenovo/x61/data.vbt")),
        };
        assert_eq!(
            ddc_pin_for_config(Port::Vga, &config),
            Some(GmbusPin::Analog)
        );
        assert_eq!(
            ddc_pin_for_config(Port::Lvds, &config),
            Some(GmbusPin::Panel)
        );
    }

    #[test]
    fn legacy_gtt_pte_encoding_and_count() {
        assert_eq!(gtt::ptes_for_bytes(0), 0);
        assert_eq!(gtt::ptes_for_bytes(1), 1);
        assert_eq!(gtt::ptes_for_bytes(4096), 1);
        assert_eq!(gtt::ptes_for_bytes(4097), 2);
        assert_eq!(
            gtt::encode_gtt_pte(PhysAddr(0x1234_5678), gtt::GttPteEncoding::Bits32).0,
            0x1234_5001
        );
    }

    #[test]
    fn maps_framebuffer_to_stolen_pages() {
        let resources = GmaResources {
            pci_bdf: PciBdf {
                bus: 0,
                dev: 2,
                func: 0,
            },
            gtt_mmio_base: PhysAddr(0xfeb0_0000),
            gtt_mmio_size: 1024 * 1024,
            gtt_pte_base: None,
            gmadr_base: Some(PhysAddr(0xd000_0000)),
            gmadr_size: 16 * 1024 * 1024,
            stolen_base: PhysAddr(0x3f00_0000),
            stolen_size: 16 * 1024 * 1024,
            gtt_size: 512 * 1024,
            gcfgc: None,
        };
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 2, 2, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 129];
        // SAFETY: test buffer has exactly `ptes.len()` writable u32 entries and
        // is cast to the equivalent tock-registers MMIO cell type for testing.
        unsafe {
            gtt::map_framebuffer_to_stolen(
                &resources,
                &surface,
                ptes.as_mut_ptr() as *mut gtt::GttPte,
                ptes.len(),
                gtt::GttPteEncoding::Bits32,
            )
            .unwrap();
        }
        assert_eq!(ptes[0], 0x3f00_0001);
        assert_eq!(ptes[128], 0x3f08_0001);
    }
}
