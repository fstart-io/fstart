// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DRAM layout of the reset image and the boot flash it comes from,
//! for every SoC. The PSP trains DRAM, writes the APOB and copies the BIOS
//! reset image (directory entry 0x62) to `image` before releasing the BSP
//! at its last 16 bytes. The arch crate's shared reset entry
//! (`amd_psp_reset.S`) opens a write-back window over the firmware's DRAM
//! ([`Layout::cache_window`]), builds the identity map in `page_tables` and
//! enters long mode with its stack, BSS and heap below the image. The
//! firmware's DRAM stays reserved through the OS handoff: parked APs,
//! tables and DMA buffers live there.
//!
//! A SoC gives the values ([`crate::picasso::LAYOUT`],
//! [`crate::zeppelin::layout::LAYOUT`], [`crate::sp3::layout::LAYOUT`]);
//! the memory map, the boot media mount and the host plan read them.

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

    /// `other` lies within this region.
    pub const fn contains(&self, other: &Region) -> bool {
        other.base >= self.base && other.end() <= self.end()
    }

    pub const fn overlaps(&self, other: &Region) -> bool {
        self.base < other.end() && other.base < self.end()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The firmware's DRAM, the image included: reserved through the OS
    /// handoff.
    pub early: Region,
    /// The PML4 and [`Layout::pdpts`] PDPTs of the reset entry's identity
    /// map.
    pub page_tables: u64,
    /// PDPTs of 1-GiB pages the reset entry builds after its PML4, each
    /// mapping 512 GiB: enough for everything the SoC addresses before
    /// the payload's tables.
    pub pdpts: u64,
    /// BSS and the per-AP stacks (initialized data runs in place in the
    /// image).
    pub data: Region,
    pub heap: Region,
    pub stack: Region,
    /// Where the firmware keeps what it hands the OS (tables, records), if
    /// the SoC has a fixed window for it.
    pub store: Option<Region>,
    /// The PSP-loaded image; its last 4 KiB hold the 16-bit entry.
    pub image: Region,
    /// The boot flash in the view the stage reads its FFS through.
    pub flash: Region,
    /// The window of the FFS with the payload in [`Layout::flash`], if the
    /// image has one.
    pub ffs: Option<Region>,
}

impl Layout {
    pub const fn reset_vector(&self) -> u64 {
        self.image.end() - 0x10
    }

    /// The page tables of the reset entry.
    pub const fn pages(&self) -> Region {
        Region::new(self.page_tables, (self.pdpts + 1) * 0x1000)
    }

    /// The firmware's DRAM below the image: page tables, BSS, heap, stack
    /// and the store.
    pub const fn below_image(&self) -> Region {
        Region::new(self.early.base, self.image.base - self.early.base)
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
        assert!(tables.base.is_multiple_of(0x1000) && self.pdpts >= 1);
        assert!(self.early.contains(&tables) && tables.end() <= self.data.base);
        assert!(self.data.end() <= self.heap.base && self.heap.end() <= self.stack.base);
        assert!(self.stack.end() <= self.image.base && self.early.contains(&self.image));
        assert!(self.early.contains(&self.data) && self.early.contains(&self.stack));
        assert!(self.image.end().is_multiple_of(0x1_0000) && self.image.end() <= 1 << 32);
        assert!(self.image.size >= 0x1000);
        if let Some(store) = self.store {
            assert!(self.below_image().contains(&store) && !store.overlaps(&self.stack));
            assert!(!store.overlaps(&self.heap) && !store.overlaps(&self.data));
        }
        if let Some(ffs) = self.ffs {
            assert!(self.flash.contains(&ffs));
        }
        self
    }
}

/// The host side: the reset image's link contract through the shared
/// reset entry, and the layout descriptor the stage reads.
#[cfg(feature = "host")]
mod host {
    extern crate std;
    use super::{Layout, Region};
    use fstart_core::layout::RegionKind;
    use fstart_image_build::{amd::reset, elf::Expectations, layout::EncodedLayout, plan::Span};
    use std::string::String;

    /// `region` as the image builder's span.
    pub fn span(region: Region) -> Span {
        Span {
            base: region.base,
            size: region.size,
        }
    }

    impl Layout {
        /// The reset image's link contract: the image, the firmware RAM
        /// below it and what lives there.
        pub fn reset(&self) -> reset::Layout {
            reset::Layout {
                image: span(self.image),
                early: span(self.below_image()),
                pages: span(self.pages()),
                data: span(self.data),
                heap: span(self.heap),
                stack: span(self.stack),
                cache: span(self.cache_window()),
            }
        }

        /// The runtime layout descriptor the stage reads: the shared
        /// entry's regions, the flash, the FFS and the store.
        pub fn descriptor(&self) -> Result<EncodedLayout, String> {
            let media: std::vec::Vec<_> = [
                Some(span(self.flash).region(RegionKind::Flash)),
                self.ffs.map(|ffs| span(ffs).region(RegionKind::Firmware)),
                self.store
                    .map(|store| span(store).region(RegionKind::FirmwareStore)),
            ]
            .into_iter()
            .flatten()
            .collect();
            self.reset().descriptor(&media)
        }

        /// The reset image's linker script.
        pub fn linker_script(&self) -> Result<String, String> {
            self.reset().linker_script(&self.descriptor()?)
        }

        /// What the linked reset image must look like.
        pub fn expectations(&self) -> Result<Expectations, String> {
            self.reset().expectations(&self.descriptor()?)
        }
    }
}
#[cfg(feature = "host")]
pub use host::span;

#[cfg(test)]
mod tests {
    #[test]
    fn cache_windows_cover_the_early_regions() {
        let picasso = crate::picasso::LAYOUT.cache_window();
        assert_eq!((picasso.base, picasso.size), (0x0200_0000, 0x200_0000));
        let zeppelin = crate::zeppelin::layout::LAYOUT.cache_window();
        assert_eq!((zeppelin.base, zeppelin.size), (0x7600_0000, 0x100_0000));
        let sp3 = crate::sp3::layout::LAYOUT.cache_window();
        assert_eq!((sp3.base, sp3.size), (0x7500_0000, 0x100_0000));
    }
}
