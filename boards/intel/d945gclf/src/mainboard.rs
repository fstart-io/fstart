//! Intel D945GCLF mainboard work the stage `main`s call.
//!
//! Ported from coreboot `mainboard/intel/d945gclf/early_init.c`: the only
//! board-specific pre-console work is the SMSC LPC47M15x PME logical device
//! (runtime registers at 0x680, the ICH7 generic decode target) plus the
//! COM1/COM2/KBC setup handled by the generic SuperIO driver.

use fstart_core::services::ServiceError;
use fstart_core::services::device::BusDevice;
use fstart_driver_superio::smsc_lpc47m15x::SmscLpc47m15x;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

/// COM1 on the SuperIO.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

/// The SuperIO's PME block, then COM1/COM2/KBC, before the console probes.
/// PME first so the 0x680 decode window has a live target.
pub fn setup_superio() -> Result<(), ServiceError> {
    unsafe { fstart_log::set_max_level(fstart_log::Level::Debug) };
    let mut superio =
        SmscLpc47m15x::new_at_base(crate::d945gclf_superio_config().0, crate::SUPERIO_PNP_BASE)
            .map_err(ServiceError::from)?;
    superio
        .enable_pme(crate::SUPERIO_PME_BASE)
        .map_err(ServiceError::from)?;
    superio.init().map_err(ServiceError::from)
}

/// The board's ACPI: sleep/power buttons and the SuperIO scope.
pub struct Acpi;
