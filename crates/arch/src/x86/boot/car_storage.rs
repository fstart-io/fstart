//! One-shot storage for the BSP's CAR-stage runtime owners.
//!
//! Declare slots as stage-local statics in the bootblock, not in immutable board
//! config. The linker places their writable storage in CAR via the existing stage
//! writable-region policy. This module cannot establish physical placement, CAR
//! capacity, cache behavior, or the lifetime of CAR: those require a linked image
//! audit and the unsafe acquisition contract below. No slot is an E820 service.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

/// Reserved, initially uninitialized storage with a permanent one-shot claim.
/// Even dropping the lease does not make the slot available again.
///
/// Construction is const: a static slot reserves space without a runtime `T`
/// temporary or an assumption that zero bytes form a valid `T`.
pub struct CarSlot<T> {
    taken: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: the atomic claim admits exactly one initializer/lease, and no method
// otherwise exposes value. Moving ownership from a static slot to the acquiring
// BSP requires T: Send; non-Send resources cannot be shared through a static slot.
unsafe impl<T: Send> Sync for CarSlot<T> {}

impl<T> CarSlot<T> {
    pub const fn new() -> Self {
        Self {
            taken: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Claim once and initialize directly in the reserved destination.
    /// The function pointer initializes STORAGE only, never a training phase or
    /// a training-success report. A failed/reentrant claim never calls it.
    ///
    /// # Safety
    /// The BSP must have live, exclusively reserved, correctly linked CAR backing
    /// this entire slot (including alignment and capacity); BSS must be initialized.
    /// `initialize` must write one valid `T` in place, without unwinding, reading
    /// uninitialized bytes, publishing aliases, or retaining the destination.
    /// All leases, reborrows AND later uses must end before CAR teardown/INVD or
    /// stage exit. Never transport a lease or CAR pointer into ramstage. The small
    /// driver owning a lease must be dropped/consumed before the stage transition;
    /// merely forgetting it is insufficient if any pointer/reference remains live.
    /// This physical lifetime obligation is not enforced by Rust's `'static`.
    pub unsafe fn take_initialized(
        &'static self,
        initialize: unsafe fn(*mut T),
    ) -> Option<CarLease<T>> {
        if self
            .taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let destination = self.value.get().cast::<T>();
        // SAFETY: claim is permanent and exclusive; caller supplies a valid
        // in-place initializer and establishes the physical stage lifetime.
        unsafe { initialize(destination) };
        Some(CarLease {
            // SAFETY: initializer completed with one valid exclusively owned T.
            value: unsafe { &mut *destination },
            _bsp: PhantomData,
        })
    }
}

impl<T> Default for CarSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A small movable owner, not a large inline workspace. Intentionally !Send and
/// !Sync: it stays on the acquiring BSP. Own this in the driver; only reborrow its
/// data. There is no static-reference extraction, reset, clone or release API.
/// Dropping it abandons the stage's value in place (no destructor is run).
pub struct CarLease<T: 'static> {
    value: &'static mut T,
    _bsp: PhantomData<*mut ()>,
}

impl<T> Deref for CarLease<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> DerefMut for CarLease<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}
