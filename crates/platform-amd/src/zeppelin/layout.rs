// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin cold-boot DRAM and flash layout.
//!
//! The PSP copies the BIOS reset image (BIOS directory entry 0x62: the last
//! `image.size` bytes of the flash) to `image.base`. The vendor firmware
//! uses 0x76000000-0x76ffffff the same way, which needs at least 2 GiB of
//! DRAM. The fstart FFS sits between the vendor's flash prefix and the reset
//! image.
use crate::noncar::layout::{Layout, Region};

const EARLY: u64 = 0x7600_0000;
const IMAGE: Region = Region::new(0x76db_0000, 0x25_0000);
const FLASH: Region = Region::new(0xff00_0000, 0x100_0000);

/// The vendor EFS, PSP/BIOS directories and signed blobs, kept byte for byte.
pub const VENDOR_PREFIX: u32 = 0x29_4000;
/// Where the PSP copies the reset image from.
pub const RESET_FLASH_OFFSET: u32 = (FLASH.size - IMAGE.size) as u32;

pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY, IMAGE.end() - EARLY),
    page_tables: EARLY,
    data: Region::new(0x7601_0000, 0x7620_0000 - 0x7601_0000),
    heap: Region::new(0x7620_0000, 0x80_0000),
    stack: Region::new(0x76d0_0000, IMAGE.base - 0x76d0_0000),
    image: IMAGE,
    flash: FLASH,
    ffs: Some(Region::new(
        FLASH.base + VENDOR_PREFIX as u64,
        (RESET_FLASH_OFFSET - VENDOR_PREFIX) as u64,
    )),
}
.check();
