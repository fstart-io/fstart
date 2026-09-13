//! Framebuffer surface layout helpers.

use fstart_core::services::FramebufferInfo;
use serde::{Deserialize, Serialize};

use crate::error::GmaError;
use crate::gtt::GTT_PAGE_SIZE;
use crate::scaler::ScalingPolicy;
use crate::types::PhysAddr;

/// Supported framebuffer pixel formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// XRGB8888 in little-endian memory, byte order B, G, R, X.
    Xrgb8888,
}

impl PixelFormat {
    /// Bits per pixel.
    pub const fn bits_per_pixel(self) -> u8 {
        match self {
            Self::Xrgb8888 => 32,
        }
    }

    /// Bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        (self.bits_per_pixel() as u32) / 8
    }
}

/// Framebuffer memory tiling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TilingMode {
    /// Linear framebuffer layout.
    #[default]
    Linear,
    /// Intel X-tiled framebuffer layout.
    X,
    /// Intel Y-tiled framebuffer layout.
    Y,
}

impl TilingMode {
    /// Tile width in bytes divided by 4, matching libgfxinit's fence pitch unit.
    pub const fn tile_width_units(self) -> u32 {
        match self {
            Self::Linear => 1,
            Self::X => 8,
            Self::Y => 32,
        }
    }

    /// Tile row granularity in scanlines.
    pub const fn tile_rows(self) -> u32 {
        match self {
            Self::Linear | Self::X => 1,
            Self::Y => 32,
        }
    }

    /// True when this mode requires a legacy fence for CPU aperture access.
    pub const fn needs_fence(self) -> bool {
        !matches!(self, Self::Linear)
    }
}

/// Framebuffer scanout rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Rotation {
    /// No rotation.
    #[default]
    None,
    /// 90 degree rotation.
    Rotate90,
    /// 180 degree rotation.
    Rotate180,
    /// 270 degree rotation.
    Rotate270,
}

impl Rotation {
    /// True for rotations that use libgfxinit's rotated GTT mapping.
    pub const fn is_90_or_270(self) -> bool {
        matches!(self, Self::Rotate90 | Self::Rotate270)
    }
}

/// CPU-visible framebuffer surface configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceConfig {
    /// CPU-visible framebuffer physical address, normally GMADR aperture base.
    pub base_addr: PhysAddr,
    /// Active width in pixels.
    pub width: u32,
    /// Active height in pixels.
    pub height: u32,
    /// Pixels per scanline.
    pub stride: u32,
    /// Allocated vertical stride in scanlines.
    pub v_stride: u32,
    /// Surface start X offset in pixels.
    pub start_x: u32,
    /// Surface start Y offset in pixels.
    pub start_y: u32,
    /// Byte offset in the aperture.
    pub offset: u32,
    /// Memory tiling mode.
    pub tiling: TilingMode,
    /// Scanout rotation.
    pub rotation: Rotation,
    /// Pixel format.
    pub pixel_format: PixelFormat,
}

impl SurfaceConfig {
    /// Create a surface with tightly packed scanlines.
    pub fn packed(base_addr: PhysAddr, width: u32, height: u32, pixel_format: PixelFormat) -> Self {
        Self {
            base_addr,
            width,
            height,
            stride: width,
            v_stride: height,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: TilingMode::Linear,
            rotation: Rotation::None,
            pixel_format,
        }
    }

    /// Return required framebuffer bytes, checking for overflow.
    pub fn required_bytes(&self) -> Result<u32, GmaError> {
        self.stride
            .checked_mul(self.v_stride)
            .and_then(|pixels| pixels.checked_mul(self.pixel_format.bytes_per_pixel()))
            .ok_or(GmaError::GttSetupFailed)
    }

    /// Validate that the surface fits a reserved framebuffer region.
    pub fn validate_fits(&self, region_size: u32) -> Result<(), GmaError> {
        if self.width == 0
            || self.height == 0
            || self.stride < self.width.saturating_add(self.start_x)
            || self.v_stride < self.height.saturating_add(self.start_y)
            || self.stride_bytes()? % (self.tiling.tile_width_units() * 4) != 0
            || !self.v_stride.is_multiple_of(self.tiling.tile_rows())
        {
            return Err(GmaError::ModeUnavailable);
        }
        if self.rotation.is_90_or_270()
            && (self.offset < crate::gtt::GTT_ROTATION_OFFSET_BYTES || self.first_page()? % 64 != 0)
        {
            return Err(GmaError::ModeUnavailable);
        }
        if self.required_bytes()? > region_size {
            return Err(GmaError::GttSetupFailed);
        }
        Ok(())
    }

    /// Return the scanline stride in bytes.
    pub fn stride_bytes(&self) -> Result<u32, GmaError> {
        self.stride
            .checked_mul(self.pixel_format.bytes_per_pixel())
            .ok_or(GmaError::GttSetupFailed)
    }

    /// Return the pitch value used by legacy fence registers.
    pub fn fence_pitch(&self) -> Result<u32, GmaError> {
        let denom = self.tiling.tile_width_units() * 4;
        Ok(self.stride_bytes()?.div_ceil(denom))
    }

    /// First GTT page covered by this surface offset.
    pub fn first_page(&self) -> Result<u32, GmaError> {
        Ok(self.offset / GTT_PAGE_SIZE)
    }

    /// Number of GTT pages covered by this surface.
    pub fn page_count(&self) -> Result<u32, GmaError> {
        Ok(self.required_bytes()?.div_ceil(GTT_PAGE_SIZE))
    }

    /// Last GTT page covered by this surface.
    pub fn last_page(&self) -> Result<u32, GmaError> {
        let first = self.first_page()?;
        let count = self.page_count()?;
        first
            .checked_add(count)
            .and_then(|v| v.checked_sub(1))
            .ok_or(GmaError::GttSetupFailed)
    }

    /// Offset used in the plane surface register.
    pub const fn plane_surface_offset(&self) -> u32 {
        self.offset & 0xffff_f000
    }

    /// Fill the framebuffer with a deterministic bring-up test pattern.
    ///
    /// This mirrors libgfxinit's framebuffer-filler role: make a successful
    /// modeset visible even before a payload draws into the handoff buffer.
    ///
    /// # Safety
    ///
    /// `base_addr` must be CPU-accessible memory for at least
    /// `required_bytes()` bytes, and no other agent may concurrently mutate the
    /// same framebuffer while this runs.
    pub unsafe fn fill_bringup_pattern(&self) -> Result<(), GmaError> {
        self.validate_fits(self.required_bytes()?)?;
        let base = self.base_addr.0 as *mut u32;
        let stride = self.stride as usize;
        let width = self.width as usize;
        let height = self.height as usize;
        for y in 0..height {
            for x in 0..width {
                let pixel = bringup_pattern_pixel(x as u32, y as u32, self.width, self.height);
                // SAFETY: caller guarantees that the framebuffer mapping covers
                // the full surface. `validate_fits(required_bytes())` checked
                // dimensions and stride arithmetic before this loop.
                unsafe { base.add(y * stride + x).write_volatile(pixel) };
            }
        }
        Ok(())
    }

    /// Convert to fstart's generic framebuffer handoff structure.
    pub const fn to_framebuffer_info(self) -> FramebufferInfo {
        match self.pixel_format {
            PixelFormat::Xrgb8888 => FramebufferInfo {
                base_addr: self.base_addr.0,
                width: self.width,
                height: self.height,
                stride: self.stride,
                bits_per_pixel: 32,
                red_pos: 16,
                red_size: 8,
                green_pos: 8,
                green_size: 8,
                blue_pos: 0,
                blue_size: 8,
            },
        }
    }
}

/// Return one XRGB8888 pixel for the firmware bring-up test pattern.
pub const fn bringup_pattern_pixel(x: u32, y: u32, width: u32, height: u32) -> u32 {
    let safe_width = if width > 1 { width - 1 } else { 1 };
    let safe_height = if height > 1 { height - 1 } else { 1 };
    let red = (x.saturating_mul(255) / safe_width) & 0xff;
    let green = (y.saturating_mul(255) / safe_height) & 0xff;
    let checker = if ((x / 32) ^ (y / 32)) & 1 == 0 {
        0x40
    } else {
        0xc0
    };
    0xff00_0000 | (red << 16) | (green << 8) | checker
}

/// Board policy for framebuffer dimensions and mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FramebufferConfig {
    /// Requested framebuffer width in pixels.
    pub width: u32,
    /// Requested framebuffer height in pixels.
    pub height: u32,
    /// Requested bits per pixel. Initial implementation supports 32.
    pub bits_per_pixel: u8,
    /// Optional scanline stride in pixels. Defaults to `width`.
    #[serde(default)]
    pub stride: Option<u32>,
    /// Optional vertical stride in scanlines. Defaults to `height`.
    #[serde(default)]
    pub v_stride: Option<u32>,
    /// Surface start X offset in pixels.
    #[serde(default)]
    pub start_x: u32,
    /// Surface start Y offset in pixels.
    #[serde(default)]
    pub start_y: u32,
    /// Aperture byte offset.
    #[serde(default)]
    pub offset: u32,
    /// Framebuffer tiling mode.
    #[serde(default)]
    pub tiling: TilingMode,
    /// Framebuffer scanout rotation.
    #[serde(default)]
    pub rotation: Rotation,
    /// Preferred mode source.
    pub preferred_mode: crate::config::PreferredMode,
    /// Optional board-policy mode. Used directly by `PreferredMode::Fixed` and
    /// as fallback for `PreferredMode::VbtPanel`.
    #[serde(default)]
    pub fallback_mode: Option<crate::mode::FallbackMode>,
    /// Requested scaler behavior. Defaults to exact-size scanout with no scaling.
    #[serde(default)]
    pub scaling: ScalingPolicy,
}

impl FramebufferConfig {
    /// Return the requested pixel format.
    pub fn pixel_format(&self) -> Result<PixelFormat, GmaError> {
        match self.bits_per_pixel {
            32 => Ok(PixelFormat::Xrgb8888),
            _ => Err(GmaError::ModeUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bringup_pattern_has_gradient_and_checkerboard() {
        assert_eq!(bringup_pattern_pixel(0, 0, 1024, 768), 0xff00_0040);
        assert_eq!(bringup_pattern_pixel(1023, 767, 1024, 768), 0xffff_ff40);
        assert_eq!(bringup_pattern_pixel(32, 0, 1024, 768) & 0xff, 0xc0);
    }

    #[test]
    fn fill_bringup_pattern_writes_surface_pixels_only() {
        let mut backing = [0u32; 8];
        let surface = SurfaceConfig {
            base_addr: PhysAddr(backing.as_mut_ptr() as u64),
            width: 2,
            height: 2,
            stride: 4,
            v_stride: 2,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: TilingMode::Linear,
            rotation: Rotation::None,
            pixel_format: PixelFormat::Xrgb8888,
        };
        // SAFETY: `backing` covers a 2x2 surface with stride 4 u32 pixels.
        unsafe { surface.fill_bringup_pattern().unwrap() };
        assert_ne!(backing[0], 0);
        assert_ne!(backing[1], 0);
        assert_eq!(backing[2], 0);
        assert_eq!(backing[3], 0);
        assert_ne!(backing[4], 0);
        assert_ne!(backing[5], 0);
        assert_eq!(backing[6], 0);
        assert_eq!(backing[7], 0);
    }
}
