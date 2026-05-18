//! Runtime context for firmware filesystem consumers.
//!
//! The generated board adapter records the current memory-mapped FFS window
//! after the `BootMedia` capability runs.  Drivers that need board assets from
//! FFS during later RAM-backed initialization can query this scalar context
//! without stage code knowing about those assets.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Memory-mapped FFS context published by the active stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryMappedFfsContext {
    /// Address of the stage's linked `FSTART_ANCHOR` bytes.
    pub anchor_addr: usize,
    /// Size of the anchor byte slice.
    pub anchor_len: usize,
    /// CPU-visible base address of the FFS image.
    pub image_base: u64,
    /// Size of the CPU-visible FFS window.
    pub image_size: u64,
}

static ANCHOR_ADDR: AtomicUsize = AtomicUsize::new(0);
static ANCHOR_LEN: AtomicUsize = AtomicUsize::new(0);
static IMAGE_BASE: AtomicU64 = AtomicU64::new(0);
static IMAGE_SIZE: AtomicU64 = AtomicU64::new(0);

/// Publish a memory-mapped FFS context for later driver initialization.
#[inline]
pub fn set_memory_mapped(anchor: &[u8], image_base: u64, image_size: u64) {
    ANCHOR_ADDR.store(anchor.as_ptr() as usize, Ordering::Relaxed);
    ANCHOR_LEN.store(anchor.len(), Ordering::Relaxed);
    IMAGE_BASE.store(image_base, Ordering::Relaxed);
    IMAGE_SIZE.store(image_size, Ordering::Release);
}

/// Return the currently published memory-mapped FFS context, if any.
#[inline]
pub fn memory_mapped() -> Option<MemoryMappedFfsContext> {
    let image_size = IMAGE_SIZE.load(Ordering::Acquire);
    let image_base = IMAGE_BASE.load(Ordering::Relaxed);
    let anchor_addr = ANCHOR_ADDR.load(Ordering::Relaxed);
    let anchor_len = ANCHOR_LEN.load(Ordering::Relaxed);

    if image_size == 0 || anchor_addr == 0 || anchor_len == 0 {
        return None;
    }

    Some(MemoryMappedFfsContext {
        anchor_addr,
        anchor_len,
        image_base,
        image_size,
    })
}

impl MemoryMappedFfsContext {
    /// Return the anchor byte slice recorded for this stage.
    ///
    /// # Safety
    ///
    /// The generated stage publishes a pointer to its static `FSTART_ANCHOR`,
    /// which lives for the entire stage execution.
    #[inline]
    pub unsafe fn anchor_bytes(&self) -> &'static [u8] {
        core::slice::from_raw_parts(self.anchor_addr as *const u8, self.anchor_len)
    }

    /// Return the memory-mapped FFS image window.
    ///
    /// # Safety
    ///
    /// Board configuration must describe a valid memory-mapped boot-media
    /// window covering the FFS image.
    #[inline]
    pub unsafe fn image_bytes(&self) -> &'static [u8] {
        core::slice::from_raw_parts(self.image_base as *const u8, self.image_size as usize)
    }
}
