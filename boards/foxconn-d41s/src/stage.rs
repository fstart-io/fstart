//! Foxconn D41S binding for the Pineview/ICH7 Intel early flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::pineview::{PineviewIch7, PineviewIch7Board, PineviewIch7Config};
use fstart_platform_intel::IntelEarlyBoard;
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageEnvironment};

use crate::{Board, D41SMainboard};

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        PineviewIch7::run_stage::<Self>(env, handoff)
    }
}

impl IntelEarlyBoard for Board {
    type Platform = PineviewIch7;
    type Hooks = D41SMainboard;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(D41SMainboard::new())
    }
}

impl PineviewIch7Board for Board {
    type Console = fstart_driver_uart::ns16550::Ns16550;
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static PineviewIch7Config = &crate::D41S_PLATFORM;

    fn flash_layout() -> fstart_core::FlashLayout {
        crate::d41s_flash_layout()
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

    #[cfg(feature = "smbios")]
    fn smbios_desc() -> &'static fstart_platform_intel::tables::SmbiosDesc<'static> {
        &crate::D41S_SMBIOS_DESC
    }
}
