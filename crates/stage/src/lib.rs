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

#[cfg(feature = "ffs")]
extern crate alloc as heap;

#[cfg(feature = "bootstrap")]
pub mod boot;
#[cfg(feature = "bootstrap")]
mod fdt_workspace;
#[cfg(feature = "fdt")]
pub use fdt_workspace::fdt_prepare_platform;
#[cfg(feature = "bootstrap")]
pub use fdt_workspace::{configure_fdt_workspace, copy_fdt_to_workspace};
#[cfg(feature = "ffs")]
pub mod directory;
#[cfg(feature = "ffs")]
mod loaded;
#[cfg(feature = "bootstrap")]
pub mod root;

#[cfg(feature = "crabefi")]
pub use fstart_boot::crabefi;

pub mod fixed_helpers;
pub mod layout;
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
    let Some(policy) = directory::load_policy() else {
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
    if let Ok(executable) = unsafe { boot::load_bootstrap(media, descriptor, &policy) } {
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

/// Storage for an FFS anchor patched after the stage has been linked.
///
/// Interior mutability prevents LLVM from treating the placeholder bytes as a
/// constant while preserving the exact `AnchorBlock` representation expected
/// by the post-build patcher.
#[repr(transparent)]
pub struct PatchableAnchor(core::cell::UnsafeCell<fstart_core::ffs::AnchorBlock>);

impl PatchableAnchor {
    const fn placeholder() -> Self {
        Self(core::cell::UnsafeCell::new(
            fstart_core::ffs::AnchorBlock::placeholder(),
        ))
    }
}

// SAFETY: firmware only reads this storage at runtime. `fbuild assemble`
// modifies the flat binary before execution, not through a Rust reference.
unsafe impl Sync for PatchableAnchor {}

/// Fixed FFS anchor placeholder for handwritten stage flow.
///
/// `fbuild assemble` patches this block in the flat stage binary after laying out
/// the complete firmware image.
#[used]
#[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.anchor"))]
pub static FSTART_ANCHOR: PatchableAnchor = PatchableAnchor::placeholder();

#[must_use]
pub fn fstart_anchor_bytes() -> &'static [u8] {
    // SAFETY: FSTART_ANCHOR is transparent over a repr(C) AnchorBlock, is
    // placed in `.fstart.anchor`, and has exactly ANCHOR_SIZE initialized bytes.
    // Runtime code only reads it after the image patcher has finished.
    unsafe {
        core::slice::from_raw_parts(
            FSTART_ANCHOR.0.get().cast::<u8>(),
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
