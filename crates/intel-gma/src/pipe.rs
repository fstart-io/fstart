//! Pipe/transcoder setup helpers for Intel GMA display initialization.
//!
//! libgfxinit models display bring-up as a filled pipe configuration that is
//! then handed to `Pipe_Setup.On`.  The current fstart hardware path still
//! writes legacy GMCH registers directly, but this module keeps the same
//! intermediate shape explicit and testable.

use crate::mode::Mode;
use crate::regs::PIPE_RANGE;
use crate::types::Pipe;

/// Resolved pipe timing configuration for one active output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PipeConfig {
    /// Pipe selected for the output.
    pub pipe: Pipe,
    /// Mode programmed on that pipe.
    pub mode: Mode,
}

impl PipeConfig {
    /// Build a pipe configuration.
    pub const fn new(pipe: Pipe, mode: Mode) -> Self {
        Self { pipe, mode }
    }

    /// Encode an Intel display range register, matching libgfxinit's
    /// `Encode(LSW, MSW)` helper and the PRM's `value - 1` convention.
    pub const fn encode_range(low: u16, high: u16) -> u32 {
        PIPE_RANGE::HIGH_MINUS_ONE.val((high as u32) - 1).value
            | PIPE_RANGE::LOW_MINUS_ONE.val((low as u32) - 1).value
    }

    /// Encoded horizontal total register value.
    pub const fn htotal(self) -> u32 {
        Self::encode_range(self.mode.hdisplay, self.mode.htotal)
    }

    /// Encoded horizontal blank register value.
    pub const fn hblank(self) -> u32 {
        Self::encode_range(self.mode.hdisplay, self.mode.htotal)
    }

    /// Encoded horizontal sync register value.
    pub const fn hsync(self) -> u32 {
        Self::encode_range(self.mode.hsync_start, self.mode.hsync_end)
    }

    /// Encoded vertical total register value.
    pub const fn vtotal(self) -> u32 {
        Self::encode_range(self.mode.vdisplay, self.mode.vtotal)
    }

    /// Encoded vertical blank register value.
    pub const fn vblank(self) -> u32 {
        Self::encode_range(self.mode.vdisplay, self.mode.vtotal)
    }

    /// Encoded vertical sync register value.
    pub const fn vsync(self) -> u32 {
        Self::encode_range(self.mode.vsync_start, self.mode.vsync_end)
    }

    /// Encoded pipe source register value.
    pub const fn pipesrc(self) -> u32 {
        Self::encode_range(self.mode.vdisplay, self.mode.hdisplay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_libgfxinit_style_ranges() {
        let pipe = PipeConfig::new(Pipe::B, Mode::XGA_1024X768_60);
        assert_eq!(pipe.htotal(), 0x053f_03ff);
        assert_eq!(pipe.vsync(), 0x0308_0302);
        assert_eq!(pipe.pipesrc(), 0x03ff_02ff);
    }
}
