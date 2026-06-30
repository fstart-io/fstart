//! Lenovo ThinkPad X61 bootblock fixed-flow adapter.

use fstart_board_lenovo_x61 as board;
use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::Ns16550;
use fstart_mainboard_lenovo_x61::LenovoX61Mainboard;
use fstart_platform_intel_gm965_ich8 as platform;
use fstart_services::{
    EarlyInit, HardwareInit, InitContext, Mainboard, MemoryController, PreConsoleInit, ServiceError,
};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedFfs, StaticConsole};
use fstart_stage_runtime::StaticBoard;

use crate::common;

pub struct BootblockDevices {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: LenovoX61Mainboard,
    console: StaticConsole<Ns16550>,
}

impl BootblockDevices {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: common::new_gm965()?,
            southbridge: common::new_ich8()?,
            mainboard: common::new_mainboard()?,
            console: StaticConsole::new(common::UART0_CONFIG.clone()),
        })
    }
}

impl HardwareInit for BootblockDevices {
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()?;
        self.mainboard
            .pre_console_init_with_southbridge(&mut self.southbridge)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.early_init()?;
        self.southbridge.early_init()
    }

    fn dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.dram_init()
    }
}

pub struct BootblockBoard {
    devices: BootblockDevices,
    ffs: MemoryMappedFfs,
    ramstage_loaded: bool,
}

impl StaticBoard for BootblockBoard {
    type Devices = BootblockDevices;

    fn new() -> Result<Self, ServiceError> {
        let runtime = common::runtime_config();
        Ok(Self {
            devices: BootblockDevices::new()?,
            ffs: MemoryMappedFfs::new(runtime.firmware_base, runtime.firmware_size),
            ramstage_loaded: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(board::UART0_NODE, "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_platform::enable_boot_media_rom_cache();
        self.ffs.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs
            .load_file_by_name(platform::GM965_NEXT_STAGE_NAME)?;
        self.ramstage_loaded = true;
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.ramstage_loaded {
            fstart_log::error!("ramstage handoff requested before load");
            Self::halt();
        }
        fstart_log::info!(
            "jumping to ramstage at {:#x}",
            platform::GM965_RAMSTAGE_LOAD_ADDR
        );
        fstart_platform::jump_to(platform::GM965_RAMSTAGE_LOAD_ADDR)
    }
}
