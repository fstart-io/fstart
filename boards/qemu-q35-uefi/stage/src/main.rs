//! Board-owned static stage binary for QEMU Q35 UEFI.

#![no_std]
#![no_main]

extern crate fstart_platform_x86_64 as fstart_platform;
extern crate ufmt;

mod bootblock;
#[cfg(feature = "crabefi")]
mod main_stage;

const HEAP_SIZE: usize = fstart_board_qemu_q35_uefi_facts::STAGE_HEAP_SIZE as usize;

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; HEAP_SIZE]);

#[no_mangle]
static _FSTART_HEAP: HeapStore = HeapStore([0; HEAP_SIZE]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = HEAP_SIZE;

#[no_mangle]
static _fstart_anchor_early: fstart_types::ffs::AnchorBlock =
    fstart_types::ffs::AnchorBlock::placeholder();

#[no_mangle]
static _fstart_early_microcode_enabled: u32 = 0;

#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    match option_env!("FSTART_STAGE_NAME") {
        Some("bootblock") => fstart_stage::run_static_board::<bootblock::BootblockBoard>(),
        #[cfg(feature = "crabefi")]
        Some("main") => fstart_stage::run_static_board::<main_stage::MainBoard>(),
        _ => fstart_platform::halt(),
    }
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
