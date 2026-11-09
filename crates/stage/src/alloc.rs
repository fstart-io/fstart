//! Reclaiming allocation over the linker-owned stage heap.
//!
//! Only heap-using stages enable `alloc`. Heapless links deliberately provide
//! neither heap symbol; KEEP-ing the reference below makes accidentally enabling
//! this module a link error even when no allocation survives LTO.
//!
//! The spin lock must not be re-entered from an interrupt or exception handler.
//! Panic/logging paths must remain allocation-free. On x86, caching must already
//! be enabled before allocation: locked RMW hangs with CR0.CD=1 on i945.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};
use linked_list_allocator::LockedHeap;

#[cfg(not(test))]
unsafe extern "C" {
    static mut _FSTART_HEAP: u8;
    static _FSTART_HEAP_SIZE: usize;
}

// A retained relocation, not merely a reference inside a dead allocator method.
#[cfg(all(not(test), target_os = "none"))]
#[used]
#[unsafe(link_section = ".fstart.keep")]
static HEAP_REQUIRED: core::sync::atomic::AtomicPtr<u8> =
    core::sync::atomic::AtomicPtr::new(core::ptr::addr_of_mut!(_FSTART_HEAP));

#[cfg(test)]
#[repr(align(4096))]
struct TestHeap(core::cell::UnsafeCell<[u8; 16 * 1024 * 1024]>);
#[cfg(test)]
unsafe impl Sync for TestHeap {}
#[cfg(test)]
static TEST_HEAP: TestHeap = TestHeap(core::cell::UnsafeCell::new([0; 16 * 1024 * 1024]));

fn heap_start() -> *mut u8 {
    #[cfg(test)]
    {
        TEST_HEAP.0.get().cast()
    }
    #[cfg(not(test))]
    {
        core::ptr::addr_of_mut!(_FSTART_HEAP)
    }
}

fn heap_size() -> usize {
    #[cfg(test)]
    {
        16 * 1024 * 1024
    }
    #[cfg(not(test))]
    {
        unsafe { _FSTART_HEAP_SIZE }
    }
}

struct StageAllocator {
    heap: LockedHeap,
}

impl StageAllocator {
    const fn new() -> Self {
        Self {
            heap: LockedHeap::empty(),
        }
    }
}

#[global_allocator]
static ALLOCATOR: StageAllocator = StageAllocator::new();

unsafe impl GlobalAlloc for StageAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.heap.lock();
        if heap.bottom().is_null() {
            let base = heap_start();
            let size = heap_size();
            // Heap::init needs two words of metadata plus alignment padding.
            // Never panic inside GlobalAlloc on a malformed linker arena.
            if base.is_null()
                || size < 3 * core::mem::size_of::<usize>()
                || (base as usize).checked_add(size).is_none()
            {
                return ptr::null_mut();
            }
            // SAFETY: the linker exclusively reserves this static arena. The
            // lock serializes initialization and every subsequent heap access.
            unsafe { heap.init(base, size) };
        }
        heap.allocate_first_fit(layout)
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: GlobalAlloc's caller supplies a live allocation with its
        // original layout. Metadata is exclusively accessed under the lock.
        unsafe {
            self.heap
                .lock()
                .deallocate(NonNull::new_unchecked(pointer), layout)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_allocator_reclaims_temporary_buffers() {
        let layout = Layout::from_size_align(1024, 256).unwrap();
        // Cumulative allocation exceeds the entire arena several times.
        for _ in 0..heap_size() / layout.size() * 4 {
            let pointer = unsafe { ALLOCATOR.alloc(layout) };
            assert!(!pointer.is_null());
            assert_eq!(pointer as usize % layout.align(), 0);
            unsafe {
                pointer.write_bytes(0xa5, layout.size());
                ALLOCATOR.dealloc(pointer, layout);
            }
        }
    }

    #[test]
    fn alignment_exhaustion_reuse_and_coalescing() {
        #[repr(align(4096))]
        struct Arena([u8; 32 * 1024]);
        let mut arena = Arena([0; 32 * 1024]);
        let allocator = StageAllocator::new();
        // Isolate exact free-space checks from the host test harness, which
        // itself allocates from ALLOCATOR and may run tests concurrently.
        unsafe {
            allocator
                .heap
                .lock()
                .init(arena.0.as_mut_ptr(), arena.0.len())
        };
        // A retained directory/asset allocation must survive temporary churn.
        let pin_layout = Layout::from_size_align(61, 8).unwrap();
        let pin = unsafe { allocator.alloc(pin_layout) };
        assert!(!pin.is_null());
        unsafe { pin.write_bytes(0x5a, pin_layout.size()) };
        let pinned_used = allocator.heap.lock().used();
        let huge = Layout::from_size_align(arena.0.len() * 2, 4096).unwrap();
        assert!(unsafe { allocator.alloc(huge) }.is_null());
        let aligned = Layout::from_size_align(71, 4096).unwrap();
        let pointer = unsafe { allocator.alloc(aligned) };
        assert!(!pointer.is_null());
        assert_eq!(pointer as usize % 4096, 0);
        unsafe { allocator.dealloc(pointer, aligned) };
        assert_eq!(unsafe { allocator.alloc(aligned) }, pointer);
        unsafe { allocator.dealloc(pointer, aligned) };

        let layout = Layout::from_size_align(1024, 256).unwrap();
        let mut pointers = heapless::Vec::<*mut u8, 32>::new();
        loop {
            let pointer = unsafe { allocator.alloc(layout) };
            if pointer.is_null() {
                break;
            }
            pointers.push(pointer).unwrap();
        }
        assert!(pointers.len() > 20);
        for parity in [0, 1] {
            for pointer in pointers.iter().skip(parity).step_by(2) {
                unsafe { allocator.dealloc(*pointer, layout) };
            }
        }
        assert_eq!(allocator.heap.lock().used(), pinned_used);
        let large = Layout::from_size_align(24 * 1024, 4096).unwrap();
        let pointer = unsafe { allocator.alloc(large) };
        assert!(!pointer.is_null());
        unsafe { allocator.dealloc(pointer, large) };
        assert_eq!(allocator.heap.lock().used(), pinned_used);
        assert!(
            unsafe { core::slice::from_raw_parts(pin, pin_layout.size()) }
                .iter()
                .all(|&byte| byte == 0x5a)
        );
        unsafe { allocator.dealloc(pin, pin_layout) };
        assert_eq!(allocator.heap.lock().used(), 0);
    }
}
