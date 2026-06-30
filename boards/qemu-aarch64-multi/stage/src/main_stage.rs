//! Main-stage endpoint for QEMU AArch64 multi-stage demo.

use fstart_board_qemu_aarch64_multi_facts as facts;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, StaticConsole};
use fstart_stage_runtime::StaticBoard;

static UART0_CONFIG: Pl011Config = Pl011Config {
    base_addr: facts::UART0_BASE,
    clock_freq: facts::UART0_CLOCK,
    baud_rate: facts::UART0_BAUD,
    acpi_name: None,
    acpi_gsiv: None,
    acpi_dbg2: false,
};

type MainDevices = StaticConsole<Pl011>;

pub struct MainBoard {
    devices: MainDevices,
}

impl StaticBoard for MainBoard {
    type Devices = MainDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: MainDevices::new(UART0_CONFIG.clone()),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "pl011");
        Ok(())
    }

    fn boot_payload(self) -> ! {
        fstart_log::info!(
            "{} main stage reached; no final payload configured",
            facts::BOARD_NAME,
        );
        Self::halt()
    }
}
