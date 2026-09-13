//! Generation-specific Intel GMA display operations.

use crate::error::GmaError;
use crate::mode::Mode;
use crate::types::{Cpu, Generation, Pipe, Plane, Port};

/// Internal generation operation table.
#[allow(dead_code)]
pub(crate) trait GenerationOps: sealed::Sealed {
    /// Display generation implemented by this marker.
    const GENERATION: Generation;

    /// Program generation-specific display path for one mode.
    fn init_display(_ctx: &mut crate::GmaContext<'_>, _mode: Mode) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program display clocks.
    fn program_clocks(_ctx: &mut crate::GmaContext<'_>, _mode: Mode) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program pipe PLL.
    fn program_pll(
        _ctx: &mut crate::GmaContext<'_>,
        _pipe: Pipe,
        _mode: Mode,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program pipe timings.
    fn program_pipe(
        _ctx: &mut crate::GmaContext<'_>,
        _pipe: Pipe,
        _mode: Mode,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Program primary plane.
    fn program_primary_plane(
        _ctx: &mut crate::GmaContext<'_>,
        _plane: Plane,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Enable output port.
    fn enable_port(
        _ctx: &mut crate::GmaContext<'_>,
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
        _mmio: &crate::mmio::Mmio,
        _cpu: Cpu,
        _pipe: Pipe,
        _port: Port,
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }

    /// Return the display to libgfxinit's `Clean_State`: every pipe, port,
    /// panel and PLL off. Used once before the first modeset, not per output.
    fn clean(_mmio: &crate::mmio::Mmio, _cpu: Cpu) {}
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
