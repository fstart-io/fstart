//! Boot media abstraction — unified interface for firmware storage access.
//!
//! This module provides the [`BootMedia`] trait that abstracts reading from
//! different firmware storage backends. The design is inspired by coreboot's
//! `region_device` architecture but redesigned for Rust's type system.
//!
//! # Coreboot Background
//!
//! Coreboot's `region_device` provides a vtable with two key operations:
//!
//! - **`mmap`**: Returns a direct pointer into memory-mapped flash (zero-copy).
//! - **`readat`**: Copies data into a caller-provided buffer (SPI, MMC, etc.).
//!
//! The `mmap_helper` layer synthesizes `mmap` for non-memory-mapped devices
//! by allocating a buffer and calling `readat`. Sub-regions (`rdev_chain`)
//! provide windowed access with offset translation.
//!
//! # Rust Approach: Zero-Cost via Monomorphization
//!
//! Instead of a vtable, we use a trait with generic parameters. When code
//! is generic over `<M: BootMedia>`, the compiler generates specialized
//! versions for each concrete type:
//!
//! - **[`MemoryMapped<F>`]**: All operations inline to direct memory access.
//!   `read_at` becomes `memmove`, `as_slice` returns a direct `&[u8]`.
//!   The optimizer eliminates all abstraction overhead. The [`FlashMap`]
//!   parameter `F` provides SoC-specific address translation from raw
//!   flash offsets to CPU-visible addresses.
//!
//! - **[`BlockDeviceMedia`]**: Delegates to a [`BlockDevice`] driver (SPI
//!   NOR flash, eMMC, virtio-blk, etc.). `as_slice()` returns `None`,
//!   so all access goes through the device I/O path.
//!
//! - **[`SubRegion`]**: Analogous to coreboot's `rdev_chain()` — provides
//!   a windowed view into a parent medium with offset translation.

use crate::TempRamBuffer;

use super::{BlockDevice, FirmwareImage, ServiceError};

/// Simple bump allocator over a caller-provided temporary RAM buffer.
///
/// The allocator is intentionally tiny and `no_std` friendly. It owns no memory;
/// it only tracks offsets inside a RAM arena declared by board/platform policy.
/// Callers may reset it between independent FFS operations.
pub struct TempRamArena {
    base: *mut u8,
    size: usize,
    used: usize,
}

impl TempRamArena {
    /// Create a scratch arena from a typed RAM buffer descriptor.
    ///
    /// # Safety
    ///
    /// `buffer.base..buffer.base + buffer.size` must be valid writable RAM for
    /// the lifetime of this arena.
    pub unsafe fn new(buffer: TempRamBuffer) -> Result<Self, ServiceError> {
        let base = usize::try_from(buffer.base).map_err(|_| ServiceError::InvalidParam)?;
        let size = usize::try_from(buffer.size).map_err(|_| ServiceError::InvalidParam)?;
        if size != 0 && base == 0 {
            return Err(ServiceError::InvalidParam);
        }
        base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
        Ok(Self {
            base: base as *mut u8,
            size,
            used: 0,
        })
    }

    /// Reset the arena, allowing previous temporary allocations to be reused.
    pub fn reset(&mut self) {
        self.used = 0;
    }

    /// Allocate `len` bytes with `align` alignment.
    pub fn alloc(&mut self, len: usize, align: usize) -> Option<&mut [u8]> {
        let align = align.max(1);
        if !align.is_power_of_two() {
            return None;
        }
        let base = self.base as usize;
        let raw_start = base.checked_add(self.used)?;
        let aligned_start = raw_start.checked_add(align - 1)? & !(align - 1);
        let start = aligned_start.checked_sub(base)?;
        let end = start.checked_add(len)?;
        if end > self.size {
            return None;
        }
        self.used = end;
        // SAFETY: new() establishes the writable RAM arena, and bounds checks
        // above ensure this allocation lies inside it. The bump cursor prevents
        // overlapping live allocations until reset().
        Some(unsafe { core::slice::from_raw_parts_mut(aligned_start as *mut u8, len) })
    }

    /// Return arena capacity in bytes.
    pub const fn capacity(&self) -> usize {
        self.size
    }

    /// Return currently used bytes.
    pub const fn used(&self) -> usize {
        self.used
    }
}

// SAFETY: TempRamArena is just a scalar descriptor for a caller-owned scratch
// RAM range. Firmware execution is single-threaded in current use; sharing the
// descriptor does not add aliasing beyond unsafe construction/alloc contracts.
unsafe impl Send for TempRamArena {}
unsafe impl Sync for TempRamArena {}

/// Read a boot-media range into temporary RAM and return the copied slice.
pub fn read_to_temp<'a>(
    media: &(impl BootMedia + ?Sized),
    offset: usize,
    len: usize,
    arena: &'a mut TempRamArena,
) -> Result<&'a [u8], ServiceError> {
    let buf = arena.alloc(len, 8).ok_or(ServiceError::InvalidParam)?;
    let read = media.read_at(offset, buf)?;
    if read != len {
        return Err(ServiceError::InvalidParam);
    }
    Ok(buf)
}

/// Abstraction over firmware boot media.
///
/// Provides a uniform read interface for the boot medium holding the
/// firmware filesystem (FFS). Concrete implementations exist for
/// memory-mapped flash ([`MemoryMapped`]), RAM slices ([`SliceMedia`]),
/// and block-device-backed storage ([`BlockDeviceMedia`]).
pub trait BootMedia: Send + Sync {
    /// Read `buf.len()` bytes starting at `offset` into `buf`.
    ///
    /// Returns the number of bytes successfully read.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidParam`] if the read would exceed
    /// the media bounds.
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError>;

    /// Total size of the boot medium in bytes.
    fn size(&self) -> usize;

    /// Attempt to get a direct memory-mapped view of `offset..offset + len`
    /// (analogous to coreboot `rdev_mmap`).
    ///
    /// Returns `Some(&[u8])` when the requested range sits contiguously inside
    /// a memory-mapped window (zero-copy access), or `None` for block media,
    /// ranges that straddle a window boundary, or mappings rooted at address 0.
    fn map(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let _ = (offset, len);
        None
    }

    /// Return true if any memory-mapped backing window of this medium overlaps
    /// the CPU address range `start..start + size`.
    ///
    /// Used by the loader's placement preflight so loading into RAM cannot
    /// overwrite a RAM-resident or memory-mapped boot medium.
    fn overlaps_range(&self, start: u64, size: u64) -> bool {
        let _ = (start, size);
        false
    }
}

// ---------------------------------------------------------------------------
// SliceMedia — boot media over an in-memory byte slice (RAM / tests)
// ---------------------------------------------------------------------------

/// Boot media backed by an ordinary Rust byte slice.
#[derive(Clone, Copy)]
pub struct SliceMedia<'a>(&'a [u8]);

impl<'a> SliceMedia<'a> {
    /// Wrap a byte slice as [`BootMedia`].
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }
}

impl BootMedia for SliceMedia<'_> {
    #[inline]
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let end = offset
            .checked_add(buf.len())
            .ok_or(ServiceError::InvalidParam)?;
        let src = self.0.get(offset..end).ok_or(ServiceError::InvalidParam)?;
        buf.copy_from_slice(src);
        Ok(buf.len())
    }

    #[inline]
    fn size(&self) -> usize {
        self.0.len()
    }

    #[inline]
    fn map(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.0.get(offset..end)
    }

    #[inline]
    fn overlaps_range(&self, start: u64, size: u64) -> bool {
        if size == 0 || self.0.is_empty() {
            return false;
        }
        let media_start = self.0.as_ptr() as u64;
        let Some(media_end) = media_start.checked_add(self.0.len() as u64) else {
            return true;
        };
        let Some(end) = start.checked_add(size) else {
            return true;
        };
        start < media_end && media_start < end
    }
}

// ---------------------------------------------------------------------------
// MemoryMapped — boot media backed by a FirmwareImage's CPU-visible windows
// ---------------------------------------------------------------------------

/// Launder a raw address through a volatile stack read so the compiler cannot
/// constant-fold `0` into a null pointer on AArch64 (where flash starts at 0).
#[inline(always)]
#[expect(
    clippy::disallowed_methods,
    reason = "hides a value from the optimizer; no memory access"
)]
fn launder_addr(addr: u64) -> *const u8 {
    // SAFETY: a u64 on the stack is always readable.
    let raw: u64 = unsafe { core::ptr::read_volatile(&addr) };
    raw as *const u8
}

/// Boot media backed by memory-mapped flash or ROM described by a
/// [`FirmwareImage`].
///
/// Supports both single-window linear mappings (`from_raw_addr`) and
/// multi-window mappings (`from_image`), such as AMD ROM2 + ROM3.
pub struct MemoryMapped {
    image: FirmwareImage,
}

impl MemoryMapped {
    /// Create a memory-mapped boot medium from a validated [`FirmwareImage`].
    ///
    /// # Safety
    ///
    /// Every active window in `image` must point to a valid, readable CPU
    /// memory region that remains mapped for the lifetime of this value.
    pub unsafe fn from_image(image: FirmwareImage) -> Result<Self, ServiceError> {
        image.validate()?;
        if usize::try_from(image.size).is_err() {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self { image })
    }

    /// Create a memory-mapped boot medium with a single contiguous window
    /// mapping flash offset 0 to `base_addr`.
    ///
    /// # Safety
    ///
    /// The address range `base_addr..base_addr + size` must be a valid,
    /// readable memory region that remains mapped for the lifetime of this
    /// value.
    #[inline(always)]
    pub unsafe fn from_raw_addr(base_addr: u64, size: usize) -> Self {
        Self {
            image: FirmwareImage::single_window(base_addr, size as u64),
        }
    }

    /// The underlying [`FirmwareImage`] window description.
    pub const fn image(&self) -> &FirmwareImage {
        &self.image
    }
}

impl BootMedia for MemoryMapped {
    #[inline(always)]
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset
            .checked_add(buf.len())
            .is_none_or(|end| end as u64 > self.image.size)
        {
            return Err(ServiceError::InvalidParam);
        }
        // Use `copy` (memmove) rather than `copy_nonoverlapping` because on
        // non-XIP platforms the image may reside in RAM near load targets.
        let mut copied = 0usize;
        while copied < buf.len() {
            let flash_offset = (offset + copied) as u64;
            let remaining = (buf.len() - copied) as u64;
            let (cpu_addr, chunk_len) = self
                .image
                .contiguous_chunk(flash_offset, remaining)
                .ok_or(ServiceError::InvalidParam)?;
            let chunk_len = chunk_len as usize;
            let src = launder_addr(cpu_addr);
            // SAFETY: caller of `from_image`/`from_raw_addr` guaranteed every
            // active window is readable for the lifetime of `self`.
            unsafe {
                core::ptr::copy(src, buf.as_mut_ptr().add(copied), chunk_len);
            }
            copied += chunk_len;
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        Ok(buf.len())
    }

    #[inline(always)]
    fn size(&self) -> usize {
        self.image.size as usize
    }

    #[inline(always)]
    fn map(&self, offset: usize, len: usize) -> Option<&[u8]> {
        if len == 0 {
            return (offset as u64 <= self.image.size).then_some(&[]);
        }
        let cpu_addr = self.image.translate_range(offset as u64, len as u64)?;
        if cpu_addr == 0 {
            return None;
        }
        let ptr = launder_addr(cpu_addr);
        // SAFETY: `translate_range` confirmed `offset..offset + len` sits in
        // one active window, and `cpu_addr != 0`.
        Some(unsafe { core::slice::from_raw_parts(ptr, len) })
    }

    #[inline(always)]
    fn overlaps_range(&self, start: u64, size: u64) -> bool {
        self.image.overlaps_cpu_range(start, size)
    }
}

// ---------------------------------------------------------------------------
// BlockDeviceMedia — adapter from BlockDevice to BootMedia
// ---------------------------------------------------------------------------

/// Adapter that presents a [`BlockDevice`] as [`BootMedia`].
///
/// Wraps any `BlockDevice` driver (SPI NOR flash, eMMC, virtio-blk, etc.)
/// and provides the `BootMedia` interface. All reads go through the
/// block device's [`read()`](BlockDevice::read) method.
///
/// The optional `base_offset` parameter allows the firmware image to
/// start at a non-zero offset on the block device (e.g., a partition
/// offset on eMMC).
///
/// # Example (conceptual)
///
/// ```ignore
/// let spi_flash = SpiNorFlash::new(&config)?;
/// let boot_media = BlockDeviceMedia::new(&spi_flash, 0, 4 * 1024 * 1024);
/// // Now use boot_media with any BootMedia-generic function...
/// ```
pub struct BlockDeviceMedia<'a, B: BlockDevice> {
    device: &'a B,
    /// Byte offset on the block device where the firmware image starts.
    base_offset: u64,
    /// Size of the firmware image in bytes.
    media_size: usize,
}

impl<'a, B: BlockDevice> BlockDeviceMedia<'a, B> {
    /// Create a boot media adapter over a block device.
    ///
    /// - `device`: The block device driver to read from.
    /// - `base_offset`: Byte offset on the block device where the firmware
    ///   image starts (0 if the image occupies the entire device).
    /// - `size`: Total firmware image size in bytes.
    pub fn new(device: &'a B, base_offset: u64, size: usize) -> Self {
        Self {
            device,
            base_offset,
            media_size: size,
        }
    }
}

impl<B: BlockDevice> BootMedia for BlockDeviceMedia<'_, B> {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset
            .checked_add(buf.len())
            .is_none_or(|end| end > self.media_size)
        {
            return Err(ServiceError::InvalidParam);
        }
        self.device.read(self.base_offset + offset as u64, buf)
    }

    fn size(&self) -> usize {
        self.media_size
    }

    // as_slice() returns None (default) — block devices cannot be memory-mapped.
}

// ---------------------------------------------------------------------------
// SubRegion — windowed view into a boot medium (cf. coreboot rdev_chain)
// ---------------------------------------------------------------------------

/// A windowed sub-region of a boot medium.
///
/// Analogous to coreboot's `rdev_chain()`, this provides a view into a
/// portion of an underlying boot medium with offset translation. Reads
/// at offset `n` translate to reads at `self.offset + n` on the parent.
///
/// This is useful for isolating a specific region of the firmware image
/// (e.g., the FFS container region within a larger flash layout with
/// an FMAP-style region table).
///
/// # Example
///
/// ```ignore
/// let flash = unsafe { MemoryMapped::from_raw_addr(base, total_size) };
/// // Create a view of just the COREBOOT region at offset 0x1000, size 0x100000
/// let region = SubRegion::new(&flash, 0x1000, 0x100000).unwrap();
/// region.read_at(0, &mut buf)?; // reads from flash offset 0x1000
/// ```
pub struct SubRegion<'a, M: BootMedia> {
    parent: &'a M,
    /// Offset within the parent medium.
    offset: usize,
    /// Size of this sub-region.
    region_size: usize,
}

impl<'a, M: BootMedia> SubRegion<'a, M> {
    /// Create a sub-region view of a boot medium.
    ///
    /// Returns `None` if the sub-region would extend beyond the parent's
    /// bounds (`offset + size > parent.size()`).
    pub fn new(parent: &'a M, offset: usize, size: usize) -> Option<Self> {
        if offset
            .checked_add(size)
            .is_none_or(|end| end > parent.size())
        {
            return None;
        }
        Some(Self {
            parent,
            offset,
            region_size: size,
        })
    }
}

impl<M: BootMedia> BootMedia for SubRegion<'_, M> {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset
            .checked_add(buf.len())
            .is_none_or(|end| end > self.region_size)
        {
            return Err(ServiceError::InvalidParam);
        }
        self.parent.read_at(self.offset + offset, buf)
    }

    fn size(&self) -> usize {
        self.region_size
    }

    fn map(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        if end > self.region_size {
            return None;
        }
        self.parent.map(self.offset + offset, len)
    }

    fn overlaps_range(&self, start: u64, size: u64) -> bool {
        self.parent.overlaps_range(start, size)
    }
}
