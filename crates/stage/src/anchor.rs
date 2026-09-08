//! Constant verification policy and initial-only mutable location storage.
use core::cell::UnsafeCell;
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
/// A stage without a root-verification consumer can garbage-collect this block.
#[used]
#[cfg_attr(target_os = "none", unsafe(link_section = ".rodata.fstart_trust"))]
static FSTART_TRUST: Patchable<TrustBlock> = Patchable(UnsafeCell::new(TrustBlock::placeholder()));

pub fn fstart_trust_bytes() -> &'static [u8] {
    // SAFETY: fixed-size initialized storage, patched before execution.
    unsafe { core::slice::from_raw_parts(FSTART_TRUST.0.get().cast(), TRUST_SIZE) }
}

#[cfg(not(any(fstart_stage_env = "ram", fstart_stage_env = "postcar")))]
#[used]
#[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.anchor"))]
static FSTART_ANCHOR: Patchable<LocatorBlock> =
    Patchable(UnsafeCell::new(LocatorBlock::placeholder()));

#[cfg(any(fstart_stage_env = "ram", fstart_stage_env = "postcar"))]
static INHERITED: Patchable<[u8; LOCATOR_SIZE]> = Patchable(UnsafeCell::new([0; LOCATOR_SIZE]));
#[cfg(any(fstart_stage_env = "ram", fstart_stage_env = "postcar"))]
static PUBLISHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Install location-only metadata received through the protected predecessor
/// chain. It cannot supply keys/family/version-floor or enlarge platform bounds.
///
/// # Safety
/// Call once during single-threaded stage entry before media services/APs start.
/// `capacity` must be the independently established platform/device limit, not
/// a value taken from the carried locator. The caller establishes provenance.
pub unsafe fn install_locator(locator: LocatorBlock, capacity: u64) -> Result<(), ()> {
    #[cfg(any(fstart_stage_env = "ram", fstart_stage_env = "postcar"))]
    {
        if PUBLISHED.load(core::sync::atomic::Ordering::Acquire)
            || !locator.media().validate(capacity)
        {
            return Err(());
        }
        let mut bytes = [0; LOCATOR_SIZE];
        locator.write_to(&mut bytes);
        // SAFETY: exclusive single-threaded publication required by the caller.
        unsafe { INHERITED.0.get().write(bytes) };
        PUBLISHED.store(true, core::sync::atomic::Ordering::Release);
        Ok(())
    }
    #[cfg(not(any(fstart_stage_env = "ram", fstart_stage_env = "postcar")))]
    {
        let _ = (locator, capacity);
        Err(())
    }
}

/// Location data only; reading it does not authenticate any root or file.
pub fn media_locator() -> Option<fstart_core::ffs::locator::MediaLocator> {
    // SAFETY: initial or published predecessor storage is readable and stable.
    unsafe { fstart_core::ffs::locator::LocatorRef::read_volatile(fstart_anchor_bytes()) }
        .map(|locator| locator.media())
}

pub fn image_size() -> usize {
    media_locator()
        .and_then(|locator| usize::try_from(locator.image_size).ok())
        .unwrap_or(0)
}

pub fn fstart_anchor_bytes() -> &'static [u8] {
    #[cfg(not(any(fstart_stage_env = "ram", fstart_stage_env = "postcar")))]
    // SAFETY: initial stage's fixed-size patch site remains readable forever.
    unsafe {
        core::slice::from_raw_parts(FSTART_ANCHOR.0.get().cast(), LOCATOR_SIZE)
    }
    #[cfg(any(fstart_stage_env = "ram", fstart_stage_env = "postcar"))]
    {
        if !PUBLISHED.load(core::sync::atomic::Ordering::Acquire) {
            return &[];
        }
        // SAFETY: release/acquire publication; never subsequently mutated.
        unsafe { &*INHERITED.0.get() }
    }
}
