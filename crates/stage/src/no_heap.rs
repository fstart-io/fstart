//! The global allocator of a stage without a heap.
//!
//! A driver crate links its allocating code (ACPI builders, say) next to its
//! early init, so a heapless stage links the `alloc` crate, and rustc requires
//! a global allocator for it. This one reserves no memory: each of its methods
//! calls a symbol nothing defines. Section garbage collection drops those calls
//! along with every allocation the stage cannot reach, so the stage links only
//! when nothing it runs can allocate. The undefined symbol names the mistake.

use core::alloc::{GlobalAlloc, Layout};

unsafe extern "Rust" {
    /// Deliberately undefined: see the module documentation.
    fn fstart_stage_without_alloc_feature_allocates() -> !;
}

struct NoHeap;

// SAFETY: neither method returns; a stage that calls them does not link.
unsafe impl GlobalAlloc for NoHeap {
    unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
        // SAFETY: never linked; see the module documentation.
        unsafe { fstart_stage_without_alloc_feature_allocates() }
    }

    unsafe fn dealloc(&self, _pointer: *mut u8, _layout: Layout) {
        // SAFETY: never linked; see the module documentation.
        unsafe { fstart_stage_without_alloc_feature_allocates() }
    }
}

#[global_allocator]
static NO_HEAP: NoHeap = NoHeap;
