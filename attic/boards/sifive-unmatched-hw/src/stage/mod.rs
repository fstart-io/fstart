//! Board-owned static stage binary for SiFive Unmatched hardware.

extern crate fstart_platform_riscv64 as fstart_platform;
extern crate ufmt;

mod stage;

/// Stage entry point. Called by the platform `_start` after register setup,
/// BSS clearing, stack pointer initialization, and DTB pointer capture.
pub extern "Rust" fn run_stage(_handoff_ptr: usize) -> ! {
    fstart_stage::run_static_board::<stage::StageBoard>()
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = run_stage;
