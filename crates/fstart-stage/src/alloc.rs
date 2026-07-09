//! Bump allocator for firmware stages.
//!
//! Provides a simple bump-only allocator whose backing store is supplied by
//! the stage link. The generated linker script reserves `_FSTART_HEAP` (the
//! storage, sized by the stage build config's `heap_size`) and emits
//! `_FSTART_HEAP_SIZE` (a pointer-sized byte count); special stages such as
//! the SMM stage may instead define both as `#[no_mangle]` statics.
//! This crate references them via `extern "C"` at link time.
//! Deallocation is a no-op — memory is never reclaimed.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

// The definitions come from the generated linker script (or `#[no_mangle]`
// statics in special stages). We declare `_FSTART_HEAP` as `u8` because only
// its *address* is used — the type mismatch is intentional and harmless (same
// pattern as C linker symbols declared as `extern char`).
#[cfg(not(test))]
extern "C" {
    /// Heap backing store (stage-provided, 16-byte aligned).
    static _FSTART_HEAP: u8;
    /// Heap size in bytes (stage-provided).
    static _FSTART_HEAP_SIZE: usize;
}

#[cfg(test)]
#[repr(align(4096))]
struct TestHeap(core::cell::UnsafeCell<[u8; 16 * 1024 * 1024]>);

#[cfg(test)]
unsafe impl Sync for TestHeap {}

#[cfg(test)]
static TEST_HEAP: TestHeap = TestHeap(core::cell::UnsafeCell::new([0; 16 * 1024 * 1024]));

/// Get the heap base address from the stage-provided symbol.
#[inline]
pub fn heap_start() -> usize {
    #[cfg(test)]
    {
        TEST_HEAP.0.get() as *mut u8 as usize
    }
    #[cfg(not(test))]
    {
        // SAFETY: `_FSTART_HEAP` is defined by the stage link; we only use its
        // address.
        unsafe { &_FSTART_HEAP as *const u8 as usize }
    }
}

/// Get the heap size from the stage-provided symbol.
#[inline]
pub fn heap_size() -> usize {
    #[cfg(test)]
    {
        16 * 1024 * 1024
    }
    #[cfg(not(test))]
    {
        // SAFETY: `_FSTART_HEAP_SIZE` is pointer-sized data defined by the
        // stage link.
        unsafe { _FSTART_HEAP_SIZE }
    }
}

/// Monotonically advancing allocation cursor (offset from heap start).
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A trivial bump allocator. Allocations advance a cursor; frees are no-ops.
struct BumpAllocator;

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();
        let limit = heap_size();

        loop {
            let current = NEXT.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > limit {
                return core::ptr::null_mut();
            }

            // CAS loop handles the (unlikely) multi-hart race.
            if NEXT
                .compare_exchange_weak(current, new_next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                // SAFETY: `aligned` is within the linker-reserved heap
                // (checked against `limit` above) and the region
                // [aligned..new_next) is exclusively ours. Deriving the
                // pointer from the heap base keeps its provenance.
                return unsafe { (heap_start() as *mut u8).add(aligned) };
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees.
        // SAFETY: `GlobalAlloc::dealloc` requires `ptr` to be non-null and allocated by this allocator.
        let _ = unsafe { core::ptr::NonNull::new_unchecked(ptr) };
    }
}
