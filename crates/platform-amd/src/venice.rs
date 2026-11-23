// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Venice (Family 1Ah models 50h–5Fh) in the SP7 socket: the WH I/O die
//! with Zen 6 CCDs and Qilian FCH ([`crate::sp7`]).

use crate::layout::{Layout, Region};

/// The Venice die (WH I/O die).
pub enum Wh {}

/// Early firmware DRAM immediately below the PSP reset image (`0x7000_0000`,
/// verified from SP7 `F01` BIOS directory entry `0x62`).
const EARLY: u64 = 0x6e00_0000;
const IMAGE: Region = Region::new(0x7000_0000, 0x30_0000);
/// Top 16 MiB ROM2 window below 4 GiB (Bank 0 of the 64 MiB SPI flash,
/// `bios_origin = 0`, carrying the FFS and x86 reset image; the ISH A/B
/// PSP partition lives in Bank 1 at `image_origin = 0x0100_0000`).
const FLASH: Region = Region::new(0xff00_0000, 0x100_0000);
/// Reserved lower half of the 16 MiB ROM2 window for FFS media layout.
pub const PSP_PARTITION_END: u32 = 0x80_0000;
/// End of the FFS window within the 16 MiB ROM2 mapping.
pub const FFS_END_OFFSET: u32 = (FLASH.size - IMAGE.size) as u32;

/// The firmware's DRAM below the PSP's APOB V2 (`0x7700_0000`, verified from
/// SP7 `F01` BIOS directory entry `0x61`).
pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY, IMAGE.end() - EARLY),
    page_tables: EARLY,
    pdpts: 8,
    data: Region::new(0x6e01_0000, 0x3f_0000),
    heap: Region::new(0x6e40_0000, 0x1b0_0000),
    stack: Region::new(0x6ff0_0000, 0x10_0000),
    store: None,
    image: IMAGE,
    flash: FLASH,
    ffs: Some(Region::new(
        FLASH.base + PSP_PARTITION_END as u64,
        (FFS_END_OFFSET - PSP_PARTITION_END) as u64,
    )),
}
.check();

impl crate::sp7::Variant for Wh {
    const NAME: &'static str = "venice";
    /// Family 1Ah WH CPUID signatures observed in Venice firmware (`0x00B5_0F00` A0,
    /// `0x00B5_0F10` B0, `0x00BC_0F00` Zen 6c).
    const SIGNATURES: &'static [u32] = &[0x00b5_0f00, 0x00b5_0f10, 0x00bc_0f00];
    const LAYOUT: Layout = LAYOUT;
    /// Base physical address where the PSP writes the APOB V2 (BIOS `0x61`).
    const APOB_BASE: u64 = 0x7700_0000;
    /// Maximum APOB V2 byte length accepted from DRAM (actual reservation is
    /// page-rounded from `apob.total_size()`).
    const APOB_MAX_SIZE: u64 = 4 << 20;
    /// Physical SPI flash size reported to SMBIOS Type 0 (64 MiB part).
    const ROM_SIZE: u32 = 64 << 20;
    /// The ACPI PM base used by Qilian FCH.
    const PMBASE: u16 = 0x800;
}

/// The Venice SoC.
pub type Venice = crate::sp7::Sp7<Wh>;
