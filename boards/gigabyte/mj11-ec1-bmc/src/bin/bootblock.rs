//! MJ11-EC1 BMC bootblock, executing SPI flash in place from the reset
//! vector. The SoC's flow is the whole `main`; the board only picks the
//! flash clock, the MAC timing, the DRAM setup and whether to test it.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11_ec1_bmc::{self as board, STRAP_WRITES};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_aspeed::ast2500::{Ddr4Odt, DramConfig, Mac, MacConfig, Organization, Speed};

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

/// SPI clock limit for the boot flash. The MT25QL512 reads at up to 133
/// MHz; the bootblock calibrates the read delay and takes the fastest
/// HCLK divisor at or below this that reads correctly. On this board no
/// delay reads correctly at HCLK/2 (99 MHz), HCLK/3 (66 MHz) does.
const FLASH_MAX_HZ: u32 = 66_000_000;

/// MAC1 is strapped to RMII for NC-SI to the host's network controller,
/// which drives the 50 MHz reference clock; MAC2 to RGMII for the
/// dedicated management PHY. Both use transmit clock delay 8 and receive
/// clock delay 2; the RGMII transmit clock duty is calibrated and
/// inverted, at stage 4 for MAC1 and 6 for MAC2. Both MDIO ports are
/// routed to their pins.
const MACS: MacConfig = MacConfig {
    mac1: Mac {
        tx_delay: 8,
        rx_delay: 2,
        tx_duty: 0x64,
        rmii_clock_out: false,
        mdio: true,
    },
    mac2: Mac {
        tx_delay: 8,
        rx_delay: 2,
        tx_duty: 0x66,
        rmii_clock_out: false,
        mdio: true,
    },
};

/// Test all usable DRAM after every training (cold boots only). Off: the
/// test takes about a minute over 424 MiB, and training passed it on this
/// board. Turn it on for DRAM work.
const MEMTEST: bool = false;

fstart_platform_aspeed::spi_head!(STRAP_WRITES);
fstart_platform_aspeed::entry!(main);

fn main() -> ! {
    fstart_platform_aspeed::ast2500::bootblock(
        &CONSOLE,
        board::console(),
        FLASH_MAX_HZ,
        &MACS,
        &DRAM,
        MEMTEST,
        board::TIMESTAMPS,
    )
}
