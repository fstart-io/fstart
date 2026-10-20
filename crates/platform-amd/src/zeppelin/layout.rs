// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin cold-boot DRAM and flash layout.
//!
//! The PSP trains DRAM, writes the APOB to `APOB_BASE` and copies the BIOS
//! reset image (BIOS directory entry 0x62: flash `RESET_FLASH_OFFSET`,
//! `IMAGE_SIZE` bytes) to `IMAGE_BASE` before releasing the BSP at its last
//! 16 bytes. The vendor firmware uses 0x76000000-0x76ffffff the same way,
//! which needs at least 2 GiB of DRAM. Everything from `EARLY_START` to
//! `EARLY_END` stays reserved through OS handoff (parked APs, tables).

pub const EARLY_START: u64 = 0x7600_0000;
/// Six pages: PML4, PDPT and four page directories for the 4-GiB identity map.
pub const PAGE_TABLES: u64 = EARLY_START;
pub const PAGE_TABLES_END: u64 = PAGE_TABLES + 0x6000;
/// Writable data and BSS.
pub const DATA_START: u64 = 0x7601_0000;
pub const HEAP_START: u64 = 0x7620_0000;
pub const HEAP_SIZE: u64 = 0x0080_0000;
pub const STACK_BOTTOM: u64 = 0x76d0_0000;
pub const STACK_TOP: u64 = IMAGE_BASE;
/// The PSP-loaded image. Its last 8 KiB hold the 16-bit entry, and the
/// reset vector is its last 16 bytes.
pub const IMAGE_BASE: u64 = 0x76db_0000;
pub const IMAGE_SIZE: u64 = 0x0025_0000;
pub const IMAGE_END: u64 = IMAGE_BASE + IMAGE_SIZE;
pub const RESET_VECTOR: u64 = IMAGE_END - 0x10;
pub const EARLY_END: u64 = IMAGE_END;

/// 16 MiB SPI flash mapped below 4 GiB.
pub const FLASH_BASE: u64 = 0xff00_0000;
pub const FLASH_SIZE: u32 = 0x0100_0000;
/// The vendor EFS, PSP/BIOS directories and signed blobs, kept byte for byte.
pub const VENDOR_PREFIX: u32 = 0x0029_4000;
/// fstart's FFS sits between the vendor prefix and the reset image.
pub const FFS_OFFSET: u32 = VENDOR_PREFIX;
pub const RESET_FLASH_OFFSET: u32 = FLASH_SIZE - IMAGE_SIZE as u32;
pub const FFS_SIZE: u32 = RESET_FLASH_OFFSET - FFS_OFFSET;
pub const FFS_BASE: u64 = FLASH_BASE + FFS_OFFSET as u64;

const _: () = assert!(DATA_START >= PAGE_TABLES_END && DATA_START < HEAP_START);
const _: () = assert!(HEAP_START + HEAP_SIZE <= STACK_BOTTOM && STACK_TOP <= IMAGE_BASE);
const _: () = assert!(IMAGE_END.is_multiple_of(0x1_0000));
