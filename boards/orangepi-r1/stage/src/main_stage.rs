//! Main-stage fixed-flow adapter for Orange Pi R1.

use core::sync::atomic::{AtomicUsize, Ordering};

use fstart_board_orangepi_r1_facts as facts;
use fstart_driver_ns16550::Ns16550;
use fstart_driver_sunxi_mmc::SunxiMmc;
use fstart_services::boot_media::BlockDeviceMedia;
use fstart_services::{Device, HardwareInit, InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, StaticConsole};
use fstart_stage_runtime::StaticBoard;
use fstart_types::ffs::{FileType, ANCHOR_SIZE};

use crate::common::{device_error_to_service_error, MMC0_CONFIG, UART0_CONFIG};

static HANDOFF_PTR: AtomicUsize = AtomicUsize::new(0);

pub fn set_handoff_ptr(ptr: usize) {
    HANDOFF_PTR.store(ptr, Ordering::Relaxed);
}

pub struct MainDevices {
    console: StaticConsole<Ns16550>,
    mmc0: Option<SunxiMmc>,
}

impl MainDevices {
    pub const fn new() -> Self {
        Self {
            console: StaticConsole::new(&UART0_CONFIG),
            mmc0: None,
        }
    }

    fn ensure_mmc0(&mut self) -> Result<&mut SunxiMmc, ServiceError> {
        if self.mmc0.is_none() {
            let mmc = SunxiMmc::new(&MMC0_CONFIG).map_err(device_error_to_service_error)?;
            self.mmc0 = Some(mmc);
        }
        Ok(self.mmc0.as_mut().expect("MMC0 constructed"))
    }

    fn mmc0(&self) -> Result<&SunxiMmc, ServiceError> {
        self.mmc0.as_ref().ok_or(ServiceError::NotInitialized)
    }
}

impl HardwareInit for MainDevices {
    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn storage(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mmc = self.ensure_mmc0()?;
        mmc.init().map_err(device_error_to_service_error)
    }
}

struct BlockLinuxBoot {
    anchor: Option<[u8; ANCHOR_SIZE]>,
    ffs_size: usize,
    kernel_loaded: bool,
    dtb_loaded: bool,
}

impl BlockLinuxBoot {
    const fn new() -> Self {
        Self {
            anchor: None,
            ffs_size: 0,
            kernel_loaded: false,
            dtb_loaded: false,
        }
    }

    fn media<'a>(&self, mmc: &'a SunxiMmc) -> Result<BlockDeviceMedia<'a, SunxiMmc>, ServiceError> {
        if self.ffs_size == 0 {
            return Err(ServiceError::NotInitialized);
        }
        Ok(BlockDeviceMedia::new(
            mmc,
            facts::MMC_FIRMWARE_IMAGE_OFFSET,
            self.ffs_size,
        ))
    }

    fn mount(&mut self, mmc: &SunxiMmc) -> Result<(), ServiceError> {
        let ffs_size = fstart_soc_sunxi::ffs_total_size() as usize;
        if ffs_size < ANCHOR_SIZE {
            fstart_log::error!("invalid eGON FFS size: {:#x}", ffs_size);
            return Err(ServiceError::InvalidParam);
        }

        self.ffs_size = ffs_size;
        let media = self.media(mmc)?;
        let anchor_offset = ffs_size - ANCHOR_SIZE;
        let anchor = fstart_capabilities::read_anchor_at_offset(&media, anchor_offset)
            .map_err(|_| ServiceError::NotInitialized)?;
        fstart_log::info!(
            "mounted MMC FFS: media_offset={:#x}, size={:#x}",
            facts::MMC_FIRMWARE_IMAGE_OFFSET,
            ffs_size,
        );
        self.anchor = Some(anchor);
        Ok(())
    }

    fn verify(&self, mmc: &SunxiMmc) -> Result<(), ServiceError> {
        let anchor = self.anchor.as_ref().ok_or(ServiceError::NotInitialized)?;
        let media = self.media(mmc)?;
        fstart_capabilities::sig_verify(anchor, &media);
        Ok(())
    }

    fn load_file(&self, mmc: &SunxiMmc, file_type: FileType) -> Result<(), ServiceError> {
        let anchor = self.anchor.as_ref().ok_or(ServiceError::NotInitialized)?;
        let media = self.media(mmc)?;
        if fstart_capabilities::load_ffs_file_by_type(anchor, &media, file_type) {
            Ok(())
        } else {
            Err(ServiceError::NotInitialized)
        }
    }

    fn load_linux(&mut self, mmc: &SunxiMmc) -> Result<(), ServiceError> {
        self.load_file(mmc, FileType::Fdt)?;
        self.dtb_loaded = true;
        self.load_file(mmc, FileType::Payload)?;
        self.kernel_loaded = true;
        Ok(())
    }

    fn prepare_fdt(&self) -> Result<(), ServiceError> {
        #[cfg(feature = "fdt")]
        fstart_capabilities::fdt_prepare_platform(
            facts::FDT_ADDR,
            facts::FDT_ADDR,
            facts::BOOTARGS,
            facts::RAM_BASE,
            facts::RAM_SIZE,
        );
        Ok(())
    }
}

pub struct MainBoard {
    devices: MainDevices,
    boot: BlockLinuxBoot,
    dram_size: u64,
}

impl StaticBoard for MainBoard {
    type Devices = MainDevices;

    fn new() -> Result<Self, ServiceError> {
        #[cfg(feature = "handoff")]
        let dram_size = {
            let ptr = HANDOFF_PTR.load(Ordering::Relaxed);
            fstart_capabilities::handoff::try_deserialize(ptr)
                .map_or(facts::RAM_SIZE, |handoff| handoff.dram_size)
        };
        #[cfg(not(feature = "handoff"))]
        let dram_size = facts::RAM_SIZE;

        Ok(Self {
            devices: MainDevices::new(),
            boot: BlockLinuxBoot::new(),
            dram_size,
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

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.mount(self.devices.mmc0()?)
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify(self.devices.mmc0()?)
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.load_linux(self.devices.mmc0()?)
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.prepare_fdt()
    }

    fn boot_payload(self) -> ! {
        if !self.boot.kernel_loaded || !self.boot.dtb_loaded {
            fstart_log::error!("Linux handoff requested before kernel/DTB load");
            Self::halt();
        }

        fstart_log::info!(
            "booting Linux kernel at {:#x}, dtb at {:#x}, dram={} MiB",
            facts::KERNEL_LOAD_ADDR,
            facts::FDT_ADDR,
            (self.dram_size >> 20) as u32,
        );
        let params = fstart_services::boot::BootLinuxParams {
            kernel_addr: facts::KERNEL_LOAD_ADDR,
            dtb_addr: facts::FDT_ADDR,
            fw_addr: 0,
            rsdp_addr: 0,
            bootargs: facts::BOOTARGS,
            e820_entries: &[],
            zero_page_addr: 0,
            hart_id: 0,
            print_x86_mtrrs: false,
        };
        fstart_platform::boot_linux(&params)
    }
}
