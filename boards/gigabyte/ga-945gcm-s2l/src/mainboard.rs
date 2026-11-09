//! Attached-device sequencing; register encodings live in reusable drivers.
use fstart_core::services::{ServiceError, device::BusDevice};
use fstart_driver_superio::{ite_env::IteEnvironmentController, ite8718f::Ite8718f};
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

/// The ITE IT8718F's console UART, before the bootblock console.
pub fn setup_superio() -> Result<(), ServiceError> {
    Ite8718f::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?.init()?;
    Ok(())
}

/// Fans at full speed, hardware monitor and the SuperIO GPIOs, before DRAM
/// training (cold and warm boots).
pub fn setup_cooling() -> Result<(), ServiceError> {
    let mut sio = Ite8718f::new_at_base(crate::cooling_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.verify_environment_revision()?;
    sio.disconnect_hwmon_irq();
    IteEnvironmentController::new(crate::HWM_BASE)?
        .full_speed([true, true, false], crate::ENVIRONMENT.fan_polarity)?;
    sio.disable_watchdog();
    sio.configure_gpio(&crate::SUPERIO_PINS, crate::SUPERIO_GPIO_BASE);
    sio.use_external_voltage_inputs();
    sio.use_voltage_input6();
    sio.use_auxiliary_fan_tachometers();
    sio.map_monitor_beep(Some(46));
    sio.capture_initial_vid();
    Ok(())
}

/// The SuperIO's keyboard, parallel port and wake events, before the
/// ramstage console.
pub fn setup_mainstage_superio() -> Result<(), ServiceError> {
    let mut sio = Ite8718f::new_at_base(crate::ga945gcm_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.verify_environment_revision()?;
    sio.configure_keyboard_native();
    sio.disconnect_hwmon_irq();
    sio.disable_apc_wake_events();
    sio.disable_floppy();
    sio.configure_parallel_spp();
    sio.init()?;
    Ok(())
}

/// Automatic fan control once the sensors qualify, before the chipset
/// devices.
pub fn setup_environment() -> Result<(), ServiceError> {
    let mut environment = IteEnvironmentController::new(crate::HWM_BASE)?;
    let pending = environment.prepare(&crate::ENVIRONMENT)?;
    // ICH7 early_init has enabled HPET. Keep full cooling on failed qualification.
    if pending
        .enable_automatic(|us| fstart_driver_intel::ich7::HPET.delay_us(us))
        .is_err()
    {
        fstart_log::warn!("ga-945gcm: sensor qualification failed; fans remain full-speed");
    }
    #[cfg(not(feature = "s2c"))]
    fstart_log::warn!("ga-945gcm-s2l: RTL8168 reset/MAC initialization is not implemented");
    Ok(())
}

/// The board's ACPI: its DSDT fragment.
pub struct Acpi;
