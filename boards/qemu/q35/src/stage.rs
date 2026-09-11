//! QEMU q35 binding for the QEMU monolithic flow.

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_qemu::{
    QEMU_Q35_UART_BAUD_RATE, QEMU_Q35_UART_CLOCK_FREQ, QEMU_Q35_UART_NODE, QEMU_Q35_UART_PIO_BASE,
    QemuQ35Board, QemuQ35Config,
};
use fstart_platform_qemu::stage_runtime::payload::BuildSelectedPayload;

use crate::Board;

impl QemuQ35Board for Board {
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static QemuQ35Config = &crate::QEMU_Q35_PLATFORM;

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio {
                base: QEMU_Q35_UART_PIO_BASE,
            },
            clock_freq: QEMU_Q35_UART_CLOCK_FREQ,
            baud_rate: QEMU_Q35_UART_BAUD_RATE,
        }
    }

    fn console_node() -> &'static str {
        QEMU_Q35_UART_NODE
    }
}
