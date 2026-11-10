//! Attached Winbond Super I/O and ICS9EPRS525 clock generator.
use fstart_core::services::{ServiceError, SmBus, device::BusDevice};
use fstart_driver_superio::w83627thg::W83627thg;
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
    let mut sio = W83627thg::new_at_base(crate::d510mo_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.enable_hwmon(crate::HWM_BASE);
    Ok(())
}

pub fn init_clock(smbus: &mut impl SmBus, resume: bool) -> Result<(), ServiceError> {
    if !resume {
        use fstart_driver_intel::generic::ck505::I2cCk505;
        I2cCk505::new_at_address(crate::d510mo_ck505_config(), 0x69)?.init_on_smbus(smbus)?;
    }
    Ok(())
}

pub struct Acpi;
