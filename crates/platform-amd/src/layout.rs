// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform-owned cold-boot DRAM layout; keep in sync with linker/picasso.ld.
//! These are firmware allocation choices, not discovered silicon facts.

pub const EARLY_START: u64 = 0x02000000;
pub const IMAGE_END: u64 = 0x02050000;
/// Capacity for the topology decoder's maximum envelope (2 CCXs, 4 cores,
/// 2 threads). This is not a claim that a particular Picasso exposes 16 CPUs.
pub const MP_CPU_CAPACITY: u16 = 16;
pub const MP_START: u64 = IMAGE_END;
/// Fifteen 4KiB stacks + 6144-byte IDTs, rounded to a whole page. Reserve
/// padding and unused slots too; parked APs retain this region through reset.
pub const MP_END: u64 = 0x02061000;
/// One selected 3200-byte vendor patch plus its permanent unused page tail.
/// Allocation policy only: APOB/TOM must establish usable RAM before ownership.
pub const MICROCODE_START: u64 = MP_END;
pub const MICROCODE_END: u64 = 0x02062000;
