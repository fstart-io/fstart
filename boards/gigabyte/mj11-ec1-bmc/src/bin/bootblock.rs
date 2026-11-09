//! MJ11-EC1 BMC bootblock, executing SPI flash in place from the reset
//! vector. The SoC's flow is the whole `main`; the board only picks the
//! DRAM setup.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11_ec1_bmc::{self as board, STRAP_WRITES};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_aspeed::ast2500::{Ddr4Odt, DramConfig, Organization, Speed};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

/// 512 MiB of DDR4 on one x16 part, run at 1440 MT/s with 48 ohm ODT for
/// margin, and ECC over 424 MiB: its code storage fits below the 32 MiB
/// VGA carve-out.
const DRAM: DramConfig = DramConfig {
    speed: Speed::Mt1440,
    organization: Organization::X16,
    ddr4_odt: Ddr4Odt::Ohm48,
    extended_temperature: false,
    ck_drive: None,
    ecc_bytes: Some(424 << 20),
};

fstart_platform_aspeed::spi_head!(STRAP_WRITES);
fstart_platform_aspeed::entry!(main);

fn main() -> ! {
    fstart_platform_aspeed::ast2500::bootblock(&CONSOLE, board::console(), &DRAM)
}
