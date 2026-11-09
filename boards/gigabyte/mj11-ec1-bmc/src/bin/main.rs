//! MJ11-EC1 BMC main stage, loaded into DRAM by the bootblock: it boots the
//! build's payload. The SoC's flow is the whole `main`.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11_ec1_bmc as board;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_aspeed::entry!(main, handoff);

fn main(handoff: usize) -> ! {
    fstart_platform_aspeed::ast2500::main_stage(&CONSOLE, board::console(), handoff)
}
