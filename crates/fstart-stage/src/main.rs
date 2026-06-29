//! fstart-stage: the single firmware stage binary crate.
//!
//! This crate links a fixed handwritten stage executor. Board/device
//! participation is selected by Cargo features and Rust board metadata at build
//! time; no Rust stage source is generated.

#![no_std]
#![no_main]

// When a feature requiring heap allocation is active, pull in fstart-alloc to
// register the global allocator. Without this explicit extern crate, the linker
// would not include it.
#[cfg(any(
    feature = "acpi",
    feature = "pci-ecam",
    feature = "q35-hostbridge",
    feature = "crabefi"
))]
extern crate fstart_alloc;

#[cfg(feature = "aarch64")]
extern crate fstart_platform_aarch64 as fstart_platform;
#[cfg(feature = "armv7")]
extern crate fstart_platform_armv7 as fstart_platform;
#[cfg(feature = "riscv64")]
extern crate fstart_platform_riscv64 as fstart_platform;
#[cfg(feature = "x86_64")]
extern crate fstart_platform_x86_64 as fstart_platform;

extern crate fstart_runtime;

mod board_adapters;

use board_adapters::StageBoard;

/// Stage entry point. Called by the platform's `_start` after register setup,
/// BSS clearing, and stack pointer initialization.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    fstart_stage_runtime::run_fixed_flow::<StageBoard>()
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
