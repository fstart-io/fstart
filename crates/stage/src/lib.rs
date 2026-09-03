//! Common fixed-flow stage glue.
//!
//! Board crates expose a stage entry implementation owned by their platform
//! family flow. This crate provides shared anchor/allocation linkage and the
//! board-owned entry macro.

#![no_std]

#[cfg(any(
    feature = "acpi",
    feature = "ffs",
    feature = "crabefi",
    feature = "smbios"
))]
mod alloc;

mod runtime;

#[cfg(feature = "crabefi")]
pub use fstart_boot::crabefi;

pub mod fixed_helpers;
pub mod payload;

// Fixed-flow stage helpers moved here so stage build metadata is not a
// separate capability model.
#[cfg(feature = "fdt")]
mod fdt_patch;

#[cfg(feature = "fit")]
pub mod fit;

#[cfg(feature = "handoff")]
pub mod handoff;

pub mod next_stage;

// ---------------------------------------------------------------------------
// FDT blob utilities
// ---------------------------------------------------------------------------

/// FDT magic number (big-endian `0xD00DFEED`).
const FDT_MAGIC: u32 = 0xD00D_FEED;

/// Read an FDT (Flattened Device Tree) blob from a raw memory address.
///
/// Validates the FDT magic at offset +0 (`0xD00DFEED`), reads the
/// `totalsize` field at offset +4, and returns a static slice covering
/// the entire blob. Returns `None` if `addr` is zero or magic is wrong.
///
/// Used by the UEFI payload path to obtain the platform-provided FDT blob
/// for passing to CrabEFI.
///
/// # Safety
///
/// Caller must ensure `addr` points to at least 8 readable bytes in
/// memory. If the magic validates, the full `totalsize` bytes starting
/// at `addr` must be readable. The returned slice borrows the memory at
/// `addr` with `'static` lifetime -- the FDT must remain valid (e.g.,
/// in DRAM) for the program's duration.
pub unsafe fn fdt_blob_from_addr(addr: u64) -> Option<&'static [u8]> {
    if addr == 0 {
        return None;
    }
    let ptr = addr as *const u8;
    // SAFETY: caller guarantees at least 8 readable bytes at `addr`.
    let magic = u32::from_be(unsafe { core::ptr::read_unaligned(ptr as *const u32) });
    if magic != FDT_MAGIC {
        return None;
    }
    // SAFETY: magic validated; caller guarantees the full blob is readable
    // and remains valid for 'static.
    let size =
        u32::from_be(unsafe { core::ptr::read_unaligned(ptr.add(4) as *const u32) }) as usize;
    Some(unsafe { core::slice::from_raw_parts(ptr, size) })
}

#[cfg(any(feature = "ffs", feature = "fdt"))]
use fstart_log::Hex;

#[cfg(feature = "ffs")]
use fstart_core::services::BootMedia;

// ---------------------------------------------------------------------------
// SigVerify
// ---------------------------------------------------------------------------

/// Verify the firmware filesystem manifest signature.
///
/// Reads the FFS anchor from the embedded `FSTART_ANCHOR` static (via
/// volatile read to see post-build patched values), then verifies the
/// manifest signature and file digests.
///
/// Generic over [`BootMedia`] — for memory-mapped flash this compiles
/// down to the same code as a direct `FfsReader` with zero overhead.
/// For block devices, metadata is read into stack buffers.
///
/// # Arguments
///
/// - `anchor_data`: Reference to the `FSTART_ANCHOR` static (raw bytes).
/// - `media`: The boot medium holding the firmware image.
#[cfg(feature = "ffs")]
pub fn sig_verify(anchor_data: &[u8], media: &(impl BootMedia + ?Sized)) {
    fstart_log::info!("stage helper: SigVerify");

    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::info!("sig verify: no flash image configured, skipping");
        return;
    }

    // Volatile-read the anchor to see the post-build patched values
    // SAFETY: FSTART_ANCHOR is emitted by this crate with proper alignment
    // and size (>= ANCHOR_SIZE) in the .fstart.anchor linker section.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!("sig verify: failed to read anchor: {}", reader_error_str(e));
            return;
        }
    };

    // Log anchor info at a moderate verbosity level
    fstart_log::info!(
        "sig verify: image_size={} key_count={}",
        Hex(anchor.total_image_size as u64),
        anchor.key_count
    );

    // Read and verify the manifest
    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!(
                "sig verify: manifest verification FAILED: {}",
                reader_error_str(e)
            );
            return;
        }
    };

    let summary = manifest.summary();

    fstart_log::info!(
        "sig verify: manifest signature verified ({} regions, {} entries); file digests verify after load",
        summary.regions,
        summary.entries
    );
}

/// Stub SigVerify when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn sig_verify(_anchor_data: &[u8], _media: &(impl fstart_core::services::BootMedia + ?Sized)) {
    fstart_log::info!("stage helper: SigVerify");
    fstart_log::info!("sig verify skipped (ffs feature not enabled)");
}

// ---------------------------------------------------------------------------
// Anchor Scanning
// ---------------------------------------------------------------------------

/// Errors from anchor scanning operations.
#[cfg(feature = "ffs")]
#[derive(Debug)]
pub enum AnchorScanError {
    /// Boot media does not support `as_slice()` (not memory-mapped).
    NotMemoryMapped,
    /// FFS anchor magic not found in the media.
    NotFound,
}

/// Scan memory-mapped boot media for the FFS anchor block.
///
/// Searches for [`FFS_MAGIC`](fstart_core::ffs::FFS_MAGIC) at 8-byte
/// aligned offsets in the media.  Returns the anchor data as a
/// fixed-size array on success.
///
/// Shared helper for non-first stages in memory-mapped multi-stage builds.
///
/// # Errors
///
/// - [`AnchorScanError::NotMemoryMapped`] if the media does not support
///   `as_slice()` (block device media).
/// - [`AnchorScanError::NotFound`] if the FFS magic is not found.
#[cfg(feature = "ffs")]
pub fn scan_anchor_in_media(
    media: &impl BootMedia,
) -> Result<[u8; fstart_core::ffs::ANCHOR_SIZE], AnchorScanError> {
    let media_slice = media.as_slice().ok_or(AnchorScanError::NotMemoryMapped)?;

    // Linear scan at 8-byte alignment. The anchor is typically near
    // the end of the FFS image, but the image offset within the media
    // is unknown to non-first stages, so we scan from the start.
    // For memory-mapped flash this is cache-friendly sequential reads.
    let magic = &fstart_core::ffs::FFS_MAGIC;
    let mut offset = 0usize;
    while offset + fstart_core::ffs::ANCHOR_SIZE <= media_slice.len() {
        if &media_slice[offset..offset + magic.len()] == magic {
            let mut buf = [0u8; fstart_core::ffs::ANCHOR_SIZE];
            buf.copy_from_slice(&media_slice[offset..offset + fstart_core::ffs::ANCHOR_SIZE]);
            fstart_log::info!(
                "FFS anchor found at offset {:#x} in boot media",
                offset as u64
            );
            return Ok(buf);
        }
        offset += 8;
    }
    Err(AnchorScanError::NotFound)
}

/// Read the FFS anchor from a block device at a known offset.
///
/// For block device media (e.g., SD/MMC), the FFS assembler patches
/// the total FFS image size into the eGON header.  The anchor is at
/// `ffs_total_size - ANCHOR_SIZE`.
///
/// This function reads the anchor at the given offset, verifies the
/// magic bytes, and returns the anchor data.
///
/// # Errors
///
/// Returns `Err` if the read fails or the magic bytes don't match.
#[cfg(feature = "ffs")]
pub fn read_anchor_at_offset(
    media: &impl BootMedia,
    anchor_offset: usize,
) -> Result<[u8; fstart_core::ffs::ANCHOR_SIZE], AnchorScanError> {
    let mut buf = [0u8; fstart_core::ffs::ANCHOR_SIZE];
    media
        .read_at(anchor_offset, &mut buf)
        .map_err(|_| AnchorScanError::NotFound)?;

    // Verify the magic bytes are present.
    let magic = &fstart_core::ffs::FFS_MAGIC;
    if buf[..magic.len()] != *magic {
        return Err(AnchorScanError::NotFound);
    }

    fstart_log::info!("FFS anchor read at offset {:#x}", anchor_offset as u64);
    Ok(buf)
}

// ---------------------------------------------------------------------------
// FdtPrepare
// ---------------------------------------------------------------------------

/// Prepare a Flattened Device Tree for OS handoff.
///
/// Copies the source DTB to the destination address (if they differ),
/// then patches the raw FDT blob:
/// 1. Sets `/chosen/bootargs` to the provided kernel command line.
/// 2. Creates or updates `/memory@<base>` with `device_type` and `reg`
///    properties if `dram_base` and `dram_size` are non-zero.
///
/// Uses [`fdt_patch::fdt_set_bootargs`] and [`fdt_patch::fdt_set_memory`]
/// — no heap allocation, no full-tree conversion.
///
/// Works on any valid DTB that already contains a `/chosen` node (all
/// standard Linux DTBs do).
///
/// # Arguments
///
/// - `src_dtb_addr` — address of the source DTB (0 = skip)
/// - `dst_dtb_addr` — target address for the patched DTB
/// - `bootargs` — kernel command line to set in `/chosen/bootargs` (empty = skip)
/// - `dram_base` — physical base address of DRAM (0 = skip memory patching)
/// - `dram_size` — DRAM size in bytes (0 = skip memory patching)
#[cfg(feature = "fdt")]
pub fn fdt_prepare_platform(
    src_dtb_addr: u64,
    dst_dtb_addr: u64,
    bootargs: &str,
    dram_base: u64,
    dram_size: u64,
) {
    fstart_log::info!("stage helper: FdtPrepare");

    if src_dtb_addr == 0 {
        fstart_log::info!("FDT: no source DTB, skipping");
        return;
    }

    if dst_dtb_addr == 0 {
        fstart_log::error!("FDT: dst_dtb_addr is 0 (misconfigured board?)");
        return;
    }

    // Validate FDT magic and read totalsize from the source header.
    let src_ptr = src_dtb_addr as *const u8;
    let magic = {
        // SAFETY: src_dtb_addr is assumed to point to readable memory.
        let raw = unsafe { core::ptr::read_volatile(src_ptr as *const u32) };
        u32::from_be(raw)
    };
    if magic != 0xD00D_FEED {
        fstart_log::error!(
            "FDT: invalid magic at {}: expected 0xD00DFEED, got {}",
            Hex(src_dtb_addr),
            Hex(magic as u64),
        );
        return;
    }
    let totalsize = {
        let raw = unsafe { core::ptr::read_volatile(src_ptr.add(4) as *const u32) };
        u32::from_be(raw) as usize
    };

    // QEMU's arm_load_dtb() inflates totalsize to the whole DTB reservation
    // (fdt_open_into over 1 MiB) so guests can edit in place. Copying that
    // much tramples whatever the board packed above the DTB window (e.g. the
    // kernel 1 MiB up on armv7 virt). Copy only the used bytes and shrink
    // the destination header to match.
    let used = fdt_used_size(src_ptr, totalsize);

    // Copy source to destination if they differ.
    let dst_ptr = dst_dtb_addr as *mut u8;
    let totalsize = if src_dtb_addr != dst_dtb_addr {
        fstart_log::info!(
            "FDT: copying {} of {} bytes to {}",
            used,
            totalsize,
            Hex(dst_dtb_addr)
        );
        // SAFETY: both regions are in DRAM, non-overlapping (board config
        // must ensure this), and `used <= totalsize` bytes are
        // readable/writable.
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, used);
            core::ptr::write_volatile(dst_ptr.add(4) as *mut u32, u32::to_be(used as u32));
        }
        used
    } else {
        totalsize
    };

    // Allow 4 KiB headroom beyond the current DTB for property insertion,
    // node creation, and strings growth. The DTB sits in DRAM with plenty
    // of room.
    let max_size = totalsize + 4096;

    // Patch bootargs in the destination blob.
    if !bootargs.is_empty() {
        // SAFETY: dst_ptr points to a valid, writable FDT blob in DRAM.
        match unsafe { fdt_patch::fdt_set_bootargs(dst_ptr, max_size, bootargs) } {
            Ok(new_size) => {
                fstart_log::info!(
                    "FDT: patched bootargs ({} -> {} bytes)",
                    totalsize,
                    new_size
                );
            }
            Err(_e) => {
                fstart_log::error!("FDT: bootargs patch failed");
            }
        }
    }

    // Patch memory node if DRAM info is provided.
    if dram_base != 0 && dram_size != 0 {
        // Re-read totalsize after bootargs patching may have grown the blob.
        let current_totalsize = {
            let raw = unsafe { core::ptr::read_volatile(dst_ptr.add(4) as *const u32) };
            u32::from_be(raw) as usize
        };
        let max_size_mem = current_totalsize + 4096;

        // SAFETY: dst_ptr points to a valid, writable FDT blob in DRAM.
        match unsafe { fdt_patch::fdt_set_memory(dst_ptr, max_size_mem, dram_base, dram_size) } {
            Ok(new_size) => {
                fstart_log::info!(
                    "FDT: patched memory node ({} -> {} bytes, base={} size={}MB)",
                    current_totalsize,
                    new_size,
                    Hex(dram_base),
                    dram_size / (1024 * 1024),
                );
            }
            Err(_e) => {
                fstart_log::error!("FDT: memory node patch failed");
            }
        }
    }

    fstart_log::info!("FDT: ready at {}", Hex(dst_dtb_addr));
}

/// Actually used byte count of an FDT whose `totalsize` may be inflated.
///
/// Takes the maximum end offset of the three header-described sections
/// (memory reservation map walked entry-by-entry, structure block, strings
/// block), clamped to the claimed `totalsize`.
#[cfg(feature = "fdt")]
fn fdt_used_size(fdt: *const u8, totalsize: usize) -> usize {
    let be32 = |off: usize| -> usize {
        // SAFETY: caller guarantees `totalsize` readable bytes; header
        // offsets are within the mandatory 40-byte FDT header.
        let raw = unsafe { core::ptr::read_volatile(fdt.add(off) as *const u32) };
        u32::from_be(raw) as usize
    };
    let off_dt_struct = be32(8);
    let off_dt_strings = be32(12);
    let off_mem_rsvmap = be32(16);
    let size_dt_strings = be32(32);
    let size_dt_struct = be32(36);

    // Walk the reservation map: 16-byte (addr, size) entries, (0, 0) ends.
    let mut rsvmap_end = off_mem_rsvmap;
    while rsvmap_end + 16 <= totalsize {
        let raw = unsafe { core::ptr::read_volatile(fdt.add(rsvmap_end) as *const [u64; 2]) };
        rsvmap_end += 16;
        if raw[0] == 0 && raw[1] == 0 {
            break;
        }
    }

    let used = (off_dt_struct + size_dt_struct)
        .max(off_dt_strings + size_dt_strings)
        .max(rsvmap_end);
    used.clamp(40, totalsize)
}

// ---------------------------------------------------------------------------
// PayloadLoad
// ---------------------------------------------------------------------------

/// Load and jump to the payload (OS kernel, shell, etc.).
///
/// Reads the payload from FFS via the provided boot medium, copies its
/// segments to load addresses, and transfers control via `jump_to`.
///
/// Generic over [`BootMedia`] — works with both memory-mapped flash and
/// block devices with zero overhead for the memory-mapped case.
#[cfg(feature = "ffs")]
pub fn payload_load(anchor_data: &[u8], media: &(impl BootMedia + ?Sized), jump_to: impl Fn(u64)) {
    fstart_log::info!("stage helper: PayloadLoad");

    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::info!("payload load: no flash image configured, skipping");
        return;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!(
                "payload load: failed to read anchor: {}",
                reader_error_str(e)
            );
            return;
        }
    };

    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!("payload load: manifest error: {}", reader_error_str(e));
            return;
        }
    };

    let file = match manifest.find_file_by_type(fstart_core::ffs::FileType::Payload) {
        Ok(file) => file,
        Err(fstart_ffs::ReaderError::FileNotFound) => {
            fstart_log::error!("payload load: no payload found in manifest");
            return;
        }
        Err(e) => {
            fstart_log::error!("payload load: manifest error: {}", reader_error_str(e));
            return;
        }
    };

    let name = file.name().unwrap_or("<invalid>");
    fstart_log::info!("payload load: loading '{}'", name);

    // Load segments
    let image_size = effective_image_size(media.size(), &anchor);
    let entry_addr = match load_file_segments_from_media(media, &file, image_size) {
        Some(addr) => addr,
        None => return,
    };
    if !verify_loaded_file_digests(&file) {
        return;
    }

    fstart_log::info!("payload load: jumping to {}", Hex(entry_addr));

    jump_to(entry_addr);
}

/// Stub PayloadLoad when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn payload_load_stub() {
    fstart_log::info!("stage helper: PayloadLoad");
    fstart_log::info!("payload load skipped (ffs feature not enabled)");
}

/// Stub PayloadLoad without FFS — called when no boot medium is configured.
pub fn payload_load_stub_no_flash() {
    fstart_log::info!("stage helper: PayloadLoad");
    fstart_log::info!("payload load skipped (not yet implemented)");
}

// ---------------------------------------------------------------------------
// StageLoad
// ---------------------------------------------------------------------------

/// Load the next stage from FFS into RAM and jump to it.
///
/// Reads the named stage binary from the firmware filesystem via the
/// provided boot medium, copies its segments to load addresses, and
/// transfers control via `jump_to`.
///
/// Generic over [`BootMedia`] — works with both memory-mapped flash and
/// block devices with zero overhead for the memory-mapped case.
#[cfg(feature = "ffs")]
#[inline(never)]
pub fn stage_load(
    next_stage: &str,
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    jump_to: impl Fn(u64),
) {
    fstart_log::info!("stage helper: StageLoad -> {}", next_stage);
    fstart_log::info!(
        "stage load: anchor_len={} media_size={:#x}",
        anchor_data.len(),
        media.size()
    );

    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::info!("stage load: no flash image configured, skipping");
        return;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    fstart_log::info!("stage load: reading anchor");
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => {
            fstart_log::info!("stage load: anchor ok");
            a
        }
        Err(e) => {
            fstart_log::error!("stage load: failed to read anchor: {}", reader_error_str(e));
            return;
        }
    };

    fstart_log::info!("stage load: reading manifest");
    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => {
            let summary = m.summary();
            fstart_log::info!("stage load: manifest ok, regions={}", summary.regions);
            m
        }
        Err(e) => {
            fstart_log::error!("stage load: manifest error: {}", reader_error_str(e));
            return;
        }
    };

    let file = match manifest.find_file_by_name(next_stage) {
        Ok(file) => file,
        Err(fstart_ffs::ReaderError::FileNotFound) => {
            fstart_log::error!("stage load: stage '{}' not found in manifest", next_stage);
            return;
        }
        Err(e) => {
            fstart_log::error!("stage load: manifest error: {}", reader_error_str(e));
            return;
        }
    };

    fstart_log::info!(
        "stage load: loading '{}' ({} segments)",
        next_stage,
        file.segments().len()
    );

    // Load all segments to their load addresses
    let image_size = effective_image_size(media.size(), &anchor);
    fstart_log::info!("stage load: loading segments, image_size={:#x}", image_size);
    let entry_addr = match load_file_segments_from_media(media, &file, image_size) {
        Some(addr) => addr,
        None => {
            fstart_log::error!("stage load: segment load failed");
            return;
        }
    };
    if !verify_loaded_file_digests(&file) {
        fstart_log::error!("stage load: digest verification failed");
        return;
    }

    fstart_log::info!("stage load: jumping to {}", Hex(entry_addr));

    jump_to(entry_addr);
}

/// Stub StageLoad — called when no boot medium is configured.
pub fn stage_load_stub(next_stage: &str) {
    fstart_log::info!("stage helper: StageLoad -> {}", next_stage);
    fstart_log::info!("stage load skipped (not yet wired to FFS)");
}

// ---------------------------------------------------------------------------
// FFS Helpers (behind `ffs` feature)
// ---------------------------------------------------------------------------

/// Read and verify the FFS manifest view directly from mapped boot media.
///
/// Zero-copy: the signed envelope is verified in place over the memory-mapped
/// flash window (coreboot's `rdev_mmap` fast path). TOCTOU between verify and
/// parse is acceptable because boot flash is mapped read-only/cached — the
/// same trust XIP code already relies on.
///
/// ponytail: non-memory-mapped media (SPI-controller-only, eMMC) need a
/// bounce buffer owned by that media's driver — add when a block-boot board
/// returns from the attic; a global static here would cost every mapped
/// board 8 KiB of CAR.
#[cfg(feature = "ffs")]
fn read_manifest_from_media<'a>(
    media: &'a (impl BootMedia + ?Sized),
    anchor: &fstart_core::ffs::AnchorBlock,
) -> Result<fstart_ffs::ManifestView<'a>, fstart_ffs::ReaderError> {
    let manifest_offset = anchor.manifest_offset as usize;
    let manifest_size = anchor.manifest_size as usize;
    if manifest_size == 0 {
        return Err(fstart_ffs::ReaderError::OutOfBounds);
    }

    let Some(image) = media.as_slice() else {
        fstart_log::error!(
            "manifest read: non-memory-mapped boot media not supported (needs driver-owned buffer)"
        );
        return Err(fstart_ffs::ReaderError::OutOfBounds);
    };
    let bytes = manifest_offset
        .checked_add(manifest_size)
        .and_then(|end| image.get(manifest_offset..end))
        .ok_or(fstart_ffs::ReaderError::OutOfBounds)?;

    fstart_ffs::reader::verify_and_manifest_view(bytes, anchor.valid_keys())
}

/// Load a file from FFS by its `FileType`, placing segments at their load addresses.
///
/// Searches all container regions in the manifest for the first file entry
/// matching `file_type`, then loads its segments. Returns `true` on success.
///
/// Used by the Linux boot path to load firmware and kernel blobs from FFS.
#[cfg(feature = "ffs")]
pub fn load_ffs_file_by_type(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    file_type: fstart_core::ffs::FileType,
) -> bool {
    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::error!("load file: no flash image configured");
        return false;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!("load file: anchor error: {}", reader_error_str(e));
            return false;
        }
    };

    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!("load file: manifest error: {}", reader_error_str(e));
            return false;
        }
    };

    let file = match manifest.find_file_by_type(file_type) {
        Ok(file) => file,
        Err(fstart_ffs::ReaderError::FileNotFound) => {
            fstart_log::error!("load file: no file of requested type in FFS");
            return false;
        }
        Err(e) => {
            fstart_log::error!("load file: manifest error: {}", reader_error_str(e));
            return false;
        }
    };

    let name = file.name().unwrap_or("<invalid>");
    for seg in file.segments() {
        fstart_log::info!(
            "load file: '{}' seg '{}' -> {} ({} bytes)",
            name,
            "<segment>",
            Hex(seg.load_addr()),
            seg.stored_size(),
        );
    }

    let image_size = effective_image_size(media.size(), &anchor);
    let loaded = load_file_segments_from_media(media, &file, image_size).is_some();
    if !loaded {
        return false;
    }

    verify_loaded_file_digests(&file)
}

/// Load a file from FFS by its manifest name, placing segments at their load addresses.
///
/// Searches all container regions in the manifest for `name`, then loads its segments.
/// Returns `true` on success.
///
/// Used by multi-stage bootblocks where both the bootblock and next stage are
/// `FileType::StageCode`; the exact next stage must be selected by name.
#[cfg(feature = "ffs")]
pub fn load_ffs_file_by_name(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    name: &str,
) -> bool {
    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::error!("load file '{}': no flash image configured", name);
        return false;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!(
                "load file '{}': anchor error: {}",
                name,
                reader_error_str(e)
            );
            return false;
        }
    };

    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!(
                "load file '{}': manifest error: {}",
                name,
                reader_error_str(e)
            );
            return false;
        }
    };

    let file = match manifest.find_file_by_name(name) {
        Ok(file) => file,
        Err(fstart_ffs::ReaderError::FileNotFound) => {
            fstart_log::error!("load file '{}': not found in FFS", name);
            return false;
        }
        Err(e) => {
            fstart_log::error!(
                "load file '{}': manifest error: {}",
                name,
                reader_error_str(e)
            );
            return false;
        }
    };

    for seg in file.segments() {
        fstart_log::info!(
            "load file: '{}' seg '{}' -> {} ({} bytes)",
            name,
            "<segment>",
            Hex(seg.load_addr()),
            seg.stored_size(),
        );
    }

    let image_size = effective_image_size(media.size(), &anchor);
    let loaded = load_file_segments_from_media(media, &file, image_size).is_some();
    if !loaded {
        return false;
    }

    verify_loaded_file_digests(&file)
}

/// Raw location of one single-segment stage file inside the FFS image.
///
/// Resolved by the bootblock from the signature-verified manifest and passed
/// to postcar through the UC-DRAM stash, so postcar loads the ramstage
/// without an FFS parser or crypto (see
/// `fstart_arch::x86_64::car_teardown::PostcarMtrrStash`). Stage files are
/// packaged as a single flat `Code` segment (`assemble.rs`), which is all
/// this describes — multi-segment or BSS-carrying files are rejected.
#[cfg(feature = "ffs")]
#[derive(Debug, Clone, Copy)]
pub struct RawFileExtent {
    /// FFS-image-relative byte offset of the data segment.
    pub file_offset: u64,
    /// Stored (possibly compressed) size.
    pub stored_size: u64,
    /// Decompressed size at `load_addr`.
    pub loaded_size: u64,
    /// Builder-verified scratch size for in-place LZ4 (`0` = uncompressed).
    pub in_place_size: u64,
    /// DRAM load address (also the entry point).
    pub load_addr: u64,
    /// Segment compression.
    pub compression: fstart_core::ffs::Compression,
}

/// Resolve a stage file to its raw extent from the verified manifest.
///
/// Bootblock side of the Cut-B handoff: parses the signature-verified
/// manifest (same trust as [`load_ffs_file_by_name`]) but loads nothing —
/// the extent goes into the postcar stash for a crypto-free raw load.
/// Memory-mapped firmware windows only (all Intel boards).
/// Returns `None` when the file is missing or not a single data segment.
#[cfg(feature = "ffs")]
pub fn ffs_file_extent_mmio(
    anchor_data: &[u8],
    base: u64,
    size: usize,
    name: &str,
) -> Option<RawFileExtent> {
    use fstart_core::services::boot_media::MemoryMapped;
    if size == 0 || anchor_data.is_empty() {
        return None;
    }
    // SAFETY: caller passes the board's readable firmware window.
    let media = unsafe { MemoryMapped::from_raw_addr(base, size) };
    ffs_file_extent(anchor_data, &media, name)
}

/// Resolve a stage file to its raw extent from the verified manifest.
///
/// Generic-media core of [`ffs_file_extent_mmio`].
#[cfg(feature = "ffs")]
pub fn ffs_file_extent(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    name: &str,
) -> Option<RawFileExtent> {
    if media.size() == 0 || anchor_data.is_empty() {
        return None;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) }.ok()?;
    let manifest = read_manifest_from_media(media, &anchor).ok()?;
    let file = manifest.find_file_by_name(name).ok()?;

    let segments = file.segments();
    if segments.len() != 1 {
        fstart_log::error!(
            "raw extent '{}': expected 1 segment, found {}",
            name,
            segments.len()
        );
        return None;
    }
    let seg = &segments[0];
    if seg.kind().ok()? != fstart_core::ffs::SegmentKind::Code {
        fstart_log::error!("raw extent '{}': not a code segment", name);
        return None;
    }

    let image_size = effective_image_size(media.size(), &anchor);
    let file_offset =
        u64::from(file.region_offset()) + u64::from(file.entry_offset()) + u64::from(seg.offset());
    let stored_size = u64::from(seg.stored_size());
    if file_offset.saturating_add(stored_size) > image_size as u64 {
        fstart_log::error!("raw extent '{}': out of bounds", name);
        return None;
    }

    Some(RawFileExtent {
        file_offset,
        stored_size,
        loaded_size: u64::from(seg.loaded_size()),
        in_place_size: u64::from(seg.in_place_size()),
        load_addr: seg.load_addr(),
        compression: seg.compression().ok()?,
    })
}

/// Raw-load one extent from a memory-mapped image to its load address.
///
/// Postcar side of the Cut-B handoff: plain copy for uncompressed extents,
/// builder-verified in-place tail decompression for LZ4 (same protocol as
/// the manifest path, minus the manifest). No verification of any kind —
/// the bytes come from ROM the bootblock already authenticated, and the
/// ramstage re-verifies its own bytes before trusting them.
/// Returns the entry address (`load_addr`) on success.
///
/// # Safety
///
/// `image_base`/`image_size` must describe the readable firmware window;
/// `extent` must describe a writable DRAM target with `in_place_size` bytes
/// available for compressed extents. Both are guaranteed by the bootblock's
/// stash fill (verified manifest + board memory map).
#[cfg(feature = "ffs")]
pub unsafe fn load_raw_extent(
    image_base: u64,
    image_size: u64,
    extent: &RawFileExtent,
) -> Option<u64> {
    let src_offset = extent.file_offset;
    if src_offset.saturating_add(extent.stored_size) > image_size {
        return None;
    }
    let stored = extent.stored_size as usize;
    let dest = extent.load_addr as *mut u8;
    // SAFETY: caller guarantees the image window is readable and the
    // target is writable DRAM.
    let src = unsafe { (image_base as *const u8).add(src_offset as usize) };

    match extent.compression {
        fstart_core::ffs::Compression::None => {
            // Plain copy of the stored bytes (trailing mem-size padding, if
            // any, is irrelevant: the stage entry zeroes its own BSS).
            // SAFETY: non-overlapping (ROM source, DRAM target), sizes checked.
            unsafe { core::ptr::copy_nonoverlapping(src, dest, stored) };
        }
        fstart_core::ffs::Compression::Lz4 => {
            let buf_size = extent.in_place_size as usize;
            let loaded_size = extent.loaded_size as usize;
            if buf_size < loaded_size || buf_size < stored {
                return None;
            }
            // SAFETY: same guarantees as the manifest path's in-place
            // protocol: the builder verified this exact operation.
            unsafe {
                let buf = core::slice::from_raw_parts_mut(dest, buf_size);
                let comp_offset = buf_size - stored;
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr().add(comp_offset), stored);
                let src_slice = core::slice::from_raw_parts(buf.as_ptr().add(comp_offset), stored);
                let dst_slice = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), loaded_size);
                fstart_ffs::lz4::decompress_block(src_slice, dst_slice).ok()?;
            }
        }
    }

    Some(extent.load_addr)
}

/// Verify a file's digests against the bytes at its packaged load addresses.
///
/// Read-only: hashes the loaded image in place without copying, zeroing, or
/// touching BSS — safe to run on a live stage checking itself. The ramstage
/// calls this on entry (postcar skips digest verification by design), so a
/// DRAM-bitflip during the raw copy is caught before any table or payload
/// trusts the bytes. Memory-mapped firmware windows only.
#[cfg(feature = "ffs")]
pub fn verify_file_digests_mmio(anchor_data: &[u8], base: u64, size: usize, name: &str) -> bool {
    use fstart_core::services::boot_media::MemoryMapped;
    if size == 0 || anchor_data.is_empty() {
        return false;
    }
    // SAFETY: caller passes the board's readable firmware window.
    let media = unsafe { MemoryMapped::from_raw_addr(base, size) };
    verify_file_digests_by_name(anchor_data, &media, name)
}

/// Verify a file's digests against the bytes at its packaged load addresses.
///
/// Generic-media core of [`verify_file_digests_mmio`].
#[cfg(feature = "ffs")]
pub fn verify_file_digests_by_name(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    name: &str,
) -> bool {
    if media.size() == 0 || anchor_data.is_empty() {
        return false;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(_) => return false,
    };
    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let file = match manifest.find_file_by_name(name) {
        Ok(file) => file,
        Err(_) => return false,
    };

    verify_loaded_file_digests(&file)
}

/// Find a file in FFS by its `FileType` and return a slice to its raw data.
///
/// This is the zero-copy path for memory-mapped flash: the returned slice
/// points directly into the flash image. Used by FIT runtime parsing to
/// access the FIT blob without copying it.
///
/// Only works with memory-mapped boot media (returns `None` for block devices).
/// The returned slice covers the first segment of the matching file entry.
#[cfg(feature = "ffs")]
pub fn find_ffs_file_data<'a>(
    anchor_data: &[u8],
    media: &'a (impl BootMedia + ?Sized),
    file_type: fstart_core::ffs::FileType,
) -> Option<&'a [u8]> {
    let image = media.as_slice()?;

    if anchor_data.is_empty() {
        fstart_log::error!("find file data: no anchor");
        return None;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!("find file data: anchor error: {}", reader_error_str(e));
            return None;
        }
    };

    let image_size = effective_image_size(media.size(), &anchor);
    let manifest_offset = anchor.manifest_offset as usize;
    let manifest_size = anchor.manifest_size as usize;
    let manifest_end = manifest_offset.checked_add(manifest_size)?;
    if manifest_end > image_size {
        return None;
    }
    let signed_manifest = image.get(manifest_offset..manifest_end)?;
    let manifest =
        match fstart_ffs::reader::verify_and_manifest_view(signed_manifest, anchor.valid_keys()) {
            Ok(m) => m,
            Err(e) => {
                fstart_log::error!("find file data: manifest error: {}", reader_error_str(e));
                return None;
            }
        };

    let file = manifest.find_file_by_type(file_type).ok()?;
    let seg = file.segments().first()?;
    let offset = (file.region_offset() + file.entry_offset() + seg.offset()) as usize;
    let size = seg.stored_size() as usize;
    if offset + size <= image.len() {
        return Some(&image[offset..offset + size]);
    }

    fstart_log::error!("find file data: file type not found in FFS");
    None
}

/// Find a file in FFS and copy it to temporary RAM when zero-copy is unavailable.
#[cfg(feature = "ffs")]
pub fn find_ffs_file_data_with_scratch<'a>(
    anchor_data: &[u8],
    media: &'a (impl BootMedia + ?Sized),
    file_type: fstart_core::ffs::FileType,
    scratch: Option<&'a mut fstart_core::services::TempRamArena>,
) -> Option<&'a [u8]> {
    if media.as_slice().is_some() {
        return find_ffs_file_data(anchor_data, media, file_type);
    }

    let scratch = scratch?;
    if anchor_data.is_empty() {
        fstart_log::error!("find file data: no anchor");
        return None;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!("find file data: anchor error: {}", reader_error_str(e));
            return None;
        }
    };

    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!("find file data: manifest error: {}", reader_error_str(e));
            return None;
        }
    };

    let file = manifest.find_file_by_type(file_type).ok()?;
    let image_size = effective_image_size(media.size(), &anchor) as u64;
    let seg = file.segments().first()?;
    let offset =
        u64::from(file.region_offset()) + u64::from(file.entry_offset()) + u64::from(seg.offset());
    let size = seg.stored_size() as usize;
    if offset.checked_add(size as u64)? > image_size {
        return None;
    }

    fstart_core::services::boot_media::read_to_temp(
        media,
        usize::try_from(offset).ok()?,
        size,
        scratch,
    )
    .ok()
}

#[cfg(feature = "ffs")]
fn verify_loaded_file_digests(file: &fstart_ffs::FileView<'_>) -> bool {
    let segments = file.segments();
    let name = file.name().unwrap_or("<invalid>");
    if segments.len() != 1 {
        fstart_log::info!(
            "load file: digest verify skipped for '{}' ({} loaded segments)",
            name,
            segments.len()
        );
        return true;
    }

    let seg = &segments[0];
    let verify_size = seg.loaded_size() as usize;
    let data = unsafe { core::slice::from_raw_parts(seg.load_addr() as *const u8, verify_size) };
    match fstart_crypto::digest::verify_digest_set(data, &file.digests()) {
        Ok(()) => {
            fstart_log::info!("load file: '{}' digest verified after load", name);
            true
        }
        Err(_) => {
            fstart_log::error!("load file: '{}' digest FAILED after load", name);
            false
        }
    }
}

#[cfg(feature = "ffs")]
fn load_file_segments_from_media(
    media: &(impl BootMedia + ?Sized),
    file: &fstart_ffs::FileView<'_>,
    image_size: usize,
) -> Option<u64> {
    let mut entry_addr: Option<u64> = None;

    for seg in file.segments() {
        let kind = match seg.kind() {
            Ok(kind) => kind,
            Err(_) => return None,
        };
        let compression = match seg.compression() {
            Ok(compression) => compression,
            Err(_) => return None,
        };

        if kind == fstart_core::ffs::SegmentKind::Bss {
            // BSS: zero-fill at load_addr
            let dest = seg.load_addr() as *mut u8;
            // SAFETY: we trust the board config; the load_addr points to writable RAM.
            unsafe {
                core::ptr::write_bytes(dest, 0, seg.loaded_size() as usize);
            }
            fstart_log::debug!(
                "  BSS: {} ({} bytes zeroed)",
                Hex(seg.load_addr()),
                seg.loaded_size()
            );
        } else {
            // Data segment: read from boot medium to load_addr
            let src_offset = (file.region_offset() + file.entry_offset() + seg.offset()) as usize;
            let stored_size = seg.stored_size() as usize;
            let dest = seg.load_addr() as *mut u8;

            // Defense-in-depth: verify the segment's source data falls within
            // the effective image size. The manifest is signature-verified so
            // this should never trip unless the signing key is compromised.
            if src_offset.saturating_add(stored_size) > image_size {
                fstart_log::error!(
                    "segment out of bounds: offset {} + size {} > image {}",
                    src_offset as u32,
                    stored_size as u32,
                    image_size as u32,
                );
                return None;
            }

            match compression {
                fstart_core::ffs::Compression::None => {
                    // Read directly from boot medium to the load address.
                    // For memory-mapped media, this inlines to memmove
                    // (handles overlap when FFS image is in RAM).
                    // For block devices, this is a single device read.
                    //
                    // SAFETY: we trust the board config; load_addr points to
                    // writable RAM with enough space for stored_size bytes.
                    let dest_buf = unsafe { core::slice::from_raw_parts_mut(dest, stored_size) };
                    if media.read_at(src_offset, dest_buf).is_err() {
                        fstart_log::error!("segment read error");
                        return None;
                    }

                    fstart_log::debug!(
                        "  segment: {} ({} bytes)",
                        Hex(seg.load_addr()),
                        stored_size
                    );
                }
                #[cfg(feature = "lz4")]
                fstart_core::ffs::Compression::Lz4 => {
                    // In-place LZ4 decompression (coreboot technique):
                    // 1. The builder verified that `in_place_size` bytes at
                    //    load_addr suffice for safe in-place decompression.
                    // 2. Read compressed data to the END of the buffer:
                    //    dest + in_place_size - stored_size
                    // 3. Decompress from tail to head — the decompressor
                    //    reads from the tail while writing from the head.
                    let buf_size = seg.in_place_size() as usize;
                    let loaded_size = seg.loaded_size() as usize;

                    // SAFETY: load_addr points to writable RAM with at least
                    // `in_place_size` bytes available (verified by the builder
                    // and guaranteed by the linker script / board config).
                    let buf = unsafe { core::slice::from_raw_parts_mut(dest, buf_size) };

                    // Read compressed data into the tail of the buffer
                    let comp_offset = buf_size - stored_size;
                    if media
                        .read_at(src_offset, &mut buf[comp_offset..comp_offset + stored_size])
                        .is_err()
                    {
                        fstart_log::error!("segment read error (lz4)");
                        return None;
                    }

                    // Decompress in-place: read from tail, write from head.
                    // SAFETY: the builder simulated this exact operation at
                    // build time and verified it succeeds. The src slice
                    // overlaps the tail of buf — our decompressor handles
                    // this (in-place guard checks output doesn't overtake input).
                    let result = unsafe {
                        let src =
                            core::slice::from_raw_parts(buf.as_ptr().add(comp_offset), stored_size);
                        let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), loaded_size);
                        fstart_ffs::lz4::decompress_block(src, dst)
                    };

                    match result {
                        Ok(n) => {
                            fstart_log::debug!(
                                "  segment: {} ({} -> {} bytes, lz4 in-place)",
                                Hex(seg.load_addr()),
                                stored_size,
                                n
                            );
                        }
                        Err(_) => {
                            fstart_log::error!("LZ4 in-place decompress failed");
                            return None;
                        }
                    }
                }
                #[cfg(not(feature = "lz4"))]
                fstart_core::ffs::Compression::Lz4 => {
                    fstart_log::error!("LZ4 compressed segment but lz4 feature not enabled");
                    return None;
                }
            }
        }

        // Use the first Code segment's load_addr as the entry point,
        // or fall back to the first segment's load_addr.
        if entry_addr.is_none() || kind == fstart_core::ffs::SegmentKind::Code {
            entry_addr = Some(seg.load_addr());
        }
    }

    entry_addr
}

/// Map a ReaderError to a static string for logging.
#[cfg(feature = "ffs")]
fn reader_error_str(err: fstart_ffs::ReaderError) -> &'static str {
    match err {
        fstart_ffs::ReaderError::OutOfBounds => "out of bounds",
        fstart_ffs::ReaderError::BadMagic => "bad magic",
        fstart_ffs::ReaderError::UnsupportedVersion => "unsupported version",
        fstart_ffs::ReaderError::DeserializeError => "deserialize error",
        fstart_ffs::ReaderError::SignatureInvalid => "signature invalid",
        fstart_ffs::ReaderError::KeyNotFound => "key not found",
        fstart_ffs::ReaderError::FileNotFound => "file not found",
        fstart_ffs::ReaderError::DigestMismatch => "digest mismatch",
        fstart_ffs::ReaderError::RegionNotFound => "region not found",
        fstart_ffs::ReaderError::UnsupportedAlgorithm => "unsupported algorithm",
        fstart_ffs::ReaderError::CannotVerifyInPlace => "cannot verify in place",
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the effective image size, preferring the anchor's `total_image_size`
/// when it's non-zero and smaller than the media size.
///
/// On XIP platforms, the media size may be the full flash bank (e.g., 128 MiB)
/// while the FFS image is much smaller. Using the anchor's total_image_size
/// ensures the reader only accesses data that was actually written by the builder.
#[cfg(feature = "ffs")]
fn effective_image_size(media_size: usize, anchor: &fstart_core::ffs::AnchorBlock) -> usize {
    if anchor.total_image_size > 0 && (anchor.total_image_size as usize) < media_size {
        anchor.total_image_size as usize
    } else {
        media_size
    }
}

/// Fixed FFS anchor placeholder for handwritten stage flow.
///
/// `fbuild assemble` patches this block in the flat stage binary after laying out
/// the complete firmware image.
#[used]
#[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.anchor"))]
pub static FSTART_ANCHOR: fstart_core::ffs::AnchorBlock =
    fstart_core::ffs::AnchorBlock::placeholder();

#[must_use]
pub fn fstart_anchor_bytes() -> &'static [u8] {
    // SAFETY: FSTART_ANCHOR is a repr(C) static placed in `.fstart.anchor` and
    // has exactly ANCHOR_SIZE initialized bytes.
    unsafe {
        core::slice::from_raw_parts(
            (&FSTART_ANCHOR as *const fstart_core::ffs::AnchorBlock).cast::<u8>(),
            fstart_core::ffs::ANCHOR_SIZE,
        )
    }
}

/// Runtime environment selected by build glue for a firmware entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageEnvironment {
    /// Single-stage/monolithic firmware image.
    Monolithic,
    /// Cache-as-RAM/SRAM-only environment: no DRAM assumptions.
    Car,
    /// DRAM-backed environment: allocation, tables, and payload handoff allowed.
    Ram,
}

impl StageEnvironment {
    /// Convert the optional `FSTART_STAGE_ENV` value passed by build glue.
    ///
    /// The Cut-B postcar loader builds with `FSTART_STAGE_ENV=postcar` but
    /// runs as a DRAM-backed stage, so it maps to [`Self::Ram`]; the
    /// postcar-vs-ramstage split inside `Ram` is a compile-time
    /// `cfg(fstart_stage_env)` dispatch in each Intel platform flow.
    #[must_use]
    pub fn from_option(env: Option<&'static str>) -> Self {
        match env {
            Some("car") => Self::Car,
            Some("ram" | "postcar") => Self::Ram,
            _ => Self::Monolithic,
        }
    }
}

/// Static typed firmware board selected by build glue.
pub trait StageBoard: Sized + 'static {
    /// Stable fstart board name.
    const NAME: &'static str;
    /// Runtime platform for this board.
    const PLATFORM: fstart_core::Platform;

    /// Run the selected environment using the board's platform-family flow.
    fn run_stage(env: StageEnvironment, handoff: usize) -> !;

    /// Resume after OpenSBI enters the selected RISC-V payload in S-mode.
    fn resume_sbi(_hart_id: u64, _dtb_addr: u64) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Declare a board-owned stage entry binary.
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
            <$board as $crate::StageBoard>::run_stage(
                $crate::StageEnvironment::from_option(option_env!("FSTART_STAGE_ENV")),
                handoff_ptr,
            )
        }

        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;

        #[cfg(all(feature = "crabefi", target_arch = "riscv64"))]
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_sbi_resume(hart_id: u64, dtb_addr: u64) -> ! {
            <$board as $crate::StageBoard>::resume_sbi(hart_id, dtb_addr)
        }
    };
}
