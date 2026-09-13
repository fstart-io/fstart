//! Pipe/transcoder setup helpers for Intel GMA display initialization.
//!
//! libgfxinit models display bring-up as a filled pipe configuration that is
//! then handed to `Pipe_Setup.On`.  The current fstart hardware path still
//! writes legacy GMCH registers directly, but this module keeps the same
//! intermediate shape explicit and testable.

use crate::mode::Mode;
use crate::regs::{PIPE_RANGE, PIPECONF};
use crate::types::{Pipe, Port};

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

/// Legacy/GMCH `PIPECONF.BPC` selector for a port.
///
/// The legacy paths implement 18bpp (6 bpc) LVDS, so dithering is enabled to
/// hide banding against the 8 bpc framebuffer, matching libgfxinit's
/// `BPC_Conf` (dither when `Framebuffer.BPC /= Mode.BPC`). VGA, HDMI and DP
/// run at 8 bpc with no dither.
pub(crate) const fn pipeconf_bpc_bits(port: Port) -> u32 {
    match port {
        Port::Lvds => PIPECONF::BPC::Bits6.value | PIPECONF::DITHER::SET.value,
        _ => PIPECONF::BPC::Bits8.value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpc_selects_six_for_lvds_and_eight_otherwise() {
        // 6 bpc = 2 << 5 and 8 bpc = 0 << 5 in hardware encoding.
        assert_eq!(pipeconf_bpc_bits(Port::Lvds) & (7 << 5), 2 << 5);
        assert_eq!(pipeconf_bpc_bits(Port::Lvds) & (1 << 4), 1 << 4);
        assert_eq!(pipeconf_bpc_bits(Port::Vga), PIPECONF::BPC::Bits8.value);
        assert_eq!(pipeconf_bpc_bits(Port::Vga) & (7 << 5), 0);
        assert_eq!(pipeconf_bpc_bits(Port::HdmiA), PIPECONF::BPC::Bits8.value);
        assert_eq!(pipeconf_bpc_bits(Port::DpA), PIPECONF::BPC::Bits8.value);
    }

    #[test]
    fn encodes_libgfxinit_style_ranges() {
        let pipe = PipeConfig::new(Pipe::B, Mode::XGA_1024X768_60);
        assert_eq!(pipe.htotal(), 0x053f_03ff);
        assert_eq!(pipe.vsync(), 0x0308_0302);
        assert_eq!(pipe.pipesrc(), 0x03ff_02ff);
    }
}
