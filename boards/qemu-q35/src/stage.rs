//! Static fixed-flow adapter for QEMU Q35.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::boot::BootLinuxParams;
use fstart_services::boot_media::{LinearMap, MemoryMapped};
use fstart_services::device::DeviceError;
use fstart_services::{Device, HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;
use fstart_types::ffs::FileType;

const FFS_BASE: u64 = 0xff90_0000;
const FFS_SIZE: usize = 0x006f_f000;
const KERNEL_LOAD_ADDR: u64 = 0x0100_0000;
const ZERO_PAGE_ADDR: u64 = 0x0009_0000;
const BOOTARGS: &str = "console=ttyS0 earlyprintk=serial,ttyS0,115200";

pub struct StageDevices {
    uart0: Option<Ns16550>,
}

impl StageDevices {
    fn new() -> Self {
        Self { uart0: None }
    }

    fn ensure_uart0(&mut self) -> Result<&mut Ns16550, ServiceError> {
        static UART0_CONFIG: Ns16550Config = Ns16550Config {
            regs: AccessMode::Pio { base: 0x3f8 },
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        };

        if self.uart0.is_none() {
            let uart = Ns16550::new(&UART0_CONFIG).map_err(device_error_to_service_error)?;
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
        let uart = unsafe { &*(uart as *const Ns16550) };
        unsafe { fstart_log::init(uart) };
        fstart_log::info!("fstart fixed-flow console ready");
        Ok(())
    }
}

pub struct StageBoard {
    devices: StageDevices,
    kernel_loaded: bool,
}

impl StageBoard {
    fn boot_media() -> MemoryMapped<LinearMap> {
        // SAFETY: QEMU Q35 maps the board-declared FFS firmware image window
        // inside pflash. The image remains readable for the whole boot flow.
        unsafe { MemoryMapped::from_raw_addr(FFS_BASE, FFS_SIZE) }
    }
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(),
            kernel_loaded: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_capabilities::console_ready("uart0", "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_log::info!("mounting memory-mapped FFS at {:#x}", FFS_BASE);
        fstart_services::ffs_context::set_memory_mapped(
            fstart_stage::fstart_anchor_bytes(),
            FFS_BASE,
            FFS_SIZE as u64,
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
            return Ok(());
        }

        #[cfg(not(feature = "ffs"))]
        {
            Err(ServiceError::NotSupported)
        }
    }

    fn boot_payload(self) -> ! {
        if !self.kernel_loaded {
            fstart_log::error!("payload handoff requested before kernel load");
            Self::halt();
        }

        fstart_log::info!("booting Linux bzImage at {:#x}", KERNEL_LOAD_ADDR);
        let params = BootLinuxParams {
            kernel_addr: KERNEL_LOAD_ADDR,
            dtb_addr: 0,
            fw_addr: 0,
            hart_id: 0,
            rsdp_addr: 0,
            bootargs: BOOTARGS,
            e820_entries: &[],
            zero_page_addr: ZERO_PAGE_ADDR,
            print_x86_mtrrs: false,
        };
        fstart_platform::boot_linux(&params)
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
