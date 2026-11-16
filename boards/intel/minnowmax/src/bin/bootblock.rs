//! MinnowBoard MAX bootblock: from cache-as-RAM to postcar, with native
//! RAM init.

#![no_std]
#![no_main]

use fstart_board_intel_minnowmax::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let mut bb = Bootblock::<Board>::enter().console(&CONSOLE, mainboard::console());
    mainboard::select_spd(bb.northbridge_mut());
    bb.train_memory().post_dram().load_postcar()
}
