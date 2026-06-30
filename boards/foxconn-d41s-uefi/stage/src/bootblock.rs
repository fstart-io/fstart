//! Foxconn D41S UEFI bootblock fixed-flow adapter.

use fstart_board_foxconn_d41s_facts as facts;
use fstart_driver_intel_ich7::IntelIch7;
use fstart_driver_intel_pineview::IntelPineview;
use fstart_driver_ite8721f::Ite8721f;
use fstart_services::{
    EarlyInit, HardwareInit, InitContext, MemoryController, PreConsoleInit, ServiceError,
};
use fstart_stage::fixed_helpers::MemoryMappedFfs;
use fstart_stage_runtime::StaticBoard;

use crate::common;

const NEXT_STAGE_NAME: &str = facts::NEXT_STAGE_NAME;
const MAIN_LOAD_ADDR: u64 = facts::RAMSTAGE_LOAD_ADDR;

pub struct BootblockDevices {
    northbridge: IntelPineview,
    southbridge: IntelIch7,
    superio: Option<Ite8721f>,
}

impl BootblockDevices {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: common::new_pineview()?,
            southbridge: common::new_ich7()?,
            superio: None,
        })
    }
}

impl HardwareInit for BootblockDevices {
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()
    }

    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let superio = common::init_superio(common::superio_config(), &mut self.southbridge)?;
        self.superio = Some(superio);
        let console = self.superio.as_ref().expect("superio console initialized");
        common::install_superio_console(console)
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
    main_loaded: bool,
}

impl StaticBoard for BootblockBoard {
    type Devices = BootblockDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: BootblockDevices::new()?,
            ffs: MemoryMappedFfs::new(facts::FLASH_FFS_BASE, facts::FLASH_FFS_SIZE),
            main_loaded: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_platform::enable_boot_media_rom_cache();
        self.ffs.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.load_file_by_name(NEXT_STAGE_NAME)?;
        self.main_loaded = true;
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.main_loaded {
            fstart_log::error!("main-stage handoff requested before load");
            Self::halt();
        }
        fstart_log::info!("jumping to main at {:#x}", MAIN_LOAD_ADDR);
        fstart_platform::jump_to(MAIN_LOAD_ADDR)
    }
}
