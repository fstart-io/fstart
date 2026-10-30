//! Measured boot of FFS contents (see `fstart-tcg`).
//!
//! Files are measured as stored in the mounted, memory-mapped boot media,
//! like coreboot measures CBFS files: the SHA-256 bank digest then equals
//! the file's stored digest in the authenticated FFS directory, and the
//! event's blob base is the file's flash address, which stays valid for the
//! OS (PFP §10.2.5).

use fstart_core::ffs::FileType;
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::ffs_context;
use fstart_crypto::digest::hash_sha256;
use fstart_ffs::FileView;
pub use fstart_tcg::Handoff;
pub use fstart_tcg::event;

/// [`fstart_tcg::handoff`]; halts when the TPM cannot be handed over
/// safely (unlocked platform hierarchy, uncapped PCRs).
pub fn handoff(kind: Handoff) {
    if fstart_tcg::handoff(kind).is_err() {
        refuse();
    }
}

/// [`fstart_tcg::check`] after measurements that follow [`handoff`].
pub fn check() {
    if fstart_tcg::check().is_err() {
        refuse();
    }
}

fn refuse() -> ! {
    fstart_log::error!("tcg: refusing to start the payload with an unprotected TPM");
    fstart_arch::halt()
}

fn with_file<R>(
    find: impl FnOnce(&fstart_ffs::ManifestView<'static>) -> Option<FileView<'static>>,
    f: impl FnOnce(&str, u64, &'static [u8]) -> R,
) -> Option<R> {
    let context = ffs_context::memory_mapped()?;
    let size = usize::try_from(context.image_size).ok()?;
    // SAFETY: the mainstage published this mapped, verified firmware window.
    let media = unsafe { MemoryMapped::from_raw_addr(context.image_base, size) };
    let directory = crate::directory::view(&media).ok()?;
    let file = find(&directory)?;
    let [segment] = file.segments() else {
        return None;
    };
    let offset = u64::from(file.region_offset())
        .checked_add(u64::from(file.entry_offset()))?
        .checked_add(u64::from(segment.offset()))?;
    let stored = usize::try_from(segment.stored_size()).ok()?;
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(stored)?;
    if end > size {
        return None;
    }
    // SAFETY: inside the published window, which lives for the stage.
    let bytes =
        unsafe { core::slice::from_raw_parts((context.image_base + offset) as *const u8, stored) };
    // The bytes measured are the bytes the loader authenticates.
    if hash_sha256(bytes) != segment.stored_digest() {
        return None;
    }
    Some(f(file.name().ok()?, context.image_base + offset, bytes))
}

fn measure_file(
    find: impl FnOnce(&fstart_ffs::ManifestView<'static>) -> Option<FileView<'static>>,
    pcr: u32,
    event_type: u32,
    description: Option<&str>,
) -> bool {
    with_file(find, |name, base, bytes| {
        fstart_tcg::measure_blob(pcr, event_type, description.unwrap_or(name), base, bytes);
    })
    .is_some()
}

/// Measure the first FFS file of `file_type` into `pcr`; the blob
/// description is `description`, or the file name.
pub fn measure_file_by_type(
    file_type: FileType,
    pcr: u32,
    event_type: u32,
    description: Option<&str>,
) -> bool {
    measure_file(
        |directory| directory.find_file_by_type(file_type).ok(),
        pcr,
        event_type,
        description,
    )
}

/// Measure the FFS file `name` into `pcr`.
pub fn measure_file_by_name(
    name: &str,
    pcr: u32,
    event_type: u32,
    description: Option<&str>,
) -> bool {
    measure_file(
        |directory| directory.find_file_by_name(name).ok(),
        pcr,
        event_type,
        description,
    )
}

/// The static root of trust measures itself (PFP §3.3.4.1, no H-CRTM): its
/// version, then the stage file `stage` it runs from as
/// `EV_S_CRTM_CONTENTS`. Measures nothing unless `fstart_tcg::start`
/// succeeded.
pub fn measure_srtm(version: &str, stage: &str) {
    if fstart_tcg::family().is_none() {
        return;
    }
    fstart_tcg::measure_crtm_version(version);
    if !measure_file_by_name(stage, 0, event::EV_S_CRTM_CONTENTS, None) {
        fstart_log::error!("tcg: cannot measure the stage file '{}'", stage);
        fstart_tcg::measurement_error();
    }
}

/// The S-CRTM version string: `fstart <crate version>`.
pub const CRTM_VERSION: &str = concat!("fstart ", env!("CARGO_PKG_VERSION"));
