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

pub fn setup_console() -> Result<(), ServiceError> {
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

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;

    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        d41s_mainboard_dsdt_aml(*config)
    }
}

mod acpi_impl {
    use alloc::vec::Vec;
    use fstart_platform_intel::pineview::PineviewIch7AcpiContext;

    pub fn d41s_mainboard_dsdt_aml(context: PineviewIch7AcpiContext) -> Vec<u8> {
        let config = crate::d41s_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        fstart_acpi::aml_linker::scope_vec(context.lpc_scope(), &sio)
            .expect("D41S required Super I/O scope emission failed")
    }
}

pub use acpi_impl::d41s_mainboard_dsdt_aml;

static D41S_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["FCBGA559"];

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
