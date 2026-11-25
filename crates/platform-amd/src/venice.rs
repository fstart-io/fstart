// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Venice (Family 1Ah models 50h–5Fh classic / C0h–CFh dense) in the SP7
//! socket: the WH dual-IOD platform ([`crate::sp7`]).

use crate::layout::{Layout, Region};
use fstart_driver_amd::ccx::wh as ccx_wh;

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

/// Every stepping-masked CPUID(1).EAX base signature admitted for Venice:
/// Family 1Ah models 50h–5Fh (Zen 6 classic) and C0h–CFh (Zen 6c dense).
///
/// Shared by early SoC dispatch ([`matches_signature`]), `sp7::init::enter`,
/// and [`fstart_driver_amd::ccx::mp::Fam17hCpu::matches`] so no CPU can pass
/// early admission and then fail MP driver matching.
pub const WH_SIGNATURES: [u32; 32] = [
    0x00b5_0f00,
    0x00b5_0f10,
    0x00b5_0f20,
    0x00b5_0f30,
    0x00b5_0f40,
    0x00b5_0f50,
    0x00b5_0f60,
    0x00b5_0f70,
    0x00b5_0f80,
    0x00b5_0f90,
    0x00b5_0fa0,
    0x00b5_0fb0,
    0x00b5_0fc0,
    0x00b5_0fd0,
    0x00b5_0fe0,
    0x00b5_0ff0,
    0x00bc_0f00,
    0x00bc_0f10,
    0x00bc_0f20,
    0x00bc_0f30,
    0x00bc_0f40,
    0x00bc_0f50,
    0x00bc_0f60,
    0x00bc_0f70,
    0x00bc_0f80,
    0x00bc_0f90,
    0x00bc_0fa0,
    0x00bc_0fb0,
    0x00bc_0fc0,
    0x00bc_0fd0,
    0x00bc_0fe0,
    0x00bc_0ff0,
];

/// Single Venice CPUID(1).EAX admission predicate used by early SoC dispatch
/// and SP7 initialization.
pub const fn matches_signature(signature: u32) -> bool {
    if ccx_wh::Revision::from_signature(signature).is_none() {
        return false;
    }
    let masked = signature & !0xf;
    let mut i = 0;
    while i < WH_SIGNATURES.len() {
        if WH_SIGNATURES[i] == masked {
            return true;
        }
        i += 1;
    }
    false
}

/// The firmware's DRAM below the PSP's APOB V2 (`0x7700_0000`, verified from
/// SP7 `F01` BIOS directory entry `0x61`).
pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY, IMAGE.end() - EARLY),
    page_tables: EARLY,
    // 4 TiB identity-mapped. Above it are ROM3 on the MD25-CG0
    // (`0x2FFD_0000_0000`: `sp7::flash` reads through SMN instead) and the
    // DF plan's 64-bit MMIO up to its `above_cap` (about 8 TiB), where BARs
    // may land: firmware touches neither through this map today.
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
    const SIGNATURES: &'static [u32] = &WH_SIGNATURES;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp7::Variant as _;

    #[test]
    fn admission_predicate_matches_mp_signature_table_exactly() {
        for model in 0x50u8..=0x5f {
            let base =
                0x00b0_0f00u32 | (u32::from(model >> 4) << 16) | (u32::from(model & 0xf) << 4);
            for stepping in 0u32..=0xf {
                let sig = base | stepping;
                assert!(matches_signature(sig), "classic sig {sig:#x}");
                assert!(Wh::SIGNATURES.contains(&(sig & !0xf)));
            }
        }
        for model in 0xc0u8..=0xcf {
            let base =
                0x00b0_0f00u32 | (u32::from(model >> 4) << 16) | (u32::from(model & 0xf) << 4);
            for stepping in 0u32..=0xf {
                let sig = base | stepping;
                assert!(matches_signature(sig), "dense sig {sig:#x}");
                assert!(Wh::SIGNATURES.contains(&(sig & !0xf)));
            }
        }
        // Non-Venice CPUs and signatures with nonzero reserved bits are rejected.
        assert!(!matches_signature(0x00a1_0f10)); // Genoa
        assert!(!matches_signature(0x00b0_0f21)); // Turin
        assert!(!matches_signature(0x10b5_0f00)); // reserved bits set
        assert!(!matches_signature(0x00b5_1f00)); // processor type bits set
    }
}
