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

/// PDPTs of 1-GiB pages the shared reset entry builds after its PML4: the
/// first 512 GiB, above everything a noncar SoC addresses before the
/// payload's tables ([`super::boot::map_ram`]).
pub const PDPT_COUNT: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Reserved through the OS handoff; holds everything below.
    pub early: Region,
    /// The PML4 and [`PDPT_COUNT`] PDPTs of the reset entry's identity map.
    pub page_tables: u64,
    /// BSS (initialized data runs in place in the image).
    pub data: Region,
    pub heap: Region,
    pub stack: Region,
    /// The PSP-loaded image; its last 4 KiB hold the 16-bit entry.
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
    /// The page tables of the reset entry.
    pub const fn pages(&self) -> Region {
        Region::new(self.page_tables, (PDPT_COUNT + 1) * 0x1000)
    }
    /// The write-back MTRR the reset entry opens until the cache plan
    /// replaces it: the smallest naturally aligned power-of-two window over
    /// the early region.
    pub const fn cache_window(&self) -> Region {
        let mut size = 0x1000;
        loop {
            let base = self.early.base & !(size - 1);
            if base + size >= self.early.end() {
                return Region::new(base, size);
            }
            size <<= 1;
        }
    }
    /// Check the layout at compile time: `const LAYOUT: Layout = Layout {..}.check();`.
    pub const fn check(self) -> Self {
        let tables = self.pages();
        assert!(tables.base.is_multiple_of(0x1000));
        assert!(tables.within(&self.early) && tables.end() <= self.data.base);
        assert!(self.data.end() <= self.heap.base && self.heap.end() <= self.stack.base);
        assert!(self.stack.end() <= self.image.base && self.image.within(&self.early));
        assert!(self.data.within(&self.early) && self.stack.within(&self.early));
        assert!(self.image.end().is_multiple_of(0x1_0000) && self.image.end() <= 1 << 32);
        assert!(self.image.size >= 0x1000);
        if let Some(ffs) = self.ffs {
            assert!(ffs.within(&self.flash));
        }
        self
    }
}

#[cfg(all(test, feature = "picasso", feature = "zeppelin"))]
mod tests {
    #[test]
    fn cache_windows_cover_the_early_regions() {
        let picasso = crate::picasso::LAYOUT.cache_window();
        assert_eq!((picasso.base, picasso.size), (0x0200_0000, 0x200_0000));
        let zeppelin = crate::zeppelin::layout::LAYOUT.cache_window();
        assert_eq!((zeppelin.base, zeppelin.size), (0x7600_0000, 0x100_0000));
    }
}
