//! Main-stage endpoint for QEMU RISC-V multi-stage demo.

use crate::facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: facts::UART0_BASE,
        reg_shift: facts::UART0_REG_SHIFT,
        reg_width: facts::UART0_REG_WIDTH,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

type MainDevices = StaticConsole<Ns16550>;

pub struct MainBoard {
    devices: MainDevices,
}

impl StaticBoard for MainBoard {
    type Devices = MainDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: MainDevices::new(UART0_CONFIG),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        super::fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "ns16550");
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
