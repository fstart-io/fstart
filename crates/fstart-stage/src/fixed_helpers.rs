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
/// The board supplies a concrete driver type and a typed config value. The
/// helper constructs the driver only when the `console` flow step runs, invokes
/// the driver's step-based console initialization, and installs it as the global
/// logger backend.
pub struct StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: Clone,
{
    device: Option<D>,
    config: D::Config,
}

impl<D> StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: Clone,
{
    /// Construct a lazy console wrapper around a driver config value.
    #[must_use]
    pub fn new(config: D::Config) -> Self {
        Self {
            device: None,
            config,
        }
    }

    fn ensure(&mut self) -> Result<&mut D, ServiceError> {
        if self.device.is_none() {
            let device = D::new(self.config.clone()).map_err(device_error_to_service_error)?;
            self.device = Some(device);
        }
        Ok(self.device.as_mut().expect("console device constructed"))
    }

    /// Return the constructed console device, if the console step already ran.
    #[must_use]
    pub fn device(&self) -> Option<&D> {
        self.device.as_ref()
    }
}

impl<D> HardwareInit for StaticConsole<D>
where
    D: Device + Console + HardwareInit,
    D::Config: Clone,
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

    /// Load one file by FFS manifest name into its packaged load address.
    pub fn load_file_by_name(self, name: &str) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = self.media();
            if fstart_capabilities::load_ffs_file_by_name(
                crate::fstart_anchor_bytes(),
                &media,
                name,
            ) {
                Ok(())
            } else {
                Err(ServiceError::NotInitialized)
            }
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = name;
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

/// UEFI payload state backed by a memory-mapped FFS image.
#[derive(Debug, Clone, Copy)]
pub struct MemoryMappedUefiBoot {
    ffs: MemoryMappedFfs,
    firmware_loaded: bool,
    fdt_addr: u64,
}

impl MemoryMappedUefiBoot {
    /// Construct a UEFI helper with the board's FFS window and platform DTB.
    #[must_use]
    pub const fn new(ffs_base: u64, ffs_size: usize, fdt_addr: u64) -> Self {
        Self {
            ffs: MemoryMappedFfs::new(ffs_base, ffs_size),
            firmware_loaded: false,
            fdt_addr,
        }
    }

    /// Current platform DTB address to expose to UEFI.
    #[must_use]
    pub const fn fdt_addr(&self) -> u64 {
        self.fdt_addr
    }

    /// Mount the memory-mapped FFS window.
    pub fn mount(&self) -> Result<(), ServiceError> {
        self.ffs.mount()
    }

    /// Verify the FFS image policy.
    pub fn verify(&self) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    /// Load board firmware, such as TF-A BL31, from FFS.
    pub fn load_firmware(&mut self) -> Result<(), ServiceError> {
        self.ffs.load_file(FileType::Firmware)?;
        self.firmware_loaded = true;
        Ok(())
    }

    /// Return whether the firmware blob has been loaded.
    #[must_use]
    pub const fn firmware_loaded(&self) -> bool {
        self.firmware_loaded
    }

    /// Borrow the platform FDT as bytes when the bootloader supplied a valid FDT.
    ///
    /// # Safety
    ///
    /// `self.fdt_addr()` must either be zero or point to a readable flattened
    /// device tree that remains valid for the rest of the boot flow.
    #[must_use]
    pub unsafe fn fdt_bytes(&self) -> Option<&'static [u8]> {
        let size = unsafe { fdt_total_size(self.fdt_addr)? };
        // SAFETY: caller guarantees the address is readable for `size` bytes.
        Some(unsafe { core::slice::from_raw_parts(self.fdt_addr as *const u8, size as usize) })
    }

    /// Return a page-aligned FDT reservation for UEFI memory-map construction.
    ///
    /// # Safety
    ///
    /// `self.fdt_addr()` must either be zero or point to a readable flattened
    /// device tree header.
    #[must_use]
    pub unsafe fn fdt_reservation(&self) -> Option<(u64, u64)> {
        let size = unsafe { fdt_total_size(self.fdt_addr)? };
        Some((self.fdt_addr, (size + 0xfff) & !0xfff))
    }
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

    /// Load an override FDT from FFS and make it the current DTB.
    pub fn load_fdt(&mut self, dtb_addr: u64) -> Result<(), ServiceError> {
        self.ffs.load_file(FileType::Fdt)?;
        self.dtb_addr = dtb_addr;
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

    /// Build x86 Linux bzImage boot params, including the zero-page address.
    #[must_use]
    pub const fn x86_boot_params<'a>(
        &self,
        kernel_addr: u64,
        zero_page_addr: u64,
        bootargs: &'a str,
    ) -> BootLinuxParams<'a> {
        BootLinuxParams {
            kernel_addr,
            dtb_addr: 0,
            fw_addr: 0,
            rsdp_addr: 0,
            bootargs,
            e820_entries: &[],
            zero_page_addr,
            hart_id: 0,
            print_x86_mtrrs: false,
        }
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

unsafe fn fdt_total_size(fdt_addr: u64) -> Option<u64> {
    const FDT_MAGIC: u32 = 0xd00d_feed;

    if fdt_addr == 0 {
        return None;
    }

    let ptr = fdt_addr as *const u8;
    // SAFETY: caller guarantees that at least the FDT header is readable.
    let magic = unsafe { u32::from_be(core::ptr::read_unaligned(ptr.cast::<u32>())) };
    if magic != FDT_MAGIC {
        return None;
    }

    // SAFETY: caller guarantees that at least the FDT header is readable.
    let total = unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(4).cast::<u32>())) };
    Some(u64::from(total))
}
