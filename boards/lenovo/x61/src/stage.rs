//! Lenovo ThinkPad X61 binding for the GM965/ICH8 Intel early flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::IntelEarlyBoard;
use fstart_platform_intel::gm965::{Gm965Ich8, Gm965Ich8Board, Gm965Ich8Config};
use fstart_platform_intel::stage_runtime::payload::BuildSelectedPayload;

use crate::{Board, X61Mainboard};

impl IntelEarlyBoard for Board {
    type Platform = Gm965Ich8;
    type Hooks = X61Mainboard;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(X61Mainboard::new())
    }
}

impl Gm965Ich8Board for Board {
    type Console = fstart_driver_uart::ns16550::Ns16550;
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static Gm965Ich8Config = &crate::X61_PLATFORM;

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio {
                base: crate::UART0_PIO_BASE as u64,
            },
            clock_freq: crate::UART0_CLOCK_FREQ,
            baud_rate: crate::UART0_BAUD_RATE,
        }
    }

    fn console_node() -> &'static str {
        crate::UART0_NODE
    }

    #[cfg(fstart_stage_env = "ram")]
    fn smbios_desc() -> &'static fstart_platform_intel::tables::SmbiosDesc<'static> {
        &crate::X61_SMBIOS_DESC
    }
}
