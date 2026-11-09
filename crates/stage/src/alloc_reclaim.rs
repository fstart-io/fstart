//! Lazy initialization over the existing linker-owned stage heap.
//!
//! Reuse linked_list_allocator for alignment, splitting, and coalescing. This
//! does not allocate more RAM or change the platform's configured heap size.
//! Like the rest of stage/CrabEFI state, this must not be entered from IRQ
//! context while interrupted code can hold the allocator lock.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};
use linked_list_allocator::LockedHeap;

pub(super) struct ReclaimingAllocator {
    heap: LockedHeap,
}

impl ReclaimingAllocator {
    pub(super) const fn new() -> Self {
        Self {
            heap: LockedHeap::empty(),
        }
    }
}

unsafe impl GlobalAlloc for ReclaimingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.heap.lock();
        if heap.bottom().is_null() {
            let base = super::heap_start();
            let size = super::heap_size();
            // Heap::init needs up to three words for alignment and metadata.
            // Malformed or disabled linker arenas must fail allocation rather
            // than panic inside a GlobalAlloc implementation.
            if base == 0
                || size < 3 * core::mem::size_of::<usize>()
                || base.checked_add(size).is_none()
            {
                return ptr::null_mut();
            }
            // SAFETY: the stage link exclusively reserves this static backing
            // store. Its existing helpers provide its base and bounded size.
            // The lock serializes the one-time initialization and all accesses.
            unsafe { heap.init(base as *mut u8, size) };
        }
        heap.allocate_first_fit(layout)
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: GlobalAlloc's caller supplies a live allocation from this
        // allocator with its original layout. Metadata stays under its lock.
        unsafe {
            self.heap
                .lock()
                .deallocate(NonNull::new_unchecked(pointer), layout)
        };
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn temporary_allocations_can_exceed_total_arena_size_repeatedly() {
        // Exercise the actual stage GlobalAlloc path, not only the dependency.
        // Cumulative traffic exceeds the backing arena by at least four times.
        let layout = Layout::from_size_align(1024, 256).unwrap();
        for _ in 0..(super::super::heap_size() / layout.size() * 4 + 1) {
            let pointer = unsafe { super::super::ALLOCATOR.alloc(layout) };
            assert!(!pointer.is_null());
            assert_eq!(pointer as usize % layout.align(), 0);
            unsafe {
                pointer.write_bytes(0xa5, layout.size());
                super::super::ALLOCATOR.dealloc(pointer, layout);
            }
        }
    }

    #[test]
    fn oversized_failure_does_not_poison_the_heap_and_overalignment_works() {
        let huge = Layout::from_size_align(super::super::heap_size() * 2, 4096).unwrap();
        assert!(unsafe { super::super::ALLOCATOR.alloc(huge) }.is_null());
        let small = Layout::from_size_align(71, 4096).unwrap();
        let pointer = unsafe { super::super::ALLOCATOR.alloc(small) };
        assert!(!pointer.is_null());
        assert_eq!(pointer as usize % 4096, 0);
        unsafe { super::super::ALLOCATOR.dealloc(pointer, small) };
    }

    #[test]
    fn fragmented_frees_coalesce_for_a_later_large_allocation() {
        use self::std::{boxed::Box, vec::Vec};
        use core::mem::MaybeUninit;
        use linked_list_allocator::Heap;
        // This isolated static arena avoids relying on host test-harness
        // allocations or concurrent tests when checking exact free space.
        let arena = Box::leak(Box::new([MaybeUninit::<u8>::uninit(); 32 * 1024]));
        let mut heap = Heap::empty();
        heap.init_from_slice(arena);
        let layout = Layout::from_size_align(1024, 256).unwrap();
        let mut pointers = Vec::new();
        while let Ok(pointer) = heap.allocate_first_fit(layout) {
            pointers.push(pointer);
        }
        assert!(pointers.len() > 20);
        for parity in [0, 1] {
            for pointer in pointers.iter().skip(parity).step_by(2) {
                unsafe { heap.deallocate(*pointer, layout) };
            }
        }
        assert_eq!(heap.used(), 0);
        let large = Layout::from_size_align(24 * 1024, 4096).unwrap();
        let pointer = heap.allocate_first_fit(large).unwrap();
        unsafe { heap.deallocate(pointer, large) };
        assert_eq!(heap.used(), 0);
    }
}
