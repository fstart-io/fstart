// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::vec;

#[test]
fn cpu_basic_patterns_and_untouched_guards() {
    assert_eq!((BLOCK_SIZE, PASSES), (4096, 20));
    for poison in [0xa5u8, 0x37] {
        let mut memory = vec![poison; BLOCK_SIZE + 32];
        let base = memory.as_mut_ptr().wrapping_add(16) as usize;
        // SAFETY: the exclusively owned RAM image contains the complete page.
        unsafe { basic_test(base, BLOCK_SIZE, MemoryBootMode::Cold) }.unwrap();
        assert!(memory[..16].iter().all(|&b| b == poison));
        assert!(memory[BLOCK_SIZE + 16..].iter().all(|&b| b == poison));
        for (offset, &value) in memory[16..16 + BLOCK_SIZE].iter().enumerate() {
            assert_eq!(value, offset as u8);
        }
    }
}
#[test]
fn cpu_basic_preflight_rejects_retained_short_or_overflow_without_access() {
    let cases = [
        (
            0,
            BLOCK_SIZE,
            MemoryBootMode::Warm,
            Error::RetainedMemoryUnsupported,
        ),
        (usize::MAX, 0, MemoryBootMode::S3, Error::MappingTooSmall),
        (0, 0, MemoryBootMode::Cold, Error::MappingTooSmall),
        (
            usize::MAX,
            BLOCK_SIZE - 1,
            MemoryBootMode::Cold,
            Error::MappingTooSmall,
        ),
        (
            usize::MAX,
            BLOCK_SIZE,
            MemoryBootMode::Cold,
            Error::AddressOverflow,
        ),
        (
            usize::MAX - BLOCK_SIZE + 2,
            BLOCK_SIZE,
            MemoryBootMode::Cold,
            Error::AddressOverflow,
        ),
    ];
    for (base, length, mode, error) in cases {
        // SAFETY: this API explicitly requires no mapping for rejected inputs.
        assert_eq!(unsafe { basic_test(base, length, mode) }, Err(error));
    }
}
#[test]
fn cpu_basic_actual_read_phase_reports_first_bad_byte_and_pass() {
    for pass in [0, 7, 19] {
        for offset in [0, 1, 255, 256, BLOCK_SIZE - 1] {
            let mut memory: std::vec::Vec<u8> = (0..BLOCK_SIZE).map(|i| i as u8).collect();
            let base = memory.as_mut_ptr() as usize;
            // SAFETY: live exclusively owned full RAM image, no other users.
            unsafe { verify_page(base, pass) }.unwrap();
            let expected = offset as u8;
            let actual = expected ^ 0x5a;
            memory[offset] = actual;
            if offset < BLOCK_SIZE - 1 {
                memory[BLOCK_SIZE - 1] ^= 1;
            }
            // Intentional corruption BETWEEN read phases, never a data race or
            // production callback. This tests the exact fatal comparator.
            assert_eq!(
                unsafe { verify_page(base, pass) },
                Err(Error::Mismatch {
                    pass,
                    offset: offset as u16,
                    expected,
                    actual
                })
            );
        }
    }
}
