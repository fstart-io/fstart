//! QEMU virt board support package.

#![no_std]

pub mod config;

/// QEMU virt board marker.
pub struct Board;

pub use config::*;

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_qemu::virt::QEMU_RISCV64_UART_BASE;

/// The virt machine's NS16550.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: QEMU_RISCV64_UART_BASE,
            reg_shift: 0,
            reg_width: 0,
        },
        clock_freq: 3_686_400,
        baud_rate: 115_200,
    }
}
