//! Foxconn D41S mainboard work called by the stage mains.

use fstart_core::services::device::BusDevice;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::generic::ck505::I2cCk505;
use fstart_driver_superio::ite8721f::Ite8721f;
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

/// Bring up the IT8721F: COM1, keyboard, and the unused functions off.
pub fn setup_superio() -> Result<(), ServiceError> {
    let mut superio = Ite8721f::new_at_base(crate::d41s_superio_config(), crate::SUPERIO_PNP_BASE)
        .map_err(ServiceError::from)?;
    superio.init().map_err(ServiceError::from)?;
    superio.disconnect_hwmon_irq();
    superio.disable_parallel_ecp_decode();
    superio.disable_floppy();
    Ok(())
}

/// Program the display, PCIe and USB clock generator; failures are nonfatal.
pub fn setup_clock(bus: &mut impl SmBus) {
    match I2cCk505::new_at_address(crate::d41s_ck505_config(), crate::CK505_ADDR) {
        Ok(mut ck505) => {
            if ck505.init_on_smbus(bus).is_err() {
                fstart_log::error!("d41s: clock generator programming failed");
            }
        }
        Err(_) => fstart_log::error!("d41s: invalid clock generator config"),
    }
}

/// The board's ACPI: its DSDT fragment.
pub struct Acpi;
