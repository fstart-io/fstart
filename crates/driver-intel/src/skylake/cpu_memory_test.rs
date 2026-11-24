// SPDX-License-Identifier: MIT OR Apache-2.0
//! CPU-visible basic test from romstage_native.c:307–325. Distinct from REUT:
//! write byte(i) to the first physical4KiB, then compare all bytes20 times.
//! This destructive check neither establishes full-RAM health nor certifies
//! the caller's physical mapping, cache policy, initialization or usable map.
use crate::sunrise_point::pmc::MemoryBootMode;
use fstart_core::mmio::{read8, write8};

pub const BLOCK_SIZE: usize = 0x1000;
const PASSES: u8 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    RetainedMemoryUnsupported,
    MappingTooSmall,
    AddressOverflow,
    Mismatch {
        pass: u8,
        offset: u16,
        expected: u8,
        actual: u8,
    },
}

/// Perform the actual native CPU-visible first-page test, leaving its byte
/// pattern in place. Returns the first observed mismatch; no retry or cleanup
/// hides a failure. Uses existing barrier-aware volatile byte accessors, not
/// a REUT surrogate, hardware callback, new MMIO suite or Rust reference at0.
///
/// Success means only these4096 byte writes and twenty complete comparisons.
/// Volatile/fences alone do not bypass caches: this is NOT physical-DRAM proof.
///
/// # Safety
/// For Cold with length>=BLOCK_SIZE and a representable checked byte range,
/// `mapped_base..mapped_base+BLOCK_SIZE` must be valid CPU-readable/writable
/// memory exclusively owned for this entire operation, with no conflicting
/// Rust references, APs, DMA or interrupt users. It is destructively overwritten.
/// In firmware it must actually map physical0..4096 to initialized normal DRAM,
/// not CAR, firmware data/code, device MMIO or a cache-only alias. The caller
/// must establish the actual mapping, effective cache policy and cold controller
/// ownership; the numeric length is not mapping evidence. Address0 is permitted
/// for identity-mapped external physical memory without creating a Rust slice.
/// Host tests use an exclusively owned RAM image instead, not physical address0.
/// Rejected mode/length/overflow inputs do not access memory or require a mapping.
pub unsafe fn basic_test(
    mapped_base: usize,
    mapped_length: usize,
    mode: MemoryBootMode,
) -> Result<(), Error> {
    validate_bounds(mapped_base, mapped_length, mode)?;
    for offset in 0..BLOCK_SIZE {
        // SAFETY: validated address arithmetic and caller's exclusive mapping.
        unsafe { write8((mapped_base + offset) as *mut u8, offset as u8) };
    }
    for pass in 0..PASSES {
        // SAFETY: the same checked, exclusively owned range remains mapped.
        unsafe { verify_page(mapped_base, pass)? };
    }
    Ok(())
}

/// Pure numeric preflight shared with the composing cold owner. This is not
/// evidence of a mapping, exclusivity, effective caching or physical DRAM.
pub(crate) fn validate_bounds(
    mapped_base: usize,
    mapped_length: usize,
    mode: MemoryBootMode,
) -> Result<(), Error> {
    if mode != MemoryBootMode::Cold {
        return Err(Error::RetainedMemoryUnsupported);
    }
    if mapped_length < BLOCK_SIZE {
        return Err(Error::MappingTooSmall);
    }
    mapped_base
        .checked_add(BLOCK_SIZE - 1)
        .ok_or(Error::AddressOverflow)?;
    Ok(())
}

// Algorithm's read phase, split only to test actual comparison failures after
// corrupting a RAM image. No production injection/transport callback exists.
unsafe fn verify_page(mapped_base: usize, pass: u8) -> Result<(), Error> {
    for offset in 0..BLOCK_SIZE {
        // SAFETY: caller owns the checked mapped page for this comparison.
        let actual = unsafe { read8((mapped_base + offset) as *const u8) };
        let expected = offset as u8;
        if actual != expected {
            return Err(Error::Mismatch {
                pass,
                offset: offset as u16,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "cpu_memory_test_tests.rs"]
mod tests;
