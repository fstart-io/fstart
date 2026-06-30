//! Static fixed-flow adapter for SiFive Unmatched hardware.

use fstart_driver_fu740_ddr::{Fu740Ddr, Fu740DdrConfig};
use fstart_driver_fu740_prci::{Fu740Prci, Fu740PrciConfig};
use fstart_driver_sifive_uart::{SifiveUart, SifiveUartConfig};
use fstart_services::device::DeviceError;
use fstart_services::{Device, HardwareInit, InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FFS_BASE: u64 = 0x0800_0000;
const FFS_SIZE: usize = 0x20_0000;
const RAM_BASE: u64 = 0x8000_0000;
const RAM_SIZE: u64 = 0x4_0000_0000;
const KERNEL_LOAD_ADDR: u64 = 0x8400_0000;
const FIRMWARE_LOAD_ADDR: u64 = 0x8300_0000;
const FDT_ADDR: u64 = 0x8f00_0000;
const BOOT_HART_ID: u64 = 1;
const BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

static PRCI_CONFIG: Fu740PrciConfig = Fu740PrciConfig {
    base_addr: 0x1000_0000,
    gpio_base: 0x1006_0000,
};

static UART0_CONFIG: SifiveUartConfig = SifiveUartConfig {
    base_addr: 0x1001_0000,
    clock_freq: 130_000_000,
    baud_rate: 115_200,
};

static DDR_CONFIG: Fu740DdrConfig = Fu740DdrConfig {
    ctl_base: 0x100b_0000,
    phy_base: 0x100b_2000,
    filter_base: 0x100b_8000,
    dram_size: RAM_SIZE,
};

pub struct StageDevices {
    prci: Option<Fu740Prci>,
    uart0: StaticConsole<SifiveUart>,
    ddr: Option<Fu740Ddr>,
}

impl StageDevices {
    const fn new() -> Self {
        Self {
            prci: None,
            uart0: StaticConsole::new(&UART0_CONFIG),
            ddr: None,
        }
    }

    fn ensure_prci(&mut self) -> Result<&mut Fu740Prci, ServiceError> {
        if self.prci.is_none() {
            self.prci = Some(Fu740Prci::new(&PRCI_CONFIG).map_err(device_error_to_service_error)?);
        }
        Ok(self.prci.as_mut().expect("PRCI device constructed"))
    }

    fn ensure_ddr(&mut self) -> Result<&mut Fu740Ddr, ServiceError> {
        if self.ddr.is_none() {
            self.ddr = Some(Fu740Ddr::new(&DDR_CONFIG).map_err(device_error_to_service_error)?);
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
            boot: MemoryMappedLinuxBoot::new(FFS_BASE, FFS_SIZE, 0),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready("uart0", "sifive-uart");
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
        self.boot.load_fdt(FDT_ADDR)
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot
            .prepare_fdt(FDT_ADDR, BOOTARGS, RAM_BASE, RAM_SIZE)
    }

    fn boot_payload(self) -> ! {
        if !self.boot.firmware_loaded() || !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before firmware/kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting OpenSBI at {:#x}, kernel at {:#x}, dtb at {:#x}",
            FIRMWARE_LOAD_ADDR,
            KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params =
            self.boot
                .boot_params(KERNEL_LOAD_ADDR, FIRMWARE_LOAD_ADDR, BOOT_HART_ID, BOOTARGS);
        fstart_platform::boot_linux(&params)
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
