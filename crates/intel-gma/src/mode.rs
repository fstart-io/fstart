//! Display mode timing and selection helpers.

use bitflags::bitflags;

use crate::error::GmaError;

bitflags! {
    /// Display mode sync and scanout flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ModeFlags: u32 {
        /// Positive hsync polarity.
        const PHSYNC = 1 << 0;
        /// Negative hsync polarity.
        const NHSYNC = 1 << 1;
        /// Positive vsync polarity.
        const PVSYNC = 1 << 2;
        /// Negative vsync polarity.
        const NVSYNC = 1 << 3;
        /// Interlaced timing.
        const INTERLACE = 1 << 4;
    }
}

/// Complete display mode timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Active horizontal pixels.
    pub hdisplay: u16,
    /// Horizontal sync start.
    pub hsync_start: u16,
    /// Horizontal sync end.
    pub hsync_end: u16,
    /// Total horizontal pixels.
    pub htotal: u16,
    /// Active vertical lines.
    pub vdisplay: u16,
    /// Vertical sync start.
    pub vsync_start: u16,
    /// Vertical sync end.
    pub vsync_end: u16,
    /// Total vertical lines.
    pub vtotal: u16,
    /// Pixel clock in kHz.
    pub pixel_clock_khz: u32,
    /// Mode flags.
    pub flags: ModeFlags,
}

impl Mode {
    /// VESA DMT 1024x768@60 fallback timing.
    pub const XGA_1024X768_60: Self = Self {
        hdisplay: 1024,
        hsync_start: 1048,
        hsync_end: 1184,
        htotal: 1344,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 777,
        vtotal: 806,
        pixel_clock_khz: 65_000,
        flags: ModeFlags::NHSYNC.union(ModeFlags::NVSYNC),
    };

    /// Return true when the timing fields are monotonic and non-zero.
    pub const fn is_valid(&self) -> bool {
        self.hdisplay != 0
            && self.vdisplay != 0
            && self.pixel_clock_khz != 0
            && self.hdisplay <= self.hsync_start
            && self.hsync_start <= self.hsync_end
            && self.hsync_end <= self.htotal
            && self.vdisplay <= self.vsync_start
            && self.vsync_start <= self.vsync_end
            && self.vsync_end <= self.vtotal
    }
}

/// Fixed width/height/refresh fallback from board policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackMode {
    /// Horizontal active pixels.
    pub width: u16,
    /// Vertical active lines.
    pub height: u16,
    /// Refresh rate in hertz.
    pub refresh_hz: u16,
}

impl FallbackMode {
    /// Convert known safe fallbacks to explicit timings.
    pub fn to_mode(self) -> Result<Mode, GmaError> {
        match (self.width, self.height, self.refresh_hz) {
            (1024, 768, 60) => Ok(Mode::XGA_1024X768_60),
            _ => Err(GmaError::ModeUnavailable),
        }
    }
}
