//! Foxconn D41S binding for the Pineview/ICH7 Intel early flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::IntelBoard;
use fstart_platform_intel::pineview::{PineviewIch7, PineviewIch7Platform};
#[cfg(fstart_stage_env = "ram")]
use fstart_platform_intel::stage_runtime::payload::BuildSelectedPayload;

use crate::{Board, D41SMainboard};

impl IntelBoard for Board {
    type Platform = PineviewIch7;
    type Hooks = D41SMainboard;
    type Console = fstart_driver_uart::ns16550::Ns16550;
    #[cfg(fstart_stage_env = "ram")]
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static PineviewIch7Platform = &crate::D41S_PLATFORM;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(D41SMainboard::new())
    }

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
    fn smbios_identity() -> &'static fstart_platform_intel::tables::SmbiosIdentity<'static> {
        &crate::D41S_SMBIOS_IDENTITY
    }
}
