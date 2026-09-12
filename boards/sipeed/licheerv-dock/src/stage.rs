//! Lichee RV Dock binding for the handwritten D1 flow.
use crate::Board;
use fstart_driver_sunxi::d1_ccu::D1_UART0_BASE;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_sunxi::d1::{D1Board, D1};
impl D1Board for Board {
    const CONFIG: &'static fstart_platform_sunxi::d1::D1Config = &crate::LICHEERV_DOCK_D1;
    const CONSOLE_CONFIG: Ns16550Config = Ns16550Config {
        regs: AccessMode::Mmio {
            base: D1_UART0_BASE,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    };
}
