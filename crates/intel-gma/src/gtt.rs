//! Shared GTT aperture layout helpers.
//!
//! Mirrors libgfxinit's `Setup_Default_GTT`, `Valid_FB`/`Validate_FB` and the
//! fence routines in `common/hw-gfx-gma-registers.adb`.

use tock_registers::interfaces::{Readable, Writeable};

use crate::error::GmaError;
use crate::framebuffer::{FramebufferConfig, SurfaceConfig, TilingMode};
use crate::mmio::Mmio;
use crate::pci::GmaResources;
use crate::regs::{FENCE_LOWER, FENCE_UPPER, GFX_FLSH_CNTL, GfxFlushRegs, LegacyFenceRegs};
use crate::types::{Cpu, PhysAddr};

/// Intel GTT page size.
pub const GTT_PAGE_SIZE: u32 = 4096;

/// GTT page index of the rotated scanout alias.
///
/// libgfxinit: `GTT_Rotation_Offset := GTT_Range'Last / 2 + 1` over the full
/// 0x80000-page GTT address space, i.e. 262144. On a 512 KiB GTT this is beyond
/// the table, so `Validate_FB` rejects 90/270 rotation there, which is correct.
pub const GTT_ROTATION_OFFSET: u32 = 262_144;
/// Byte offset of the rotated alias inside the GTT address space.
pub const GTT_ROTATION_OFFSET_BYTES: u32 = GTT_ROTATION_OFFSET * GTT_PAGE_SIZE;

const FENCE_LEGACY_BASE: usize = 0x3000;
const FENCE_GEN3_BASE: usize = 0x2000;
const FENCE_COUNT_LEGACY: usize = 16;
const FENCE_PAGE_MASK: u32 = FENCE_LOWER::PAGE.val(0x000f_ffff).value;

/// MMIO-visible GTT page-table entry.
///
/// Kept as an untyped `ReadWrite<u32>` so chipset drivers can hand in raw
/// pointers to the GTT aperture; PTE bit layout lives in [`encode_gtt_pte`].
pub type GttPte = tock_registers::registers::ReadWrite<u32>;

/// GTT page-table entry encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GttPteEncoding {
    /// i945 (not Pineview): 32-bit PTE carrying only physical bits 31:12.
    I945Simple,
    /// Other 32-bit GTTs: physical bits 38:32 are carried in PTE bits 10:4.
    Bits32,
}

/// PTE encoding for a CPU, or `None` for platforms with 64-bit PTEs.
pub(crate) const fn gtt_pte_encoding(cpu: Cpu) -> Option<GttPteEncoding> {
    match cpu {
        Cpu::I945G | Cpu::I945GM => Some(GttPteEncoding::I945Simple),
        Cpu::Gm965 | Cpu::G45 | Cpu::Gm45 | Cpu::Pineview | Cpu::PineviewM | Cpu::Ironlake
        | Cpu::Sandybridge | Cpu::Ivybridge => Some(GttPteEncoding::Bits32),
        // Haswell and later use 64-bit PTEs; not modelled here.
        _ => None,
    }
}

/// Default MMIO-relative GTT PTE table offset, matching libgfxinit's
/// `MMIO_GTT_32_Offset` (`Has_I945_GTT_BAR` -> 0, GM965 -> 0x80000, other
/// 32-bit GTTs -> 0x200000).
pub(crate) const fn gtt_pte_offset(cpu: Cpu) -> Option<usize> {
    match cpu {
        Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM => Some(0x0000_0000),
        Cpu::Gm965 => Some(0x0008_0000),
        Cpu::G45 | Cpu::Gm45 | Cpu::Ironlake | Cpu::Sandybridge | Cpu::Ivybridge => {
            Some(0x0020_0000)
        }
        _ => None,
    }
}

/// PTE size in bytes for an encoding.
pub(crate) const fn gtt_pte_size(encoding: GttPteEncoding) -> usize {
    match encoding {
        GttPteEncoding::I945Simple | GttPteEncoding::Bits32 => 4,
    }
}

/// Encode one GTT page-table entry as a (low, high) word pair.
pub(crate) const fn encode_gtt_pte(
    phys: PhysAddr,
    encoding: GttPteEncoding,
) -> (u32, u32) {
    let base = (phys.0 & 0x000f_ffff_f000) as u32;
    match encoding {
        GttPteEncoding::I945Simple => (base | 1, 0),
        GttPteEncoding::Bits32 => {
            // Physical bits 38:32 land in PTE bits 10:4.
            let high = (((phys.0 >> 28) & 0x7f0) as u32) & 0x0000_07f0;
            (base | high | 1, 0)
        }
    }
}

/// Offset of the surface inside stolen memory (libgfxinit `Phys_Offset`).
///
/// For 90/270 rotation the scanout offset sits above the rotation alias, so the
/// alias base is subtracted to get the physical location.
pub(crate) fn physical_offset(surface: &SurfaceConfig) -> Result<u64, GmaError> {
    let offset = u64::from(surface.offset);
    if surface.rotation.is_90_or_270() {
        offset
            .checked_sub(u64::from(GTT_ROTATION_OFFSET_BYTES))
            .ok_or(GmaError::ModeUnavailable)
    } else {
        Ok(offset)
    }
}

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
    // libgfxinit `Validate_FB`: the framebuffer must fit both stolen memory and
    // the aperture. The GTT-entry bound is checked when the mapping is built.
    let region = resources.stolen_size.min(resources.gmadr_size);
    surface.validate_fits(region)?;
    Ok(surface)
}

/// Return the number of GTT PTEs required for `bytes`.
pub const fn ptes_for_bytes(bytes: u32) -> u32 {
    bytes.div_ceil(GTT_PAGE_SIZE)
}

/// Clear the MMIO-visible GTT aperture.
///
/// # Safety
///
/// `gtt_aperture` must point at the start of an MMIO-visible GTT table with at
/// least `entries * pte_words` writable `u32` slots.
pub unsafe fn clear_gtt(gtt_aperture: *mut GttPte, entries: usize) {
    for idx in 0..entries {
        // SAFETY: caller guarantees the MMIO-visible GTT table range.
        unsafe { (&*gtt_aperture.add(idx)).set(0) };
    }
}

/// Number of guard PTEs libgfxinit maps after a framebuffer to avoid buggy
/// VT-d prefetch/fetches just beyond the visible surface.
pub const GTT_DUMMY_PAGES_AFTER_FB: usize = 128;

/// Map a framebuffer surface into stolen memory through the GTT.
///
/// Matches libgfxinit's `Setup_Default_GTT`: PTEs start at the surface's
/// aperture page (`Phys_Offset / 4 KiB`, not page 0), physical addresses start
/// at `stolen_base + Phys_Offset`, 128 valid guard PTEs follow, and a rotated
/// Y-tiled alias is installed above `GTT_ROTATION_OFFSET` when requested.
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
    encoding: GttPteEncoding,
) -> Result<(), GmaError> {
    let bytes = surface.required_bytes()?;
    let pte_size = gtt_pte_size(encoding);
    let pte_words = pte_size / core::mem::size_of::<u32>();
    let phys_offset = physical_offset(surface)?;
    let first_page = (phys_offset / u64::from(GTT_PAGE_SIZE)) as usize;
    let pages = ptes_for_bytes(bytes) as usize;
    if pages == 0 || gtt_entries == 0 {
        return Err(GmaError::GttSetupFailed);
    }
    let last_page = first_page
        .checked_add(pages)
        .and_then(|v| v.checked_sub(1))
        .ok_or(GmaError::GttSetupFailed)?;
    let rotation = surface.rotation.is_90_or_270();
    let rotation_pages = if rotation {
        GTT_ROTATION_OFFSET as usize
    } else {
        0
    };

    // libgfxinit `Validate_FB`.
    let table_entries = gtt_entries / pte_words;
    if last_page + 1 + GTT_DUMMY_PAGES_AFTER_FB + rotation_pages > table_entries {
        return Err(GmaError::GttSetupFailed);
    }
    if (last_page as u64) >= u64::from(resources.stolen_size) / u64::from(GTT_PAGE_SIZE) {
        return Err(GmaError::GttSetupFailed);
    }
    if (last_page as u64) >= u64::from(resources.gmadr_size) / u64::from(GTT_PAGE_SIZE) {
        return Err(GmaError::GttSetupFailed);
    }

    let base_phys = resources
        .stolen_base
        .0
        .checked_add(phys_offset)
        .ok_or(GmaError::GttSetupFailed)?;
    // SAFETY: bounds were checked against `gtt_entries` above.
    unsafe {
        write_gtt_run(
            gtt_aperture,
            first_page,
            pages,
            base_phys,
            encoding,
            pte_words,
        );
    }
    let guard_phys = base_phys + (pages as u64) * u64::from(GTT_PAGE_SIZE);
    // SAFETY: guard PTEs are covered by the `last_page + 1 + 128` bound above.
    unsafe {
        write_gtt_run(
            gtt_aperture,
            last_page + 1,
            GTT_DUMMY_PAGES_AFTER_FB,
            guard_phys,
            encoding,
            pte_words,
        );
    }

    if rotation && surface.tiling == TilingMode::Y && surface.v_stride >= 32 {
        // SAFETY: the rotation bound was checked above.
        unsafe {
            map_rotated_y_tiled_alias(
                surface,
                gtt_aperture,
                pte_words,
                encoding,
                first_page,
                pages,
                base_phys,
            )?;
        }
    }

    // Order the PTE stream before the caller flushes GFX_FLSH_CNTL.
    // SAFETY: `pages > 0` and the range was bounds-checked.
    let _ = unsafe { (&*gtt_aperture.add((last_page) * pte_words)).get() };
    Ok(())
}

/// Write `count` sequential valid PTEs starting at `first_page`.
///
/// # Safety
///
/// The caller must have bounds-checked `first_page .. first_page + count`.
unsafe fn write_gtt_run(
    gtt_aperture: *mut GttPte,
    first_page: usize,
    count: usize,
    base_phys: u64,
    encoding: GttPteEncoding,
    pte_words: usize,
) {
    for idx in 0..count {
        let phys = PhysAddr(base_phys + (idx as u64) * u64::from(GTT_PAGE_SIZE));
        let (low, high) = encode_gtt_pte(phys, encoding);
        let slot = first_page + idx;
        // SAFETY: caller bounds-checked the range.
        unsafe {
            (&*gtt_aperture.add(slot * pte_words)).set(low);
            if pte_words == 2 {
                (&*gtt_aperture.add(slot * pte_words + 1)).set(high);
            }
        }
    }
}

/// Map the selected framebuffer surface using the resources and CPU encoding.
pub(crate) fn map_surface_to_stolen(
    resources: &GmaResources,
    cpu: Cpu,
    surface: &SurfaceConfig,
) -> Result<(), GmaError> {
    let encoding = gtt_pte_encoding(cpu).ok_or(GmaError::UnsupportedPlatform)?;
    let gtt_base = match resources.gtt_pte_base {
        Some(base) => base.0,
        // i945 exposes its GTT through a separate BAR that the chipset driver
        // must supply; the MMIO-relative offset is the fallback for the rest.
        None => {
            let offset = gtt_pte_offset(cpu).ok_or(GmaError::UnsupportedPlatform)?;
            resources.gtt_mmio_base.0 + offset as u64
        }
    };
    let gtt_entries = (resources.gtt_size / gtt_pte_size(encoding) as u32) as usize;
    // SAFETY: generation callers invoke this only after board/chipset code has
    // supplied a decoded display MMIO BAR; resources describe the MMIO-visible
    // GTT table and selected stolen-memory framebuffer surface.
    unsafe {
        map_framebuffer_to_stolen(
            resources,
            surface,
            gtt_base as *mut GttPte,
            gtt_entries,
            encoding,
        )
    }
}

/// Install the rotated Y-tiled alias above `GTT_ROTATION_OFFSET`.
///
/// # Safety
///
/// The caller must have validated that the alias range fits `gtt_entries`.
#[allow(clippy::too_many_arguments)]
unsafe fn map_rotated_y_tiled_alias(
    surface: &SurfaceConfig,
    gtt_aperture: *mut GttPte,
    pte_words: usize,
    encoding: GttPteEncoding,
    first_page: usize,
    pages: usize,
    base_phys: u64,
) -> Result<(), GmaError> {
    let v_pages = surface.v_stride / 32;
    if v_pages == 0 {
        return Err(GmaError::GttSetupFailed);
    }
    let bytes_per_row = surface
        .stride_bytes()?
        .checked_mul(32)
        .ok_or(GmaError::GttSetupFailed)?;
    let mut phys = base_phys
        .checked_add(u64::from(surface.required_bytes()?))
        .ok_or(GmaError::GttSetupFailed)?;
    let alias_base = GTT_ROTATION_OFFSET as usize;
    for page in 0..pages {
        phys = phys
            .checked_sub(u64::from(bytes_per_row))
            .ok_or(GmaError::GttSetupFailed)?;
        let (low, high) = encode_gtt_pte(PhysAddr(phys), encoding);
        let slot = alias_base + first_page + page;
        // SAFETY: caller validated the alias range.
        unsafe {
            (&*gtt_aperture.add(slot * pte_words)).set(low);
            if pte_words == 2 {
                (&*gtt_aperture.add(slot * pte_words + 1)).set(high);
            }
        }
        if ((page as u32) + 1).is_multiple_of(v_pages) {
            phys = phys
                .checked_add(u64::from(GTT_PAGE_SIZE))
                .and_then(|v| v.checked_add(u64::from(v_pages) * u64::from(bytes_per_row)))
                .ok_or(GmaError::GttSetupFailed)?;
        }
    }
    for idx in 0..GTT_DUMMY_PAGES_AFTER_FB {
        let (low, high) = encode_gtt_pte(PhysAddr(phys), encoding);
        let slot = alias_base + first_page + pages + idx;
        // SAFETY: caller validated the alias range.
        unsafe {
            (&*gtt_aperture.add(slot * pte_words)).set(low);
            if pte_words == 2 {
                (&*gtt_aperture.add(slot * pte_words + 1)).set(high);
            }
        }
    }
    Ok(())
}

const fn uses_gen3_fences(cpu: Cpu) -> bool {
    matches!(cpu, Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM)
}

/// Clear legacy fence registers for a CPU's fence layout.
pub(crate) fn clear_legacy_fences(mmio: &Mmio, cpu: Cpu) {
    if uses_gen3_fences(cpu) {
        for fence in 0..FENCE_COUNT_LEGACY {
            mmio.write32(gen3_fence_offset(fence), 0);
        }
    } else {
        for fence in 0..FENCE_COUNT_LEGACY {
            legacy_fence_regs(mmio, fence).lower.set(0);
        }
    }
}

/// Program the first free legacy fence register for a tiled surface.
pub(crate) fn add_legacy_fence(
    mmio: &Mmio,
    cpu: Cpu,
    surface: &SurfaceConfig,
) -> Result<(), GmaError> {
    if !surface.tiling.needs_fence() {
        return Ok(());
    }
    let first_page = surface.first_page()?;
    let last_page = surface.last_page()?;
    let pitch = surface.fence_pitch()?;
    if uses_gen3_fences(cpu) {
        return add_gen3_fence(mmio, surface, first_page, last_page, pitch);
    }
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

/// Gen3 fences are one 32-bit register each, split 0-7 at 0x2000 and 8-15 at
/// 0x3000, with Y at bit 12, log2(size in MB) at 11:8 and log2(pitch tiles) at
/// 7:4 (`common/hw-gfx-gma-registers.adb` `Add_Fence`).
fn add_gen3_fence(
    mmio: &Mmio,
    surface: &SurfaceConfig,
    first_page: u32,
    last_page: u32,
    pitch: u32,
) -> Result<(), GmaError> {
    let y_tiled = surface.tiling == TilingMode::Y;
    // i945 X tiling uses 512-byte tiles, Y tiling 128-byte tiles.
    let tile_bytes = if y_tiled { 128 } else { 512 };
    let stride_tiles = pitch / tile_bytes;
    let size_pages = last_page.saturating_sub(first_page) + 1;
    let size_mb = size_pages / 256;
    for fence in 0..FENCE_COUNT_LEGACY {
        let offset = gen3_fence_offset(fence);
        let current = mmio.read32(offset);
        if (current & FENCE_LOWER::VALID::SET.value) == 0 {
            let size_bits = if size_mb >= 1 {
                floor_log2(size_mb)
            } else {
                0
            };
            let pitch_bits = if stride_tiles >= 1 {
                floor_log2(stride_tiles)
            } else {
                0
            };
            let value = (first_page << 12)
                | if y_tiled { 1 << 12 } else { 0 }
                | (size_bits << 8)
                | (pitch_bits << 4)
                | 1;
            mmio.write32(offset, value);
            return Ok(());
        }
    }
    Err(GmaError::GttSetupFailed)
}

const fn gen3_fence_offset(fence: usize) -> usize {
    FENCE_GEN3_BASE + (fence / 8) * 0x1000 + (fence % 8) * 4
}

const fn floor_log2(value: u32) -> u32 {
    31 - value.leading_zeros()
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
    unsafe { mmio.reg_block::<LegacyFenceRegs>(FENCE_LEGACY_BASE + fence * 8) }
}

/// Flush legacy graphics/GTT writes through GFX_FLSH_CNTL.
pub(crate) fn flush_gfx(mmio: &Mmio) {
    // SAFETY: GFX_FLSH_CNTL is a fixed register in the decoded display MMIO BAR.
    let regs = unsafe { mmio.reg_block::<GfxFlushRegs>(GfxFlushRegs::OFFSET) };
    regs.control.write(GFX_FLSH_CNTL::RAW.val(0));
    let _ = regs.control.get();
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

    fn map(ptes: &mut [u32], surface: &SurfaceConfig) -> Result<(), GmaError> {
        unsafe {
            map_framebuffer_to_stolen(
                &resources(),
                surface,
                ptes.as_mut_ptr() as *mut GttPte,
                ptes.len(),
                GttPteEncoding::Bits32,
            )
        }
    }

    #[test]
    fn zero_sized_surface_rejects_without_pte_underflow() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 0, 0, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 1];
        assert_eq!(map(&mut ptes, &surface), Err(GmaError::GttSetupFailed));
        assert_eq!(ptes[0], 0);
    }

    #[test]
    fn zero_gtt_entries_rejects_even_for_valid_surface() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 1];
        assert_eq!(
            unsafe {
                map_framebuffer_to_stolen(
                    &resources(),
                    &surface,
                    ptes.as_mut_ptr() as *mut GttPte,
                    0,
                    GttPteEncoding::Bits32,
                )
            },
            Err(GmaError::GttSetupFailed)
        );
        assert_eq!(ptes[0], 0);
    }

    #[test]
    fn maps_libgfxinit_style_dummy_pages_after_framebuffer() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let visible = ptes_for_bytes(surface.required_bytes().unwrap()) as usize;
        let total = visible + GTT_DUMMY_PAGES_AFTER_FB;
        let mut ptes = [0u32; 132];
        assert_eq!(visible, 4);
        assert_eq!(map(&mut ptes, &surface), Ok(()));
        let (expected, _) = encode_gtt_pte(resources().stolen_base, GttPteEncoding::Bits32);
        assert_eq!(ptes[0], expected);
        assert_eq!(ptes[visible - 1] & 1, 1);
        assert_eq!(ptes[total - 1] & 1, 1);
    }

    #[test]
    fn rejects_when_dummy_pages_do_not_fit_gtt() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 64, 64, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 4];
        assert_eq!(map(&mut ptes, &surface), Err(GmaError::GttSetupFailed));
        assert_eq!(ptes[0], 0);
    }

    #[test]
    fn rejects_when_stolen_memory_cannot_hold_the_surface() {
        // 1024x768x4 needs 3 MiB; shrink stolen to 1 MiB.
        let mut res = resources();
        res.stolen_size = 1024 * 1024;
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let mut ptes = [0u32; 4096];
        assert_eq!(
            unsafe {
                map_framebuffer_to_stolen(
                    &res,
                    &surface,
                    ptes.as_mut_ptr() as *mut GttPte,
                    ptes.len(),
                    GttPteEncoding::Bits32,
                )
            },
            Err(GmaError::GttSetupFailed)
        );
    }

    #[test]
    fn i945_simple_pte_omits_high_address_bits() {
        let phys = PhysAddr(0x1_2345_6000);
        let (simple, _) = encode_gtt_pte(phys, GttPteEncoding::I945Simple);
        let (wide, _) = encode_gtt_pte(phys, GttPteEncoding::Bits32);
        assert_eq!(simple & 0x0000_07f0, 0);
        assert_eq!(wide & 0x0000_07f0, 0x0010);
        assert_eq!(simple & 0xffff_f000, 0x2345_6000);
    }

    #[test]
    fn pte_base_offset_matches_libgfxinit_per_platform() {
        assert_eq!(gtt_pte_offset(Cpu::I945GM), Some(0));
        assert_eq!(gtt_pte_offset(Cpu::Pineview), Some(0));
        assert_eq!(gtt_pte_offset(Cpu::Gm965), Some(0x0008_0000));
        assert_eq!(gtt_pte_offset(Cpu::G45), Some(0x0020_0000));
        assert!(gtt_pte_offset(Cpu::Haswell).is_none());
        assert_eq!(gtt_pte_encoding(Cpu::I945G), Some(GttPteEncoding::I945Simple));
        assert_eq!(gtt_pte_encoding(Cpu::Pineview), Some(GttPteEncoding::Bits32));
        assert!(gtt_pte_encoding(Cpu::Skylake).is_none());
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
        // libgfxinit `Tile_Width (X_Tiled) = 128` u32 units = 512 bytes.
        assert_eq!(surface.validate_fits(1024 * 1024), Ok(()));
        assert_eq!(surface.fence_pitch(), Ok(1));
    }

    #[test]
    fn rotation_alias_does_not_fit_a_512kib_gtt() {
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
        let mut ptes = [0u32; 200_000];
        assert_eq!(map(&mut ptes, &surface), Err(GmaError::GttSetupFailed));
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
