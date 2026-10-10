use crate::Board;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::IntelBoard;
impl IntelBoard for Board {
    #[cfg(fstart_stage_env = "car")]
    type EarlyHooks = crate::GaD510udMainboard;
    #[cfg(fstart_stage_env = "ram")]
    type MainstageHooks = crate::GaD510udMainboard;
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
        "superio/com1"
    }
    #[cfg(fstart_stage_env = "ram")]
    fn smbios_identity() -> &'static fstart_platform_intel::tables::SmbiosIdentity<'static> {
        &crate::GA_D510UD_SMBIOS_IDENTITY
    }
}
