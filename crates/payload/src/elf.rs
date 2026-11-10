//! Bare ELF payload launcher (TamaGo images, bare-metal programs): loads the
//! verified, flattened ELF and enters it at the entry point from its
//! verified manifest, with no arguments.

use crate::{Handoff, MainstagePayload};
use fstart_core::ffs::FileType;
use fstart_core::payload_manifest::{ELF_MANIFEST_ASSET, ElfManifest};
use fstart_core::services::ffs_context;

/// Boots the build-selected bare ELF payload.
pub struct ElfPayload;

impl<D> MainstagePayload<D> for ElfPayload {
    fn boot(_: Handoff<'_>, _: D) -> ! {
        let Some(manifest) =
            ffs_context::read_verified_asset(ELF_MANIFEST_ASSET).and_then(ElfManifest::decode)
        else {
            halt("no verified manifest");
        };
        let base = load_payload().unwrap_or_else(|reason| halt(reason));
        fstart_log::info!(
            "ELF payload: loaded at {:#x}, entering {:#x}",
            base,
            manifest.entry
        );
        fstart_stage::timestamps::handoff();
        fstart_log::flush();
        enter(manifest.entry)
    }
}

fn load_payload() -> Result<u64, &'static str> {
    let context = ffs_context::memory_mapped().ok_or("no mounted boot media")?;
    let size = usize::try_from(context.image_size).map_err(|_| "boot media too large")?;
    // SAFETY: the mainstage published this mapped, verified firmware window.
    let media = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(context.image_base, size)
    };
    // The payload is platform firmware from the signed image: PCR 0, as it
    // loads.
    let measurement = fstart_stage::boot::Measurement {
        log: fstart_stage::boot::measurement_log(),
        event: fstart_stage::boot::Event {
            pcr: 0,
            kind: fstart_core::services::measure::kind::EV_POST_CODE2,
            description: fstart_core::services::measure::post_code::POST_CODE,
            base: 0,
        },
    };
    fstart_stage::load_ffs_file_measured(&media, FileType::Payload, &measurement)
        .ok_or("payload load or verification failed")
}

/// Jump to `entry`; the arch helper makes the freshly loaded code visible
/// to instruction fetch first.
fn enter(entry: u64) -> ! {
    #[cfg(all(feature = "armv5te", target_arch = "arm"))]
    fstart_arch::armv5te::jump_to(entry);
    #[cfg(not(all(feature = "armv5te", target_arch = "arm")))]
    {
        let _ = entry;
        halt("not supported on this architecture")
    }
}

fn halt(reason: &str) -> ! {
    fstart_log::error!("ELF payload: {}", reason);
    fstart_arch::halt()
}
