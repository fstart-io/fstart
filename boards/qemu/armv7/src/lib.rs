//! QEMU virt board support package.

#![no_std]

pub mod config;

/// QEMU virt board marker.
pub struct Board;

pub use config::*;

use fstart_driver_uart::pl011::Pl011Config;
use fstart_platform_qemu::virt::QEMU_ARMV7_UART_BASE;

/// The virt machine's PL011.
pub const fn console() -> Pl011Config {
    Pl011Config {
        base_addr: QEMU_ARMV7_UART_BASE,
        clock_freq: 1_843_200,
        baud_rate: 115_200,
    }
}
