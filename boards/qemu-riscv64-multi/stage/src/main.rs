//! Board-owned static stage binary for QEMU RISC-V multi-stage demo.

#![no_std]
#![no_main]

extern crate fstart_platform_riscv64 as fstart_platform;
extern crate ufmt;

mod bootblock;
mod main_stage;

#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    match option_env!("FSTART_STAGE_NAME") {
        Some("bootblock") => fstart_stage::run_static_board::<bootblock::BootblockBoard>(),
        Some("main") => fstart_stage::run_static_board::<main_stage::MainBoard>(),
        _ => fstart_platform::halt(),
    }
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
