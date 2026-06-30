//! Static fixed-flow adapter for QEMU ARMv7 `virt`.

use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::boot::BootLinuxParams;
use fstart_services::boot_media::{LinearMap, MemoryMapped};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;
use fstart_types::ffs::FileType;

const FLASH_BASE: u64 = 0x0000_0000;
const FLASH_SIZE: usize = 0x0800_0000;
const RAM_BASE: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 0x0800_0000;
const KERNEL_LOAD_ADDR: u64 = 0x4100_0000;
const SRC_FDT_ADDR: u64 = 0x4000_0000;
const FDT_ADDR: u64 = 0x40f0_0000;
const BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000";

pub struct StageDevices {
    uart0: Option<Pl011>,
}

impl StageDevices {
    fn new() -> Self {
        Self { uart0: None }
    }

    fn ensure_uart0(&mut self) -> Result<&mut Pl011, ServiceError> {
        static UART0_CONFIG: Pl011Config = Pl011Config {
            base_addr: 0x0900_0000,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
            acpi_name: None,
            acpi_gsiv: None,
            acpi_dbg2: false,
        };

        if self.uart0.is_none() {
            let uart = Pl011::new(&UART0_CONFIG).map_err(device_error_to_service_error)?;
            self.uart0 = Some(uart);
        }

        Ok(self.uart0.as_mut().expect("uart0 constructed"))
    }
}

impl HardwareInit for StageDevices {
    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let uart = self.ensure_uart0()?;
        uart.console(ctx)?;
        // SAFETY: uart0 is stored in StageDevices and fstart_main never returns.
        let uart = unsafe { &*(uart as *const Pl011) };
        unsafe { fstart_log::init(uart) };
        fstart_log::info!("fstart fixed-flow console ready");
        Ok(())
    }
}

pub struct StageBoard {
    devices: StageDevices,
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
            kernel_loaded: false,
            dtb_addr: SRC_FDT_ADDR,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_capabilities::console_ready("uart0", "pl011");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_log::info!("mounting memory-mapped FFS at {:#x}", FLASH_BASE);
        fstart_services::ffs_context::set_memory_mapped(
            fstart_stage::fstart_anchor_bytes(),
            FLASH_BASE,
            FLASH_SIZE as u64,
        );
        Ok(())
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = Self::boot_media();
            fstart_capabilities::sig_verify(fstart_stage::fstart_anchor_bytes(), &media);
        }
        Ok(())
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = Self::boot_media();
            if !fstart_capabilities::load_ffs_file_by_type(
                fstart_stage::fstart_anchor_bytes(),
                &media,
                FileType::Payload,
            ) {
                return Err(ServiceError::NotInitialized);
            }
            self.kernel_loaded = true;
            Ok(())
        }

        #[cfg(not(feature = "ffs"))]
        {
            Err(ServiceError::NotSupported)
        }
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "fdt")]
        {
            fstart_capabilities::fdt_prepare_platform(
                self.dtb_addr,
                FDT_ADDR,
                BOOTARGS,
                RAM_BASE,
                RAM_SIZE,
            );
            self.dtb_addr = FDT_ADDR;
        }
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.kernel_loaded {
            fstart_log::error!("payload handoff requested before kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting ARM Linux at {:#x}, dtb at {:#x}",
            KERNEL_LOAD_ADDR,
            self.dtb_addr,
        );
        let params = BootLinuxParams {
            kernel_addr: KERNEL_LOAD_ADDR,
            dtb_addr: self.dtb_addr,
            fw_addr: 0,
            hart_id: 0,
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
