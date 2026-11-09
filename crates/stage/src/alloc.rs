//! The stage's global allocator: a linked-list heap over the linker-owned
//! arena, so a dropped buffer's memory is reused.
//!
//! [`init`] hands the arena over at stage entry. That reference to
//! `_FSTART_HEAP` is reachable from the entry point, so a stage without a heap
//! that selects the allocator fails to link even when nothing allocates.
//!
//! The spin lock must not be taken by an interrupt or exception handler that
//! may have interrupted an allocation; panic and logging paths do not
//! allocate. An x86 stage with a heap must enter with caching enabled: the
//! lock's locked RMW hangs with CR0.CD=1 on i945.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

use linked_list_allocator::Heap;
use spin::mutex::SpinMutex;

/// A linked-list heap behind a spin lock whose atomics come from
/// portable-atomic: ARMv5TE and riscv32imc have no compare-and-swap, which
/// rules out linked_list_allocator's own `LockedHeap`.
struct StageHeap(SpinMutex<Heap>);

impl StageHeap {
    const fn empty() -> Self {
        Self(SpinMutex::new(Heap::empty()))
    }
}

// SAFETY: the lock serialises the heap, which only hands out memory of the
// arena it was given.
unsafe impl GlobalAlloc for StageHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .allocate_first_fit(layout)
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller frees a live allocation of this heap, made with
        // `layout`; allocations are never null.
        unsafe {
            self.0
                .lock()
                .deallocate(NonNull::new_unchecked(pointer), layout)
        }
    }
}

#[cfg(target_os = "none")]
#[global_allocator]
static ALLOCATOR: StageHeap = StageHeap::empty();

/// Give the stage heap to the allocator, before anything allocates.
#[cfg(target_os = "none")]
pub(crate) fn init() {
    // Defined by the stage's linker script.
    unsafe extern "C" {
        static mut _FSTART_HEAP: u8;
        static _FSTART_HEAP_SIZE: usize;
    }
    // SAFETY: the link reserves the arena for the allocator alone, and the
    // stage entry calls this once, before any allocation.
    unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(&raw mut _FSTART_HEAP, _FSTART_HEAP_SIZE)
    };
}

#[cfg(test)]
mod tests {
    use super::StageHeap;
    use core::alloc::{GlobalAlloc, Layout};

    #[test]
    fn freed_memory_is_reused() {
        #[repr(align(4096))]
        struct Arena([u8; 32 * 1024]);
        let mut arena = Arena([0; 32 * 1024]);
        let heap = StageHeap::empty();
        // SAFETY: the arena outlives the heap and nothing else uses it.
        unsafe { heap.0.lock().init(arena.0.as_mut_ptr(), arena.0.len()) };

        // Many times the arena, in buffers that are freed again.
        let layout = Layout::from_size_align(1024, 256).unwrap();
        for _ in 0..4 * arena.0.len() / layout.size() {
            // SAFETY: a non-zero layout; the buffer is freed with it.
            let buffer = unsafe { heap.alloc(layout) };
            assert!(!buffer.is_null());
            assert!((buffer as usize).is_multiple_of(layout.align()));
            unsafe { heap.dealloc(buffer, layout) };
        }
        assert_eq!(heap.0.lock().used(), 0);
        // Exhaustion is a null pointer, not a panic.
        let huge = Layout::from_size_align(2 * arena.0.len(), 8).unwrap();
        assert!(unsafe { heap.alloc(huge) }.is_null());
    }
}
