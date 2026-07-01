//! Board-owned static stage binary for Foxconn D41S UEFI.

extern crate fstart_platform_x86_64 as fstart_platform;
extern crate ufmt;

mod bootblock;
mod common;
#[cfg(feature = "crabefi")]
mod main_stage;

#[cfg(feature = "stage-bootblock")]
const HEAP_SIZE: usize = fstart_board_foxconn_d41s::facts::BOOTBLOCK_HEAP_SIZE;
#[cfg(not(feature = "stage-bootblock"))]
const HEAP_SIZE: usize = fstart_board_foxconn_d41s::facts::RAMSTAGE_HEAP_SIZE;

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; HEAP_SIZE]);

#[no_mangle]
static mut _FSTART_HEAP: HeapStore = HeapStore([0; HEAP_SIZE]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = HEAP_SIZE;

#[no_mangle]
static _fstart_anchor_early: fstart_types::ffs::AnchorBlock =
    fstart_types::ffs::AnchorBlock::placeholder();

#[cfg(feature = "stage-bootblock")]
#[no_mangle]
static _fstart_early_microcode_enabled: u32 = 1;
#[cfg(not(feature = "stage-bootblock"))]
#[no_mangle]
static _fstart_early_microcode_enabled: u32 = 0;

pub extern "Rust" fn run_stage(_handoff_ptr: usize) -> ! {
    match option_env!("FSTART_STAGE_NAME") {
        Some("bootblock") => fstart_stage::run_static_board::<bootblock::BootblockBoard>(),
        #[cfg(feature = "crabefi")]
        Some("ramstage") => fstart_stage::run_static_board::<main_stage::MainBoard>(),
        _ => fstart_platform::halt(),
    }
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = run_stage;
