//! Board-owned static stage binary for LeMaker Banana Pi M1.

#![no_std]
#![no_main]

#[cfg(fstart_stage_bootblock)]
use core::arch::global_asm;

extern crate fstart_platform_armv7 as fstart_platform;
extern crate ufmt;

mod bootblock;
mod common;
#[cfg(feature = "ffs")]
mod main_stage;

#[cfg(fstart_stage_bootblock)]
global_asm!(
    r#"
    .section .head.text, "ax"
    .global _head_jump
    .arm
_head_jump:
    b _start
"#
);

#[cfg(fstart_stage_bootblock)]
#[used]
#[cfg_attr(target_os = "none", link_section = ".head.egon")]
static EGON_HEAD: fstart_soc_sunxi::EgonHead = fstart_soc_sunxi::EgonHead::new();

/// Stage entry point. Called by the ARMv7/sunxi platform `_start` after eGON
/// entry setup, BSS clearing, and stack initialization.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    match option_env!("FSTART_STAGE_NAME") {
        Some("bootblock") => fstart_stage::run_static_board::<bootblock::BootblockBoard>(),
        #[cfg(feature = "ffs")]
        Some("main") => {
            main_stage::set_handoff_ptr(_handoff_ptr);
            fstart_stage::run_static_board::<main_stage::MainBoard>()
        }
        _ => fstart_platform::halt(),
    }
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
