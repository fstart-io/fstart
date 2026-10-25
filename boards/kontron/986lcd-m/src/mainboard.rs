//! Dual-controller resource isolation and board phase policy.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::{ServiceError, device::BusDevice};
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_driver_superio::{
    w83627thg::W83627thg,
    winbond_hwm::{WinbondMonitor, WinbondMonitorProfile},
};

#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct Lcd986Mainboard;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl<S> fstart_platform_intel::IntelSmbusRouting<S> for Lcd986Mainboard {}

#[cfg(fstart_stage_env = "car")]
impl fstart_platform_intel::IntelEarlyBoardHooks<crate::Hardware> for Lcd986Mainboard {
    fn before_console(
        &mut self,
        _: &mut fstart_platform_intel::IntelEarlyCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        // Verify and quiesce the UART-only controller before touching shared
        // legacy ports. Relocate its UARTs before enabling the primary console.
        let mut secondary = W83627thg::new_at_base(
            fstart_driver_superio::SuperIoConfig::default(),
            crate::SECONDARY_SUPERIO_PNP_BASE,
        )?;
        secondary.init()?;
        secondary.disable_non_serial_functions();
        secondary.configure_standard_uart_clocks();
        W83627thg::new_at_base(
            crate::secondary_superio_config(),
            crate::SECONDARY_SUPERIO_PNP_BASE,
        )?
        .init()?;

        let mut primary = W83627thg::new_at_base(
            fstart_driver_superio::SuperIoConfig::default(),
            crate::SUPERIO_PNP_BASE,
        )?;
        primary.init()?;
        primary.set_clock_input(fstart_driver_superio::w83627thg::ClockInput::Mhz48);
        primary.configure_standard_uart_clocks();
        W83627thg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?.init()?;
        Ok(())
    }

    fn before_memory(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelEarlyCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        if ctx.boot_path == fstart_platform_intel::BootPath::S3Resume {
            return Ok(());
        }
        let mut primary =
            W83627thg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?;
        primary.init()?;
        // Nonserial resources are re-enabled only where policy is verified.
        // GPIO1/2/4/5 and game/MIDI remain inactive, unlike the reference script.
        primary.disable_non_serial_functions();
        primary.configure_fan_output_modes(&crate::COOLING)?;
        primary.enable_hwmon(crate::HWM_BASE);
        WinbondMonitor::new(crate::HWM_BASE, WinbondMonitorProfile::ThgE)?
            .force_full_speed(&crate::COOLING)?;
        primary.configure_gpio(&crate::superio_gpio_pins(false));
        Ok(())
    }
}

#[cfg(fstart_stage_env = "ram")]
impl fstart_platform_intel::IntelMainstageBoardHooks<crate::Hardware> for Lcd986Mainboard {
    fn before_console(
        &mut self,
        _: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        let mut secondary = W83627thg::new_at_base(
            crate::secondary_superio_config(),
            crate::SECONDARY_SUPERIO_PNP_BASE,
        )?;
        secondary.init()?;
        secondary.disable_non_serial_functions();

        let mut primary =
            W83627thg::new_at_base(crate::lcd986_superio_config(), crate::SUPERIO_PNP_BASE)?;
        primary.init()?;
        primary.set_keyboard_a20_acceleration(true);
        primary.configure_gpio(&crate::superio_gpio_pins(true));
        // Retain early HWM activation and manual-maximum outputs; re-enabling
        // the logical device would first deactivate it.
        Ok(())
    }

    fn before_devices(
        &mut self,
        _: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        // Re-establish the exact E-revision and matching pad-mode contract;
        // runtime identity alone cannot verify either condition.
        let mut primary =
            W83627thg::new_at_base(crate::lcd986_superio_config(), crate::SUPERIO_PNP_BASE)?;
        primary.configure_fan_output_modes(&crate::COOLING)?;
        WinbondMonitor::new(crate::HWM_BASE, WinbondMonitorProfile::ThgE)?.init(&crate::MONITOR)?;
        fstart_log::warn!(
            "986lcd-m: experimental VGA-only firmware; GPIO banks, LVDS and legacy combined storage incomplete; cooling unvalidated"
        );
        Ok(())
    }
}

#[cfg(fstart_stage_env = "ram")]
extern crate alloc;
#[cfg(fstart_stage_env = "ram")]
impl fstart_acpi::device::AcpiDevice for Lcd986Mainboard {
    type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        let mut sio = fstart_driver_superio::superio_dsdt_aml(&crate::lcd986_superio_config());
        sio.extend(fstart_driver_superio::superio_dsdt_aml_with_uart_policy(
            &crate::secondary_superio_config(),
            fstart_driver_superio::UartAcpiPolicy {
                numbers: [3, 4],
                shared_irq: true,
            },
        ));
        fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio)
            .expect("986LCD-M Super I/O AML")
    }
}

pub static LCD986_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Kontron",
        sys_product: "986LCD-M",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Kontron",
        bb_product: "986LCD-M",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Kontron",
        processor_sockets: &["Socket M"],
        oem_string: None,
    };
