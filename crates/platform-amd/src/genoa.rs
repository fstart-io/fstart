// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Genoa (EPYC 9004) in the SP5 socket: the RS I/O die with Zen 4
//! CCDs ([`crate::rs`]).
use crate::layout::{Layout, Region};
use fstart_driver_amd::df::v4::{IOS, Ios};
use fstart_driver_amd::smn::{SmnAccess, SmnReg};
use tock_registers::interfaces::Readable;

/// The Genoa die.
pub enum Stones {}

const EARLY: u64 = 0x0800_0000;
const IMAGE: Region = Region::new(0x0a00_0000, 0x30_0000);
const FLASH: Region = Region::new(0xff00_0000, 0x100_0000);
/// The end of the PSP partition (`[package.metadata.fstart.amd.genoa]`).
pub const PSP_PARTITION_END: u32 = 0x80_0000;
/// Where the PSP copies the reset image from: the end of the 16 MiB the
/// boot ROM maps.
pub const RESET_FLASH_OFFSET: u32 = (FLASH.size - IMAGE.size) as u32;
/// The fuse word of the Genoa parts: none has an IOS fused off. Its
/// layout is not Siena's, and a part with any bit set is not supported.
const IOS_FUSES: SmnReg = SmnReg::new(0x5d280);

/// The firmware's DRAM above the PSP's APOB (0x0700_1000; the PSP writes
/// up to 0x3DF58 bytes there).
pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY, IMAGE.end() - EARLY),
    page_tables: EARLY,
    pdpts: 8,
    data: Region::new(0x0801_0000, 0x3f_0000),
    heap: Region::new(0x0840_0000, 0x1b0_0000),
    stack: Region::new(0x09f0_0000, 0x10_0000),
    store: None,
    image: IMAGE,
    flash: FLASH,
    ffs: Some(Region::new(
        FLASH.base + PSP_PARTITION_END as u64,
        (RESET_FLASH_OFFSET - PSP_PARTITION_END) as u64,
    )),
}
.check();

impl crate::rs::Variant for Stones {
    const NAME: &'static str = "genoa";
    /// Family 19h model 11h, the RS cores.
    const SIGNATURES: &'static [u32] = &[0x00a1_0f10];
    const LAYOUT: Layout = LAYOUT;
    const APOB: Region = Region::new(0x0700_1000, 0x4_0000);
    /// Genoa's SMU firmware (its sender at `2005ea27`) rejects IDs above
    /// 0x47.
    const SMU_ID_LIMIT: u32 = 0x48;
    /// Where the PSP leaves the ACPI blocks.
    const PMBASE: u16 = 0x400;
    const SOCKET: &'static str = "SP5 Socket0";

    fn ios(smn: &impl SmnAccess) -> Option<heapless::Vec<Ios, 4>> {
        (smn.reg(IOS_FUSES).get() == 0).then(|| IOS.into_iter().collect())
    }
}

/// The Genoa SoC.
pub type Genoa = crate::rs::Rs<Stones>;
