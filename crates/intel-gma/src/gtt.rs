//! Shared GTT aperture layout helpers.

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::registers::ReadWrite;

use crate::error::GmaError;
use crate::framebuffer::{FramebufferConfig, SurfaceConfig, TilingMode};
use crate::mmio::Mmio;
use crate::pci::GmaResources;
use crate::regs::{
    FENCE_LOWER, FENCE_UPPER, GFX_FLSH_CNTL, GTT_PTE, GfxFlushRegs, LegacyFenceRegs,
};
use crate::types::PhysAddr;

/// Intel GTT page size.
pub const GTT_PAGE_SIZE: u32 = 4096;
/// GTT page offset used by libgfxinit for rotated scanout aliases.
pub const GTT_ROTATION_OFFSET: u32 = 256 * 1024 / GTT_PAGE_SIZE;
/// Byte offset of the rotated alias in the GMADR aperture.
pub const GTT_ROTATION_OFFSET_BYTES: u32 = GTT_ROTATION_OFFSET * GTT_PAGE_SIZE;

const FENCE_BASE_LEGACY: usize = 0x3000;
const FENCE_COUNT_LEGACY: usize = 16;
const FENCE_PAGE_MASK: u32 = FENCE_LOWER::PAGE.val(0x000f_ffff).value;

/// MMIO-visible GTT page-table entry.
///
/// Kept as an untyped `ReadWrite<u32>` so chipset drivers can hand in raw
/// pointers to the GTT aperture; PTE bit layout lives in [`legacy_pte`].
pub type GttPte = ReadWrite<u32>;

/// Choose and validate the initial framebuffer surface.
pub fn choose_framebuffer_surface(
    resources: &GmaResources,
    config: &FramebufferConfig,
) -> Result<SurfaceConfig, GmaError> {
    let gmadr_base = resources.gmadr_base.ok_or(GmaError::GttSetupFailed)?;
    let format = config.pixel_format()?;
    let stride = config.stride.unwrap_or(config.width);
    let v_stride = config.v_stride.unwrap_or(config.height);
    let surface = SurfaceConfig {
        base_addr: PhysAddr(gmadr_base.0 + u64::from(config.offset)),
        width: config.width,
        height: config.height,
        stride,
        v_stride,
        start_x: config.start_x,
        start_y: config.start_y,
        offset: config.offset,
        tiling: config.tiling,
        rotation: config.rotation,
        pixel_format: format,
    };
    let usable_stolen = resources.stolen_size.saturating_sub(resources.gtt_size);
    let region_size = resources.gmadr_size.min(usable_stolen);
    surface.validate_fits(region_size)?;
    Ok(surface)
}

/// Return the number of GTT PTEs required for `bytes`.
pub const fn ptes_for_bytes(bytes: u32) -> u32 {
    bytes.div_ceil(GTT_PAGE_SIZE)
}

/// Build a legacy Intel GTT PTE for a physical page.
pub const fn legacy_pte(phys: PhysAddr) -> u32 {
    GTT_PTE::ADDR
        .val(((phys.0 >> 12) & 0x000f_ffff) as u32)
        .value
        | GTT_PTE::VALID::SET.value
}

/// Clear the MMIO-visible GTT aperture.
///
/// # Safety
///
/// `gtt_aperture` must point at the start of an MMIO-visible GTT table with at
/// least `entries` `u32` PTE slots.
pub unsafe fn clear_gtt(gtt_aperture: *mut GttPte, entries: usize) {
    for idx in 0..entries {
        // SAFETY: caller guarantees the MMIO-visible GTT table range.
        unsafe { (&*gtt_aperture.add(idx)).set(0) };
    }
}

/// Number of guard PTEs libgfxinit maps after a framebuffer to avoid buggy
/// VT-d prefetch/fetches just beyond the visible surface.
pub const GTT_DUMMY_PAGES_AFTER_FB: usize = 128;

/// Map the selected framebuffer surface to the first GMADR aperture pages.
///
/// The first framebuffer bytes are backed by the beginning of stolen memory.
/// This matches the initial fstart policy of using stolen memory for GOP-style
/// linear framebuffer writes.  As in libgfxinit, the mapper also installs 128
/// valid guard PTEs after the framebuffer when the GTT and stolen-memory ranges
/// can cover them.
///
/// # Safety
///
/// `gtt_aperture` must point at a writable MMIO-visible GTT table for the
/// platform, and `gtt_entries` must describe its valid PTE count.
pub unsafe fn map_framebuffer_to_stolen(
    resources: &GmaResources,
    surface: &SurfaceConfig,
    gtt_aperture: *mut GttPte,
    gtt_entries: usize,
) -> Result<(), GmaError> {
    let bytes = surface.required_bytes()?;
    let usable_stolen = resources.stolen_size.saturating_sub(resources.gtt_size);
    let first_page = surface.first_page()? as usize;
    let needed = ptes_for_bytes(bytes) as usize;
    let guard_pages = GTT_DUMMY_PAGES_AFTER_FB;
    let mapped = needed
        .checked_add(guard_pages)
        .ok_or(GmaError::GttSetupFailed)?;
    let mapped_bytes = (mapped as u64)
        .checked_mul(u64::from(GTT_PAGE_SIZE))
        .ok_or(GmaError::GttSetupFailed)?;
    if needed == 0
        || gtt_entries == 0
        || first_page
            .checked_add(mapped)
            .ok_or(GmaError::GttSetupFailed)?
            > gtt_entries
        || bytes > usable_stolen
        || mapped_bytes > u64::from(usable_stolen)
    {
        return Err(GmaError::GttSetupFailed);
    }
    for idx in 0..mapped {
        let phys = PhysAddr(resources.stolen_base.0 + (idx as u64 * u64::from(GTT_PAGE_SIZE)));
        // SAFETY: caller guarantees the MMIO-visible GTT table range.
        unsafe { (&*gtt_aperture.add(first_page + idx)).set(legacy_pte(phys)) };
    }

    if surface.rotation.is_90_or_270() && surface.tiling == TilingMode::Y && surface.v_stride >= 32
    {
        map_rotated_y_tiled_alias(
            resources,
            surface,
            gtt_aperture,
            gtt_entries,
            needed,
            mapped,
        )?;
    }

    // Linux/coreboot-style legacy GMCH setup relies on posted MMIO writes
    // reaching the GTT before the graphics flush. Read back the final PTE we
    // touched to order the PTE stream before the caller flushes GFX_FLSH_CNTL.
    // SAFETY: `mapped > 0`, and bounds were checked against `gtt_entries` above.
    let _ = unsafe { (&*gtt_aperture.add(first_page + mapped - 1)).get() };
    Ok(())
}

/// Map the selected framebuffer surface using the GTT aperture described by resources.
///
/// This is the common safe wrapper used by generation code after chipset setup
/// has validated and decoded the display MMIO BAR.
pub(crate) fn map_surface_to_stolen(
    resources: &GmaResources,
    surface: &SurfaceConfig,
) -> Result<(), GmaError> {
    let gtt_base = resources
        .gtt_pte_base
        .map(|base| base.0)
        .unwrap_or(resources.gtt_mmio_base.0 + 512 * 1024);
    let gtt_entries = (resources.gtt_size / core::mem::size_of::<u32>() as u32) as usize;
    // SAFETY: generation callers invoke this only after board/chipset code has
    // supplied a decoded display MMIO BAR; resources describe the MMIO-visible
    // GTT table and selected stolen-memory framebuffer surface.
    unsafe { map_framebuffer_to_stolen(resources, surface, gtt_base as *mut GttPte, gtt_entries) }
}

fn map_rotated_y_tiled_alias(
    resources: &GmaResources,
    surface: &SurfaceConfig,
    gtt_aperture: *mut GttPte,
    gtt_entries: usize,
    needed: usize,
    mapped: usize,
) -> Result<(), GmaError> {
    let first_page = surface.first_page()? as usize;
    let rotation_first = GTT_ROTATION_OFFSET as usize + first_page;
    if rotation_first
        .checked_add(mapped)
        .ok_or(GmaError::GttSetupFailed)?
        > gtt_entries
    {
        return Err(GmaError::GttSetupFailed);
    }

    let v_pages = surface.v_stride / 32;
    if v_pages == 0 {
        return Err(GmaError::GttSetupFailed);
    }
    let bytes_per_row = surface
        .stride_bytes()?
        .checked_mul(32)
        .ok_or(GmaError::GttSetupFailed)?;
    let mut phys = resources
        .stolen_base
        .0
        .checked_add(u64::from(surface.required_bytes()?))
        .ok_or(GmaError::GttSetupFailed)?;
    for page in 0..needed {
        phys = phys
            .checked_sub(u64::from(bytes_per_row))
            .ok_or(GmaError::GttSetupFailed)?;
        // SAFETY: caller guarantees the MMIO-visible GTT table range and bounds are checked above.
        unsafe { (&*gtt_aperture.add(rotation_first + page)).set(legacy_pte(PhysAddr(phys))) };
        if ((page as u32) + 1).is_multiple_of(v_pages) {
            phys = phys
                .checked_add(u64::from(GTT_PAGE_SIZE))
                .and_then(|v| v.checked_add(u64::from(v_pages) * u64::from(bytes_per_row)))
                .ok_or(GmaError::GttSetupFailed)?;
        }
    }
    for idx in needed..mapped {
        // SAFETY: bounds are checked above.
        unsafe { (&*gtt_aperture.add(rotation_first + idx)).set(legacy_pte(PhysAddr(phys))) };
    }
    Ok(())
}

/// Clear legacy fence registers.
pub(crate) fn clear_legacy_fences(mmio: &Mmio) {
    for fence in 0..FENCE_COUNT_LEGACY {
        legacy_fence_regs(mmio, fence).lower.set(0);
    }
}

/// Program the first free legacy fence register for a tiled surface.
pub(crate) fn add_legacy_fence(mmio: &Mmio, surface: &SurfaceConfig) -> Result<(), GmaError> {
    if !surface.tiling.needs_fence() {
        return Ok(());
    }
    let first_page = surface.first_page()?;
    let last_page = surface.last_page()?;
    let pitch = surface.fence_pitch()?;
    for fence in 0..FENCE_COUNT_LEGACY {
        let regs = legacy_fence_regs(mmio, fence);
        if !regs.lower.is_set(FENCE_LOWER::VALID) {
            let y_major = if surface.tiling == TilingMode::Y {
                FENCE_LOWER::TILE_WALK_YMAJOR::SET.value
            } else {
                0
            };
            let pitch_field = pitch
                .checked_mul(if surface.tiling == TilingMode::Y {
                    1
                } else {
                    4
                })
                .and_then(|v| v.checked_sub(1))
                .ok_or(GmaError::GttSetupFailed)?;
            regs.lower.set(
                FENCE_LOWER::PAGE.val(first_page).value | y_major | FENCE_LOWER::VALID::SET.value,
            );
            regs.upper
                .write(FENCE_UPPER::PAGE.val(last_page) + FENCE_UPPER::PITCH.val(pitch_field));
            return Ok(());
        }
    }
    Err(GmaError::GttSetupFailed)
}

/// Remove a fence matching the given surface if one is present.
#[allow(dead_code)]
pub(crate) fn remove_legacy_fence(mmio: &Mmio, surface: &SurfaceConfig) -> Result<(), GmaError> {
    let page_lower = FENCE_LOWER::PAGE.val(surface.first_page()?).value;
    let page_upper = FENCE_UPPER::PAGE.val(surface.last_page()?).value;
    for fence in 0..FENCE_COUNT_LEGACY {
        let regs = legacy_fence_regs(mmio, fence);
        let lower = regs.lower.get();
        let upper = regs.upper.get();
        if (lower & FENCE_PAGE_MASK) == page_lower && (upper & FENCE_PAGE_MASK) == page_upper {
            regs.lower.set(0);
            return Ok(());
        }
    }
    Ok(())
}

fn legacy_fence_regs(mmio: &Mmio, fence: usize) -> &'static LegacyFenceRegs {
    // SAFETY: the legacy fence table is a fixed MMIO register array in the
    // decoded GTTMMADR BAR; callers iterate only over `FENCE_COUNT_LEGACY`.
    unsafe { mmio.reg_block::<LegacyFenceRegs>(fence_lower_offset(fence)) }
}

/// Flush legacy graphics/GTT writes through GFX_FLSH_CNTL.
pub(crate) fn flush_gfx(mmio: &Mmio) {
    // SAFETY: GFX_FLSH_CNTL is a fixed register in the decoded display MMIO BAR.
    let regs = unsafe { mmio.reg_block::<GfxFlushRegs>(GfxFlushRegs::OFFSET) };
    regs.control.write(GFX_FLSH_CNTL::RAW.val(0));
    let _ = regs.control.get();
}

const fn fence_lower_offset(fence: usize) -> usize {
    FENCE_BASE_LEGACY + fence * 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::{PixelFormat, Rotation};
    use crate::types::PciBdf;

    fn resources() -> GmaResources {
        GmaResources {
            pci_bdf: PciBdf {
                bus: 0,
                dev: 2,
                func: 0,
            },
            gtt_mmio_base: PhysAddr(0xfeb0_0000),
            gtt_mmio_size: 512 * 1024,
            gtt_pte_base: None,
            gmadr_base: Some(PhysAddr(0xd000_0000)),
            gmadr_size: 256 * 1024 * 1024,
            stolen_base: PhysAddr(0xcff0_0000),
            stolen_size: 8 * 1024 * 1024,
            gtt_size: 512 * 1024,
            gcfgc: None,
        }
    }

    #[test]
    fn zero_sized_surface_rejects_without_pte_underflow() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 0, 0, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 1];
        let result = unsafe {
            map_framebuffer_to_stolen(
                &resources(),
                &surface,
                ptes.as_mut_ptr() as *mut GttPte,
                ptes.len(),
            )
        };
        assert_eq!(result, Err(GmaError::GttSetupFailed));
        assert_eq!(ptes[0], 0);
    }

    #[test]
    fn zero_gtt_entries_rejects_even_for_valid_surface() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 1];
        let result = unsafe {
            map_framebuffer_to_stolen(&resources(), &surface, ptes.as_mut_ptr() as *mut GttPte, 0)
        };
        assert_eq!(result, Err(GmaError::GttSetupFailed));
        assert_eq!(ptes[0], 0);
    }

    #[test]
    fn maps_libgfxinit_style_dummy_pages_after_framebuffer() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let visible = ptes_for_bytes(surface.required_bytes().unwrap()) as usize;
        let total = visible + GTT_DUMMY_PAGES_AFTER_FB;
        let mut ptes = [0u32; 132];
        assert_eq!(visible, 4);
        let result = unsafe {
            map_framebuffer_to_stolen(
                &resources(),
                &surface,
                ptes.as_mut_ptr() as *mut GttPte,
                ptes.len(),
            )
        };
        assert_eq!(result, Ok(()));
        assert_eq!(ptes[0], legacy_pte(resources().stolen_base));
        assert_eq!(ptes[visible - 1] & 1, 1);
        assert_eq!(ptes[total - 1] & 1, 1);
    }

    #[test]
    fn rejects_when_dummy_pages_do_not_fit_gtt() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 4];
        let result = unsafe {
            map_framebuffer_to_stolen(
                &resources(),
                &surface,
                ptes.as_mut_ptr() as *mut GttPte,
                ptes.len(),
            )
        };
        assert_eq!(result, Err(GmaError::GttSetupFailed));
        assert_eq!(ptes[0], 0);
    }
    #[test]
    fn x_tiled_surface_programs_fence_pitch_like_libgfxinit() {
        let surface = SurfaceConfig {
            base_addr: PhysAddr(0xd000_0000),
            width: 128,
            height: 64,
            stride: 128,
            v_stride: 64,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: TilingMode::X,
            rotation: Rotation::None,
            pixel_format: PixelFormat::Xrgb8888,
        };
        assert_eq!(surface.validate_fits(1024 * 1024), Ok(()));
        assert_eq!(surface.fence_pitch(), Ok(16));
    }

    #[test]
    fn rotated_y_tiled_surface_requires_rotation_alias_space() {
        let surface = SurfaceConfig {
            base_addr: PhysAddr(0xd000_0000 + u64::from(GTT_ROTATION_OFFSET_BYTES)),
            width: 64,
            height: 64,
            stride: 64,
            v_stride: 64,
            start_x: 0,
            start_y: 0,
            offset: GTT_ROTATION_OFFSET_BYTES,
            tiling: TilingMode::Y,
            rotation: Rotation::Rotate90,
            pixel_format: PixelFormat::Xrgb8888,
        };
        let visible = ptes_for_bytes(surface.required_bytes().unwrap()) as usize;
        let first = surface.first_page().unwrap() as usize;
        let mut ptes = [0u32; 512];
        unsafe {
            map_framebuffer_to_stolen(
                &resources(),
                &surface,
                ptes.as_mut_ptr() as *mut GttPte,
                ptes.len(),
            )
            .unwrap();
        }
        assert_eq!(first, GTT_ROTATION_OFFSET as usize);
        assert_eq!(ptes[first], legacy_pte(resources().stolen_base));
        assert_ne!(ptes[GTT_ROTATION_OFFSET as usize + first], 0);
        assert_eq!(visible, 4);
    }

    #[test]
    fn rotated_surface_without_alias_offset_is_rejected() {
        let surface = SurfaceConfig {
            base_addr: PhysAddr(0xd000_0000),
            width: 64,
            height: 64,
            stride: 64,
            v_stride: 64,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: TilingMode::Y,
            rotation: Rotation::Rotate90,
            pixel_format: PixelFormat::Xrgb8888,
        };
        assert_eq!(
            surface.validate_fits(1024 * 1024),
            Err(GmaError::ModeUnavailable)
        );
    }
}
