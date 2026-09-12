//! Intel D945GCLF binding for the i945/ICH7 Intel early flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::IntelEarlyBoard;
use fstart_platform_intel::i945::{I945Ich7, I945Ich7Board, I945Ich7Config};
use fstart_platform_intel::stage_runtime::payload::BuildSelectedPayload;

use crate::{Board, D945GclfMainboard};

impl IntelEarlyBoard for Board {
    type Platform = I945Ich7;
    type Hooks = D945GclfMainboard;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(D945GclfMainboard::new())
    }
}

impl I945Ich7Board for Board {
    type Console = fstart_driver_uart::ns16550::Ns16550;
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static I945Ich7Config = &crate::D945GCLF_PLATFORM;

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
        &crate::D945GCLF_SMBIOS_DESC
    }
}
