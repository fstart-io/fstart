//! Gigabyte MJ11-EC1 AST2500 BMC board support package.

#![no_std]

use fstart_core::QemuMachine;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_aspeed::facts::{AspeedBoardFacts, AspeedSoc, BoardFacts};
use fstart_platform_aspeed::head::StrapWrite;

/// MJ11-EC1 BMC marker.
pub struct Board;

impl AspeedBoardFacts for Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: AspeedSoc::Ast2500,
        // Micron MT25QL512ABB on CE0.
        flash_size: 64 * 1024 * 1024,
        qemu_machine: Some(QemuMachine::Ast2500Evb),
    };
}

/// Strap block: unlock the SCU, then strap a 32 MiB VGA memory carve-out
/// (SCU70 bits 3:2 = 0b10).
pub const STRAP_WRITES: [StrapWrite; 3] = [
    StrapWrite::new(0x1E6E_2000, 0x1688_A8A8),
    StrapWrite::new(0x1E6E_2070, 0x0000_0008),
    StrapWrite::new(0x1E6E_207C, 0x0000_0004),
];

/// UART5, the AST2500 debug console, at 115200 baud from its 24 MHz clock.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: 0x1E78_4000,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    }
}
