//! Generation-specific Intel GMA display operations.

use crate::gma::GmaInitConfig;
use crate::gma::error::GmaError;
use crate::gma::mmio::Mmio;
use crate::gma::mode::Mode;
use crate::gma::port_detect::{self, PortDetectState};
use crate::gma::state::GenerationState;
use crate::gma::types::{Cpu, Generation, Pipe, Plane, Port};

/// Select a pipe within the existing generation's routing constraints.
/// libgfxinit's `Scan_Ports` hands out the pipes in order; only pre-i965
/// LVDS needs pipe B, and G45 ports may be limited to some pipes. Connector
/// probing and framebuffer sizing do not need to know these restrictions.
pub(crate) fn output_pipe<G: GenerationOps>(
    cpu: Cpu,
    port: Port,
    _mode: Mode,
    available: &[Pipe],
) -> Result<Pipe, GmaError> {
    let pipe = match G::GENERATION {
        // Pre-i965 LVDS needs pipe B; libgfxinit swaps it there.
        Generation::I945 => crate::gma::port::pipe_for_legacy_gmch_port(port)?,
        // libgfxinit `Scan_Ports`: probed outputs take the pipes in order.
        Generation::G45 => *available
            .iter()
            .find(|pipe| crate::gma::port::legacy_gmch_port_can_use_pipe(cpu, port, **pipe))
            .ok_or(GmaError::UnsupportedPort)?,
        _ => *available.first().ok_or(GmaError::UnsupportedPort)?,
    };
    if available.contains(&pipe) {
        Ok(pipe)
    } else {
        Err(GmaError::UnsupportedPort)
    }
}

/// A display generation's modeset executor.
///
/// The chipset driver names its generation as a type ([`I945`](i945::I945),
/// [`G45`](g45::G45), [`Ironlake`](ironlake::Ironlake)), so a stage links only
/// that generation's executor; the [`Cpu`] then only picks a variant within it.
/// The generation stays internal: it is the northbridge's fact, not the board's.
#[allow(dead_code)]
pub(crate) trait GenerationOps: sealed::Sealed {
    /// Display generation implemented by this marker.
    const GENERATION: Generation;

    /// Program generation-specific display path for one mode.
    fn init_display(_ctx: &mut crate::gma::GmaContext<'_>, _mode: Mode) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program display clocks.
    fn program_clocks(_ctx: &mut crate::gma::GmaContext<'_>, _mode: Mode) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program pipe PLL.
    fn program_pll(
        _ctx: &mut crate::gma::GmaContext<'_>,
        _pipe: Pipe,
        _mode: Mode,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program pipe timings.
    fn program_pipe(
        _ctx: &mut crate::gma::GmaContext<'_>,
        _pipe: Pipe,
        _mode: Mode,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program primary plane.
    fn program_primary_plane(
        _ctx: &mut crate::gma::GmaContext<'_>,
        _plane: Plane,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Enable output port.
    fn enable_port(
        _ctx: &mut crate::gma::GmaContext<'_>,
        _port: Port,
        _pipe: Pipe,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Disable one pipe's display controller and its output port.
    ///
    /// libgfxinit's `Update_Outputs` disables only the pipes whose
    /// configuration changed; enabling a second output must not tear down the
    /// first. Generation code implements this per pipe and port.
    fn disable_output(
        _mmio: &Mmio,
        _cpu: Cpu,
        _pipe: Pipe,
        _port: Port,
        _state: &mut GenerationState,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Return the display to libgfxinit's `Clean_State`: every pipe, port,
    /// panel and PLL off. Used once before the first modeset, not per output.
    fn clean(_mmio: &Mmio, _cpu: Cpu) {}

    /// libgfxinit `Initialize`, once before the first probe. Generations
    /// without more to do only clean up.
    fn initialize(
        mmio: &Mmio,
        cpu: Cpu,
        _config: &GmaInitConfig<'_>,
        _state: &mut GenerationState,
    ) {
        Self::clean(mmio, cpu);
    }

    /// libgfxinit `Port_Detect.Initialize`: the ports that may carry a
    /// display. With `initialize`, hotplug detection is (re)armed as well.
    fn port_detect(mmio: &Mmio, cpu: Cpu, initialize: bool) -> Option<PortDetectState> {
        Some(if initialize {
            port_detect::initialize_legacy_gmch(mmio, cpu)
        } else {
            port_detect::legacy_gmch_detected(mmio, cpu)
        })
    }

    /// libgfxinit `Port_Detect.Hotplug_Detect`: report and clear a pending
    /// hotplug event of `port`.
    fn hotplug_detect(mmio: &Mmio, port: Port) -> Result<bool, GmaError> {
        port_detect::hotplug_detect(mmio, port)
    }

    /// Power panels before probing them (libgfxinit `Scan_Ports`).
    fn prepare_panel_probe(mmio: &Mmio, config: &GmaInitConfig<'_>, _state: &mut GenerationState) {
        if config
            .outputs
            .iter()
            .any(|output| output.enabled && output.port == Port::Lvds)
        {
            g45::prepare_panel_probe(mmio, config.vbt);
        }
    }

    /// Prepare probing one port (libgfxinit `Probe_Port`).
    fn before_probe(_mmio: &Mmio, _port: Port, _state: &mut GenerationState) {}

    /// Turn off a probed panel that no pipe drives (libgfxinit `Scan_Ports`).
    fn release_unused_panel(mmio: &Mmio, _state: &mut GenerationState) {
        g45::panel_backlight_off(mmio);
        g45::panel_power_off(mmio);
    }
}

pub(crate) mod sealed {
    /// Prevent external generation implementations.
    pub trait Sealed {}
}

pub mod broxton;
pub mod g45;
pub mod haswell;
pub mod i945;
pub mod ironlake;
pub mod skylake;
pub mod tigerlake;
