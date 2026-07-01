//! Lenovo ThinkPad X61 binding for the reusable GM965/ICH8 recipe.

#[cfg(feature = "acpi")]
use fstart_acpi::device::AcpiDevice;
#[cfg(feature = "acpi")]
use fstart_acpi::platform::{PlatformConfig, X86PlatformProvider};
use fstart_board_lenovo_x61 as x61;
use fstart_board_lenovo_x61::LenovoX61Southbridge;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
#[cfg(feature = "acpi")]
use fstart_platform_intel_gm965_ich8::Gm965Ich8RamstageDevices;
#[cfg(feature = "mp")]
use fstart_platform_intel_gm965_ich8::ICH8_PMBASE;
use fstart_platform_intel_gm965_ich8::{Gm965Ich8Config, Gm965Ich8UefiBoard};
use fstart_services::{Device, DeviceError, ServiceError};

/// Selected-board marker used by the GM965/ICH8 recipe.
pub struct Board;

impl Gm965Ich8UefiBoard for Board {
    type Southbridge = LenovoX61Southbridge<IntelIch8>;

    fn platform_config() -> Gm965Ich8Config {
        x61::gm965_ich8_config()
    }

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio {
                base: x61::UART0_PIO_BASE as u64,
            },
            clock_freq: x61::UART0_CLOCK_FREQ,
            baud_rate: x61::UART0_BAUD_RATE,
        }
    }

    fn console_node() -> &'static str {
        x61::UART0_NODE
    }

    fn new_southbridge() -> Result<Self::Southbridge, ServiceError> {
        let config = x61::gm965_ich8_config();
        let southbridge = IntelIch8::new(config.ich8).map_err(device_error_to_service_error)?;
        let mainboard = x61::LenovoX61Mainboard::new(x61::x61_mainboard_config())
            .map_err(device_error_to_service_error)?;
        Ok(LenovoX61Southbridge::new(southbridge, mainboard))
    }

    #[cfg(feature = "mp")]
    fn init_mp() -> Result<(), ServiceError> {
        let cpu = fstart_cpu_intel::core2_cpu::Core2CpuDriver::new(ICH8_PMBASE, None);
        let drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu];
        fstart_mp::mp_init(&fstart_mp::MpConfig {
            cpu_drivers: &drivers,
            smm: None,
            smm_image: None,
            max_cpus: 2,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }

    #[cfg(feature = "acpi")]
    fn prepare_acpi(devices: &mut Gm965Ich8RamstageDevices<Self>) -> Option<u64> {
        let platform = PlatformConfig::X86(
            devices
                .southbridge()
                .southbridge()
                .x86_platform_config(fstart_mp::online_cpus() as u32),
        );
        let rsdp =
            fstart_capabilities::acpi::prepare_with_options(&platform, true, |dsdt, extra| {
                let southbridge = devices.southbridge().southbridge();
                let mainboard = devices.southbridge().mainboard();

                dsdt.extend(
                    devices
                        .northbridge()
                        .dsdt_aml(devices.northbridge().config()),
                );
                dsdt.extend(southbridge.dsdt_aml(southbridge.config()));
                dsdt.extend(mainboard.dsdt_aml(mainboard.config()));
                extra.extend(
                    devices
                        .northbridge()
                        .extra_tables(devices.northbridge().config()),
                );
                extra.extend(southbridge.extra_tables(southbridge.config()));
            });
        Some(rsdp)
    }

    #[cfg(feature = "smbios")]
    fn prepare_smbios() {
        fstart_capabilities::smbios::prepare(&x61::X61_SMBIOS_DESC);
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
