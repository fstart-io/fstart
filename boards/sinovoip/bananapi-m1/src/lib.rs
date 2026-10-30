//! LeMaker Banana Pi M1 board support package.

#![no_std]

pub mod config;

/// Banana Pi M1 marker.
pub struct Board;

pub use config::*;

use fstart_driver_sunxi::a20_ccu::A20_UART0_BASE;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

/// UART0 at 115200 baud. The SoC's CCU init routes its pins.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: A20_UART0_BASE,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    }
}
