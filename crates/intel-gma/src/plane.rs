//! Primary plane setup helpers for Intel GMA display initialization.
//!
//! This mirrors the split in libgfxinit's `Setup_Hires_Plane`: older GMCH
//! platforms use DSPADDR/DSPPOS/DSPSIZE, while i965+ style platforms use
//! DSPSURF plus optional linear/tile offsets.

use crate::error::GmaError;
use crate::framebuffer::SurfaceConfig;
use crate::regs::{DSPCNTR, DSPTILEOFF, PLANE_SIZE};
use crate::types::{Pipe, Plane};

/// Hardware programming model for a legacy primary plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaneAddressModel {
    /// Pre-i965/Pineview-style base address register (`DSPADDR`).
    Address,
    /// i965+/GM965-style surface register (`DSPSURF`).
    Surface,
}

/// Resolved primary plane configuration for one pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaneConfig {
    /// Primary plane selected for the pipe.
    pub plane: Plane,
    /// Pipe fed by the plane.
    pub pipe: Pipe,
    /// Address programming model.
    pub address_model: PlaneAddressModel,
    /// Framebuffer surface.
    pub surface: SurfaceConfig,
}

impl PlaneConfig {
    /// Build a primary plane configuration.
    pub const fn new(
        plane: Plane,
        pipe: Pipe,
        address_model: PlaneAddressModel,
        surface: SurfaceConfig,
    ) -> Self {
        Self {
            plane,
            pipe,
            address_model,
            surface,
        }
    }

    /// Return the scanline stride in bytes.
    pub fn stride_bytes(self) -> Result<u32, GmaError> {
        self.surface.stride_bytes()
    }

    /// Return DSPLINOFF for a linear surface.
    pub fn linear_offset_bytes(self) -> Result<u32, GmaError> {
        self.surface
            .start_y
            .checked_mul(self.surface.stride)
            .and_then(|pixels| pixels.checked_add(self.surface.start_x))
            .and_then(|pixels| pixels.checked_mul(self.surface.pixel_format.bytes_per_pixel()))
            .ok_or(GmaError::GttSetupFailed)
    }

    /// Return DSPTILEOFF for tiled or non-linear offset programming.
    pub const fn tile_offset(self) -> u32 {
        DSPTILEOFF::START_Y.val(self.surface.start_y).value
            | DSPTILEOFF::START_X.val(self.surface.start_x).value
    }

    /// Return legacy DSPCNTR tiling bits.
    pub const fn legacy_tiling_bits(self) -> u32 {
        match self.surface.tiling {
            crate::framebuffer::TilingMode::Linear => 0,
            crate::framebuffer::TilingMode::X => DSPCNTR::TILED_SURFACE::SET.value,
            // Legacy DSPCNTR cannot express Y tiling; libgfxinit leaves this as zero.
            crate::framebuffer::TilingMode::Y => 0,
        }
    }

    /// Encode libgfxinit-style width/height `value - 1` size fields.
    pub fn encoded_size(self) -> Result<u32, GmaError> {
        let width = u16::try_from(self.surface.width).map_err(|_| GmaError::ModeUnavailable)?;
        let height = u16::try_from(self.surface.height).map_err(|_| GmaError::ModeUnavailable)?;
        Ok(encode_size(width, height))
    }
}

/// Encode a plane size register value.
pub(crate) const fn encode_size(width: u16, height: u16) -> u32 {
    PLANE_SIZE::HEIGHT_MINUS_ONE.val((height as u32) - 1).value
        | PLANE_SIZE::WIDTH_MINUS_ONE.val((width as u32) - 1).value
}

/// Return the primary plane normally paired with a pipe on legacy GMCH.
pub(crate) const fn primary_for_pipe(pipe: Pipe) -> Plane {
    match pipe {
        Pipe::A => Plane::PrimaryA,
        Pipe::B => Plane::PrimaryB,
        Pipe::C => Plane::PrimaryC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::PixelFormat;
    use crate::types::PhysAddr;

    #[test]
    fn encodes_plane_size_and_stride() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let plane = PlaneConfig::new(
            Plane::PrimaryA,
            Pipe::A,
            PlaneAddressModel::Address,
            surface,
        );
        assert_eq!(plane.stride_bytes(), Ok(4096));
        assert_eq!(plane.encoded_size(), Ok(0x02ff_03ff));
    }

    #[test]
    fn maps_primary_plane_to_pipe() {
        assert_eq!(primary_for_pipe(Pipe::A), Plane::PrimaryA);
        assert_eq!(primary_for_pipe(Pipe::B), Plane::PrimaryB);
    }
}
