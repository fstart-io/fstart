//! QEMU q35 board support package.

#![no_std]

pub mod config;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;

/// QEMU q35 board marker.
pub struct Board;

pub use config::*;

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_qemu::{
    QEMU_Q35_UART_BAUD_RATE, QEMU_Q35_UART_CLOCK_FREQ, QEMU_Q35_UART_PIO_BASE,
};

/// COM1.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: QEMU_Q35_UART_PIO_BASE,
        },
        clock_freq: QEMU_Q35_UART_CLOCK_FREQ,
        baud_rate: QEMU_Q35_UART_BAUD_RATE,
    }
}
