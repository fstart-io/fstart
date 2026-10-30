//! Attached Super I/O resources and CPU strap sequencing.
use fstart_core::services::{ServiceError, device::BusDevice};
use fstart_driver_superio::w83627dhg::W83627dhg;
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

/// The Winbond W83627DHG console UART, before the bootblock console.
pub fn setup_early_superio() -> Result<(), ServiceError> {
    W83627dhg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?.init()?;
    Ok(())
}

/// Drive the SuperIO GPIOs that strap the CPU bus select to what the CPU
/// reports, resetting once if they changed. Cold and warm boots only.
pub fn configure_cpu_straps() -> Result<(), ServiceError> {
    let bsel = fstart_arch::x86::cpu::intel::core2_cpu::Core2CpuDriver::bus_select()
        .ok_or(ServiceError::NotSupported)?;
    let mut sio = W83627dhg::new_at_base(crate::early_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.set_vid_input_gtl(true);
    let update = sio.configure_gpio(&crate::bsel_gpio_pins(bsel));
    if update.output_or_routing_changed {
        fstart_log::info!("p5gc-mx: reset to latch CPU BSEL straps");
        fstart_driver_intel::i945::cf9_full_reset();
    }
    Ok(())
}

/// The SuperIO's UART B, floppy, parallel port, GPIOs and hardware monitor,
/// before the ramstage console.
pub fn setup_superio() -> Result<(), ServiceError> {
    let mut sio = W83627dhg::new_at_base(crate::p5gc_mx_superio_config(), crate::SUPERIO_PNP_BASE)?;
    sio.select_uart_b_pins();
    sio.init()?;
    sio.enable_floppy(0x3f0, 6, 2);
    sio.set_parallel_dma(3);
    sio.disable_spi();
    sio.disable_watchdog();
    sio.enable_gpio6();
    sio.enable_dram_standby_gate();
    sio.enable_hwmon(crate::HWM_BASE);
    Ok(())
}

/// The board's ACPI: its DSDT fragment.
pub struct Acpi;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::p5gc_mx_superio_config());
        fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio).expect("P5GC-MX Super I/O AML")
    }
}

pub static P5GC_MX_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "ASUSTeK Computer Inc.",
        sys_product: "P5GC-MX",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "ASUSTeK Computer Inc.",
        bb_product: "P5GC-MX",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "ASUSTeK Computer Inc.",
        processor_sockets: &["LGA775"],
        oem_string: None,
    };
