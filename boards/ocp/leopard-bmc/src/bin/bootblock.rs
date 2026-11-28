//! Leopard BMC bootblock, executing SPI flash in place from the reset
//! vector. The SoC's flow is the whole `main`; the board only picks the
//! flash clock and whether to test DRAM.

#![no_std]
#![no_main]

use fstart_board_ocp_leopard_bmc::{self as board, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_aspeed::facts::AspeedBoardFacts;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

/// SPI clock limit for the boot flash; the bootblock calibrates the read
/// delay and takes the fastest HCLK divisor at or below this that reads
/// correctly. 50 MHz until the flash part is known.
const FLASH_MAX_HZ: u32 = 50_000_000;

/// Test all usable DRAM on every boot. Off: turn it on for DRAM work.
const MEMTEST: bool = false;

fstart_platform_aspeed::spi_head!();
fstart_platform_aspeed::entry!(main);

fn main() -> ! {
    fstart_platform_aspeed::ast2400::bootblock(
        &CONSOLE,
        board::console(),
        Board::FACTS.soc,
        FLASH_MAX_HZ,
        MEMTEST,
        board::TIMESTAMPS,
    )
}
