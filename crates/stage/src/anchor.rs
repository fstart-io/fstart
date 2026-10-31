//! Constant verification policy and initial-only mutable location storage.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicPtr, Ordering};
use fstart_core::ffs::{
    locator::{LOCATOR_SIZE, LocatorBlock},
    trust::{TRUST_SIZE, TrustBlock},
};

#[repr(transparent)]
struct Patchable<T>(UnsafeCell<T>);
// SAFETY: packaging patches file bytes offline. Runtime writes only the
// separately published inherited slot, once, before services/APs can read it.
unsafe impl<T> Sync for Patchable<T> {}

/// Constant policy is finalized before the containing artifact is compressed.
/// Kept only by a root-verification consumer (not `#[used]`, which would
/// retain it in every stage).
#[cfg_attr(target_os = "none", unsafe(link_section = ".rodata.fstart_trust"))]
static FSTART_TRUST: Patchable<TrustBlock> = Patchable(UnsafeCell::new(TrustBlock::placeholder()));

pub fn fstart_trust_bytes() -> &'static [u8] {
    // SAFETY: fixed-size initialized storage, patched before execution.
    unsafe { core::slice::from_raw_parts(FSTART_TRUST.0.get().cast(), TRUST_SIZE) }
}

/// The locator patched into the image's initial stage. Kept only by
/// [`initial_stage`]: later stages do not carry it, so packaging finds
/// exactly one placeholder.
#[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.anchor"))]
static FSTART_ANCHOR: Patchable<LocatorBlock> =
    Patchable(UnsafeCell::new(LocatorBlock::placeholder()));

/// A later stage's locator, received from its predecessor.
static INHERITED: Patchable<[u8; LOCATOR_SIZE]> = Patchable(UnsafeCell::new([0; LOCATOR_SIZE]));

/// The locator this stage reads: its own patched anchor or the inherited
/// copy, published once; null until then.
static LOCATOR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());

/// Mark this stage as the image's initial stage: its own patched anchor
/// locates the image. The initial phase of each platform calls this
/// first; a later stage installs its predecessor's locator instead
/// ([`install_locator`]).
pub fn initial_stage() {
    LOCATOR.store(FSTART_ANCHOR.0.get().cast(), Ordering::Release);
}

/// Install location-only metadata received through the protected predecessor
/// chain. It cannot supply keys/family/version-floor or enlarge platform bounds.
/// Fails once a locator is published, including an initial stage's own.
///
/// # Safety
/// Call once during single-threaded stage entry before media services/APs start.
/// `capacity` must be the independently established platform/device limit, not
/// a value taken from the carried locator. The caller establishes provenance.
pub unsafe fn install_locator(locator: LocatorBlock, capacity: u64) -> Result<(), ()> {
    if !LOCATOR.load(Ordering::Acquire).is_null() || !locator.media().validate(capacity) {
        return Err(());
    }
    let mut bytes = [0; LOCATOR_SIZE];
    locator.write_to(&mut bytes);
    // SAFETY: exclusive single-threaded publication required by the caller.
    unsafe { INHERITED.0.get().write(bytes) };
    LOCATOR.store(INHERITED.0.get().cast(), Ordering::Release);
    Ok(())
}

/// The published locator. Location data only; reading it does not
/// authenticate any root or file.
pub fn locator() -> Option<LocatorBlock> {
    // SAFETY: initial or published predecessor storage is readable and stable.
    unsafe { fstart_core::ffs::locator::LocatorRef::read_volatile(fstart_anchor_bytes()) }
        .map(|locator| locator.value())
}

/// [`locator`] as media-relative values.
pub fn media_locator() -> Option<fstart_core::ffs::locator::MediaLocator> {
    locator().map(LocatorBlock::media)
}

pub fn image_size() -> usize {
    media_locator()
        .and_then(|locator| usize::try_from(locator.image_size).ok())
        .unwrap_or(0)
}

/// The published locator's bytes; empty before [`initial_stage`] or
/// [`install_locator`].
pub fn fstart_anchor_bytes() -> &'static [u8] {
    let locator = LOCATOR.load(Ordering::Acquire);
    if locator.is_null() {
        return &[];
    }
    // SAFETY: release/acquire publication of a 'static, LOCATOR_SIZE-byte
    // block (patched offline, or written once before publication).
    unsafe { core::slice::from_raw_parts(locator, LOCATOR_SIZE) }
}
