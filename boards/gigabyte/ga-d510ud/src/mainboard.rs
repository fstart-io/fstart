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

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::ga_d510ud_superio_config());
        fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio)
            .expect("GA-D510UD Super I/O AML")
    }
}
pub static GA_D510UD_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Gigabyte",
        sys_product: "GA-D510UD",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Gigabyte",
        bb_product: "GA-D510UD",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Gigabyte",
        processor_sockets: &["FCBGA559"],
        oem_string: None,
    };
