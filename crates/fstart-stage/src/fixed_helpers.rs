//! Reusable pieces for board-owned fixed-flow stage adapters.
//!
//! These helpers intentionally stop short of selecting a board or platform. Board
//! crates still own their concrete [`StaticBoard`](fstart_stage_runtime::StaticBoard)
//! type, platform boot call, and board constants. The helpers just remove the
//! repeated mechanics around lazy console construction, memory-mapped FFS access,
//! payload state tracking, and FDT patching.

use core::marker::PhantomData;
#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
use core::sync::atomic::{AtomicUsize, Ordering};

use fstart_services::boot::BootLinuxParams;
use fstart_services::boot_media::{BlockDeviceMedia, LinearMap, MemoryMapped};
use fstart_services::{
    BlockDevice, Console, Device, DeviceError, HardwareInit, InitContext, ServiceError,
};
use fstart_types::ffs::{FileType, ANCHOR_SIZE};

/// Lazily constructed device whose legacy [`Device::init`] runs in a fixed-flow step.
///
/// This is the bridge for drivers that have not yet grown step-specific
/// [`HardwareInit`] implementations. Board adapters declare the desired flow slot in
/// the type instead of hand-writing `Option<D>`, `ensure_*`, and a one-method
/// `HardwareInit` impl for every device.
pub struct StaticDevice<D, Step>
where
    D: Device,
{
    state: StaticDeviceState<D>,
    _step: PhantomData<fn() -> Step>,
}

enum StaticDeviceState<D>
where
    D: Device,
{
    Pending(D::Config),
    Ready(D),
    Failed,
}

impl<D, Step> StaticDevice<D, Step>
where
    D: Device,
{
    /// Construct a lazy device wrapper around a driver config value.
    #[must_use]
    pub const fn new(config: D::Config) -> Self {
        Self {
            state: StaticDeviceState::Pending(config),
            _step: PhantomData,
        }
    }

    fn initialize(&mut self) -> Result<&mut D, ServiceError> {
        match self.state {
            StaticDeviceState::Ready(ref mut device) => return Ok(device),
            StaticDeviceState::Pending(_) => {}
            StaticDeviceState::Failed => return Err(ServiceError::NotInitialized),
        }

        let StaticDeviceState::Pending(config) =
            core::mem::replace(&mut self.state, StaticDeviceState::Failed)
        else {
            return Err(ServiceError::NotInitialized);
        };

        let mut device = D::new(config).map_err(device_error_to_service_error)?;
        device.init().map_err(device_error_to_service_error)?;
        self.state = StaticDeviceState::Ready(device);

        match &mut self.state {
            StaticDeviceState::Ready(device) => Ok(device),
            StaticDeviceState::Pending(_) | StaticDeviceState::Failed => unreachable!(),
        }
    }

    /// Return the constructed device if its flow step has already run.
    #[must_use]
    pub fn device(&self) -> Option<&D> {
        match &self.state {
            StaticDeviceState::Ready(device) => Some(device),
            StaticDeviceState::Pending(_) | StaticDeviceState::Failed => None,
        }
    }

    /// Return the constructed device mutably if its flow step has already run.
    pub fn device_mut(&mut self) -> Option<&mut D> {
        match &mut self.state {
            StaticDeviceState::Ready(device) => Some(device),
            StaticDeviceState::Pending(_) | StaticDeviceState::Failed => None,
        }
    }
}

/// Marker for a [`StaticDevice`] initialized in `very_early`.
pub enum VeryEarlyStep {}
/// Marker for a [`StaticDevice`] initialized in `early_clocks`.
pub enum EarlyClocksStep {}
/// Marker for a [`StaticDevice`] initialized in `pinmux`.
pub enum PinmuxStep {}
/// Marker for a [`StaticDevice`] initialized in `pre_console`.
pub enum PreConsoleStep {}
/// Marker for a [`StaticDevice`] initialized in `post_console`.
pub enum PostConsoleStep {}
/// Marker for a [`StaticDevice`] initialized in `memory_discovery`.
pub enum MemoryDiscoveryStep {}
/// Marker for a [`StaticDevice`] initialized in `dram`.
pub enum DramStep {}
/// Marker for a [`StaticDevice`] initialized in `post_dram`.
pub enum PostDramStep {}
/// Marker for a [`StaticDevice`] initialized in `bus_early`.
pub enum BusEarlyStep {}
/// Marker for a [`StaticDevice`] initialized in `bus_probe`.
pub enum BusProbeStep {}
/// Marker for a [`StaticDevice`] initialized in `drivers_ready`.
pub enum DriversReadyStep {}
/// Marker for a [`StaticDevice`] initialized in `storage`.
pub enum StorageStep {}
/// Marker for a [`StaticDevice`] initialized in `security`.
pub enum SecurityStep {}
/// Marker for a [`StaticDevice`] initialized in `payload_load`.
pub enum PayloadLoadStep {}
/// Marker for a [`StaticDevice`] initialized in `handoff`.
pub enum HandoffStep {}

pub type VeryEarlyDevice<D> = StaticDevice<D, VeryEarlyStep>;
pub type EarlyClocksDevice<D> = StaticDevice<D, EarlyClocksStep>;
pub type PinmuxDevice<D> = StaticDevice<D, PinmuxStep>;
pub type PreConsoleDevice<D> = StaticDevice<D, PreConsoleStep>;
pub type PostConsoleDevice<D> = StaticDevice<D, PostConsoleStep>;
pub type MemoryDiscoveryDevice<D> = StaticDevice<D, MemoryDiscoveryStep>;
pub type DramDevice<D> = StaticDevice<D, DramStep>;
pub type PostDramDevice<D> = StaticDevice<D, PostDramStep>;
pub type BusEarlyDevice<D> = StaticDevice<D, BusEarlyStep>;
pub type BusProbeDevice<D> = StaticDevice<D, BusProbeStep>;
pub type DriversReadyDevice<D> = StaticDevice<D, DriversReadyStep>;
pub type StorageDevice<D> = StaticDevice<D, StorageStep>;
pub type SecurityDevice<D> = StaticDevice<D, SecurityStep>;
pub type PayloadLoadDevice<D> = StaticDevice<D, PayloadLoadStep>;
pub type HandoffDevice<D> = StaticDevice<D, HandoffStep>;

macro_rules! impl_static_device_step {
    ($step:ty, $method:ident) => {
        impl<D> HardwareInit for StaticDevice<D, $step>
        where
            D: Device,
        {
            fn $method(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                self.initialize()?;
                Ok(())
            }
        }
    };
}

impl_static_device_step!(VeryEarlyStep, very_early);
impl_static_device_step!(EarlyClocksStep, early_clocks);
impl_static_device_step!(PinmuxStep, pinmux);
impl_static_device_step!(PreConsoleStep, pre_console);
impl_static_device_step!(PostConsoleStep, post_console);
impl_static_device_step!(MemoryDiscoveryStep, memory_discovery);
impl_static_device_step!(DramStep, dram);
impl_static_device_step!(PostDramStep, post_dram);
impl_static_device_step!(BusEarlyStep, bus_early);
impl_static_device_step!(BusProbeStep, bus_probe);
impl_static_device_step!(DriversReadyStep, drivers_ready);
impl_static_device_step!(StorageStep, storage);
impl_static_device_step!(SecurityStep, security);
impl_static_device_step!(PayloadLoadStep, payload_load);
impl_static_device_step!(HandoffStep, handoff);

/// Lazily constructed boot console device for static fixed-flow boards.
///
/// The board supplies a concrete driver type and a typed config value. The
/// helper constructs the driver only when the `console` flow step runs, invokes
/// the driver's step-based console initialization, and installs it as the global
/// logger backend.
pub struct StaticConsole<D>
where
    D: Device + Console + HardwareInit,
{
    state: StaticConsoleState<D>,
}

enum StaticConsoleState<D>
where
    D: Device + Console + HardwareInit,
{
    Pending(D::Config),
    Ready(D),
    Failed,
}

impl<D> StaticConsole<D>
where
    D: Device + Console + HardwareInit,
{
    /// Construct a lazy console wrapper around a driver config value.
    #[must_use]
    pub fn new(config: D::Config) -> Self {
        Self {
            state: StaticConsoleState::Pending(config),
        }
    }

    fn ensure(&mut self) -> Result<&mut D, ServiceError> {
        match self.state {
            StaticConsoleState::Ready(ref mut device) => return Ok(device),
            StaticConsoleState::Pending(_) => {}
            StaticConsoleState::Failed => return Err(ServiceError::NotInitialized),
        }

        let StaticConsoleState::Pending(config) =
            core::mem::replace(&mut self.state, StaticConsoleState::Failed)
        else {
            return Err(ServiceError::NotInitialized);
        };

        let device = D::new(config).map_err(device_error_to_service_error)?;
        self.state = StaticConsoleState::Ready(device);

        match &mut self.state {
            StaticConsoleState::Ready(device) => Ok(device),
            StaticConsoleState::Pending(_) | StaticConsoleState::Failed => unreachable!(),
        }
    }

    /// Return the constructed console device, if the console step already ran.
    #[must_use]
    pub fn device(&self) -> Option<&D> {
        match &self.state {
            StaticConsoleState::Ready(device) => Some(device),
            StaticConsoleState::Pending(_) | StaticConsoleState::Failed => None,
        }
    }
}

impl<D> HardwareInit for StaticConsole<D>
where
    D: Device + Console + HardwareInit,
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

/// Firmware filesystem stored behind a block device such as MMC or SPI flash.
pub struct BlockDeviceFfs {
    media_offset: u64,
    anchor: Option<[u8; ANCHOR_SIZE]>,
    ffs_size: usize,
}

impl BlockDeviceFfs {
    /// Construct a block-backed FFS helper.
    #[must_use]
    pub const fn new(media_offset: u64) -> Self {
        Self {
            media_offset,
            anchor: None,
            ffs_size: 0,
        }
    }

    fn media<'a, B>(&self, block: &'a B) -> Result<BlockDeviceMedia<'a, B>, ServiceError>
    where
        B: BlockDevice,
    {
        if self.ffs_size == 0 {
            return Err(ServiceError::NotInitialized);
        }
        Ok(BlockDeviceMedia::new(
            block,
            self.media_offset,
            self.ffs_size,
        ))
    }

    /// Mount the FFS image and cache its anchor.
    pub fn mount<B>(&mut self, block: &B, ffs_size: usize) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        #[cfg(feature = "ffs")]
        {
            if ffs_size < ANCHOR_SIZE {
                fstart_log::error!("invalid block FFS size: {:#x}", ffs_size);
                return Err(ServiceError::InvalidParam);
            }

            self.ffs_size = ffs_size;
            let media = self.media(block)?;
            let anchor_offset = ffs_size - ANCHOR_SIZE;
            let anchor = fstart_capabilities::read_anchor_at_offset(&media, anchor_offset)
                .map_err(|_| ServiceError::NotInitialized)?;
            fstart_log::info!(
                "mounted block FFS: media_offset={:#x}, size={:#x}",
                self.media_offset,
                ffs_size,
            );
            self.anchor = Some(anchor);
            Ok(())
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = (block, ffs_size);
            Err(ServiceError::NotSupported)
        }
    }

    /// Verify the mounted FFS image policy.
    pub fn verify<B>(&self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        let anchor = self.anchor.as_ref().ok_or(ServiceError::NotInitialized)?;
        let media = self.media(block)?;
        fstart_capabilities::sig_verify(anchor, &media);
        Ok(())
    }

    /// Load one file by FFS type into its packaged load address.
    pub fn load_file<B>(&self, block: &B, file_type: FileType) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        #[cfg(feature = "ffs")]
        {
            let anchor = self.anchor.as_ref().ok_or(ServiceError::NotInitialized)?;
            let media = self.media(block)?;
            if fstart_capabilities::load_ffs_file_by_type(anchor, &media, file_type) {
                Ok(())
            } else {
                Err(ServiceError::NotInitialized)
            }
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = (block, file_type);
            Err(ServiceError::NotSupported)
        }
    }
}

/// LinuxBoot payload state backed by a block-device FFS image.
pub struct BlockDeviceLinuxBoot {
    ffs: BlockDeviceFfs,
    firmware_loaded: bool,
    kernel_loaded: bool,
    dtb_addr: u64,
}

impl BlockDeviceLinuxBoot {
    /// Construct a LinuxBoot helper with a block-device FFS window.
    #[must_use]
    pub const fn new(media_offset: u64, dtb_addr: u64) -> Self {
        Self {
            ffs: BlockDeviceFfs::new(media_offset),
            firmware_loaded: false,
            kernel_loaded: false,
            dtb_addr,
        }
    }

    /// Mount the FFS image and cache its anchor.
    pub fn mount<B>(&mut self, block: &B, ffs_size: usize) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.mount(block, ffs_size)
    }

    /// Verify the mounted FFS image policy.
    pub fn verify<B>(&self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.verify(block)
    }

    /// Load firmware, such as OpenSBI or BL31, from FFS.
    pub fn load_firmware<B>(&mut self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.load_file(block, FileType::Firmware)?;
        self.firmware_loaded = true;
        Ok(())
    }

    /// Load an override FDT from FFS and make it the current DTB.
    pub fn load_fdt<B>(&mut self, block: &B, dtb_addr: u64) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.load_file(block, FileType::Fdt)?;
        self.dtb_addr = dtb_addr;
        Ok(())
    }

    /// Load a Linux kernel payload from FFS.
    pub fn load_kernel<B>(&mut self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.load_file(block, FileType::Payload)?;
        self.kernel_loaded = true;
        Ok(())
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

#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
static SUNXI_HANDOFF_PTR: AtomicUsize = AtomicUsize::new(0);

/// Store the inter-stage handoff pointer for generic Sunxi main-stage recipes.
#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
pub fn set_sunxi_handoff_ptr(ptr: usize) {
    SUNXI_HANDOFF_PTR.store(ptr, Ordering::Relaxed);
}

/// Board policy consumed by the generic Sunxi bootblock recipe.
#[cfg(all(feature = "sunxi", feature = "ns16550"))]
pub trait SunxiFixedFlowBoard: Sized {
    type Ccu: Device;
    type Dramc: Device + fstart_services::MemoryController;
    type Mmc: Device + BlockDevice;

    const UART0_NODE: &'static str;
    const MMC0_NODE: &'static str;
    const RAM_BASE: u64;
    const RAM_SIZE: u64;
    const MMC_FIRMWARE_IMAGE_OFFSET: u64;
    const MAIN_LOAD_ADDR: u64;
    const HANDOFF_ADDR: u64;

    fn ccu_config() -> <Self::Ccu as Device>::Config;
    fn dramc_config() -> <Self::Dramc as Device>::Config;
    fn mmc_config() -> <Self::Mmc as Device>::Config;
    fn uart0_config() -> fstart_driver_ns16550::Ns16550Config;

    fn halt() -> !;
    fn jump_to_main(main_addr: u64, handoff_addr: usize) -> !;
}

/// Additional board policy consumed only by the generic Sunxi Linux main stage.
#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
pub trait SunxiLinuxBoard: SunxiFixedFlowBoard {
    const FDT_ADDR: u64;
    const KERNEL_LOAD_ADDR: u64;
    const FIRMWARE_LOAD_ADDR: u64;
    const BOOTARGS: &'static str;
    const LOAD_FIRMWARE: bool;
    const FIRMWARE_NAME: &'static str;
    const FDT_RAM_FROM_HANDOFF: bool;

    fn boot_linux(params: &BootLinuxParams<'_>) -> !;
}

#[cfg(all(feature = "sunxi", feature = "ns16550"))]
type SunxiBootblockDevices<B> = (
    EarlyClocksDevice<<B as SunxiFixedFlowBoard>::Ccu>,
    StaticConsole<fstart_driver_ns16550::Ns16550>,
    DramDevice<<B as SunxiFixedFlowBoard>::Dramc>,
    StorageDevice<<B as SunxiFixedFlowBoard>::Mmc>,
);

/// Generic fixed-flow bootblock for Sunxi boards that load a main stage from MMC.
#[cfg(all(feature = "sunxi", feature = "ns16550"))]
pub struct SunxiBootblockBoard<B: SunxiFixedFlowBoard> {
    devices: SunxiBootblockDevices<B>,
    _board: PhantomData<B>,
}

#[cfg(all(feature = "sunxi", feature = "ns16550"))]
impl<B> SunxiBootblockBoard<B>
where
    B: SunxiFixedFlowBoard,
{
    fn new_devices() -> SunxiBootblockDevices<B> {
        (
            EarlyClocksDevice::new(B::ccu_config()),
            StaticConsole::new(B::uart0_config()),
            DramDevice::new(B::dramc_config()),
            StorageDevice::new(B::mmc_config()),
        )
    }

    fn dram_size(&self) -> u64 {
        self.devices.2.device().map_or(
            B::RAM_SIZE,
            fstart_services::MemoryController::detected_size_bytes,
        )
    }

    fn mmc0(&self) -> Result<&B::Mmc, ServiceError> {
        self.devices.3.device().ok_or(ServiceError::NotInitialized)
    }

    fn load_main(&self) -> Result<(), ServiceError> {
        let offset = u64::from(fstart_soc_sunxi::next_stage_offset());
        let size = fstart_soc_sunxi::next_stage_size() as usize;
        if offset == 0 || size == 0 {
            fstart_log::error!("missing eGON next-stage metadata");
            return Err(ServiceError::InvalidParam);
        }

        let media_offset = B::MMC_FIRMWARE_IMAGE_OFFSET + offset;
        let read = fstart_capabilities::next_stage::read_stage_to_addr(
            self.mmc0()?,
            B::MMC0_NODE,
            "main",
            media_offset,
            B::MAIN_LOAD_ADDR,
            size,
        )?;
        if read != size {
            fstart_log::error!("short read loading main: {} of {} bytes", read, size);
            return Err(ServiceError::IoError);
        }
        Ok(())
    }
}

#[cfg(all(feature = "sunxi", feature = "ns16550"))]
impl<B> fstart_stage_runtime::StaticBoard for SunxiBootblockBoard<B>
where
    B: SunxiFixedFlowBoard,
{
    type Devices = SunxiBootblockDevices<B>;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: Self::new_devices(),
            _board: PhantomData,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        B::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(B::UART0_NODE, "ns16550");
        Ok(())
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.load_main()
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "handoff")]
        {
            fstart_capabilities::next_stage::serialize_handoff(self.dram_size(), B::HANDOFF_ADDR)
                .map_err(|_| ServiceError::HardwareError)?;
        }
        Ok(())
    }

    fn boot_payload(self) -> ! {
        fstart_log::info!(
            "jumping to main at {:#x} with handoff {:#x}",
            B::MAIN_LOAD_ADDR,
            B::HANDOFF_ADDR,
        );
        B::jump_to_main(B::MAIN_LOAD_ADDR, B::HANDOFF_ADDR as usize)
    }
}

#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
type SunxiMainDevices<B> = (
    StaticConsole<fstart_driver_ns16550::Ns16550>,
    StorageDevice<<B as SunxiFixedFlowBoard>::Mmc>,
);

/// Generic fixed-flow main stage for Sunxi boards that boot Linux from MMC FFS.
#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
pub struct SunxiMainBoard<B: SunxiLinuxBoard> {
    devices: SunxiMainDevices<B>,
    boot: BlockDeviceLinuxBoot,
    dram_size: u64,
    _board: PhantomData<B>,
}

#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
impl<B> SunxiMainBoard<B>
where
    B: SunxiLinuxBoard,
{
    fn new_devices() -> SunxiMainDevices<B> {
        (
            StaticConsole::new(B::uart0_config()),
            StorageDevice::new(B::mmc_config()),
        )
    }

    fn mmc0_from(devices: &SunxiMainDevices<B>) -> Result<&B::Mmc, ServiceError> {
        devices.1.device().ok_or(ServiceError::NotInitialized)
    }

    fn fdt_ram_size(&self) -> u64 {
        if B::FDT_RAM_FROM_HANDOFF {
            self.dram_size
        } else {
            B::RAM_SIZE
        }
    }
}

#[cfg(all(feature = "sunxi", feature = "ns16550", feature = "ffs"))]
impl<B> fstart_stage_runtime::StaticBoard for SunxiMainBoard<B>
where
    B: SunxiLinuxBoard,
{
    type Devices = SunxiMainDevices<B>;

    fn new() -> Result<Self, ServiceError> {
        #[cfg(feature = "handoff")]
        let dram_size = {
            let ptr = SUNXI_HANDOFF_PTR.load(Ordering::Relaxed);
            fstart_capabilities::handoff::try_deserialize(ptr)
                .map_or(B::RAM_SIZE, |handoff| handoff.dram_size)
        };
        #[cfg(not(feature = "handoff"))]
        let dram_size = B::RAM_SIZE;

        Ok(Self {
            devices: Self::new_devices(),
            boot: BlockDeviceLinuxBoot::new(B::MMC_FIRMWARE_IMAGE_OFFSET, B::FDT_ADDR),
            dram_size,
            _board: PhantomData,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        B::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(B::UART0_NODE, "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let ffs_size = fstart_soc_sunxi::ffs_total_size() as usize;
        self.boot.mount(Self::mmc0_from(&self.devices)?, ffs_size)
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify(Self::mmc0_from(&self.devices)?)
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mmc = Self::mmc0_from(&self.devices)?;
        if B::LOAD_FIRMWARE {
            self.boot.load_firmware(mmc)?;
        }
        self.boot.load_fdt(mmc, B::FDT_ADDR)?;
        self.boot.load_kernel(mmc)
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot
            .prepare_fdt(B::FDT_ADDR, B::BOOTARGS, B::RAM_BASE, self.fdt_ram_size())
    }

    fn boot_payload(self) -> ! {
        if !self.boot.kernel_loaded() || (B::LOAD_FIRMWARE && !self.boot.firmware_loaded()) {
            if B::LOAD_FIRMWARE {
                fstart_log::error!(
                    "Linux handoff requested before {}/kernel/DTB load",
                    B::FIRMWARE_NAME,
                );
            } else {
                fstart_log::error!("Linux handoff requested before kernel/DTB load");
            }
            B::halt();
        }

        if B::LOAD_FIRMWARE {
            fstart_log::info!(
                "booting {} at {:#x}, kernel at {:#x}, dtb at {:#x}, dram={} MiB",
                B::FIRMWARE_NAME,
                B::FIRMWARE_LOAD_ADDR,
                B::KERNEL_LOAD_ADDR,
                B::FDT_ADDR,
                (self.dram_size >> 20) as u32,
            );
        } else {
            fstart_log::info!(
                "booting Linux kernel at {:#x}, dtb at {:#x}, dram={} MiB",
                B::KERNEL_LOAD_ADDR,
                B::FDT_ADDR,
                (self.dram_size >> 20) as u32,
            );
        }

        let params =
            self.boot
                .boot_params(B::KERNEL_LOAD_ADDR, B::FIRMWARE_LOAD_ADDR, 0, B::BOOTARGS);
        B::boot_linux(&params)
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
