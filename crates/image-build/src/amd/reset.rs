// SPDX-License-Identifier: MIT OR Apache-2.0
//! Link and audit contract of an image entered through the shared AMD
//! PSP-copied reset entry (the arch crate's `asm/amd_psp_reset.S`): SP3,
//! Picasso and Zeppelin. The PSP copies BIOS directory entry 0x62 to
//! `image` and releases the BSP at its last 16 bytes; text, read-only data
//! and `.data` run there in place. Page tables, BSS, heap and stack live in
//! the firmware RAM below it. No flash geometry, firmware selection or
//! silicon policy.
use crate::elf::{Architecture, Descriptor, Expectations};
use crate::layout::EncodedLayout;
use crate::plan::Span;
use fstart_core::layout::{Region, RegionKind};
use std::collections::BTreeMap;

/// The linker template of the shared reset entry.
pub const TEMPLATE: &str = include_str!("../../linker/amd_psp_reset.ld.in");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The reset image at its PSP copy destination; ends on a 64-KiB
    /// boundary below 4 GiB (its last 4 KiB hold the 16-bit entry).
    pub image: Span,
    /// The firmware RAM below the image, reserved through the OS handoff.
    pub early: Span,
    /// The PML4 and PDPTs the entry builds, page aligned.
    pub pages: Span,
    /// BSS.
    pub data: Span,
    pub heap: Span,
    pub stack: Span,
    /// The write-back MTRR the entry opens over the firmware RAM and the
    /// image before it writes anything: a naturally aligned power of two.
    pub cache: Span,
}

impl Layout {
    pub fn validate(&self) -> Result<(), String> {
        let firmware = [self.image, self.pages, self.data, self.heap, self.stack];
        for r in firmware.iter().chain([&self.early]) {
            r.end()?;
        }
        for (i, a) in firmware.iter().enumerate() {
            if firmware[i + 1..].iter().any(|b| a.overlaps(*b)) {
                return Err("AMD reset image regions overlap".into());
            }
        }
        let end = self.image.end()?;
        if self.image.size < 0x1000 || end > 1 << 32 || end & 0xffff != 0 {
            return Err(
                "AMD reset image needs its entry page and a 64-KiB boundary below 4 GiB".into(),
            );
        }
        if self.early.end()? > self.image.base {
            return Err("AMD reset firmware RAM must lie below the image".into());
        }
        if self.pages.base & 0xfff != 0 || self.pages.size & 0xfff != 0 {
            return Err("AMD reset page tables must be page aligned".into());
        }
        for r in [self.pages, self.data, self.heap, self.stack] {
            if !self.early.contains(r.base, r.size) {
                return Err(
                    "AMD reset page tables, BSS, heap or stack outside firmware RAM".into(),
                );
            }
        }
        if self.data.end()? > self.heap.base || self.heap.end()? > self.stack.base {
            return Err("AMD reset data/heap/stack order".into());
        }
        let cache = self.cache;
        if !cache.size.is_power_of_two()
            || cache.base & (cache.size - 1) != 0
            || cache.end()? > 1 << 32
            || !cache.contains(self.early.base, end - self.early.base)
        {
            return Err(
                "AMD reset cache window must be an aligned power of two below 4 GiB over the firmware RAM and the image".into(),
            );
        }
        if self.pages.size < 0x2000 {
            return Err("AMD reset page tables need a PML4 and a PDPT".into());
        }
        Ok(())
    }

    /// Where the PSP releases the BSP: the image's last 16 bytes.
    pub fn reset_vector(&self) -> Result<u64, String> {
        Ok(self.image.end()? - 0x10)
    }

    /// The runtime layout descriptor the stage reads: the image, the
    /// firmware's writable RAM, stack and heap, then the platform's `extra`
    /// regions (flash, FFS, stores), then the page tables as an exclusion.
    pub fn descriptor(&self, extra: &[Region]) -> Result<EncodedLayout, String> {
        self.validate()?;
        let writable = Span {
            base: self.data.base,
            size: self.stack.end()? - self.data.base,
        };
        let regions: Vec<Region> = [
            self.image.region(RegionKind::Image),
            writable.region(RegionKind::Writable),
            self.stack.region(RegionKind::Stack),
            self.heap.region(RegionKind::Heap),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .chain([self.pages.region(RegionKind::Reserved)])
        .collect();
        EncodedLayout::encode(0, &regions).map_err(|e| format!("AMD reset layout: {e:?}"))
    }

    /// The image's linker script, with `descriptor` in its layout section.
    pub fn linker_script(&self, descriptor: &EncodedLayout) -> Result<String, String> {
        self.validate()?;
        crate::linker::fill(
            TEMPLATE,
            &[
                ("IMAGE_BASE", self.image.base),
                ("IMAGE_SIZE", self.image.size),
                ("IMAGE_END", self.image.end()?),
                ("PT_BASE", self.pages.base),
                ("PT_SIZE", self.pages.size),
                ("DATA_BASE", self.data.base),
                ("DATA_SIZE", self.data.size),
                ("HEAP_BASE", self.heap.base),
                ("HEAP_SIZE", self.heap.size),
                ("STACK_BASE", self.stack.base),
                ("STACK_SIZE", self.stack.size),
                ("CACHE_BASE", self.cache.base),
                ("CACHE_MASK", !(self.cache.size - 1) & 0xffff_ffff),
                ("PDPTS", self.pages.size / 0x1000 - 1),
            ],
            descriptor,
            "IMAGE",
        )
    }

    /// What the linked image must look like: stored exactly as the reset
    /// image, running there and in the firmware RAM, identity mapped,
    /// entered at the reset vector.
    pub fn expectations(&self, descriptor: &EncodedLayout) -> Result<Expectations, String> {
        self.validate()?;
        let end = self.image.end()?;
        let expectations = Expectations {
            architecture: Architecture::X86_64,
            elf64: true,
            little_endian: true,
            stored: vec![self.image],
            runtime: vec![self.image, self.early],
            identity_mapping: true,
            entry: Some(end - 0x10),
            copy: None,
            descriptor: Some(Descriptor {
                section: ".fstart.layout".into(),
                start_symbol: "_fstart_layout_start".into(),
                end_symbol: "_fstart_layout_end".into(),
                bytes: descriptor.as_bytes().to_vec(),
                reservation: self.image,
            }),
            stored_flags: None,
            symbols: BTreeMap::from([
                ("_sp3_reset_vector".into(), end - 0x10),
                ("_sp3_entry16".into(), end - 0x1000),
                ("_sp3_page_tables".into(), self.pages.base),
            ]),
        };
        expectations.validate()?;
        Ok(expectations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn span(base: u64, size: u64) -> Span {
        Span { base, size }
    }

    fn layout() -> Layout {
        Layout {
            image: span(0x76d0_0000, 0x30_0000),
            early: span(0x7600_0000, 0xd0_0000),
            pages: span(0x7600_0000, 0x2000),
            data: span(0x7601_0000, 0xf_0000),
            heap: span(0x7610_0000, 0x60_0000),
            stack: span(0x7670_0000, 0x10_0000),
            cache: span(0x7600_0000, 0x100_0000),
        }
    }

    #[test]
    fn contract_places_the_entry_and_the_descriptor() {
        let l = layout();
        let flash = span(0xff00_0000, 0x100_0000).region(RegionKind::Flash);
        let descriptor = l.descriptor(&[flash]).unwrap();
        let script = l.linker_script(&descriptor).unwrap();
        assert!(!script.contains('@'));
        assert!(script.contains("ORIGIN = 0x76d00000, LENGTH = 0x300000"));
        assert!(script.contains("_fstart_layout_start"));
        let expected = l.expectations(&descriptor).unwrap();
        assert_eq!(expected.entry, Some(0x76ff_fff0));
        assert_eq!(expected.symbols["_sp3_entry16"], 0x76ff_f000);
        assert_eq!(expected.runtime[1], l.early);
    }

    #[test]
    fn rejects_overlap_misplacement_and_misalignment() {
        assert!(layout().validate().is_ok());
        for bad in [
            Layout {
                pages: layout().image,
                ..layout()
            },
            Layout {
                heap: layout().data,
                ..layout()
            },
            Layout {
                stack: span(u64::MAX, 1),
                ..layout()
            },
            // No entry page, or not ending on a 64-KiB boundary.
            Layout {
                image: span(0x76ff_f800, 0x800),
                ..layout()
            },
            Layout {
                image: span(0x76d0_0000, 0x30_1000),
                ..layout()
            },
            // Firmware RAM overlapping the image.
            Layout {
                early: span(0x7600_0000, 0xd1_0000),
                ..layout()
            },
            Layout {
                pages: span(0x7600_0001, 0x2000),
                ..layout()
            },
            Layout {
                stack: span(0x75f0_0000, 0x10_0000),
                ..layout()
            },
        ] {
            assert!(bad.validate().is_err());
            assert!(bad.descriptor(&[]).is_err());
        }
    }
}
