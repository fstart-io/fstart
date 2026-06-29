//! Static fixed-flow adapter for the QEMU RISC-V `virt` board.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::boot::BootLinuxParams;
use fstart_services::boot_media::{LinearMap, MemoryMapped};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;
use fstart_types::ffs::FileType;

const FLASH_BASE: u64 = 0x2000_0000;
const FLASH_SIZE: usize = 0x0200_0000;
const RAM_BASE: u64 = 0x8000_0000;
const RAM_SIZE: u64 = 0x0800_0000;
const KERNEL_LOAD_ADDR: u64 = 0x8200_0000;
const FIRMWARE_LOAD_ADDR: u64 = 0x8010_0000;
const FDT_ADDR: u64 = 0x87f0_0000;
const BOOTARGS: &str = "console=ttyS0 earlycon=sbi";

pub struct StageDevices {
    uart0: Option<Ns16550>,
}

impl StageDevices {
    fn new() -> Self {
        Self { uart0: None }
    }

    fn ensure_uart0(&mut self) -> Result<&'static Ns16550, ServiceError> {
        static UART0_CONFIG: Ns16550Config = Ns16550Config {
            regs: AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        };

        if self.uart0.is_none() {
            let mut uart = Ns16550::new(&UART0_CONFIG).map_err(device_error_to_service_error)?;
            uart.init().map_err(device_error_to_service_error)?;
            self.uart0 = Some(uart);
        }

        // SAFETY: fixed-flow stages never return from fstart_main. The console
        // object is stored inside the static board for the rest of execution.
        let uart = self.uart0.as_ref().expect("uart0 initialized");
        Ok(unsafe { &*(uart as *const Ns16550) })
    }
}

impl HardwareInit for StageDevices {
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let uart = self.ensure_uart0()?;
        // SAFETY: uart0 is stored in StageDevices and fstart_main never returns.
        unsafe { fstart_log::init(uart) };
        fstart_log::info!("fstart fixed-flow console ready");
        Ok(())
    }
}

pub struct StageBoard {
    devices: StageDevices,
    firmware_loaded: bool,
    kernel_loaded: bool,
    dtb_addr: u64,
}

impl StageBoard {
    fn boot_media() -> MemoryMapped<LinearMap> {
        // SAFETY: QEMU `virt` maps the firmware image at the board-declared ROM
        // window. The image remains readable for the whole boot flow.
        unsafe { MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE) }
    }
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(),
            firmware_loaded: false,
            kernel_loaded: false,
            dtb_addr: fstart_platform::boot_dtb_addr(),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "stage-flow-console-init")]
        {
            fstart_capabilities::console_ready("uart0", "ns16550");
        }
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_log::info!("mounting memory-mapped FFS at {:#x}", FLASH_BASE);
        fstart_services::ffs_context::set_memory_mapped(
            crate::fstart_anchor_bytes(),
            FLASH_BASE,
            FLASH_SIZE as u64,
        );
        Ok(())
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = Self::boot_media();
            fstart_capabilities::sig_verify(crate::fstart_anchor_bytes(), &media);
        }
        Ok(())
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = Self::boot_media();
            if !fstart_capabilities::load_ffs_file_by_type(
                crate::fstart_anchor_bytes(),
                &media,
                FileType::Firmware,
            ) {
                return Err(ServiceError::NotInitialized);
            }
            self.firmware_loaded = true;

            if !fstart_capabilities::load_ffs_file_by_type(
                crate::fstart_anchor_bytes(),
                &media,
                FileType::Payload,
            ) {
                return Err(ServiceError::NotInitialized);
            }
            self.kernel_loaded = true;
            return Ok(());
        }

        #[cfg(not(feature = "ffs"))]
        {
            Err(ServiceError::NotSupported)
        }
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "fdt")]
        {
            let src_dtb = self.dtb_addr;
            fstart_capabilities::fdt_prepare_platform(
                src_dtb, FDT_ADDR, BOOTARGS, RAM_BASE, RAM_SIZE,
            );
            self.dtb_addr = FDT_ADDR;
        }
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.firmware_loaded || !self.kernel_loaded {
            fstart_log::error!("payload handoff requested before firmware/kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting OpenSBI at {:#x}, kernel at {:#x}, dtb at {:#x}",
            FIRMWARE_LOAD_ADDR,
            KERNEL_LOAD_ADDR,
            self.dtb_addr,
        );
        let params = BootLinuxParams {
            kernel_addr: KERNEL_LOAD_ADDR,
            dtb_addr: self.dtb_addr,
            fw_addr: FIRMWARE_LOAD_ADDR,
            hart_id: fstart_platform::boot_hart_id(),
            rsdp_addr: 0,
            bootargs: BOOTARGS,
            e820_entries: &[],
            zero_page_addr: 0,
            print_x86_mtrrs: false,
        };
        fstart_platform::boot_linux(&params)
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
