//! Board-owned static stage binary for QEMU ARMv7 virt.

#![no_std]
#![no_main]

extern crate fstart_platform_armv7 as fstart_platform;
extern crate ufmt;

mod stage;

/// Stage entry point. Called by the platform's `_start` after register setup,
/// BSS clearing, and stack pointer initialization.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    fstart_stage::run_static_board::<stage::StageBoard>()
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
