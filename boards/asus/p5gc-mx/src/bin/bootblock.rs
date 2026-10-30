//! P5GC-MX bootblock: initialize the board and train DRAM.

#![no_std]
#![no_main]

use fstart_board_asus_p5gc_mx::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::BootPath;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    if mainboard::setup_early_superio().is_err() {
        bb.fail("early SuperIO setup");
    }
    let bb = bb.console(&CONSOLE, mainboard::console());
    // Preserve live strap outputs and do not read the BSEL MSR on S3.
    if bb.boot_path() != BootPath::S3Resume && mainboard::configure_cpu_straps().is_err() {
        bb.fail("CPU strap setup");
    }
    let mut bb = bb.train_memory();
    bb.southbridge_mut().use_pcie_root_port_clock_gating_only();
    bb.post_dram().load_postcar()
}
