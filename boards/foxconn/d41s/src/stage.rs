//! Foxconn D41S binding for the Pineview/ICH7 Intel early flow.

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::IntelBoard;

use crate::Board;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use crate::D41SMainboard;

impl IntelBoard for Board {
    #[cfg(fstart_stage_env = "car")]
    type EarlyHooks = D41SMainboard;
    #[cfg(fstart_stage_env = "ram")]
    type MainstageHooks = D41SMainboard;
    type Console = fstart_driver_uart::ns16550::Ns16550;

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
