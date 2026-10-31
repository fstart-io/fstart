//! Common fixed-flow stage glue.
//!
//! Board crates expose a stage entry implementation owned by their platform
//! family flow. This crate provides shared anchor/allocation linkage and the
//! board-owned entry macro.

#![no_std]

#[cfg(any(
    feature = "acpi",
    feature = "ffs",
    feature = "alloc",
    feature = "smbios"
))]
mod alloc;

mod runtime;

#[cfg(any(feature = "ffs", feature = "alloc"))]
extern crate alloc as heap;

// Selected through features even when no payload calls into it: the arch
// crate supplies entry code and the early FFS anchor.
use fstart_arch as _;

#[cfg(feature = "bootstrap")]
pub mod boot;
#[cfg(feature = "bootstrap")]
mod fdt_workspace;
#[cfg(feature = "bootstrap")]
pub mod stage_cache;
#[cfg(feature = "fdt")]
pub use fdt_workspace::fdt_prepare_platform;
#[cfg(feature = "bootstrap")]
pub use fdt_workspace::{configure_fdt_workspace, copy_fdt_to_workspace, fdt_workspace_capacity};
#[cfg(feature = "ffs")]
pub mod directory;
#[cfg(feature = "ffs")]
mod loaded;
#[cfg(feature = "bootstrap")]
pub mod root;

pub mod fixed_helpers;
pub mod layout;
#[cfg(feature = "tcg")]
pub mod tcg;
pub mod timestamps;

#[cfg(feature = "fit")]
pub mod fit;

#[cfg(feature = "handoff")]
pub mod handoff;

pub mod next_stage;

#[cfg(all(test, feature = "ffs"))]
mod ram_locator_tests;

#[cfg(any(feature = "ffs", feature = "fdt"))]
use fstart_log::Hex;

#[cfg(any(feature = "ffs", feature = "ffs-signature"))]
use fstart_core::services::BootMedia;

// ---------------------------------------------------------------------------
// SigVerify
// ---------------------------------------------------------------------------

/// Confirm an installed verified directory, or authenticate the bounded boot
/// root. This performs no file loading and does not authenticate running RAM.
/// Mainstage services must separately install the directory after DRAM setup.
/// Works with both mapped and block media; root reads use a 512-byte buffer.
///
/// # Arguments
///
/// - `anchor_data`: Reference to the `FSTART_ANCHOR` static (raw bytes).
/// - `media`: The boot medium holding the firmware image.
#[cfg(feature = "ffs")]
pub fn sig_verify(anchor_data: &[u8], media: &(impl BootMedia + ?Sized)) -> bool {
    if directory::view(media).is_ok() {
        return true;
    }
    // Early compatibility helper uses only the bounded root. Mainstage must
    // explicitly install its owned directory after DRAM/allocator setup.
    root::authenticate_boot_root(anchor_data, media).is_ok()
}

/// Stub SigVerify when FFS feature is not enabled.
#[cfg(not(feature = "ffs"))]
pub fn sig_verify(
    _anchor_data: &[u8],
    _media: &(impl fstart_core::services::BootMedia + ?Sized),
) -> bool {
    false
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

    let manifest = match read_manifest_from_media(media, anchor) {
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
    let image_size = effective_image_size(media.size(), anchor);
    let entry_addr = match load_file_segments_from_media(media, &file, image_size) {
        Some(addr) => addr,
        None => return,
    };

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
    let role = match next_stage {
        "postcar" => fstart_ffs::root::BootstrapRole::Postcar,
        "ramstage" | "mainstage" | "main" => fstart_ffs::root::BootstrapRole::Mainstage,
        _ => return,
    };
    let Ok(root) = root::authenticate_boot_root(anchor_data, media) else {
        return;
    };
    let (Some(policy), Some(codec)) = (directory::load_policy(), directory::codec()) else {
        return;
    };
    let mut descriptors = root
        .descriptors()
        .iter()
        .flatten()
        .filter(|d| d.role == role);
    let Some(descriptor) = descriptors.next() else {
        return;
    };
    if descriptors.next().is_some() {
        return;
    }
    // SAFETY: the platform installed the trusted physical mapping policy.
    if let Ok(executable) = unsafe { boot::load_bootstrap(media, descriptor, &policy, codec) } {
        jump_to(executable.entry());
    }
}

/// Stub StageLoad — called when no boot medium is configured.
pub fn stage_load_stub(next_stage: &str) {
    fstart_log::info!("stage helper: StageLoad -> {}", next_stage);
    fstart_log::info!("stage load skipped (not yet wired to FFS)");
}

// ---------------------------------------------------------------------------
// FFS Helpers (behind `ffs` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "ffs")]
mod ffs_helpers;
#[cfg(feature = "ffs")]
pub use ffs_helpers::*;
#[cfg(feature = "ffs")]
use ffs_helpers::{load_file_segments_from_media, read_manifest_from_media, reader_error_str};

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
fn effective_image_size(media_size: usize, anchor: fstart_core::ffs::AnchorRef<'_>) -> usize {
    let total_image_size = anchor.total_image_size() as usize;
    if total_image_size > 0 && total_image_size < media_size {
        total_image_size
    } else {
        media_size
    }
}

pub mod anchor;
pub use anchor::{fstart_anchor_bytes, fstart_trust_bytes};

/// Stop the boot: log what failed and halt.
#[inline(never)]
pub fn fail(what: &str) -> ! {
    fstart_log::error!("{} failed", what);
    fstart_arch::halt()
}

/// Run one named boot step: log it, run it, and [`fail`] if it did.
pub fn step<T, E>(name: &str, f: impl FnOnce() -> Result<T, E>) -> T {
    fstart_log::info!("{}", name);
    match f() {
        Ok(value) => value,
        Err(_) => fail(name),
    }
}

/// Bind a board's stage `main` to the stage entry (`fstart_main`). Platform
/// `entry!` macros expand to this.
///
/// Platforms that offer the CrabEFI payload add `sbi_resume` so RISC-V
/// stages export the SBI resume entry; the `fstart_payload` cfg it checks is
/// only declared for such platforms.
#[macro_export]
macro_rules! stage_bin {
    // A board-owned stage `main`: the binary is the flow.
    (main: $main:path) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn fstart_main(_handoff: usize) -> ! {
            $main()
        }
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
    };
    // A board-owned `main` that takes the predecessor's handoff address.
    (main_handoff: $main:path) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn fstart_main(handoff: usize) -> ! {
            $main(handoff)
        }
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
    };
    // A board-owned `main` plus the function OpenSBI returns to before a
    // RISC-V CrabEFI launch.
    (main: $main:path, sbi_resume: $resume:path) => {
        $crate::stage_bin!(main: $main);
        #[cfg(all(fstart_payload = "crabefi", target_arch = "riscv64"))]
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_sbi_resume(hart_id: u64, dtb_addr: u64) -> ! {
            $resume(hart_id, dtb_addr)
        }
        #[cfg(not(all(fstart_payload = "crabefi", target_arch = "riscv64")))]
        const _: fn(u64, u64) -> ! = $resume;
    };
}
