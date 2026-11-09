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

#[cfg(feature = "acpi")]
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> alloc::vec::Vec<u8> {
        d945gclf_mainboard_dsdt_aml()
    }
}

#[cfg(feature = "acpi")]
static D945GCLF_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket 441"];

#[cfg(feature = "acpi")]
pub static D945GCLF_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "D945GCLF",
        sys_version: "1.0",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Intel",
        bb_product: "D945GCLF",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processor_sockets: &D945GCLF_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(feature = "acpi")]
mod acpi_impl {
    use alloc::vec::Vec;
    use fstart_acpi_macros::acpi_dsl;

    /// Board DSDT fragment: sleep/power buttons (coreboot `mainboard.asl`)
    /// plus the SuperIO scope (coreboot `superio.asl`) under LPCB.
    pub fn d945gclf_mainboard_dsdt_aml() -> Vec<u8> {
        let mut out = Vec::new();

        out.extend_from_slice(&acpi_dsl! {
            Scope("\\_SB_") {
                Device("SLPB") {
                    Name("_HID", "PNP0C0E");
                    Name("_PRW", Package(0x1du32, 0x04u32));
                }
                Device("PWRB") {
                    Name("_HID", "PNP0C0C");
                    Name("_PRW", Package(0x1du32, 0x04u32));
                }
            }
        });

        let config = crate::d945gclf_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        out.extend(
            fstart_acpi::aml_linker::scope_vec(
                <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
                &sio,
            )
            .expect("D945GCLF required Super I/O scope emission failed"),
        );
        out
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::d945gclf_mainboard_dsdt_aml;
