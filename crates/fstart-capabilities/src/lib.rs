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
//! See [docs/driver-model.md](../../docs/driver-model.md) for the full
//! driver model architecture.

#![no_std]

#[cfg(feature = "fdt")]
extern crate alloc;

#[cfg(any(feature = "ffs", feature = "fdt"))]
use fstart_log::Hex;

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
// SigVerify
// ---------------------------------------------------------------------------

/// Verify the firmware filesystem manifest signature.
///
/// `anchor_data` is a reference to the `FSTART_ANCHOR` static embedded in
/// the bootblock binary. Read via `read_volatile` to see post-build patched
/// values.
///
/// `flash_base` and `flash_size` specify where the firmware image sits
/// in memory. If `flash_size` is 0, verification is skipped.
#[cfg(feature = "ffs")]
pub fn sig_verify(anchor_data: &[u8], flash_base: u64, flash_size: u64) {
    fstart_log::info!("capability: SigVerify");

    if flash_size == 0 || anchor_data.is_empty() {
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

    // Use the smaller of flash_size and total_image_size as the reader's
    // window. On XIP platforms, flash_size may be the full flash bank
    // (e.g., 128 MiB) while the FFS image is much smaller. Using the
    // anchor's total_image_size ensures the reader only accesses data
    // that was actually written by the builder.
    let image_size = if anchor.total_image_size > 0 && (anchor.total_image_size as u64) < flash_size
    {
        anchor.total_image_size as usize
    } else {
        flash_size as usize
    };

    // SAFETY: we trust that flash_base..flash_base+image_size is mapped and
    // readable memory (guaranteed by the board config and platform setup).
    //
    // When flash_base is 0 (AArch64 XIP), we must avoid creating a slice
    // with a null data pointer, as Rust considers `from_raw_parts(null, n)`
    // UB for n > 0 — the compiler may exploit this (e.g., optimizing
    // `.get()` to always return None). We use `opaque_addr()` to hide the
    // zero value from the optimizer.
    let image = unsafe { core::slice::from_raw_parts(opaque_addr(flash_base), image_size) };
    let reader = fstart_ffs::FfsReader::new(image);

    // Verify the manifest signature
    let manifest = match reader.read_manifest(&anchor) {
        Ok(m) => m,
        Err(e) => {
            fstart_log::error!(
                "sig verify: manifest verification FAILED: {}",
                reader_error_str(e)
            );
            return;
        }
    };

    // Find the first container region and verify its file digests
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
                    Err(fstart_ffs::ReaderError::CannotVerifyInPlace) => total_skipped += 1,
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
}

/// Stub SigVerify when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn sig_verify(_anchor_data: &[u8], _flash_base: u64, _flash_size: u64) {
    fstart_log::info!("capability: SigVerify");
    fstart_log::info!("sig verify skipped (ffs feature not enabled)");
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

/// Prepare a Flattened Device Tree for OS handoff from a platform-provided DTB.
///
/// Parses the DTB that QEMU/firmware passed at reset (via `a1` on RISC-V
/// or `x0` on AArch64), patches `/chosen/bootargs`, and serializes the
/// result to `dst_dtb_addr` where the payload (Linux) expects it.
///
/// Requires the `fdt` feature (pulls in `dtoolkit` + bump allocator).
///
/// # Arguments
///
/// - `src_dtb_addr` — address of the platform-provided DTB (0 = skip)
/// - `dst_dtb_addr` — target address for the patched DTB
/// - `bootargs` — kernel command line to set in `/chosen/bootargs` (empty = skip)
#[cfg(feature = "fdt")]
pub fn fdt_prepare_platform(src_dtb_addr: u64, dst_dtb_addr: u64, bootargs: &str) {
    use alloc::vec::Vec;
    use dtoolkit::fdt::Fdt;
    use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};

    fstart_log::info!("capability: FdtPrepare");

    if src_dtb_addr == 0 {
        fstart_log::info!("FDT prepare: no DTB from platform, skipping");
        return;
    }

    if dst_dtb_addr == 0 {
        fstart_log::error!("FDT prepare: dst_dtb_addr is 0, skipping (misconfigured board?)");
        return;
    }

    fstart_log::info!("FDT prepare: source DTB at {}", Hex(src_dtb_addr));

    // SAFETY: the platform entry code saved the DTB address from a register
    // provided by QEMU. The pointer is valid and the FDT blob is in
    // memory-mapped RAM/flash that is readable at this point.
    let fdt = match unsafe { Fdt::from_raw(src_dtb_addr as *const u8) } {
        Ok(f) => f,
        Err(_) => {
            fstart_log::error!("FDT prepare: failed to parse source DTB");
            return;
        }
    };

    // Convert the zero-copy FDT into a mutable tree (allocates via bump allocator)
    let mut tree = match DeviceTree::from_fdt(&fdt) {
        Ok(t) => t,
        Err(_) => {
            fstart_log::error!("FDT prepare: failed to convert to mutable tree");
            return;
        }
    };

    // Patch /chosen/bootargs if bootargs is non-empty
    if !bootargs.is_empty() {
        // Find or create /chosen node
        if tree.find_node_mut("/chosen").is_none() {
            tree.root.add_child(DeviceTreeNode::new("chosen"));
        }
        let chosen = match tree.find_node_mut("/chosen") {
            Some(n) => n,
            None => {
                fstart_log::error!("FDT prepare: failed to find /chosen");
                return;
            }
        };

        // Build null-terminated bootargs value (DTB spec requires it)
        let mut args_bytes = Vec::from(bootargs.as_bytes());
        args_bytes.push(0);

        if let Some(prop) = chosen.property_mut("bootargs") {
            prop.set_value(args_bytes);
        } else {
            chosen.add_property(DeviceTreeProperty::new("bootargs", args_bytes));
        }

        fstart_log::info!("FDT prepare: bootargs = \"{}\"", bootargs);
    }

    // Serialize the modified tree back to a DTB blob
    let dtb_bytes = tree.to_dtb();

    fstart_log::info!(
        "FDT prepare: serialized {} bytes to {}",
        dtb_bytes.len(),
        Hex(dst_dtb_addr)
    );

    // SAFETY: dst_dtb_addr points to writable RAM with enough space for the
    // DTB blob. The board config must ensure this region doesn't overlap with
    // firmware, kernel, or stack.
    unsafe {
        core::ptr::copy_nonoverlapping(
            dtb_bytes.as_ptr(),
            dst_dtb_addr as *mut u8,
            dtb_bytes.len(),
        );
    }

    fstart_log::info!("FDT prepare complete");
}

// ---------------------------------------------------------------------------
// PayloadLoad
// ---------------------------------------------------------------------------

/// Load and jump to the payload (OS kernel, shell, etc.).
///
/// Reads the payload from FFS, copies its segments to load addresses,
/// and transfers control via the provided `jump_to` function.
///
/// `anchor_data` is a reference to the `FSTART_ANCHOR` static embedded
/// in the bootblock binary. Read via volatile to see patched values.
#[cfg(feature = "ffs")]
pub fn payload_load(anchor_data: &[u8], flash_base: u64, flash_size: u64, jump_to: fn(u64) -> !) {
    fstart_log::info!("capability: PayloadLoad");

    if flash_size == 0 || anchor_data.is_empty() {
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

    // Use the anchor's total_image_size when smaller than flash_size.
    let image_size = if anchor.total_image_size > 0 && (anchor.total_image_size as u64) < flash_size
    {
        anchor.total_image_size as usize
    } else {
        flash_size as usize
    };

    // SAFETY: flash_base..flash_base+image_size is mapped readable memory.
    let image = unsafe { core::slice::from_raw_parts(opaque_addr(flash_base), image_size) };
    let reader = fstart_ffs::FfsReader::new(image);

    let manifest = match reader.read_manifest(&anchor) {
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
    let entry_addr = match load_entry_segments(&reader, entry, region) {
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
/// Reads the named stage binary from the firmware filesystem, copies its
/// segments to the load addresses, and transfers control via `jump_to`.
///
/// `anchor_data` is a reference to the `FSTART_ANCHOR` static embedded
/// in the bootblock binary. Read via volatile to see patched values.
#[cfg(feature = "ffs")]
pub fn stage_load(
    next_stage: &str,
    anchor_data: &[u8],
    flash_base: u64,
    flash_size: u64,
    jump_to: fn(u64) -> !,
) {
    fstart_log::info!("capability: StageLoad -> {}", next_stage);

    if flash_size == 0 || anchor_data.is_empty() {
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

    // Use the anchor's total_image_size when smaller than flash_size.
    let image_size = if anchor.total_image_size > 0 && (anchor.total_image_size as u64) < flash_size
    {
        anchor.total_image_size as usize
    } else {
        flash_size as usize
    };

    // SAFETY: flash_base..flash_base+image_size is mapped readable memory.
    let image = unsafe { core::slice::from_raw_parts(opaque_addr(flash_base), image_size) };
    let reader = fstart_ffs::FfsReader::new(image);

    let manifest = match reader.read_manifest(&anchor) {
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
    let entry_addr = match load_entry_segments(&reader, entry, region) {
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

/// Load a file from FFS by its `FileType`, placing segments at their load addresses.
///
/// Searches all container regions in the manifest for the first file entry
/// matching `file_type`, then loads its segments. Returns `true` on success.
///
/// Used by the Linux boot path to load firmware and kernel blobs from FFS.
#[cfg(feature = "ffs")]
pub fn load_ffs_file_by_type(
    anchor_data: &[u8],
    flash_base: u64,
    flash_size: u64,
    file_type: fstart_types::ffs::FileType,
) -> bool {
    if flash_size == 0 || anchor_data.is_empty() {
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

    // Use the anchor's total_image_size when smaller than flash_size,
    // so the reader only accesses FFS image data (see sig_verify comment).
    let image_size = if anchor.total_image_size > 0 && (anchor.total_image_size as u64) < flash_size
    {
        anchor.total_image_size as usize
    } else {
        flash_size as usize
    };

    // SAFETY: flash_base..flash_base+image_size is mapped readable memory.
    let image = unsafe { core::slice::from_raw_parts(opaque_addr(flash_base), image_size) };
    let reader = fstart_ffs::FfsReader::new(image);

    let manifest = match reader.read_manifest(&anchor) {
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

    fstart_log::info!("load file: loading '{}'", entry.name.as_str());

    load_entry_segments(&reader, entry, region).is_some()
}

/// Load all segments of a file entry to their load addresses.
///
/// Returns the entry address (load_addr of the first Code segment, or
/// load_addr of the first segment if no Code segments).
#[cfg(feature = "ffs")]
fn load_entry_segments(
    reader: &fstart_ffs::FfsReader<'_>,
    entry: &fstart_types::ffs::RegionEntry,
    region: &fstart_types::ffs::Region,
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
            // Data segment: copy from flash to load_addr
            let data = match reader.read_segment_data(seg, region, entry) {
                Ok(d) => d,
                Err(e) => {
                    fstart_log::error!("segment read error: {}", reader_error_str(e));
                    return None;
                }
            };

            let dest = seg.load_addr as *mut u8;

            match seg.compression {
                fstart_types::ffs::Compression::None => {
                    // SAFETY: we trust the board config; the load_addr points to
                    // writable RAM and `data.len()` bytes fit. We use `copy`
                    // (memmove) instead of `copy_nonoverlapping` (memcpy) for
                    // robustness: when the FFS image is loaded into RAM (not
                    // flash), source and destination regions may overlap —
                    // e.g., loading a large kernel from FFS at 0x80000000 to
                    // a nearby load address.
                    unsafe {
                        core::ptr::copy(data.as_ptr(), dest, data.len());
                    }
                    fstart_log::debug!(
                        "  {}: {} ({} bytes)",
                        seg.name.as_str(),
                        Hex(seg.load_addr),
                        data.len()
                    );
                }
                #[cfg(feature = "lz4")]
                fstart_types::ffs::Compression::Lz4 => {
                    // In-place LZ4 decompression (coreboot technique):
                    // 1. The builder verified that `in_place_size` bytes at
                    //    load_addr suffice for safe in-place decompression.
                    // 2. Copy compressed data to the END of the buffer:
                    //    dest + in_place_size - stored_size
                    // 3. Decompress from tail to head — the decompressor
                    //    reads from the tail while writing from the head.
                    let buf_size = seg.in_place_size as usize;
                    let stored_size = seg.stored_size as usize;
                    let loaded_size = seg.loaded_size as usize;

                    // SAFETY: load_addr points to writable RAM with at least
                    // `in_place_size` bytes available (verified by the builder
                    // and guaranteed by the linker script / board config).
                    let buf = unsafe { core::slice::from_raw_parts_mut(dest, buf_size) };

                    // Copy compressed data to the tail of the buffer
                    let src_offset = buf_size - stored_size;
                    // SAFETY: src (flash) and dst (RAM tail) don't overlap.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            data.as_ptr(),
                            buf.as_mut_ptr().add(src_offset),
                            stored_size,
                        );
                    }

                    // Decompress in-place: read from tail, write from head.
                    // SAFETY: the builder simulated this exact operation at
                    // build time and verified it succeeds. The src slice
                    // overlaps the tail of buf — our decompressor handles
                    // this (in-place guard checks output doesn't overtake input).
                    let result = unsafe {
                        let src =
                            core::slice::from_raw_parts(buf.as_ptr().add(src_offset), stored_size);
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

/// Convert a `u64` address to a `*const u8`, preventing the compiler from
/// recognising zero as null.
///
/// On AArch64 XIP, flash genuinely resides at physical address 0x0. Rust
/// treats `from_raw_parts(null, n)` as UB for n > 0, and the compiler may
/// exploit this — for example by making `.get()` always return `None`.
/// Passing the address through `read_volatile` hides the concrete value
/// from the optimizer so it cannot fold the null check.
#[cfg(feature = "ffs")]
#[inline(always)]
fn opaque_addr(addr: u64) -> *const u8 {
    let result: u64;
    // SAFETY: a u64 on the stack is always readable. Volatile read prevents
    // the compiler from constant-folding the value.
    unsafe {
        result = core::ptr::read_volatile(&addr);
    }
    result as *const u8
}
