//! S3 resume: what a cold boot keeps in reserved DRAM so a resume runs
//! without the flash it may have been updated while suspended.
//!
//! The stage cache ([`stage_cache`]) holds the compressed bodies of the
//! stages a resume runs again. A cold boot loads a stage from flash and
//! caches it ([`load_stage`]); a resume loads it from the cache through the
//! same verified loader. A platform advertises S3 only once every stage it
//! needs is cached ([`cached`]).

#![no_std]

pub mod stage_cache;

use fstart_core::services::BootMedia;
use fstart_core::services::boot_media::{LinearMap, MemoryMapped};
use fstart_ffs::root::BootstrapDescriptor;
use fstart_load::{Codec, Measurement, MemoryPolicy, VerifiedExecutable};
use fstart_store::Store;
use stage_cache::{CachedStage, STAGE_CACHE_HEADER_LEN};

/// Why [`load_stage`] could not hand out a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    /// The cold boot load from flash failed.
    Load(fstart_load::LoadError),
    /// A resume found no cached copy that verifies. Resuming a mixed or
    /// corrupted image is never acceptable: the platform resets to a cold
    /// boot.
    NotCached,
}

/// A store entry's bytes, read through the boot media interface.
fn entry_media(store: &Store, entry: &fstart_store::Entry) -> MemoryMapped<LinearMap> {
    // SAFETY: the entry lies in the store, which stays mapped and reserved;
    // only shared reads of it exist while this view lives.
    unsafe { MemoryMapped::from_raw_addr(store.address(entry) as u64, entry.len()) }
}

/// Load the bootstrap `stage` named by the authenticated `descriptor` and
/// keep its S3 cache entry coherent.
///
/// Cold boot: verify the stage from `media` into `policy`'s window,
/// measured as `measurement`, then
/// copy the stored bytes it just verified in RAM into a store entry. A
/// failed copy is logged; the platform then withholds S3 ([`cached`]).
/// Resume: load the stage from that entry, which re-checks the stored
/// digest against the freshly authenticated descriptor, and is not
/// measured: the TPM keeps the PCRs the cold boot extended.
///
/// # Safety
/// As [`fstart_load::load_bootstrap`]: `policy` describes exclusive writable
/// RAM, and its reservations exclude live code, stack, heap and the store.
#[allow(clippy::too_many_arguments)] // the loader's, plus the store and the path
pub unsafe fn load_stage(
    media: &(impl BootMedia + ?Sized),
    stage: CachedStage,
    descriptor: &BootstrapDescriptor,
    policy: &MemoryPolicy<'_>,
    store: &mut Store,
    resume: bool,
    codec: &dyn Codec,
    measurement: &Measurement<'_>,
) -> Result<VerifiedExecutable, Error> {
    if resume {
        let verified = store.find(stage.tag()).and_then(|entry| {
            stage_cache::load_from_slot(
                &entry_media(store, &entry),
                stage,
                descriptor,
                policy,
                codec,
            )
        });
        return match verified {
            Some(verified) => {
                fstart_log::info!("stage cache: {} loaded from store", stage.name());
                Ok(verified)
            }
            None => {
                fstart_log::error!("stage cache: no valid {} entry on resume", stage.name());
                Err(Error::NotCached)
            }
        };
    }

    // SAFETY: the caller's mapping contract.
    let verified =
        unsafe { fstart_load::load_bootstrap(media, descriptor, policy, codec, measurement) }
            .map_err(Error::Load)?;
    fstart_timestamp::add(fstart_timestamp::id::STORE_STAGE_CACHE);
    // Copy the stored bytes the loader just verified in RAM rather than
    // reading the flash a second time.
    let input = fstart_load::stored_input(descriptor).map_err(Error::Load)?;
    let stored =
        usize::try_from(input.size).map_err(|_| Error::Load(fstart_load::LoadError::Bounds))?;
    // SAFETY: the loader filled this part of the exclusive destination window
    // and nothing runs from it before entry.
    let input = unsafe { MemoryMapped::from_raw_addr(input.start, stored) };
    let in_ram = BootstrapDescriptor {
        offset: 0,
        ..*descriptor
    };
    let cached = store
        .add(stage.tag(), STAGE_CACHE_HEADER_LEN + stored, 3)
        .ok()
        .and_then(|entry| {
            // SAFETY: a fresh entry nothing else references.
            let slot = unsafe { store.bytes_mut(&entry) };
            stage_cache::store(slot, stage, &input, &in_ram).ok()
        });
    match cached {
        Some(()) => fstart_log::info!("stage cache: {} stored", stage.name()),
        None => fstart_log::error!(
            "stage cache: cannot store {} bytes of {}; S3 withheld",
            stored as u32,
            stage.name()
        ),
    }
    Ok(verified)
}

/// Whether the store holds a complete cold-boot copy of every one of
/// `stages`: what gates S3 advertisement. A resume still authenticates the
/// whole cached body.
pub fn cached(store: &Store, stages: &[CachedStage]) -> bool {
    stages.iter().all(|&stage| {
        store
            .find(stage.tag())
            .is_some_and(|entry| stage_cache::has_complete_slot(&entry_media(store, &entry), stage))
    })
}
