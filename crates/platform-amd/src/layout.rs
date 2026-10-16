// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso cold-boot DRAM layout; keep in sync with linker/picasso.ld.
//!
//! The PSP trains DRAM, writes the APOB and copies the BIOS reset image
//! (directory entry 0x62) to `IMAGE_BASE` before releasing the BSP at
//! `RESET_VECTOR`. Everything from `EARLY_START` to `EARLY_END` stays
//! reserved through OS handoff.

pub const EARLY_START: u64 = 0x0200_0000;
/// PSP-written APOB (directory entry 0x61).
pub const APOB_BASE: u64 = 0x0200_1000;
pub const APOB_SIZE: u32 = 0x1_0000;
/// PSP shared memory, only used with a PSP verstage.
pub const SHARED_BASE: u64 = 0x0201_1000;
pub const SHARED_SIZE: u32 = 0x8000;
/// Permanent pages: staged microcode patch and the PSP mailbox buffer.
pub const MICROCODE_START: u64 = 0x0201_9000;
pub const MICROCODE_END: u64 = 0x0201_a000;
pub const PSP_START: u64 = MICROCODE_END;
pub const PSP_END: u64 = 0x0201_b000;
/// Zero-initialised firmware data (BSS), page tables and the BSP stack.
pub const BSS_START: u64 = 0x0202_0000;
pub const PAGE_TABLES: u64 = 0x020d_0000;
pub const STACK_BOTTOM: u64 = 0x020e_0000;
pub const STACK_TOP: u64 = 0x0210_0000;
/// The PSP-loaded image. Its last 64 KiB hold the 16-bit entry, and the
/// reset vector is its last 16 bytes.
pub const IMAGE_BASE: u64 = 0x0210_0000;
pub const IMAGE_SIZE: u32 = 0x0020_0000;
pub const IMAGE_END: u64 = IMAGE_BASE + IMAGE_SIZE as u64;
pub const RESET_VECTOR: u64 = IMAGE_END - 0x10;
pub const EARLY_END: u64 = IMAGE_END;

/// Capacity for the topology decoder's maximum envelope (2 CCXs, 4 cores,
/// 2 threads). Not a claim that a particular Picasso exposes 16 CPUs.
pub const MP_CPU_CAPACITY: u16 = 16;

const _: () = assert!(IMAGE_END % 0x1_0000 == 0 && STACK_TOP <= IMAGE_BASE);
