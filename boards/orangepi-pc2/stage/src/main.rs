//! Board-owned static stage binary for Xunlong Orange Pi PC2.

#![no_std]
#![no_main]

#[cfg(fstart_stage_bootblock)]
use core::arch::global_asm;

extern crate fstart_platform_aarch64 as fstart_platform;
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
_head_jump:
    // ARM32 branch from eGON offset 0 to _start at offset 0x60.
    .word 0xea000016
"#
);

#[cfg(fstart_stage_bootblock)]
#[used]
#[cfg_attr(target_os = "none", link_section = ".head.egon")]
static EGON_HEAD: fstart_soc_sunxi::EgonHead = fstart_soc_sunxi::EgonHead::new();

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
