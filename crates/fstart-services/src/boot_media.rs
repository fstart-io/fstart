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
//!
//! # Example: Stage Loading
//!
//! ```ignore
//! // Memory-mapped with linear mapping (zero-cost):
//! let media = unsafe { MemoryMapped::from_raw_addr(0x20000000, 0x2000000) };
//! fstart_capabilities::stage_load("main", anchor, &media, jump_to);
//!
//! // Block device (SPI NOR flash — same API, different backend):
//! let spi_flash = SpiNorFlash::new(&config)?;
//! let media = BlockDeviceMedia::new(&spi_flash, 0, 0x2000000);
//! fstart_capabilities::stage_load("main", anchor, &media, jump_to);
//! ```

use crate::{BlockDevice, ServiceError};

/// Abstraction over firmware boot media.
///
/// Provides a uniform read interface for the boot medium holding the
/// firmware filesystem (FFS). Concrete implementations exist for
/// memory-mapped flash ([`MemoryMapped`]) and block-device-backed
/// storage ([`BlockDeviceMedia`]).
///
/// # Zero-Cost for Memory-Mapped Media
///
/// When used with generics (`fn foo<M: BootMedia>(m: &M)`), the compiler
/// monomorphizes the code for each concrete type. For [`MemoryMapped`]:
///
/// - `read_at` inlines to a single `memmove`
/// - `as_slice` returns a direct reference into flash
/// - Branch elimination removes all "is this mappable?" checks
///
/// There is **no vtable overhead** — the abstraction compiles away entirely.
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

    /// Attempt to get a direct memory-mapped view of the entire boot medium.
    ///
    /// Returns `Some(&[u8])` for memory-mapped media (zero-copy access).
    /// Returns `None` for non-memory-mapped media (SPI, MMC, etc.).
    ///
    /// When `Some`, the returned slice spans the entire boot medium and
    /// can be indexed directly — no copies needed. This enables the
    /// existing [`FfsReader`](../../fstart_ffs/reader/struct.FfsReader.html)
    /// fast path for manifest reading and digest verification.
    fn as_slice(&self) -> Option<&[u8]> {
        None
    }
}

// ---------------------------------------------------------------------------
// FlashMap — SoC-specific flash-to-CPU address translation
// ---------------------------------------------------------------------------

/// Flash address translation — maps raw flash offsets to CPU-visible pointers.
///
/// Different SoCs map flash memory into the CPU address space differently:
///
/// - **Linear** ([`LinearMap`]): contiguous at a fixed base, `offset → base + offset`.
///   Most common for NOR flash on simple SoCs (QEMU virt, many ARM SoCs).
///
/// - **Banked**: hardware bank registers select which flash window is visible.
///   The translation depends on the bank configuration and window size.
///
/// - **Non-contiguous**: multiple flash regions at different CPU addresses
///   (e.g., a boot ROM + a larger flash region with a gap between them).
///
/// Implement this trait in the platform crate for SoCs with non-trivial
/// flash mappings. The [`MemoryMapped<F>`] boot media delegates all
/// address translation to the `FlashMap` implementation, so the rest of
/// the firmware stack is agnostic to the mapping details.
///
/// # Safety Contract
///
/// Implementations must ensure that `translate(offset)` returns a valid,
/// readable pointer for all `offset < size` where `size` is the flash
/// region size passed to [`MemoryMapped::new`]. Returning an invalid
/// pointer is immediate UB when `MemoryMapped::read_at` dereferences it.
pub trait FlashMap: Send + Sync {
    /// Translate a raw flash byte offset to a CPU-visible pointer.
    ///
    /// The returned pointer must be valid and readable. The caller
    /// guarantees `flash_offset` is within the boot media bounds.
    fn translate(&self, flash_offset: usize) -> *const u8;

    /// Return a contiguous slice covering the entire mapped region.
    ///
    /// Returns `Some` only if the mapping is contiguous in CPU address
    /// space (e.g., [`LinearMap`]). Returns `None` for banked or
    /// non-contiguous mappings where a single slice cannot represent
    /// the entire flash.
    ///
    /// When `Some`, the FFS reader uses the fast zero-copy path.
    fn as_contiguous_slice(&self, size: usize) -> Option<&[u8]> {
        let _ = size;
        None
    }
}

// ---------------------------------------------------------------------------
// LinearMap — contiguous flash at a fixed CPU base address
// ---------------------------------------------------------------------------

/// Linear flash mapping — contiguous at a fixed CPU base address.
///
/// This is the common case for memory-mapped NOR flash. Flash offset `n`
/// maps to CPU address `base + n`. Used by most embedded platforms and
/// QEMU virt machines.
///
/// For SoCs with non-contiguous or banked flash, implement [`FlashMap`]
/// directly in the platform crate instead.
pub struct LinearMap {
    base: *const u8,
}

impl LinearMap {
    /// Create a linear flash mapping from a raw pointer.
    ///
    /// # Safety
    ///
    /// `base` must point to a valid, readable, contiguous memory region
    /// that covers the entire flash size. The region must remain mapped
    /// for the lifetime of this value.
    pub const unsafe fn new(base: *const u8) -> Self {
        Self { base }
    }

    /// Create a linear flash mapping from a raw address.
    ///
    /// Uses `read_volatile` on the address to prevent the compiler from
    /// constant-folding it. This is critical on AArch64 where flash
    /// genuinely starts at address `0x0` — Rust considers null pointers
    /// as UB in references, and the optimizer could exploit a constant `0`
    /// to miscompile slice operations.
    ///
    /// # Safety
    ///
    /// The address must point to a valid, readable, contiguous memory
    /// region that covers the entire flash size.
    #[inline(always)]
    pub unsafe fn from_raw_addr(base_addr: u64) -> Self {
        // SAFETY: a u64 on the stack is always readable. Volatile read
        // prevents the compiler from constant-folding the value.
        let addr: u64 = unsafe { core::ptr::read_volatile(&base_addr) };
        Self {
            base: addr as *const u8,
        }
    }
}

// SAFETY: LinearMap provides read-only access to a fixed memory region
// (flash/ROM). The region is accessible from any execution context and is
// not mutated through this interface.
unsafe impl Send for LinearMap {}
unsafe impl Sync for LinearMap {}

impl FlashMap for LinearMap {
    #[inline(always)]
    fn translate(&self, flash_offset: usize) -> *const u8 {
        // SAFETY: caller guarantees flash_offset is within bounds.
        unsafe { self.base.add(flash_offset) }
    }

    #[inline(always)]
    fn as_contiguous_slice(&self, size: usize) -> Option<&[u8]> {
        if size == 0 {
            return Some(&[]);
        }
        // SAFETY: base..base+size is guaranteed valid by the caller of
        // LinearMap::new() / from_raw_addr(). Non-zero size guarantees non-null.
        Some(unsafe { core::slice::from_raw_parts(self.base, size) })
    }
}

// ---------------------------------------------------------------------------
// MemoryMapped — zero-cost boot media with pluggable address translation
// ---------------------------------------------------------------------------

/// Boot media backed by memory-mapped flash or ROM.
///
/// The type parameter `F` provides the SoC-specific address translation
/// via the [`FlashMap`] trait. For the common case of contiguous linear
/// mapping, use [`LinearMap`]:
///
/// ```ignore
/// let media = unsafe { MemoryMapped::from_raw_addr(0x20000000, 0x2000000) };
/// ```
///
/// All operations are zero-cost through inlining and monomorphization:
///
/// - `read_at` compiles to `memmove` (handles potential overlap when the
///   FFS image resides in RAM alongside load targets)
/// - `as_slice` returns a direct `&[u8]` reference (contiguous mappings only)
/// - The `FlashMap::translate` call inlines away for simple mappings
///
/// # Use Cases
///
/// - XIP flash on embedded platforms (flash memory-mapped into CPU address space)
/// - QEMU `-bios` loading (firmware loaded directly into RAM or pflash)
/// - SoCs with banked or non-contiguous flash (custom [`FlashMap`] impl)
///
/// # Safety
///
/// The `FlashMap` implementation must return valid pointers for all offsets
/// within the media bounds. This is typically guaranteed by the hardware
/// memory map and board configuration.
pub struct MemoryMapped<F: FlashMap> {
    map: F,
    size: usize,
}

impl<F: FlashMap> MemoryMapped<F> {
    /// Create a new memory-mapped boot medium with a custom flash mapping.
    ///
    /// - `map`: The SoC-specific flash address translation.
    /// - `size`: Total size of the mapped flash region in bytes.
    pub fn new(map: F, size: usize) -> Self {
        Self { map, size }
    }
}

/// Convenience constructors for the common linear mapping case.
impl MemoryMapped<LinearMap> {
    /// Create a memory-mapped boot medium with linear mapping from a raw address.
    ///
    /// This is the most common construction path. Flash offset `n` maps
    /// to CPU address `base_addr + n`.
    ///
    /// Uses `read_volatile` internally to prevent null-pointer optimization
    /// on platforms where flash starts at address 0x0.
    ///
    /// # Safety
    ///
    /// The address range `base_addr..base_addr+size` must be a valid,
    /// readable memory region that remains mapped for the lifetime of
    /// this value.
    #[inline(always)]
    pub unsafe fn from_raw_addr(base_addr: u64, size: usize) -> Self {
        let map = unsafe { LinearMap::from_raw_addr(base_addr) };
        Self { map, size }
    }
}

// SAFETY: MemoryMapped provides read-only access to a fixed memory region
// (flash/ROM). The region is accessible from any execution context and is
// not mutated through this interface. Safety of the underlying pointer is
// guaranteed by the FlashMap implementation.
unsafe impl<F: FlashMap> Send for MemoryMapped<F> {}
unsafe impl<F: FlashMap> Sync for MemoryMapped<F> {}

impl<F: FlashMap> BootMedia for MemoryMapped<F> {
    #[inline(always)]
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset
            .checked_add(buf.len())
            .is_none_or(|end| end > self.size)
        {
            return Err(ServiceError::InvalidParam);
        }
        // Translate the flash offset to a CPU-visible pointer via the
        // SoC-specific FlashMap, then copy into the caller's buffer.
        //
        // We use `copy` (memmove) rather than `copy_nonoverlapping` (memcpy)
        // for robustness: when the FFS image is loaded into RAM (not XIP
        // flash), source and destination regions may overlap — e.g., loading
        // a large kernel from FFS at 0x80000000 to a nearby load address.
        let src = self.map.translate(offset);
        unsafe {
            core::ptr::copy(src, buf.as_mut_ptr(), buf.len());
        }
        Ok(buf.len())
    }

    #[inline(always)]
    fn size(&self) -> usize {
        self.size
    }

    #[inline(always)]
    fn as_slice(&self) -> Option<&[u8]> {
        self.map.as_contiguous_slice(self.size)
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

    fn as_slice(&self) -> Option<&[u8]> {
        self.parent
            .as_slice()
            .map(|s| &s[self.offset..self.offset + self.region_size])
    }
}
