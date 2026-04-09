//! Composable capability modules for firmware stages.
//!
//! Each capability is a unit of firmware functionality. The board RON file
//! declares which capabilities run in which order. Codegen generates a
//! `fstart_main()` that calls them in sequence.
//!
//! Capability functions use the global logger ([`fstart_log`]) for output
//! rather than accepting `&dyn Console` parameters. The console must be
//! initialised via `fstart_log::init()` before any capability is called.
//!
//! ## FFS Capabilities
//!
//! When the `ffs` feature is enabled, `sig_verify`, `stage_load`, and
//! `payload_load` perform real FFS operations: reading the anchor via
//! volatile from the embedded `FSTART_ANCHOR` static, verifying manifest
//! signatures, reading file segments, and jumping to loaded code. Without
//! the `ffs` feature, stub variants are used.
//!
//! ## Boot Media Abstraction
//!
//! All FFS capability functions are generic over
//! [`BootMedia`](fstart_services::BootMedia), accepting any boot medium
//! implementation. For memory-mapped flash
//! ([`MemoryMapped`](fstart_services::MemoryMapped)), the code uses the
//! existing [`FfsReader`](fstart_ffs::FfsReader) fast path — all operations
//! inline to direct memory access with zero overhead. For block devices
//! ([`BlockDeviceMedia`](fstart_services::BlockDeviceMedia)), metadata is
//! read into stack buffers and segments are loaded via the device I/O path.
//!
//! See [docs/driver-model.md](../../docs/driver-model.md) for the full
//! driver model architecture.

#![no_std]

#[cfg(feature = "fdt")]
mod fdt_patch;

#[cfg(feature = "fit")]
pub mod fit;

#[cfg(feature = "handoff")]
pub mod handoff;

pub mod next_stage;

#[cfg(feature = "smbios")]
pub mod smbios;

#[cfg(feature = "acpi")]
pub mod acpi;

// ---------------------------------------------------------------------------
// FDT blob utilities
// ---------------------------------------------------------------------------

/// Read an FDT (Flattened Device Tree) blob from a raw memory address.
///
/// Reads the `totalsize` field at offset +4 of the FDT header (big-endian
/// u32) and returns a static slice covering the entire blob. Returns `None`
/// if `addr` is zero.
///
/// Used by the UEFI payload path to obtain the platform-provided FDT blob
/// for passing to CrabEFI.
///
/// # Safety
///
/// Caller must ensure `addr` points to a valid, readable FDT blob in
/// memory. The FDT header must be intact (magic + totalsize). The returned
/// slice borrows the memory at `addr` with `'static` lifetime — the FDT
/// must remain valid (e.g., in DRAM) for the program's duration.
pub unsafe fn fdt_blob_from_addr(addr: u64) -> Option<&'static [u8]> {
    if addr == 0 {
        return None;
    }
    let ptr = addr as *const u8;
    let size = u32::from_be(core::ptr::read_unaligned(ptr.add(4) as *const u32)) as usize;
    Some(core::slice::from_raw_parts(ptr, size))
}

#[cfg(any(feature = "ffs", feature = "fdt"))]
use fstart_log::Hex;

#[cfg(feature = "ffs")]
use fstart_services::BootMedia;

// ---------------------------------------------------------------------------
// ConsoleInit
// ---------------------------------------------------------------------------

/// Log the console-ready banner after a console device is initialised.
///
/// Called by generated code after `Device::init()` succeeds on a console device.
pub fn console_ready(device_name: &str, driver_name: &str) {
    fstart_log::info!("{}: {} console ready", device_name, driver_name);
}

// ---------------------------------------------------------------------------
// MemoryInit
// ---------------------------------------------------------------------------

/// Initialise DRAM (memory training / memory controller setup).
///
/// In a real board this would perform SPD reads, memory training, and
/// controller configuration. For QEMU virt machines, RAM is always available
/// so this is a no-op that logs its execution.
///
/// Future: Accept a platform-specific memory-init trait or configuration.
pub fn memory_init() {
    fstart_log::info!("capability: MemoryInit");
    // QEMU virt: RAM is pre-initialised, nothing to do.
    // Real boards: SPD read → training → controller init would go here.
    fstart_log::info!("memory init complete (no-op on QEMU)");
}

// ---------------------------------------------------------------------------
// DriverInit
// ---------------------------------------------------------------------------

/// Enumerate and initialise all declared devices/drivers.
///
/// Codegen generates the actual `Device::init()` calls for each device
/// that was not already initialised by an earlier capability (e.g.,
/// ConsoleInit already inits the UART). This function logs the phase
/// boundary; the individual init calls are inlined by codegen.
///
/// `device_count` is the total number of devices that were initialised
/// in this phase (provided by codegen, which knows the count).
pub fn driver_init_complete(device_count: usize) {
    fstart_log::info!("capability: DriverInit ({} devices)", device_count);
}

// ---------------------------------------------------------------------------
// LateDriverInit
// ---------------------------------------------------------------------------

/// Device lockdown and security hardening — post-boot phase.
///
/// Called after OS handoff preparation but before the final jump.
/// Currently a stub that logs its execution. Future: iterate over
/// devices and call a `lockdown()` trait method for flash write-protect,
/// fuse locking, debug port disable, etc.
pub fn late_driver_init_complete(device_count: usize) {
    fstart_log::info!("capability: LateDriverInit ({} devices)", device_count);
}

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
pub fn sig_verify(anchor_data: &[u8], media: &impl BootMedia) {
    fstart_log::info!("capability: SigVerify");

    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::info!("sig verify: no flash image configured, skipping");
        return;
    }

    // Volatile-read the anchor to see the post-build patched values
    // SAFETY: FSTART_ANCHOR is emitted by codegen with proper alignment
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

    // Verify file digests — only possible with memory-mapped access
    // (requires reading full file data for hashing). For non-memory-mapped
    // media, the manifest signature already provides integrity.
    if let Some(image) = media.as_slice() {
        let image_size = effective_image_size(media.size(), &anchor);
        let reader = fstart_ffs::FfsReader::new(&image[..image_size]);

        let mut total_verified = 0usize;
        let mut total_skipped = 0usize;

        for region in &manifest.regions {
            if let fstart_types::ffs::RegionContent::Container { children } = &region.content {
                fstart_log::info!(
                    "sig verify: region '{}' ({} entries)",
                    region.name.as_str(),
                    children.len()
                );

                for entry in children {
                    match reader.verify_entry_digests(entry, region) {
                        Ok(()) => total_verified += 1,
                        Err(fstart_ffs::ReaderError::CannotVerifyInPlace) => {
                            total_skipped += 1;
                        }
                        Err(_) => {
                            fstart_log::error!(
                                "sig verify: digest FAILED for: {}",
                                entry.name.as_str()
                            );
                            return;
                        }
                    }
                }
            }
        }

        fstart_log::info!(
            "sig verify: {} files verified, {} skipped (multi-segment)",
            total_verified,
            total_skipped
        );
    } else {
        // Non-memory-mapped media: manifest signature verified above;
        // per-file digest verification requires incremental hashing
        // (future enhancement).
        fstart_log::info!(
            "sig verify: manifest signature verified (per-file digests skipped on block device)"
        );
    }
}

/// Stub SigVerify when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn sig_verify(_anchor_data: &[u8], _media: &impl fstart_services::BootMedia) {
    fstart_log::info!("capability: SigVerify");
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
/// Searches for [`FFS_MAGIC`](fstart_types::ffs::FFS_MAGIC) at 8-byte
/// aligned offsets in the media.  Returns the anchor data as a
/// fixed-size array on success.
///
/// This replaces the inline scanning code that codegen previously
/// generated for non-first stages in memory-mapped multi-stage builds.
///
/// # Errors
///
/// - [`AnchorScanError::NotMemoryMapped`] if the media does not support
///   `as_slice()` (block device media).
/// - [`AnchorScanError::NotFound`] if the FFS magic is not found.
#[cfg(feature = "ffs")]
pub fn scan_anchor_in_media(
    media: &impl BootMedia,
) -> Result<[u8; fstart_types::ffs::ANCHOR_SIZE], AnchorScanError> {
    let media_slice = media.as_slice().ok_or(AnchorScanError::NotMemoryMapped)?;

    let magic = &fstart_types::ffs::FFS_MAGIC;
    let mut offset = 0usize;
    let mut found = false;
    while offset + magic.len() <= media_slice.len() {
        if &media_slice[offset..offset + magic.len()] == magic {
            found = true;
            break;
        }
        offset += 8;
    }
    if !found || offset + fstart_types::ffs::ANCHOR_SIZE > media_slice.len() {
        return Err(AnchorScanError::NotFound);
    }
    let mut buf = [0u8; fstart_types::ffs::ANCHOR_SIZE];
    buf.copy_from_slice(&media_slice[offset..offset + fstart_types::ffs::ANCHOR_SIZE]);
    fstart_log::info!(
        "FFS anchor found at offset {:#x} in boot media",
        offset as u64
    );
    Ok(buf)
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
) -> Result<[u8; fstart_types::ffs::ANCHOR_SIZE], AnchorScanError> {
    let mut buf = [0u8; fstart_types::ffs::ANCHOR_SIZE];
    media
        .read_at(anchor_offset, &mut buf)
        .map_err(|_| AnchorScanError::NotFound)?;

    // Verify the magic bytes are present.
    let magic = &fstart_types::ffs::FFS_MAGIC;
    if buf[..magic.len()] != *magic {
        return Err(AnchorScanError::NotFound);
    }

    fstart_log::info!("FFS anchor read at offset {:#x}", anchor_offset as u64);
    Ok(buf)
}

// ---------------------------------------------------------------------------
// FdtPrepare
// ---------------------------------------------------------------------------

/// Prepare a Flattened Device Tree for OS handoff (stub — no FDT feature).
///
/// When the `fdt` feature is not enabled, this logs a skip message.
/// The real implementation is `fdt_prepare_platform()` behind `#[cfg(feature = "fdt")]`.
pub fn fdt_prepare_stub() {
    fstart_log::info!("capability: FdtPrepare");
    fstart_log::info!("FDT prepare skipped (fdt feature not enabled)");
}

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
    fstart_log::info!("capability: FdtPrepare");

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

    // Copy source to destination if they differ.
    let dst_ptr = dst_dtb_addr as *mut u8;
    if src_dtb_addr != dst_dtb_addr {
        fstart_log::info!("FDT: copying {} bytes to {}", totalsize, Hex(dst_dtb_addr));
        // SAFETY: both regions are in DRAM, non-overlapping (board config
        // must ensure this), and totalsize bytes are readable/writable.
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, totalsize);
        }
    }

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
pub fn payload_load(anchor_data: &[u8], media: &impl BootMedia, jump_to: fn(u64) -> !) {
    fstart_log::info!("capability: PayloadLoad");

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

    // Look for a Payload file type in any container region
    let mut payload_found = None;
    for region in &manifest.regions {
        if let fstart_types::ffs::RegionContent::Container { children } = &region.content {
            for entry in children {
                if let fstart_types::ffs::EntryContent::File { file_type, .. } = &entry.content {
                    if *file_type == fstart_types::ffs::FileType::Payload {
                        payload_found = Some((region, entry));
                        break;
                    }
                }
            }
        }
        if payload_found.is_some() {
            break;
        }
    }

    let (region, entry) = match payload_found {
        Some(found) => found,
        None => {
            fstart_log::error!("payload load: no payload found in manifest");
            return;
        }
    };

    fstart_log::info!("payload load: loading '{}'", entry.name.as_str());

    // Load segments
    let image_size = effective_image_size(media.size(), &anchor);
    let entry_addr = match load_entry_segments_from_media(media, entry, region, image_size) {
        Some(addr) => addr,
        None => return,
    };

    fstart_log::info!("payload load: jumping to {}", Hex(entry_addr));

    jump_to(entry_addr);
}

/// Stub PayloadLoad when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn payload_load_stub() {
    fstart_log::info!("capability: PayloadLoad");
    fstart_log::info!("payload load skipped (ffs feature not enabled)");
}

/// Stub PayloadLoad without FFS — called when flash_base is not configured.
pub fn payload_load_stub_no_flash() {
    fstart_log::info!("capability: PayloadLoad");
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
pub fn stage_load(
    next_stage: &str,
    anchor_data: &[u8],
    media: &impl BootMedia,
    jump_to: fn(u64) -> !,
) {
    fstart_log::info!("capability: StageLoad -> {}", next_stage);

    if media.size() == 0 || anchor_data.is_empty() {
        fstart_log::info!("stage load: no flash image configured, skipping");
        return;
    }

    // SAFETY: FSTART_ANCHOR is properly aligned and sized.
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(e) => {
            fstart_log::error!("stage load: failed to read anchor: {}", reader_error_str(e));
            return;
        }
    };

    let manifest = match read_manifest_from_media(media, &anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!("stage load: manifest error: {}", reader_error_str(e));
            return;
        }
    };

    // Search all container regions for the named stage
    let mut stage_found = None;
    for region in &manifest.regions {
        match fstart_ffs::FfsReader::find_entry(region, next_stage) {
            Ok(entry) => {
                stage_found = Some((region, entry));
                break;
            }
            Err(_) => continue,
        }
    }

    let (region, entry) = match stage_found {
        Some(found) => found,
        None => {
            fstart_log::error!("stage load: stage '{}' not found in manifest", next_stage);
            return;
        }
    };

    let segments_len = match &entry.content {
        fstart_types::ffs::EntryContent::File { segments, .. } => segments.len(),
        _ => 0,
    };

    fstart_log::info!(
        "stage load: loading '{}' ({} segments)",
        next_stage,
        segments_len
    );

    // Load all segments to their load addresses
    let image_size = effective_image_size(media.size(), &anchor);
    let entry_addr = match load_entry_segments_from_media(media, entry, region, image_size) {
        Some(addr) => addr,
        None => return,
    };

    fstart_log::info!("stage load: jumping to {}", Hex(entry_addr));

    jump_to(entry_addr);
}

/// Stub StageLoad — called when flash_base is not configured.
pub fn stage_load_stub(next_stage: &str) {
    fstart_log::info!("capability: StageLoad -> {}", next_stage);
    fstart_log::info!("stage load skipped (not yet wired to FFS)");
}

// ---------------------------------------------------------------------------
// FFS Helpers (behind `ffs` feature)
// ---------------------------------------------------------------------------

/// Maximum signed manifest size for buffered reads from block devices.
///
/// Manifests typically serialize to 1-4 KB with postcard. 8 KB provides
/// generous headroom for boards with many files and regions.
#[cfg(feature = "ffs")]
const MAX_MANIFEST_SIZE: usize = 8192;

/// Wrapper that implements `Sync` for `UnsafeCell`, allowing it to live
/// in a `static`.
///
/// This is exactly what `core::cell::SyncUnsafeCell` does, but that API
/// is still gated behind `#![feature(sync_unsafe_cell)]`.  A two-line
/// wrapper avoids the nightly dependency entirely.
#[cfg(feature = "ffs")]
#[repr(transparent)]
struct SyncBuf(core::cell::UnsafeCell<[u8; MAX_MANIFEST_SIZE]>);

// SAFETY: firmware boot is single-threaded.  The buffer is only accessed
// inside `read_manifest_from_media`, which is never called concurrently.
#[cfg(feature = "ffs")]
unsafe impl Sync for SyncBuf {}

/// Static buffer for manifest reads from non-memory-mapped media.
///
/// Placed in BSS (zero-initialized at startup) rather than on the stack
/// to avoid consuming 8 KiB of stack space per call.  Firmware boot is
/// single-threaded so concurrent access is not a concern.
#[cfg(feature = "ffs")]
static MANIFEST_BUF: SyncBuf = SyncBuf(core::cell::UnsafeCell::new([0u8; MAX_MANIFEST_SIZE]));

/// Read and verify the FFS manifest from any boot medium.
///
/// For memory-mapped media, uses the existing [`FfsReader`] fast path
/// (the `as_slice()` branch). For non-memory-mapped media, reads the
/// signed manifest into a static buffer and verifies it there.
///
/// In both cases, the manifest signature is verified and the inner
/// [`ImageManifest`](fstart_types::ffs::ImageManifest) is returned.
#[cfg(feature = "ffs")]
fn read_manifest_from_media(
    media: &impl BootMedia,
    anchor: &fstart_types::ffs::AnchorBlock,
) -> Result<fstart_types::ffs::ImageManifest, fstart_ffs::ReaderError> {
    // Fast path: memory-mapped — use existing FfsReader (zero-cost)
    if let Some(image) = media.as_slice() {
        let image_size = effective_image_size(media.size(), anchor);
        let reader = fstart_ffs::FfsReader::new(&image[..image_size]);
        return reader.read_manifest(anchor);
    }

    // Slow path: read signed manifest into the static buffer.
    // Uses a static rather than a stack allocation to keep stack usage
    // predictable for bootblocks with small stacks (e.g., 32 KiB).
    let manifest_offset = anchor.manifest_offset as usize;
    let manifest_size = anchor.manifest_size as usize;

    if manifest_size == 0 || manifest_size > MAX_MANIFEST_SIZE {
        return Err(fstart_ffs::ReaderError::OutOfBounds);
    }

    // SAFETY: firmware boot is single-threaded; no concurrent access to
    // MANIFEST_BUF. The buffer is only used within this function scope
    // and the parsed manifest is returned by value (no references escape).
    let buf = unsafe { &mut *MANIFEST_BUF.0.get() };
    media
        .read_at(manifest_offset, &mut buf[..manifest_size])
        .map_err(|_| fstart_ffs::ReaderError::OutOfBounds)?;

    fstart_ffs::verify_and_parse_manifest(&buf[..manifest_size], anchor.valid_keys())
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
    media: &impl BootMedia,
    file_type: fstart_types::ffs::FileType,
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

    // Search for first file matching the requested type
    let mut found = None;
    for region in &manifest.regions {
        if let fstart_types::ffs::RegionContent::Container { children } = &region.content {
            for entry in children {
                if let fstart_types::ffs::EntryContent::File { file_type: ft, .. } = &entry.content
                {
                    if *ft == file_type {
                        found = Some((region, entry));
                        break;
                    }
                }
            }
        }
        if found.is_some() {
            break;
        }
    }

    let (region, entry) = match found {
        Some(f) => f,
        None => {
            fstart_log::error!("load file: no file of requested type in FFS");
            return false;
        }
    };

    // Log load address from segment metadata for debugging.
    if let fstart_types::ffs::EntryContent::File { segments, .. } = &entry.content {
        for seg in segments {
            fstart_log::info!(
                "load file: '{}' seg '{}' -> {} ({} bytes)",
                entry.name.as_str(),
                seg.name.as_str(),
                Hex(seg.load_addr),
                seg.stored_size,
            );
        }
    }

    let image_size = effective_image_size(media.size(), &anchor);
    load_entry_segments_from_media(media, entry, region, image_size).is_some()
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
    media: &'a impl BootMedia,
    file_type: fstart_types::ffs::FileType,
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
    let reader = fstart_ffs::FfsReader::new(&image[..image_size]);

    let manifest = match reader.read_manifest(&anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!("find file data: manifest error: {}", reader_error_str(e));
            return None;
        }
    };

    // Search for the file entry
    for region in &manifest.regions {
        if let fstart_types::ffs::RegionContent::Container { children } = &region.content {
            for entry in children {
                if let fstart_types::ffs::EntryContent::File {
                    file_type: ft,
                    segments,
                    ..
                } = &entry.content
                {
                    if *ft == file_type {
                        // Return a slice to the first segment's data
                        if let Some(seg) = segments.first() {
                            let offset = (region.offset + entry.offset + seg.offset) as usize;
                            let size = seg.stored_size as usize;
                            if offset + size <= image.len() {
                                return Some(&image[offset..offset + size]);
                            }
                        }
                    }
                }
            }
        }
    }

    fstart_log::error!("find file data: file type not found in FFS");
    None
}

/// Load all segments of a file entry to their load addresses from any boot medium.
///
/// For each segment, reads data from the boot medium directly to the
/// target load address. This works uniformly for both memory-mapped and
/// block-device-backed media:
///
/// - **Memory-mapped**: `media.read_at()` inlines to `memmove`, identical
///   to the previous direct `ptr::copy` implementation.
/// - **Block device**: `media.read_at()` calls the device's `read()` method,
///   copying data directly to the load address — single copy, no intermediate
///   buffer.
///
/// `image_size` is the effective image size (capped by the anchor's
/// `total_image_size`). All segment source offsets are bounds-checked
/// against this limit as defense-in-depth — even though the manifest is
/// signature-verified, corrupt offsets would require a compromised key.
///
/// Returns the entry address (load_addr of the first Code segment, or
/// load_addr of the first segment if no Code segments).
#[cfg(feature = "ffs")]
fn load_entry_segments_from_media(
    media: &impl BootMedia,
    entry: &fstart_types::ffs::RegionEntry,
    region: &fstart_types::ffs::Region,
    image_size: usize,
) -> Option<u64> {
    let segments = match &entry.content {
        fstart_types::ffs::EntryContent::File { segments, .. } => segments,
        _ => {
            fstart_log::error!("entry is not a file");
            return None;
        }
    };

    let mut entry_addr: Option<u64> = None;

    for seg in segments {
        if seg.kind == fstart_types::ffs::SegmentKind::Bss {
            // BSS: zero-fill at load_addr
            let dest = seg.load_addr as *mut u8;
            // SAFETY: we trust the board config; the load_addr points to writable RAM.
            unsafe {
                core::ptr::write_bytes(dest, 0, seg.loaded_size as usize);
            }
            fstart_log::debug!(
                "  BSS: {} ({} bytes zeroed)",
                Hex(seg.load_addr),
                seg.loaded_size
            );
        } else {
            // Data segment: read from boot medium to load_addr
            let src_offset = (region.offset + entry.offset + seg.offset) as usize;
            let stored_size = seg.stored_size as usize;
            let dest = seg.load_addr as *mut u8;

            // Defense-in-depth: verify the segment's source data falls within
            // the effective image size. The manifest is signature-verified so
            // this should never trip unless the signing key is compromised.
            if src_offset.saturating_add(stored_size) > image_size {
                fstart_log::error!(
                    "segment '{}' out of bounds: offset {} + size {} > image {}",
                    seg.name.as_str(),
                    src_offset as u32,
                    stored_size as u32,
                    image_size as u32,
                );
                return None;
            }

            match seg.compression {
                fstart_types::ffs::Compression::None => {
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
                        "  {}: {} ({} bytes)",
                        seg.name.as_str(),
                        Hex(seg.load_addr),
                        stored_size
                    );
                }
                #[cfg(feature = "lz4")]
                fstart_types::ffs::Compression::Lz4 => {
                    // In-place LZ4 decompression (coreboot technique):
                    // 1. The builder verified that `in_place_size` bytes at
                    //    load_addr suffice for safe in-place decompression.
                    // 2. Read compressed data to the END of the buffer:
                    //    dest + in_place_size - stored_size
                    // 3. Decompress from tail to head — the decompressor
                    //    reads from the tail while writing from the head.
                    let buf_size = seg.in_place_size as usize;
                    let loaded_size = seg.loaded_size as usize;

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
                                "  {}: {} ({} -> {} bytes, lz4 in-place)",
                                seg.name.as_str(),
                                Hex(seg.load_addr),
                                stored_size,
                                n
                            );
                        }
                        Err(_) => {
                            fstart_log::error!(
                                "LZ4 in-place decompress failed: {}",
                                seg.name.as_str()
                            );
                            return None;
                        }
                    }
                }
                #[cfg(not(feature = "lz4"))]
                fstart_types::ffs::Compression::Lz4 => {
                    fstart_log::error!("LZ4 compressed segment but lz4 feature not enabled");
                    return None;
                }
            }
        }

        // Use the first Code segment's load_addr as the entry point,
        // or fall back to the first segment's load_addr.
        if entry_addr.is_none() || seg.kind == fstart_types::ffs::SegmentKind::Code {
            entry_addr = Some(seg.load_addr);
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
fn effective_image_size(media_size: usize, anchor: &fstart_types::ffs::AnchorBlock) -> usize {
    if anchor.total_image_size > 0 && (anchor.total_image_size as usize) < media_size {
        anchor.total_image_size as usize
    } else {
        media_size
    }
}
