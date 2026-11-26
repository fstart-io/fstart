// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Siena (EPYC 8004) in the SP6 socket: the RS I/O die with Zen 4c
//! CCDs ([`crate::rs`]), a part of which may have IOS fused off.
use crate::layout::{Layout, Region};
use fstart_driver_amd::df::v4::{self, Ios};
use fstart_driver_amd::smn::SmnAccess;
use tock_registers::interfaces::Readable;

/// The Siena die.
pub enum Dense {}

const EARLY: u64 = 0x7500_0000;
const IMAGE: Region = Region::new(0x75d0_0000, 0x30_0000);
const FLASH: Region = Region::new(0xff00_0000, 0x100_0000);
/// The end of the PSP partition (`[package.metadata.fstart.amd.siena]`):
/// 8 MiB as Genoa's. What the table packs ends below 3 MiB; the vendor
/// image's partition runs to 10 MiB, which left the FFS 3 MiB.
pub const PSP_PARTITION_END: u32 = 0x80_0000;
/// Where the PSP copies the reset image from: the end of the 16 MiB the
/// boot ROM maps.
pub const RESET_FLASH_OFFSET: u32 = (FLASH.size - IMAGE.size) as u32;
/// fstart's EFI variable store, outside both the FFS and PSP partition.
pub const NVRAM: Region = Region::new(FLASH.base + RESET_FLASH_OFFSET as u64 - 0x4_0000, 0x4_0000);
/// Warm-reset-preserved RAM buffer for CrabEFI deferred runtime variable
/// writes (128 KiB, inside `LAYOUT.early` between the stack and APOB).
pub const DEFERRED_BUFFER: Region = Region::new(0x7580_0000, 0x2_0000);
const _: () = assert!(NVRAM.base >= FLASH.base + PSP_PARTITION_END as u64);

/// The firmware's DRAM where the S8040's vendor firmware has its own: the
/// PSP's APOB at 0x7590_0000 (the BIOS directory's entry 0x61), the reset
/// image at the top, page tables, data, heap and stack below the APOB.
pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY, IMAGE.end() - EARLY),
    page_tables: EARLY,
    pdpts: 8,
    data: Region::new(0x7501_0000, 0x3f_0000),
    heap: Region::new(0x7540_0000, 0x30_0000),
    stack: Region::new(0x7570_0000, 0x10_0000),
    store: None,
    image: IMAGE,
    flash: FLASH,
    ffs: Some(Region::new(
        FLASH.base + PSP_PARTITION_END as u64,
        NVRAM.base - (FLASH.base + PSP_PARTITION_END as u64),
    )),
}
.check();

const _: () = {
    assert!(DEFERRED_BUFFER.base >= LAYOUT.stack.end());
    assert!(DEFERRED_BUFFER.end() <= 0x7590_0000);
    assert!(DEFERRED_BUFFER.base >= LAYOUT.early.base);
    assert!(DEFERRED_BUFFER.end() <= LAYOUT.early.end());
    assert!(DEFERRED_BUFFER.base.is_multiple_of(0x1000));
    assert!(DEFERRED_BUFFER.size.is_multiple_of(0x1000));
};

impl crate::rs::Variant for Dense {
    const NAME: &'static str = "siena";
    /// Family 19h model A0h, the RSDN cores.
    const SIGNATURES: &'static [u32] = &[0x00aa_0f00];
    const LAYOUT: Layout = LAYOUT;
    const APOB: Region = Region::new(0x7590_0000, 0x10_0000);
    const NVRAM: Option<Region> = Some(NVRAM);
    const DEFERRED_BUFFER: Option<Region> = Some(DEFERRED_BUFFER);
    /// Siena's SMU firmware rejects, rather than silently completing, IDs
    /// from 0x51 on.
    const SMU_ID_LIMIT: u32 = 0x51;
    /// The NFX FchSongshanPei's (`ffa512d8`).
    const PMBASE: u16 = 0x800;
    const SOCKET: &'static str = "SP6 Socket0";

    fn ios(smn: &impl SmnAccess) -> Option<heapless::Vec<Ios, 4>> {
        v4::present_ios(smn.reg(v4::IOS_DISABLE).get())
    }
}

/// The Siena SoC.
pub type Siena = crate::rs::Rs<Dense>;
