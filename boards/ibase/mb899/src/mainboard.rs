//! MB899 attached-device sequencing; drivers own all register encodings.
use fstart_core::services::{ServiceError, device::BusDevice};
use fstart_driver_superio::w83627ehg::{ClockInput, FanOutput, OutputDriver};
use fstart_driver_superio::{
    w83627ehg::W83627ehg,
    winbond_hwm::{WinbondMonitor, WinbondMonitorProfile},
};
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

/// The SuperIO's 48 MHz clock and UART A pins, before the bootblock console.
pub fn setup_early_superio() -> Result<(), ServiceError> {
    let mut sio = W83627ehg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.set_clock_input(ClockInput::Mhz48);
    sio.select_uart_a_pins();
    Ok(())
}
/// Fans at full speed, hardware monitor, and the rest of the SuperIO, before
/// DRAM training (cold and warm boots).
pub fn setup_cooling() -> Result<(), ServiceError> {
    let mut sio = W83627ehg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.set_fan_output_driver(FanOutput::System, OutputDriver::OpenDrain);
    sio.set_fan_output_driver(FanOutput::CpuPrimary, OutputDriver::OpenDrain);
    sio.enable_hwmon(crate::HWM_BASE);
    WinbondMonitor::new(crate::HWM_BASE, WinbondMonitorProfile::Ehg)?
        .force_full_speed(&crate::COOLING)?;
    sio.disable_watchdog();
    sio.disable_floppy();
    sio.disable_parallel();
    sio.disable_serial_flash();
    sio.set_vid_input_vrm10(false);
    sio.select_uart_b_pins();
    sio.select_secondary_cpu_fan_pins();
    sio.configure_gpio(&crate::superio_gpio_pins(false));
    Ok(())
}

/// The SuperIO's UART B, GPIOs and hardware monitor, before the ramstage
/// console.
pub fn setup_superio() -> Result<(), ServiceError> {
    let mut sio = W83627ehg::new_at_base(crate::mb899_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.init()?;
    sio.configure_uart_b_normal();
    sio.configure_gpio(&crate::superio_gpio_pins(true));
    sio.enable_hwmon(crate::HWM_BASE);
    Ok(())
}
/// The hardware monitor limits, before the chipset devices.
pub fn init_hwmon() -> Result<(), ServiceError> {
    WinbondMonitor::new(crate::HWM_BASE, WinbondMonitorProfile::Ehg)?.init(&crate::MONITOR)?;
    fstart_log::warn!(
        "mb899: experimental VGA-only firmware; LVDS/VBT, EC and riser support incomplete"
    );
    Ok(())
}

/// The board's ACPI: its DSDT fragment.
pub struct Acpi;
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::mb899_superio_config());
        fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio).expect("MB899 Super I/O AML")
    }
}

pub static MB899_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "IBASE",
        sys_product: "MB899",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "IBASE",
        bb_product: "MB899",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "IBASE",
        processor_sockets: &["Socket M"],
        oem_string: None,
    };
