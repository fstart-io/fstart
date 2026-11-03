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

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        use fstart_acpi_macros::acpi_dsl;
        let mut aml = acpi_dsl! {
            Scope("\\_SB_") {
                Device("PWRB") { Name("_HID", "PNP0C0C"); Name("_PRW", Package(0x1du32, 0x04u32)); }
                Device("SLPB") { Name("_HID", "PNP0C0E"); Name("_PRW", Package(0x1du32, 0x04u32)); }
            }
        }
        .to_vec();
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::d510mo_superio_config());
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio)
                .expect("D510MO Super I/O AML"),
        );
        aml
    }
}
pub static D510MO_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "D510MO",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Intel",
        bb_product: "D510MO",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processor_sockets: &["FCBGA559"],
        oem_string: None,
    };
