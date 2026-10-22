// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DRAM layout of a PSP-booted ("noncar") reset image and the boot
//! flash it comes from. The PSP trains DRAM, writes the APOB and copies the
//! BIOS reset image (directory entry 0x62) to `image` before releasing the
//! BSP at its last 16 bytes. The firmware's DRAM stays reserved through the
//! OS handoff: parked APs, tables and DMA buffers live there.

/// A range of physical memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
}
impl Region {
    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
    const fn within(&self, outer: &Region) -> bool {
        self.base >= outer.base && self.end() <= outer.end()
    }
}

/// Size of the 16-bit entry at the end of the image (`.init16`, `.reset`).
pub const INIT16_SIZE: u64 = 0x2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Reserved through the OS handoff; holds everything below.
    pub early: Region,
    /// Six pages: PML4, PDPT and four page directories for the 4-GiB
    /// identity map of the reset entry.
    pub page_tables: u64,
    /// Writable data and BSS.
    pub data: Region,
    pub heap: Region,
    pub stack: Region,
    /// The PSP-loaded image; its last 8 KiB hold the 16-bit entry.
    pub image: Region,
    /// The boot flash, mapped below 4 GiB.
    pub flash: Region,
    /// The FFS with the payload in the boot flash, if the image has one.
    pub ffs: Option<Region>,
}
impl Layout {
    pub const fn reset_vector(&self) -> u64 {
        self.image.end() - 0x10
    }
    pub const fn init16(&self) -> u64 {
        self.image.end() - INIT16_SIZE
    }
    /// Check the layout at compile time: `const LAYOUT: Layout = Layout {..}.check();`.
    pub const fn check(self) -> Self {
        let tables = Region::new(self.page_tables, 0x6000);
        assert!(tables.within(&self.early) && tables.end() <= self.data.base);
        assert!(self.data.end() <= self.heap.base && self.heap.end() <= self.stack.base);
        assert!(self.stack.end() <= self.image.base && self.image.within(&self.early));
        assert!(self.data.within(&self.early) && self.stack.within(&self.early));
        assert!(self.image.end().is_multiple_of(0x1_0000) && self.image.end() <= 1 << 32);
        if let Some(ffs) = self.ffs {
            assert!(ffs.within(&self.flash));
        }
        self
    }
}
