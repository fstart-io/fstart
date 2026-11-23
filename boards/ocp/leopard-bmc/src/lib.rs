//! OCP Leopard AST1250 BMC board support package.
//!
//! Brought up in QEMU's AST2400 `palmetto-bmc` machine first. The flash
//! size and console below are that machine's and the AST1250 defaults;
//! they are still to be confirmed on the board.

#![no_std]

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_aspeed::facts::{AspeedBoardFacts, AspeedEmulator, AspeedSoc, BoardFacts};

/// Leopard BMC marker.
pub struct Board;

impl AspeedBoardFacts for Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: AspeedSoc::Ast1250,
        // 32 MiB SPI NOR on CE0.
        flash_size: 32 * 1024 * 1024,
        emulator: Some(AspeedEmulator::PalmettoBmc),
    };
}

/// Time the boot milestones on a spare SoC timer and report them before
/// the payload jump.
pub const TIMESTAMPS: bool = false;

/// UART5, the AST1250 debug console, at 115200 baud from its 24 MHz clock.
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
