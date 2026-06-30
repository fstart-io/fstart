//! Bootblock fixed-flow adapter for Orange Pi PC2.

use fstart_board_orangepi_pc2_facts as facts;
use fstart_driver_ns16550::Ns16550;
use fstart_driver_sunxi_h3_ccu::SunxiH3Ccu;
use fstart_driver_sunxi_h3_dramc::SunxiH3Dramc;
use fstart_driver_sunxi_mmc::SunxiMmc;
use fstart_services::{Device, HardwareInit, InitContext, MemoryController, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, StaticConsole};
use fstart_stage_runtime::StaticBoard;

use crate::common::{
    device_error_to_service_error, CCU_CONFIG, DRAMC_CONFIG, MMC0_CONFIG, UART0_CONFIG,
};

pub struct BootblockDevices {
    ccu: Option<SunxiH3Ccu>,
    console: StaticConsole<Ns16550>,
    dramc: Option<SunxiH3Dramc>,
    mmc0: Option<SunxiMmc>,
}

impl BootblockDevices {
    pub fn new() -> Self {
        Self {
            ccu: None,
            console: StaticConsole::new(UART0_CONFIG),
            dramc: None,
            mmc0: None,
        }
    }

    fn ensure_mmc0(&mut self) -> Result<&mut SunxiMmc, ServiceError> {
        if self.mmc0.is_none() {
            let mmc = SunxiMmc::new(MMC0_CONFIG.clone()).map_err(device_error_to_service_error)?;
            self.mmc0 = Some(mmc);
        }
        Ok(self.mmc0.as_mut().expect("MMC0 constructed"))
    }

    fn dram_size(&self) -> u64 {
        self.dramc
            .as_ref()
            .map_or(facts::RAM_SIZE, MemoryController::detected_size_bytes)
    }

    fn load_main(&self) -> Result<(), ServiceError> {
        let offset = u64::from(fstart_soc_sunxi::next_stage_offset());
        let size = fstart_soc_sunxi::next_stage_size() as usize;
        if offset == 0 || size == 0 {
            fstart_log::error!("missing eGON next-stage metadata");
            return Err(ServiceError::InvalidParam);
        }

        let mmc = self.mmc0.as_ref().ok_or(ServiceError::NotInitialized)?;
        let media_offset = facts::MMC_FIRMWARE_IMAGE_OFFSET + offset;
        let read = fstart_capabilities::next_stage::read_stage_to_addr(
            mmc,
            facts::MMC0_NODE,
            "main",
            media_offset,
            facts::MAIN_LOAD_ADDR,
            size,
        )?;
        if read != size {
            fstart_log::error!("short read loading main: {} of {} bytes", read, size);
            return Err(ServiceError::IoError);
        }
        Ok(())
    }
}

impl HardwareInit for BootblockDevices {
    fn early_clocks(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mut ccu = SunxiH3Ccu::new(CCU_CONFIG.clone()).map_err(device_error_to_service_error)?;
        ccu.init().map_err(device_error_to_service_error)?;
        self.ccu = Some(ccu);
        Ok(())
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mut dramc =
            SunxiH3Dramc::new(DRAMC_CONFIG.clone()).map_err(device_error_to_service_error)?;
        dramc.init().map_err(device_error_to_service_error)?;
        self.dramc = Some(dramc);
        Ok(())
    }

    fn storage(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mmc = self.ensure_mmc0()?;
        mmc.init().map_err(device_error_to_service_error)
    }
}

pub struct BootblockBoard {
    devices: BootblockDevices,
}

impl StaticBoard for BootblockBoard {
    type Devices = BootblockDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: BootblockDevices::new(),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "ns16550");
        Ok(())
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.devices.load_main()
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "handoff")]
        {
            fstart_capabilities::next_stage::serialize_handoff(
                self.devices.dram_size(),
                facts::HANDOFF_ADDR,
            )
            .map_err(|_| ServiceError::HardwareError)?;
        }
        Ok(())
    }

    fn boot_payload(self) -> ! {
        fstart_log::info!(
            "jumping to main at {:#x} with handoff {:#x}",
            facts::MAIN_LOAD_ADDR,
            facts::HANDOFF_ADDR,
        );
        fstart_platform::jump_to_with_handoff(facts::MAIN_LOAD_ADDR, facts::HANDOFF_ADDR as usize)
    }
}
