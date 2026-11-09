use super::*;
use core::alloc::{GlobalAlloc, Layout};

#[test]
#[cfg(feature = "reclaiming-alloc")]
fn freed_stage_allocation_is_reused() {
    let layout = Layout::from_size_align(1024, 256).unwrap();
    let first = unsafe { ALLOCATOR.alloc(layout) };
    assert!(!first.is_null());
    unsafe { ALLOCATOR.dealloc(first, layout) };
    let second = unsafe { ALLOCATOR.alloc(layout) };
    assert!(!second.is_null());
    // No intervening allocation: first-fit should reuse the returned space.
    // Unlike exhausting the global heap, this can fail without also denying
    // memory to the test harness reporting the failure.
    unsafe { ALLOCATOR.dealloc(second, layout) };
    assert_eq!(first, second, "stage deallocation did not return the block");
}

#[test]
#[cfg(not(feature = "reclaiming-alloc"))]
fn boot_only_profile_keeps_bump_allocation() {
    let layout = Layout::from_size_align(1024, 256).unwrap();
    let first = unsafe { ALLOCATOR.alloc(layout) };
    assert!(!first.is_null());
    unsafe { ALLOCATOR.dealloc(first, layout) };
    let second = unsafe { ALLOCATOR.alloc(layout) };
    assert!(!second.is_null());
    unsafe { ALLOCATOR.dealloc(second, layout) };
    assert_ne!(first, second);
}
