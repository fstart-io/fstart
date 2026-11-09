//! HiFive Unmatched LIM bootblock: clocks, console and DDR, then the
//! authenticated DRAM mainstage.

#![no_std]
#![no_main]

use fstart_board_sifive_unmatched::{self as board, HIFIVE_UNMATCHED, HIFIVE_UNMATCHED_UART};
use fstart_driver_uart::sifive::SifiveUart;
use fstart_log::StaticConsole;
use fstart_platform_sifive::Fu740;

static CONSOLE: StaticConsole<SifiveUart> = StaticConsole::new();

fstart_platform_sifive::entry!(main);

fn main() -> ! {
    let fu740 = Fu740::enter(&HIFIVE_UNMATCHED);
    board::reset_ethernet_phy();
    fu740
        .enable_ethernet()
        .console(&CONSOLE, &HIFIVE_UNMATCHED_UART)
        .train_memory()
        .boot_next()
}
