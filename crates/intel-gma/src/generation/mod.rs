//! Generation-specific Intel GMA display operations.

use crate::error::GmaError;
use crate::mode::Mode;
use crate::types::{Generation, Pipe, Plane, Port};

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
}

pub(crate) mod sealed {
    /// Prevent external generation implementations.
    pub trait Sealed {}
}

pub mod broxton;
pub mod g45;
pub mod haswell;
pub mod i9xx;
pub mod ironlake;
pub mod skylake;
pub mod tigerlake;
