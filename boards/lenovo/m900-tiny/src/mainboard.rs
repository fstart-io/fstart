// SPDX-License-Identifier: MIT OR Apache-2.0
//! M900 Tiny board functions: the NCT6687D's console and its ramstage
//! setup (COM1, ACPI and EC decodes, the hardware monitor).
use crate::superio;
use fstart_core::services::device::DeviceError;
use fstart_driver_superio::nct6687d::Nct6687d;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

pub const CONSOLE_BAUD: u32 = 115_200;

/// COM1 on the NCT6687D (the optional serial header).
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: superio::COM1_BASE as u64,
        },
        clock_freq: 1_843_200,
        baud_rate: CONSOLE_BAUD,
    }
}

/// Bring out COM1 before the console (coreboot's `bootblock_mainboard_early_init`).
pub fn early() -> Result<(), DeviceError> {
    let mut sio =
        Nct6687d::new_at_base(superio::console_config(CONSOLE_BAUD), superio::CONFIG_PORT)?;
    sio.configure_pins(superio::PIN_CONFIG)?;
    sio.init()?;
    sio.verify_com_ports()
}

#[derive(Debug)]
pub enum Error {
    Thermal(fstart_driver_superio::nct6687d::hwm::Error),
    SuperIo(DeviceError),
}

/// The Super I/O's ramstage devicetree: COM1, the ACPI and EC decodes, the
/// power-loss policy and the hardware monitor. Returns the board's ACPI
/// device, which describes what it set up.
#[cfg(feature = "acpi")]
pub fn init_superio(
    tpm: Option<fstart_driver_tpm::Family>,
) -> Result<crate::acpi::M900Acpi, Error> {
    use crate::hwm;
    use fstart_driver_intel::sunrise_point::pmc::AfterPowerLoss;
    use fstart_driver_superio::nct6687d::{self, AfterPowerFailure};
    let config = superio::console_config(CONSOLE_BAUD);
    let mut sio =
        Nct6687d::new_at_base(config.clone(), superio::CONFIG_PORT).map_err(Error::SuperIo)?;
    sio.configure_pins(superio::PIN_CONFIG)
        .map_err(Error::SuperIo)?;
    sio.init().map_err(Error::SuperIo)?;
    // The generic init leaves the functions it was not given as they are.
    sio.disable_unconfigured_functions()
        .map_err(Error::SuperIo)?;
    sio.verify_com_ports().map_err(Error::SuperIo)?;
    let after_power_loss = match crate::profile::SOUTHBRIDGE.power.after_power_loss {
        AfterPowerLoss::Off => AfterPowerFailure::Off,
        AfterPowerLoss::On => AfterPowerFailure::On,
        AfterPowerLoss::Last => AfterPowerFailure::PreviousState,
    };
    sio.enable_acpi_decode(superio::ACPI_BASE, after_power_loss)
        .map_err(Error::SuperIo)?;
    let mut ec = sio
        .enable_ec_decode(superio::EC_BASE)
        .map_err(Error::SuperIo)?;
    sio.configure_tach_pwm(
        hwm::TACH_PWM_ASSIGNMENTS[0].1,
        hwm::TACH_PWM_ASSIGNMENTS[1].1,
    )
    .map_err(Error::SuperIo)?;
    nct6687d::hwm::initialize(&mut ec, &mut fstart_arch::x86::Udelay, &hwm::CONFIG)
        .map_err(Error::Thermal)?;
    Ok(crate::acpi::M900Acpi::new(config, tpm))
}
