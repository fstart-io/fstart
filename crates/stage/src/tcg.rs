//! Measured boot (see `fstart-tcg`): the TPM handoff and the static root
//! of trust's self-measurement.
//!
//! What the boot loads is measured on the loading chain
//! ([`crate::boot::Measurement`]), as the bytes it verified and then uses.
//! Only the root of trust's own image is measured where the boot media maps
//! it ([`crate::boot::InPlace`]), like coreboot measures CBFS files: the
//! SHA-256 bank digest equals the file's stored digest in the authenticated
//! FFS directory, and the event's blob base is its flash address, which
//! stays valid for the OS (PFP §10.2.5).

use crate::boot::{Event, InPlace, Measurement};
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::ffs_context;
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

/// The static root of trust measures itself (PFP §3.3.4.1, no H-CRTM): its
/// version, then the stage file `stage` it runs from as
/// `EV_S_CRTM_CONTENTS`. Measures nothing unless `fstart_tcg::start`
/// succeeded.
pub fn measure_srtm(version: &str, stage: &str) {
    if fstart_tcg::family().is_none() {
        return;
    }
    fstart_tcg::measure_crtm_version(version);
    if measure_stage_file(stage).is_none() {
        fstart_log::error!("tcg: cannot measure the stage file '{}'", stage);
        fstart_tcg::measurement_error();
    }
}

/// The stored bytes of the FFS file `name`, where the mounted boot media
/// maps them, as `EV_S_CRTM_CONTENTS` in PCR 0.
fn measure_stage_file(name: &str) -> Option<()> {
    let context = ffs_context::memory_mapped()?;
    let size = usize::try_from(context.image_size).ok()?;
    // SAFETY: the mainstage published this mapped, verified firmware window.
    let media = unsafe { MemoryMapped::from_raw_addr(context.image_base, size) };
    let directory = crate::directory::view(&media).ok()?;
    let file = directory.find_file_by_name(name).ok()?;
    let [segment] = file.segments() else {
        return None;
    };
    let offset = u64::from(file.region_offset())
        .checked_add(u64::from(file.entry_offset()))?
        .checked_add(u64::from(segment.offset()))?;
    let bytes = InPlace::new(&media, offset, u64::from(segment.stored_size())).ok()?;
    let measurement = Measurement {
        log: &fstart_tcg::Tcg,
        event: Event {
            pcr: 0,
            kind: event::EV_S_CRTM_CONTENTS,
            description: file.name().ok()?,
            base: bytes.address(),
        },
    };
    bytes
        .check::<fstart_load::Sha256>(&segment.stored_digest())
        .ok()?
        .measure(&measurement);
    Some(())
}

/// The S-CRTM version string: `fstart <crate version>`.
pub const CRTM_VERSION: &str = concat!("fstart ", env!("CARGO_PKG_VERSION"));
