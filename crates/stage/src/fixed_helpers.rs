//! Reusable pieces for board-owned handwritten stage flows.

#[cfg(all(test, feature = "bootstrap"))]
#[path = "entry_tests.rs"]
mod entry_tests;

use fstart_core::ffs::{ANCHOR_SIZE, FileType};
use fstart_core::services::boot::BootLinuxParams;
use fstart_core::services::boot_media::{BlockDeviceMedia, LinearMap, MemoryMapped};
use fstart_core::services::{BlockDevice, ServiceError};

#[cfg_attr(not(feature = "ffs"), allow(dead_code))]
#[repr(align(8))]
struct AlignedAnchorBytes([u8; ANCHOR_SIZE]);

#[cfg_attr(not(feature = "ffs"), allow(dead_code))]
pub struct BlockDeviceFfs {
    media_offset: u64,
    anchor: Option<AlignedAnchorBytes>,
    ffs_size: usize,
}

#[cfg_attr(not(feature = "ffs"), allow(dead_code))]
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

    /// Mount the FFS image and cache the stage's embedded anchor.
    ///
    /// The anchor is not stored at a fixed media offset — the image builder
    /// patches it into each stage's embedded `FSTART_ANCHOR` static — so the
    /// caller passes its own patched copy (`fstart_stage::fstart_anchor_bytes()`).
    pub fn mount<B>(
        &mut self,
        block: &B,
        ffs_size: usize,
        anchor_bytes: &[u8],
    ) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        #[cfg(feature = "ffs")]
        {
            if ffs_size < ANCHOR_SIZE {
                fstart_log::error!("invalid block FFS size: {:#x}", ffs_size);
                return Err(ServiceError::InvalidParam);
            }
            // SAFETY: stage anchor statics are aligned and contain ANCHOR_SIZE bytes.
            if unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_bytes) }.is_err() {
                fstart_log::error!("embedded FFS anchor invalid");
                return Err(ServiceError::NotInitialized);
            }

            self.ffs_size = ffs_size;
            let _ = self.media(block)?;
            let mut anchor = AlignedAnchorBytes([0u8; ANCHOR_SIZE]);
            anchor.0.copy_from_slice(&anchor_bytes[..ANCHOR_SIZE]);
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
            let _ = (block, ffs_size, anchor_bytes);
            Err(ServiceError::NotSupported)
        }
    }

    /// Verify the mounted FFS image policy.
    pub fn verify<B>(&mut self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        #[cfg(feature = "ffs")]
        {
            let _ = self.manifest_view(block)?;
            Ok(())
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = block;
            Err(ServiceError::NotSupported)
        }
    }

    /// Load one file by FFS type into its packaged load address.
    pub fn load_file<B>(&mut self, block: &B, file_type: FileType) -> Result<u64, ServiceError>
    where
        B: BlockDevice,
    {
        #[cfg(feature = "ffs")]
        {
            let media = self.media(block)?;
            let image_size = self.ffs_size;
            let manifest = self.manifest_view(block)?;
            let file = manifest
                .find_file_by_type(file_type)
                .map_err(|_| ServiceError::NotInitialized)?;
            let entry = crate::load_file_segments_from_media(&media, &file, image_size)
                .ok_or(ServiceError::NotInitialized)?;
            Ok(entry)
        }

        #[cfg(not(feature = "ffs"))]
        {
            let _ = (block, file_type);
            Err(ServiceError::NotSupported)
        }
    }

    #[cfg(feature = "ffs")]
    fn manifest_view<B>(&self, block: &B) -> Result<fstart_ffs::ManifestView<'static>, ServiceError>
    where
        B: BlockDevice,
    {
        let anchor = &self.anchor.as_ref().ok_or(ServiceError::NotInitialized)?.0;
        let media = self.media(block)?;
        crate::directory::ensure_context(anchor, &media)
            .map_err(|_| ServiceError::NotInitialized)?;
        crate::directory::view(&media).map_err(|_| ServiceError::NotInitialized)
    }
}

/// LinuxBoot payload state backed by a block-device FFS image.
pub struct BlockDeviceLinuxBoot {
    ffs: BlockDeviceFfs,
    firmware_loaded: bool,
    kernel_loaded: bool,
    firmware_addr: u64,
    kernel_addr: u64,
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
            firmware_addr: 0,
            kernel_addr: 0,
            dtb_addr,
        }
    }

    /// Mount the FFS image and cache the stage's embedded anchor.
    pub fn mount<B>(
        &mut self,
        block: &B,
        ffs_size: usize,
        anchor_bytes: &[u8],
    ) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.ffs.mount(block, ffs_size, anchor_bytes)
    }

    /// Verify the mounted FFS image policy.
    pub fn verify<B>(&mut self, block: &B) -> Result<(), ServiceError>
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
        self.firmware_addr = self.ffs.load_file(block, FileType::Firmware)?;
        self.firmware_loaded = true;
        Ok(())
    }

    /// Load an override FDT from FFS and make it the current DTB.
    pub fn load_fdt<B>(&mut self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.dtb_addr = self.ffs.load_file(block, FileType::Fdt)?;
        Ok(())
    }

    /// Load a Linux kernel payload from FFS.
    pub fn load_kernel<B>(&mut self, block: &B) -> Result<(), ServiceError>
    where
        B: BlockDevice,
    {
        self.kernel_addr = self.ffs.load_file(block, FileType::Payload)?;
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
            crate::fdt_prepare_platform(self.dtb_addr, dst_dtb_addr, bootargs, ram_base, ram_size)?;
            self.dtb_addr = dst_dtb_addr;
            Ok(())
        }

        #[cfg(not(feature = "fdt"))]
        {
            let _ = (dst_dtb_addr, bootargs, ram_base, ram_size);
            Err(ServiceError::NotSupported)
        }
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

    /// Build common Linux boot params from loaded FFS entries.
    #[must_use]
    pub const fn boot_params<'a>(&self, bootargs: &'a str) -> BootLinuxParams<'a> {
        BootLinuxParams {
            kernel_addr: self.kernel_addr,
            dtb_addr: self.dtb_addr,
            fw_addr: self.firmware_addr,
            rsdp_addr: 0,
            bootargs,
            e820_entries: &[],
            zero_page_addr: 0,
            hart_id: 0,
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

    #[cfg_attr(not(feature = "ffs"), allow(dead_code))]
    fn media(self) -> MemoryMapped<LinearMap> {
        // SAFETY: callers construct this descriptor from board-declared ROM/flash
        // windows that remain readable for the whole boot flow.
        unsafe { MemoryMapped::from_raw_addr(self.base, self.size) }
    }

    /// Publish this memory-mapped FFS window to stage-local services.
    pub fn mount(self) -> Result<(), ServiceError> {
        fstart_log::info!("mounting memory-mapped FFS at {:#x}", self.base);
        fstart_core::services::ffs_context::set_memory_mapped(
            crate::fstart_anchor_bytes(),
            self.base,
            self.size as u64,
        );
        Ok(())
    }

    /// Open the authenticated directory once, after DRAM/allocator setup.
    /// A verified predecessor context is reused without any root signature.
    pub fn verify(self) -> Result<(), ServiceError> {
        #[cfg(feature = "ffs")]
        {
            crate::directory::ensure_context(crate::fstart_anchor_bytes(), &self.media())
                .map_err(|_| ServiceError::NotInitialized)
        }
        #[cfg(not(feature = "ffs"))]
        {
            Err(ServiceError::NotSupported)
        }
    }

    /// Load one file by FFS type into its packaged load address.
    pub fn load_file(self, file_type: FileType) -> Result<u64, ServiceError> {
        #[cfg(feature = "ffs")]
        {
            let media = self.media();
            crate::load_ffs_file_entry_by_type(&media, file_type)
                .ok_or(ServiceError::NotInitialized)
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
            if crate::load_ffs_file_by_name(crate::fstart_anchor_bytes(), &media, name) {
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
    firmware_addr: u64,
    kernel_addr: u64,
    dtb_addr: u64,
}

/// UEFI payload state backed by a memory-mapped FFS image.
#[derive(Debug, Clone, Copy)]
pub struct MemoryMappedUefiBoot {
    ffs: MemoryMappedFfs,
    firmware_addr: u64,
    fdt_addr: u64,
}

impl MemoryMappedUefiBoot {
    /// Construct a UEFI helper with the board's FFS window and platform DTB.
    #[must_use]
    pub const fn new(ffs_base: u64, ffs_size: usize, fdt_addr: u64) -> Self {
        Self {
            ffs: MemoryMappedFfs::new(ffs_base, ffs_size),
            firmware_addr: 0,
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
        self.firmware_addr = self.ffs.load_file(FileType::Firmware)?;
        Ok(())
    }

    /// Return whether the firmware blob has been loaded.
    #[must_use]
    pub const fn firmware_loaded(&self) -> bool {
        self.firmware_addr != 0
    }

    /// Return the actual verified firmware entry only when it matches trusted
    /// platform configuration. Callers must use this value, not a loaded flag.
    pub fn checked_firmware_entry(&self, expected: u64) -> Result<u64, ServiceError> {
        checked_entry(self.firmware_addr, expected)
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
            firmware_addr: 0,
            kernel_addr: 0,
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
        self.kernel_addr = self.ffs.load_file(FileType::Payload)?;
        Ok(())
    }

    /// Load an override FDT from FFS and make it the current DTB.
    pub fn load_fdt(&mut self, dtb_addr: u64) -> Result<(), ServiceError> {
        let actual = self.ffs.load_file(FileType::Fdt)?;
        self.dtb_addr = checked_entry(actual, dtb_addr)?;
        Ok(())
    }

    /// Load firmware, such as OpenSBI or BL31, and then the Linux kernel.
    pub fn load_firmware_and_kernel(&mut self) -> Result<(), ServiceError> {
        self.firmware_addr = self.ffs.load_file(FileType::Firmware)?;
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
            crate::fdt_prepare_platform(self.dtb_addr, dst_dtb_addr, bootargs, ram_base, ram_size)?;
            self.dtb_addr = dst_dtb_addr;
            Ok(())
        }

        #[cfg(not(feature = "fdt"))]
        {
            let _ = (dst_dtb_addr, bootargs, ram_base, ram_size);
            Err(ServiceError::NotSupported)
        }
    }

    /// Return whether the kernel payload has been loaded.
    #[must_use]
    pub const fn kernel_loaded(&self) -> bool {
        self.kernel_addr != 0
    }

    /// Return whether the firmware payload has been loaded.
    #[must_use]
    pub const fn firmware_loaded(&self) -> bool {
        self.firmware_addr != 0
    }

    /// Validate trusted configured destinations against the entries actually
    /// loaded and verified. Returned params contain only those actual entries.
    pub fn checked_boot_params<'a>(
        &self,
        kernel_addr: u64,
        firmware_addr: u64,
        hart_id: u64,
        bootargs: &'a str,
    ) -> Result<BootLinuxParams<'a>, ServiceError> {
        let kernel = checked_entry(self.kernel_addr, kernel_addr)?;
        let firmware = if self.firmware_addr == 0 && firmware_addr == 0 {
            0
        } else {
            checked_entry(self.firmware_addr, firmware_addr)?
        };
        Ok(BootLinuxParams {
            kernel_addr: kernel,
            dtb_addr: self.dtb_addr,
            fw_addr: firmware,
            rsdp_addr: 0,
            bootargs,
            e820_entries: &[],
            zero_page_addr: 0,
            hart_id,
            print_x86_mtrrs: false,
        })
    }

    /// Fixed-flow convenience wrapper. A mismatched/unloaded entry halts via
    /// the stage panic handler; it can never become an unchecked jump address.
    #[must_use]
    pub fn boot_params<'a>(
        &self,
        kernel_addr: u64,
        firmware_addr: u64,
        hart_id: u64,
        bootargs: &'a str,
    ) -> BootLinuxParams<'a> {
        self.checked_boot_params(kernel_addr, firmware_addr, hart_id, bootargs)
            .expect("payload entry was not loaded and verified at its configured address")
    }

    /// Build x86 Linux bzImage boot params, including the zero-page address.
    #[must_use]
    pub fn x86_boot_params<'a>(
        &self,
        kernel_addr: u64,
        zero_page_addr: u64,
        bootargs: &'a str,
    ) -> BootLinuxParams<'a> {
        let kernel_addr = checked_entry(self.kernel_addr, kernel_addr)
            .expect("x86 payload entry was not loaded and verified at its configured address");
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

fn checked_entry(actual: u64, expected: u64) -> Result<u64, ServiceError> {
    if actual == 0 || actual != expected {
        return Err(ServiceError::InvalidParam);
    }
    Ok(actual)
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
