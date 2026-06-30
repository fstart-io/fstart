//! Static fixed-flow adapter for SiFive Unmatched hardware.

use fstart_board_sifive_unmatched_hw_facts as facts;
use fstart_driver_fu740_ddr::{Fu740Ddr, Fu740DdrConfig};
use fstart_driver_fu740_prci::{Fu740Prci, Fu740PrciConfig};
use fstart_driver_sifive_uart::{SifiveUart, SifiveUartConfig};
use fstart_services::device::DeviceError;
use fstart_services::{Device, HardwareInit, InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

static PRCI_CONFIG: Fu740PrciConfig = Fu740PrciConfig {
    base_addr: facts::PRCI_BASE,
    gpio_base: facts::PRCI_GPIO_BASE,
};

static UART0_CONFIG: SifiveUartConfig = SifiveUartConfig {
    base_addr: facts::UART0_BASE,
    clock_freq: facts::UART0_CLOCK,
    baud_rate: facts::UART0_BAUD,
};

static DDR_CONFIG: Fu740DdrConfig = Fu740DdrConfig {
    ctl_base: facts::DDR_CTL_BASE,
    phy_base: facts::DDR_PHY_BASE,
    filter_base: facts::DDR_FILTER_BASE,
    dram_size: facts::RAM_SIZE,
};

pub struct StageDevices {
    prci: Option<Fu740Prci>,
    uart0: StaticConsole<SifiveUart>,
    ddr: Option<Fu740Ddr>,
}

impl StageDevices {
    fn new() -> Self {
        Self {
            prci: None,
            uart0: StaticConsole::new(UART0_CONFIG),
            ddr: None,
        }
    }

    fn ensure_prci(&mut self) -> Result<&mut Fu740Prci, ServiceError> {
        if self.prci.is_none() {
            self.prci =
                Some(Fu740Prci::new(PRCI_CONFIG.clone()).map_err(device_error_to_service_error)?);
        }
        Ok(self.prci.as_mut().expect("PRCI device constructed"))
    }

    fn ensure_ddr(&mut self) -> Result<&mut Fu740Ddr, ServiceError> {
        if self.ddr.is_none() {
            self.ddr =
                Some(Fu740Ddr::new(DDR_CONFIG.clone()).map_err(device_error_to_service_error)?);
        }
        Ok(self.ddr.as_mut().expect("DDR device constructed"))
    }
}

impl HardwareInit for StageDevices {
    fn early_clocks(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ensure_prci()?.early_clocks(ctx)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.uart0.console(ctx)
    }

    fn dram(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ensure_ddr()?.dram(ctx)
    }
}

pub struct StageBoard {
    devices: StageDevices,
    boot: MemoryMappedLinuxBoot,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(),
            boot: MemoryMappedLinuxBoot::new(facts::FFS_BASE, facts::FFS_SIZE, 0),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "sifive-uart");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.load_firmware_and_kernel()?;
        self.boot.load_fdt(facts::FDT_ADDR)
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.prepare_fdt(
            facts::FDT_ADDR,
            facts::BOOTARGS,
            facts::RAM_BASE,
            facts::RAM_SIZE,
        )
    }

    fn boot_payload(self) -> ! {
        if !self.boot.firmware_loaded() || !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before firmware/kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting OpenSBI at {:#x}, kernel at {:#x}, dtb at {:#x}",
            facts::FIRMWARE_LOAD_ADDR,
            facts::KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params = self.boot.boot_params(
            facts::KERNEL_LOAD_ADDR,
            facts::FIRMWARE_LOAD_ADDR,
            u64::from(facts::BOOT_HART_ID),
            facts::BOOTARGS,
        );
        fstart_platform::boot_linux(&params)
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
