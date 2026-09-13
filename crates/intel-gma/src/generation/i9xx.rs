//! I9xx Intel GMA generation support.
//!
//! Pineview and GM965-class legacy GMCH display blocks share the same basic
//! pipe/plane/LVDS/VGA register sequencing for the initial framebuffer path.
//! Reuse the G45 module for that legacy GMCH sequencing only; this is not full
//! G45 equivalence. CPU-specific PLL encoding/limits and resource layout remain
//! selected by `Cpu`, so Pineview uses its i9xx/Pineview PLL path and separate
//! GTT PTE BAR resources.

use crate::GmaContext;
use crate::error::GmaError;
use crate::generation::{GenerationOps, g45::G45, sealed};
use crate::mode::Mode;
use crate::types::Generation;

/// I9xx generation marker.
pub struct I9xx;

impl sealed::Sealed for I9xx {}

impl GenerationOps for I9xx {
    const GENERATION: Generation = Generation::I9xx;

    fn init_display(ctx: &mut GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        G45::init_display(ctx, mode)
    }
}
