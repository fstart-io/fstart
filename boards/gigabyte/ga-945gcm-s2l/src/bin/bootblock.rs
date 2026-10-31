//! GA-945GCM bootblock: initialize memory and load postcar.

#![no_std]
#![no_main]

use fstart_board_gigabyte_ga_945gcm_s2l::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::{BootPath, bootblock::Bootblock};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    if mainboard::setup_superio().is_err() {
        bb.fail("SuperIO setup");
    }
    let bb = bb.console(&CONSOLE, mainboard::console());
    if bb.boot_path() != BootPath::S3Resume && mainboard::setup_cooling().is_err() {
        bb.fail("SuperIO cooling");
    }
    let mut bb = bb.train_memory();
    bb.southbridge_mut().use_pcie_root_port_clock_gating_only();
    bb.post_dram().load_postcar()
}
