//! Bump allocator for firmware stages.
//!
//! Provides a simple bump-only allocator backed by a static 64 KiB buffer.
//! Used during the FDT preparation phase (dtoolkit write API needs `alloc`).
//! Deallocation is a no-op — memory is never reclaimed.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Heap size in bytes (256 KiB — enough for FDT manipulation).
///
/// QEMU AArch64 virt with `secure=on,virtualization=on` generates a
/// DTB of ~40 KiB, and `DeviceTree::from_fdt()` + `to_dtb()` allocate
/// roughly 3× the DTB size in intermediate structures. 256 KiB provides
/// comfortable headroom.
const HEAP_SIZE: usize = 256 * 1024;

/// 16-byte-aligned heap backing store.
#[repr(align(16))]
struct HeapStorage(UnsafeCell<[u8; HEAP_SIZE]>);

// SAFETY: Access is synchronised via the NEXT atomic counter.
// In firmware we are single-threaded at this point anyway.
unsafe impl Sync for HeapStorage {}

static HEAP: HeapStorage = HeapStorage(UnsafeCell::new([0; HEAP_SIZE]));

/// Monotonically advancing allocation cursor.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A trivial bump allocator. Allocations advance a cursor; frees are no-ops.
struct BumpAllocator;

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        loop {
            let current = NEXT.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }

            // CAS loop handles the (unlikely) multi-hart race.
            if NEXT
                .compare_exchange_weak(current, new_next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                // SAFETY: `aligned` is within bounds (checked above) and
                // the region [aligned..new_next) is exclusively ours.
                return unsafe { (*HEAP.0.get()).as_mut_ptr().add(aligned) };
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees.
    }
}
