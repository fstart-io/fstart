//! Generic selected-board stage entry point.

#![no_std]
#![no_main]

use fstart_board_selected::Board;

#[no_mangle]
pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
    fstart_stage::run_board::<Board>(
        fstart_stage::StageKind::from_option(option_env!("FSTART_STAGE_NAME")),
        handoff_ptr,
    )
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
