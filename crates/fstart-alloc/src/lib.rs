//! Bump allocator for firmware stages.
//!
//! Provides a simple bump-only allocator whose backing store is supplied by
//! the board-owned stage binary/linker layout. The stage binary emits
//! `_FSTART_HEAP` (the storage) and `_FSTART_HEAP_SIZE` (the byte count) as
//! `#[no_mangle]` statics;
//! this crate references them via `extern "C"` at link time.
//! Deallocation is a no-op — memory is never reclaimed.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

// The actual definitions are `_FstartHeapStore` (a repr(align(16)) struct) and
// `usize` respectively, emitted by the selected board stage binary. We
// declare `_FSTART_HEAP` as `u8` because only its *address* is
// used — the type mismatch is intentional and harmless (same pattern as C
// linker symbols declared as `extern char`).
extern "C" {
    /// Heap backing store (stage-provided, 16-byte aligned).
    static _FSTART_HEAP: u8;
    /// Heap size in bytes (stage-provided).
    static _FSTART_HEAP_SIZE: usize;
}

/// Get the heap base address from the stage-provided symbol.
#[inline]
pub fn heap_start() -> usize {
    // SAFETY: `_FSTART_HEAP` is a `#[no_mangle]` static defined in the
    // stage binary; we only use its address.
    unsafe { &_FSTART_HEAP as *const u8 as usize }
}

/// Get the heap size from the stage-provided symbol.
#[inline]
pub fn heap_size() -> usize {
    // SAFETY: `_FSTART_HEAP_SIZE` is a `#[no_mangle]` static defined in
    // the stage binary.
    unsafe { _FSTART_HEAP_SIZE }
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
                // SAFETY: `aligned` is within bounds (checked above) and
                // the region [aligned..new_next) is exclusively ours.
                return (heap_start() + aligned) as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees.
    }
}
