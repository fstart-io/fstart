//! Lenovo ThinkPad X61 binding for the GM965/ICH8 Intel early flow.

#[cfg(feature = "acpi")]
use fstart_acpi::device::AcpiDevice;
#[cfg(feature = "acpi")]
use fstart_acpi::platform::{PlatformConfig, X86PlatformProvider};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
#[cfg(feature = "acpi")]
use fstart_platform_intel_gm965_ich8::Gm965Ich8Mainstage;
#[cfg(feature = "mp")]
use fstart_platform_intel_gm965_ich8::ICH8_PMBASE;
use fstart_platform_intel_gm965_ich8::{
    Gm965Ich8, Gm965Ich8Board, Gm965Ich8Config, IntelEarlyBoard,
};
use fstart_services::ServiceError;
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageKind};

use crate::{Board, X61Mainboard};

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_types::Platform = crate::PLATFORM;

    fn run_stage(stage: StageKind, handoff: usize) -> ! {
        Gm965Ich8::run_stage::<Self>(stage, handoff)
    }
}

impl IntelEarlyBoard for Board {
    type Platform = Gm965Ich8;
    type Hooks = X61Mainboard;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(X61Mainboard::new())
    }
}

impl Gm965Ich8Board for Board {
    type Payload = BuildSelectedPayload;

    fn config() -> &'static Gm965Ich8Config {
        &crate::X61_PLATFORM
    }

    fn ifd_flash_layout() -> fstart_types::IntelIfdFlashLayout {
        crate::x61_ifd_flash_layout()
    }

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio {
                base: crate::UART0_PIO_BASE as u64,
            },
            clock_freq: crate::UART0_CLOCK_FREQ,
            baud_rate: crate::UART0_BAUD_RATE,
        }
    }

    fn console_node() -> &'static str {
        crate::UART0_NODE
    }

    fn halt() -> ! {
        fstart_platform_x86_64::halt()
    }

    #[cfg(feature = "mp")]
    fn init_mp() -> Result<(), ServiceError> {
        let cpu = fstart_arch::cpu_intel::core2_cpu::Core2CpuDriver::new(ICH8_PMBASE, None);
        let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
        fstart_arch::mp::mp_init(&fstart_arch::mp::MpConfig {
            cpu_drivers: &drivers,
            smm: None,
            smm_image: None,
            max_cpus: 2,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }

    #[cfg(feature = "acpi")]
    fn prepare_acpi(devices: &mut Gm965Ich8Mainstage<Self>) -> Option<u64> {
        let southbridge = devices.southbridge();
        let platform = PlatformConfig::X86(
            southbridge.x86_platform_config(fstart_arch::mp::online_cpus() as u32),
        );
        let rsdp =
            fstart_platform_intel_gm965_ich8::tables::prepare_acpi(&platform, |dsdt, extra| {
                dsdt.extend(
                    devices
                        .northbridge()
                        .dsdt_aml(devices.northbridge().config()),
                );
                dsdt.extend(southbridge.dsdt_aml(southbridge.config()));
                dsdt.extend(crate::x61_mainboard_dsdt_aml(devices.acpi_context()));
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
        fstart_platform_intel_gm965_ich8::tables::prepare_smbios(&crate::X61_SMBIOS_DESC);
    }
}
