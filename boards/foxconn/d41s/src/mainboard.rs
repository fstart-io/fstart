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

#[cfg(feature = "acpi")]
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> alloc::vec::Vec<u8> {
        d41s_mainboard_dsdt_aml()
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    use alloc::vec::Vec;

    pub fn d41s_mainboard_dsdt_aml() -> Vec<u8> {
        let config = crate::d41s_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        fstart_acpi::aml_linker::scope_vec(
            <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
            &sio,
        )
        .expect("D41S required Super I/O scope emission failed")
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::d41s_mainboard_dsdt_aml;

#[cfg(feature = "acpi")]
static D41S_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["FCBGA559"];

#[cfg(feature = "acpi")]
pub static D41S_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Foxconn",
        sys_product: "D41S",
        sys_version: "1.0",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Foxconn",
        bb_product: "D41S",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Foxconn",
        processor_sockets: &D41S_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };
