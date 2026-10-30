//! T60 bootblock: from cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_t60::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    let mut legacy_pin = bb
        .southbridge()
        .gpio()
        .pin(fstart_driver_lenovo::t6::dock::LEGACY_MODULE_GPIO);
    let _ = mainboard::setup_console(&mut legacy_pin);
    let bb = bb.console(&CONSOLE, mainboard::console());
    if bb.boot_path() == fstart_platform_intel::BootPath::S3Resume {
        bb.fail("S3 resume");
    }
    bb.train_memory().post_dram().load_postcar()
}
