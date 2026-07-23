//! QEMU q35 binding for the QEMU monolithic flow.

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_qemu::{QemuQ35, QemuQ35Board, QemuQ35Config};
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        QemuQ35::run_stage::<Self>(env, handoff)
    }
}

impl QemuQ35Board for Board {
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static QemuQ35Config = &crate::QEMU_Q35_PLATFORM;

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio {
                base: crate::UART0_PIO_BASE,
            },
            clock_freq: crate::UART0_CLOCK_FREQ,
            baud_rate: crate::UART0_BAUD_RATE,
        }
    }

    fn console_node() -> &'static str {
        crate::UART0_NODE
    }
}
