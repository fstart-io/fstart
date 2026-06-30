//! Reusable pieces for board-owned fixed-flow stage adapters.
//!
//! These helpers intentionally stop short of selecting a board or platform. Board
//! crates still own their concrete [`StaticBoard`](fstart_stage_runtime::StaticBoard)
//! type, platform boot call, and board constants. The helpers just remove the
//! repeated mechanics around lazy console construction, memory-mapped FFS access,
//! payload state tracking, and FDT patching.

use fstart_services::boot::BootLinuxParams;
use fstart_services::boot_media::{LinearMap, MemoryMapped};
use fstart_services::{Console, Device, DeviceError, HardwareInit, InitContext, ServiceError};
use fstart_types::ffs::FileType;

/// Lazily constructed boot console device for static fixed-flow boards.
///
/// The board supplies a concrete driver type and a static typed config. The
/// helper constructs the driver only when the `console` flow step runs, invokes
/// the driver's step-based console initialization, and installs it as the global
/// logger backend.
pub struct StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: 'static,
{
    device: Option<D>,
    config: &'static D::Config,
}

impl<D> StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: 'static,
{
    /// Construct a lazy console wrapper around a static driver config.
    #[must_use]
    pub const fn new(config: &'static D::Config) -> Self {
        Self {
            device: None,
            config,
        }
    }

    fn ensure(&mut self) -> Result<&mut D, ServiceError> {
        if self.device.is_none() {
            let device = D::new(self.config).map_err(device_error_to_service_error)?;
            self.device = Some(device);
        }
        Ok(self.device.as_mut().expect("console device constructed"))
    }
}

impl<D> HardwareInit for StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: 'static,
{
    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let console = self.ensure()?;
        console.console(ctx)?;
        // SAFETY: the console object is stored in the board's StaticBoard device
        // container, and fixed-flow stage entry never returns after construction.
        unsafe { fstart_log::init(console) };
        fstart_log::info!("fstart fixed-flow console ready");
        Ok(())
    }
}

/// Announce that the selected console is available.
pub fn console_ready(device_name: &str, driver_name: &str) {
    fstart_capabilities::console_ready(device_name, driver_name);
}

/// Memory-mapped firmware image window used as FFS boot media.
#[derive(Debug, Clone, Copy)]
pub struct MemoryMappedFfs {
    base: u64,
    size: usize,
}

impl MemoryMappedFfs {
    /// Construct a memory-mapped FFS descriptor.
    #[must_use]
    pub const fn new(base: u64, size: usize) -> Self {
        Self { base, size }
    }

    /// Return the CPU-visible base of the firmware image.
    #[must_use]
    pub const fn base(self) -> u64 {
        self.base
    }

    fn media(self) -> MemoryMapped<LinearMap> {
        // SAFETY: callers construct this descriptor from board-declared ROM/flash
        // windows that remain readable for the whole boot flow.
        unsafe { MemoryMapped::from_raw_addr(self.base, self.size) }
    }

    /// Publish this memory-mapped FFS window to stage-local services.
    pub fn mount(self) -> Result<(), ServiceError> {
        fstart_log::info!("mounting memory-mapped FFS at {:#x}", self.base);
        fstart_services::ffs_context::set_memory_mapped(
            crate::fstart_anchor_bytes(),
            self.base,
            self.size as u64,
        );
        Ok(())
    }

    /// Verify the FFS manifest/signature policy when the backend is enabled.
    pub fn verify(self) -> Result<(), ServiceError> {
        let media = self.media();
        fstart_capabilities::sig_verify(crate::fstart_anchor_bytes(), &media);
        Ok(())
    }

    /// Load one file by FFS type into its packaged load address.
    pub fn load_file(self, file_type: FileType) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = self.media();
            if fstart_capabilities::load_ffs_file_by_type(
                crate::fstart_anchor_bytes(),
                &media,
                file_type,
            ) {
                Ok(())
            } else {
                Err(ServiceError::NotInitialized)
            }
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = file_type;
            Err(ServiceError::NotSupported)
        }
    }
}

/// LinuxBoot payload state backed by a memory-mapped FFS image.
#[derive(Debug, Clone, Copy)]
pub struct MemoryMappedLinuxBoot {
    ffs: MemoryMappedFfs,
    firmware_loaded: bool,
    kernel_loaded: bool,
    dtb_addr: u64,
}

impl MemoryMappedLinuxBoot {
    /// Construct a LinuxBoot helper with the board's FFS window and initial DTB.
    #[must_use]
    pub const fn new(ffs_base: u64, ffs_size: usize, dtb_addr: u64) -> Self {
        Self {
            ffs: MemoryMappedFfs::new(ffs_base, ffs_size),
            firmware_loaded: false,
            kernel_loaded: false,
            dtb_addr,
        }
    }

    /// Current DTB address to pass to the payload.
    #[must_use]
    pub const fn dtb_addr(&self) -> u64 {
        self.dtb_addr
    }

    /// Mount the memory-mapped FFS window.
    pub fn mount(&self) -> Result<(), ServiceError> {
        self.ffs.mount()
    }

    /// Verify the FFS image policy.
    pub fn verify(&self) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    /// Load a Linux kernel payload from FFS.
    pub fn load_kernel(&mut self) -> Result<(), ServiceError> {
        self.ffs.load_file(FileType::Payload)?;
        self.kernel_loaded = true;
        Ok(())
    }

    /// Load firmware, such as OpenSBI or BL31, and then the Linux kernel.
    pub fn load_firmware_and_kernel(&mut self) -> Result<(), ServiceError> {
        self.ffs.load_file(FileType::Firmware)?;
        self.firmware_loaded = true;
        self.load_kernel()
    }

    /// Patch/copy the platform FDT for Linux handoff when the FDT backend is enabled.
    pub fn prepare_fdt(
        &mut self,
        dst_dtb_addr: u64,
        bootargs: &str,
        ram_base: u64,
        ram_size: u64,
    ) -> Result<(), ServiceError> {
        #[cfg(feature = "fdt")]
        {
            fstart_capabilities::fdt_prepare_platform(
                self.dtb_addr,
                dst_dtb_addr,
                bootargs,
                ram_base,
                ram_size,
            );
            self.dtb_addr = dst_dtb_addr;
        }

        #[cfg(not(feature = "fdt"))]
        {
            let _ = (dst_dtb_addr, bootargs, ram_base, ram_size);
        }

        Ok(())
    }

    /// Return whether the kernel payload has been loaded.
    #[must_use]
    pub const fn kernel_loaded(&self) -> bool {
        self.kernel_loaded
    }

    /// Return whether the firmware payload has been loaded.
    #[must_use]
    pub const fn firmware_loaded(&self) -> bool {
        self.firmware_loaded
    }

    /// Build common Linux boot params from board/platform constants.
    #[must_use]
    pub const fn boot_params<'a>(
        &self,
        kernel_addr: u64,
        firmware_addr: u64,
        hart_id: u64,
        bootargs: &'a str,
    ) -> BootLinuxParams<'a> {
        BootLinuxParams {
            kernel_addr,
            dtb_addr: self.dtb_addr,
            fw_addr: firmware_addr,
            rsdp_addr: 0,
            bootargs,
            e820_entries: &[],
            zero_page_addr: 0,
            hart_id,
            print_x86_mtrrs: false,
        }
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
