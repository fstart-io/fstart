// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD RO/A/B firmware update architecture for SP3 (Rome/Milan) and RS/Siena.
//!
//! ## Flash Layout (within the lower 16 MiB boot window)
//!
//! | Region | SP3 (Tyan S8030) | Siena (Tyan S8040) | Purpose |
//! |---|---|---|---|
//! | PSP partition | `0x00_0000..0x60_0000` (6 MiB) | `0x00_0000..0x80_0000` (8 MiB) | EFS, PSP/BIOS directories, ABL, APCB |
//! | `SLOT_STATE` | `0x60_0000..0x64_0000` (256 KiB, 2×4 KiB ping-pong) | `0x80_0000..0x84_0000` | Monotonic CRC-32C slot journal |
//! | `RW_A` | `0x64_0000..0xac_0000` (4.5 MiB) | `0x84_0000..0xbc_0000` (3.5 MiB) | Signed RW FFS slot A (LZ4 RW stage + microcode + payload) |
//! | `RW_B` | `0xac_0000..0xf4_0000` (4.5 MiB) | `0xbc_0000..0xf4_0000` (3.5 MiB) | Signed RW FFS slot B |
//! | `NVRAM` | `0xf4_0000..0xf8_0000` (256 KiB) | `0xf4_0000..0xf8_0000` (256 KiB) | EFI variable & option store |
//! | `RO` | `0xf8_0000..0x100_0000` (512 KiB) | `0xf8_0000..0x100_0000` (512 KiB) | Immutable PSP BIOS entry `0x62` RO selector |
//!
//! ## DRAM Layout (within the 16 MiB write-back window `0x7500_0000..0x7600_0000`)
//!
//! - `0x7500_0000..`: RO identity page tables (256 PDPTs on SP3; 8 PDPTs on Siena), reused by RW.
//! - `0x75af_0000..0x75af_1000`: [`AB_HANDOFF_ADDR`] — 4 KiB RO -> RW handoff page.
//! - `0x75c8_0000..0x75f8_0000`: RW stage load & execution region (3 MiB, entry `_start_ram`).
//! - `0x75f8_0000..0x7600_0000`: RO reset image copied by PSP (512 KiB, reset vector `0x75ff_fff0`;
//!   `.amd.entry16` with `_sp3_ap_route` stays intact in reserved early RAM for SMU AP startup).

use crate::layout::{Layout, Region};
use crate::sp3::layout::{EARLY_REGION, IMAGE_END, ROM3};

#[cfg(any(feature = "ab-ro", feature = "ab-rw"))]
pub mod handoff;
#[cfg(all(feature = "ab-ro", target_os = "none"))]
pub mod ro;
#[cfg(all(feature = "ab-rw", target_os = "none"))]
pub mod rw;

#[cfg(all(feature = "ab-rw", target_os = "none"))]
pub use handoff::handoff;

/// Size of the immutable RO selector stage (PSP BIOS directory entry `0x62`): 512 KiB.
pub const AB_RO_SIZE: u32 = 0x08_0000;
/// Flash offset of RO at the top of the 16 MiB half (`0xf8_0000..0x100_0000`).
pub const AB_RO_FLASH_OFFSET: u32 = 0x100_0000 - AB_RO_SIZE;
/// DRAM load address of RO (`0x75f8_0000..0x7600_0000`, reset vector `0x75ff_fff0`).
pub const AB_RO_DRAM_BASE: u64 = IMAGE_END - AB_RO_SIZE as u64;

/// Shared EFI NVRAM / option store just below RO (256 KiB, 64 KiB aligned).
pub const AB_NVRAM_SIZE: u32 = 0x04_0000;
pub const AB_NVRAM_OFFSET: u32 = AB_RO_FLASH_OFFSET - AB_NVRAM_SIZE;

/// Dedicated 4 KiB DRAM handoff page between RO and RW (`0x75af_0000..0x75af_1000`).
///
/// Disjoint from SP3's `stack` (`0x75a0_0000..0x75a4_0000`) and `store`
/// (`0x75b0_0000..0x75c0_0000`), and disjoint from Siena's `APOB`
/// (`0x7590_0000..0x75a0_0000`) and RW image (`0x75c8_0000..0x75f8_0000`).
pub const AB_HANDOFF_ADDR: u64 = 0x75af_0000;

/// Fixed DRAM load & execution window for the RW stage (3 MiB below RO).
pub const AB_RW_IMAGE_BASE: u64 = 0x75c8_0000;
pub const AB_RW_IMAGE_SIZE: u64 = 0x30_0000;

/// Disjoint 5 MiB early-RAM scratch window used by RO while reading and
/// SHA-256-verifying the compressed RW stage before decompressing it into
/// [`AB_RW_IMAGE_BASE`]..[`AB_RO_DRAM_BASE`].
///
/// Lies strictly above both SP3 RO (`0x7500_0000..0x752c_0000`) and Siena RO
/// (`0x7500_0000..0x750d_0000`) and strictly below Siena's APOB (`0x7590_0000`).
pub const AB_RW_SCRATCH_BASE: u64 = 0x7530_0000;
pub const AB_RW_SCRATCH_SIZE: u64 = 0x50_0000;

/// TPM 2.0 NV index holding the monotonic anti-rollback floor when a discrete
/// FIFO TPM 2.0 is fitted.
pub const AB_ROLLBACK_NV_INDEX: u32 = 0x0140_0001;

// ---------------------------------------------------------------------------
// SP3 (Tyan S8030) flash slot geometry
// ---------------------------------------------------------------------------

pub const SP3_AB_STATE_SECTOR_0: u32 = 0x60_0000;
pub const SP3_AB_STATE_SECTOR_1: u32 = 0x60_1000;
pub const SP3_AB_SLOT_A_OFFSET: u32 = 0x64_0000;
pub const SP3_AB_SLOT_SIZE: u32 = 0x48_0000;
pub const SP3_AB_SLOT_B_OFFSET: u32 = SP3_AB_SLOT_A_OFFSET + SP3_AB_SLOT_SIZE;

// ---------------------------------------------------------------------------
// Siena (Tyan S8040) flash slot geometry
// ---------------------------------------------------------------------------

pub const SIENA_AB_STATE_SECTOR_0: u32 = 0x80_0000;
pub const SIENA_AB_STATE_SECTOR_1: u32 = 0x80_1000;
pub const SIENA_AB_SLOT_A_OFFSET: u32 = 0x84_0000;
pub const SIENA_AB_SLOT_SIZE: u32 = 0x38_0000;
pub const SIENA_AB_SLOT_B_OFFSET: u32 = SIENA_AB_SLOT_A_OFFSET + SIENA_AB_SLOT_SIZE;

// ---------------------------------------------------------------------------
// RO stage DRAM layouts (linked through amd_psp_reset.S)
// ---------------------------------------------------------------------------

/// SP3 RO selector stage layout: 512 KiB image at `0x75f8_0000..0x7600_0000`,
/// 256 PDPTs at `0x7500_0000` (matching SP3 RW's page table reservation),
/// small scratch BSS/heap/stack at `0x7520_0000..0x752c_0000`.
pub const SP3_RO_LAYOUT: Layout = Layout {
    early: Region::new(EARLY_REGION.base, IMAGE_END - EARLY_REGION.base),
    page_tables: EARLY_REGION.base,
    pdpts: 256,
    data: Region::new(0x7520_0000, 0x4_0000),
    heap: Region::new(0x7524_0000, 0x4_0000),
    stack: Region::new(0x7528_0000, 0x4_0000),
    store: None,
    image: Region::new(AB_RO_DRAM_BASE, AB_RO_SIZE as u64),
    flash: ROM3,
    ffs: None,
}
.check();

/// Siena RO selector stage layout: 512 KiB image at `0x75f8_0000..0x7600_0000`,
/// 8 PDPTs at `0x7500_0000` (matching Siena RW's page table reservation),
/// small scratch BSS/heap/stack at `0x7501_0000..0x750d_0000` (well below
/// Siena's APOB at `0x7590_0000`).
pub const SIENA_RO_LAYOUT: Layout = Layout {
    early: Region::new(EARLY_REGION.base, IMAGE_END - EARLY_REGION.base),
    page_tables: EARLY_REGION.base,
    pdpts: 8,
    data: Region::new(0x7501_0000, 0x4_0000),
    heap: Region::new(0x7505_0000, 0x4_0000),
    stack: Region::new(0x7509_0000, 0x4_0000),
    store: None,
    image: Region::new(AB_RO_DRAM_BASE, AB_RO_SIZE as u64),
    flash: Region::new(fstart_amdfw_format::ROM2_BASE, 0x100_0000),
    ffs: None,
}
.check();

// Compile-time invariants covering both flash and DRAM layouts.
const _: () = {
    // Flash geometry: PSP -> SLOT_STATE -> RW_A -> RW_B -> NVRAM -> RO == 16 MiB.
    assert!(AB_RO_FLASH_OFFSET + AB_RO_SIZE == 0x100_0000);
    assert!(AB_NVRAM_OFFSET + AB_NVRAM_SIZE == AB_RO_FLASH_OFFSET);
    // 64 KiB alignment for FCH RomProtect ranges.
    assert!(SP3_AB_STATE_SECTOR_0.is_multiple_of(0x1_0000));
    assert!(SIENA_AB_STATE_SECTOR_0.is_multiple_of(0x1_0000));
    assert!(AB_NVRAM_OFFSET.is_multiple_of(0x1_0000));
    assert!(AB_RO_FLASH_OFFSET.is_multiple_of(0x1_0000));
    // SP3 slots fit cleanly between PSP (0x60_0000) and NVRAM (0xf4_0000).
    assert!(SP3_AB_STATE_SECTOR_0 == crate::sp3::layout::FFS_OFFSET);
    assert!(SP3_AB_STATE_SECTOR_1 == SP3_AB_STATE_SECTOR_0 + 0x1000);
    assert!(SP3_AB_SLOT_A_OFFSET >= SP3_AB_STATE_SECTOR_1 + 0x1000);
    assert!(SP3_AB_SLOT_B_OFFSET + SP3_AB_SLOT_SIZE == AB_NVRAM_OFFSET);
    // Siena slots fit cleanly between PSP (0x80_0000) and NVRAM (0xf4_0000).
    assert!(SIENA_AB_STATE_SECTOR_0 == crate::siena::PSP_PARTITION_END);
    assert!(SIENA_AB_STATE_SECTOR_1 == SIENA_AB_STATE_SECTOR_0 + 0x1000);
    assert!(SIENA_AB_SLOT_A_OFFSET >= SIENA_AB_STATE_SECTOR_1 + 0x1000);
    assert!(SIENA_AB_SLOT_B_OFFSET + SIENA_AB_SLOT_SIZE == AB_NVRAM_OFFSET);
    // DRAM: RW image lies below RO, above SP3 store and Siena APOB & handoff.
    assert!(AB_RW_IMAGE_BASE + AB_RW_IMAGE_SIZE == AB_RO_DRAM_BASE);
    assert!(AB_HANDOFF_ADDR >= crate::sp3::layout::LAYOUT.stack.end());
    assert!(AB_HANDOFF_ADDR + 0x1000 <= crate::sp3::layout::LAYOUT.store.unwrap().base);
    assert!(crate::sp3::layout::LAYOUT.store.unwrap().end() <= AB_RW_IMAGE_BASE);
    assert!(AB_HANDOFF_ADDR >= crate::siena::LAYOUT.stack.end());
    assert!(AB_HANDOFF_ADDR >= 0x75a0_0000); // Siena APOB ends at 0x75a0_0000
    assert!(AB_HANDOFF_ADDR + 0x1000 <= AB_RW_IMAGE_BASE);
    // Disjoint RO decompression scratch lies inside the 16 MiB early WB window,
    // strictly above both RO stacks and strictly below Siena's APOB (`0x7590_0000`).
    assert!(AB_RW_SCRATCH_BASE >= SP3_RO_LAYOUT.stack.end());
    assert!(AB_RW_SCRATCH_BASE >= SIENA_RO_LAYOUT.stack.end());
    assert!(AB_RW_SCRATCH_BASE + AB_RW_SCRATCH_SIZE <= 0x7590_0000);
    assert!(AB_RW_SCRATCH_SIZE >= SP3_AB_SLOT_SIZE as u64);
    assert!(AB_RW_SCRATCH_SIZE >= SIENA_AB_SLOT_SIZE as u64);
};

/// Select a 64-byte lower-half probe offset and expected bytes for SPI100
/// `Flash::new` address-mode verification.
///
/// Probes `Slot::A` -> `Slot::B` -> lower-half EFS (`0x02_0000`) rather than
/// `AB_RO_FLASH_OFFSET` (`0xf8_0000`) so any 3-byte/4-byte addressing mismatch
/// against flash contents in the upper 16 MiB of a 32 MiB SPI part is caught
/// cleanly before any SPI write occurs.
#[cfg(all(any(feature = "ab-ro", feature = "ab-rw"), target_os = "none"))]
pub(crate) fn lower_half_probe(
    layout: fstart_ffs::slot_state::SlotLayout,
    rom_window_base: u64,
) -> (u32, [u8; 64]) {
    use fstart_core::services::BootMedia;
    use fstart_core::services::boot_media::MemoryMapped;
    use fstart_ffs::slot_state::Slot;

    const EFS_OFFSET: u32 = 0x02_0000;
    const LOWER_HALF_SIZE: usize = 0x100_0000;

    // SAFETY: lower 16 MiB ROM window is identity-mapped read-only flash.
    let window = unsafe { MemoryMapped::from_raw_addr(rom_window_base, LOWER_HALF_SIZE) };
    let candidates = [
        layout.extent(Slot::A).map(|e| e.offset),
        layout.extent(Slot::B).map(|e| e.offset),
        Some(EFS_OFFSET),
    ];
    let mut fallback = [0u8; 64];
    for offset in candidates.into_iter().flatten() {
        let mut buf = [0u8; 64];
        if window.read_at(offset as usize, &mut buf).is_ok() {
            if !buf.iter().all(|&b| b == 0xff) && !buf.iter().all(|&b| b == 0x00) {
                return (offset, buf);
            }
            if offset == EFS_OFFSET {
                fallback = buf;
            }
        }
    }
    (EFS_OFFSET, fallback)
}

#[cfg(all(any(feature = "ab-ro", feature = "ab-rw"), target_os = "none"))]
pub(crate) fn tpm_err_parts(err: fstart_driver_tpm::Error) -> (&'static str, u32) {
    match err {
        fstart_driver_tpm::Error::Timeout => ("timeout", 0),
        fstart_driver_tpm::Error::Locality => ("locality", 0),
        fstart_driver_tpm::Error::Protocol => ("protocol", 0),
        fstart_driver_tpm::Error::Response => ("response", 0),
        fstart_driver_tpm::Error::Code(code) => ("code", code),
    }
}

#[cfg(feature = "host")]
pub mod host {
    extern crate std;
    use super::*;
    use crate::layout::Layout;
    use fstart_core::layout::RegionKind;
    use fstart_image_build::{
        amd::reset::RwLayout, capsule::AbSlotLayout, elf::Expectations, layout::EncodedLayout,
        plan::Span,
    };
    use std::string::String;

    const fn span(r: Region) -> Span {
        Span {
            base: r.base,
            size: r.size,
        }
    }

    pub const fn sp3_slot_layout() -> AbSlotLayout {
        AbSlotLayout::RoAB {
            state_sector_0: SP3_AB_STATE_SECTOR_0,
            state_sector_1: SP3_AB_STATE_SECTOR_1,
            slot_a_offset: SP3_AB_SLOT_A_OFFSET,
            slot_a_size: SP3_AB_SLOT_SIZE,
            slot_b_offset: SP3_AB_SLOT_B_OFFSET,
            slot_b_size: SP3_AB_SLOT_SIZE,
        }
    }

    pub const fn siena_slot_layout() -> AbSlotLayout {
        AbSlotLayout::RoAB {
            state_sector_0: SIENA_AB_STATE_SECTOR_0,
            state_sector_1: SIENA_AB_STATE_SECTOR_1,
            slot_a_offset: SIENA_AB_SLOT_A_OFFSET,
            slot_a_size: SIENA_AB_SLOT_SIZE,
            slot_b_offset: SIENA_AB_SLOT_B_OFFSET,
            slot_b_size: SIENA_AB_SLOT_SIZE,
        }
    }

    /// Build the [`RwLayout`] link contract for an AMD RW stage derived from
    /// its SoC's base [`Layout`] (reusing RO's page tables and early RAM, with
    /// the stage binary placed at [`AB_RW_IMAGE_BASE`]..[`AB_RO_DRAM_BASE`]).
    pub const fn rw_link_layout(base: &Layout) -> RwLayout {
        RwLayout {
            image: Span {
                base: AB_RW_IMAGE_BASE,
                size: AB_RW_IMAGE_SIZE,
            },
            early: Span {
                base: base.early.base,
                size: AB_RW_IMAGE_BASE - base.early.base,
            },
            pages: span(base.pages()),
            data: span(base.data),
            heap: span(base.heap),
            stack: span(base.stack),
        }
    }

    pub fn rw_descriptor(base: &Layout) -> Result<EncodedLayout, String> {
        let extra: std::vec::Vec<_> = [
            Some(span(base.flash).region(RegionKind::Flash)),
            base.store
                .map(|store| span(store).region(RegionKind::FirmwareStore)),
        ]
        .into_iter()
        .flatten()
        .collect();
        rw_link_layout(base).descriptor(&extra)
    }

    pub fn rw_linker_script(base: &Layout) -> Result<String, String> {
        rw_link_layout(base).linker_script(&rw_descriptor(base)?)
    }

    pub fn rw_expectations(base: &Layout) -> Result<Expectations, String> {
        rw_link_layout(base).expectations(&rw_descriptor(base)?)
    }
}
