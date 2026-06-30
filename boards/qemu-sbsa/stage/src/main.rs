//! Board-owned static stage binary for QEMU SBSA-ref.

#![no_std]
#![no_main]

extern crate fstart_platform_aarch64 as fstart_platform;
extern crate ufmt;

mod stage;

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; 0x40000]);

#[no_mangle]
static _FSTART_HEAP: HeapStore = HeapStore([0; 0x40000]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = 0x40000;

/// Stage entry point. Called by the platform's `_start` after register setup,
/// BSS clearing, stack pointer initialization, and DTB pointer capture.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    fstart_stage::run_static_board::<stage::StageBoard>()
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
