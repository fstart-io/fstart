//! MJ11-EC1 BMC bootblock, executing SPI flash in place from the reset
//! vector. There is no board work: the SoC's flow is the whole `main`.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11_ec1_bmc::{self as board, STRAP_WRITES};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_aspeed::spi_head!(STRAP_WRITES);
fstart_platform_aspeed::entry!(main);

fn main() -> ! {
    fstart_platform_aspeed::ast2500::bootblock(&CONSOLE, board::console())
}
