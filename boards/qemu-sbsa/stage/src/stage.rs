//! Static fixed-flow adapter for QEMU SBSA-ref.

use fstart_acpi::device::AcpiDevice;
use fstart_board_qemu_sbsa_facts as common;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, StaticConsole};
use fstart_stage_runtime::StaticBoard;

fn uart0_config() -> Pl011Config {
    Pl011Config {
        base_addr: common::UART0_BASE,
        clock_freq: common::UART0_CLOCK,
        baud_rate: common::UART0_BAUD,
        acpi_name: None,
        acpi_gsiv: Some(common::UART0_GSIV),
        acpi_dbg2: common::UART0_DBG2,
    }
}

type StageDevices = StaticConsole<Pl011>;

pub struct StageBoard {
    devices: StageDevices,
}

impl StageBoard {
    fn prepare_acpi(&self) -> Result<(), ServiceError> {
        let console = match self.devices.device() {
            Some(console) => console,
            None => return Err(ServiceError::NotInitialized),
        };

        let uart0_config = uart0_config();

        fstart_capabilities::acpi::prepare(&common::ACPI_PLATFORM, |dsdt, tables| {
            let uart_aml = console.dsdt_aml(&uart0_config);
            dsdt.extend_from_slice(&uart_aml);
            tables.extend(console.extra_tables(&uart0_config));

            let pci_aml = common::PCI0_ACPI.dsdt_aml();
            dsdt.extend_from_slice(&pci_aml);
            tables.extend(common::PCI0_ACPI.extra_tables());

            let ahci_aml = common::AHCI0_ACPI.dsdt_aml();
            dsdt.extend_from_slice(&ahci_aml);

            let xhci_aml = common::XHCI0_ACPI.dsdt_aml();
            dsdt.extend_from_slice(&xhci_aml);
        });
        Ok(())
    }
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(uart0_config()),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(common::UART0_NODE, common::UART0_DRIVER);
        Ok(())
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.prepare_acpi()?;
        fstart_capabilities::smbios::prepare(&common::SMBIOS_DESC);
        Ok(())
    }

    fn boot_payload(self) -> ! {
        fstart_log::info!("qemu-sbsa table preparation complete; halting");
        Self::halt()
    }
}
