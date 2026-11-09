//! IT8720F board policy through semantic driver operations.
use fstart_core::services::{ServiceError, device::BusDevice};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

pub fn setup_superio() -> Result<(), ServiceError> {
    use fstart_driver_superio::ite8720f::Ite8720f;
    let mut sio =
        Ite8720f::new_at_base(crate::ga_d510ud_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.configure_gpio(&crate::GA_D510UD_SUPERIO_GPIO, crate::SUPERIO_GPIO_BASE);
    sio.use_internal_power_good();
    sio.disable_watchdog();
    sio.preset_bus_select(crate::GA_D510UD_BSEL_PRESET);
    sio.select_main_flash();
    sio.disconnect_hwmon_irq();
    sio.disable_parallel_ecp_decode();
    Ok(())
}

pub struct Acpi;
