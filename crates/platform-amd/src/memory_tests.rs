// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::memory::Memory;
use fstart_core::services::memory_detect::{E820Entry, E820Kind, MemoryDetector};
use fstart_driver_amd::apob::Apob;
use std::vec;

fn apob_bytes(holes: &[(u64, u64, u32)]) -> std::vec::Vec<u8> {
    let mut bytes = vec![0; 16 + 48 + 16 + holes.len() * 24];
    let size = bytes.len() as u32;
    bytes[0..4].copy_from_slice(b"APOB");
    bytes[8..12].copy_from_slice(&size.to_le_bytes());
    bytes[12..16].copy_from_slice(&16u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&9u32.to_le_bytes());
    bytes[20..24].copy_from_slice(&9u32.to_le_bytes());
    bytes[28..32].copy_from_slice(&(size - 16).to_le_bytes());
    bytes[64..72].copy_from_slice(&(16u64 << 30).to_le_bytes());
    bytes[72..76].copy_from_slice(&(holes.len() as u32).to_le_bytes());
    for (index, (base, size, kind)) in holes.iter().enumerate() {
        let offset = 80 + index * 24;
        bytes[offset..offset + 8].copy_from_slice(&base.to_le_bytes());
        bytes[offset + 8..offset + 16].copy_from_slice(&size.to_le_bytes());
        bytes[offset + 16..offset + 20].copy_from_slice(&kind.to_le_bytes());
    }
    bytes
}

#[test]
fn apob_holes_psp_window_and_native_tseg_are_never_handed_to_the_os() {
    let holes = [
        (0xd0000000u64, 0x30000000u64, 1u32),
        (0xc8000000, 0x08000000, 0),
        (6 << 30, 0x08000000, 2),
        (0x20000000, 0x01000000, 99), // an unknown PSP reservation still isn't RAM
    ];
    let bytes = apob_bytes(&holes);
    let apob = Apob::parse(&bytes).unwrap().memory_map().unwrap();
    assert!(Memory::from_apob(&apob, 0xd0000000, None, 8 << 20).is_err());
    assert!(Memory::from_apob(&apob, 0xd0000000, Some(8 << 30), 8 << 20).is_err());
    let memory = Memory::from_apob(&apob, 0xd0000000, Some(16 << 30), 8 << 20).unwrap();
    assert!(memory.microcode_reserved());
    assert!(memory.psp_reserved());
    assert_eq!(memory.tseg_base, 0xc7800000);
    assert_eq!(memory.allocation_top, 0xc7800000);
    let mut entries = [E820Entry::zeroed(); 128];
    let count = memory.detect_memory(&mut entries).unwrap();
    for (base, size) in holes.iter().map(|&(base, size, _)| (base, size)).chain([
        (
            crate::layout::EARLY_START,
            crate::layout::EARLY_END - crate::layout::EARLY_START,
        ),
        (memory.tseg_base, memory.tseg_size),
        (fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 4096),
    ]) {
        assert!(
            !entries[..count]
                .iter()
                .any(|entry| entry.kind == E820Kind::Ram as u32
                    && entry.addr < base + size
                    && entry.addr + entry.size > base)
        );
    }
    assert!(
        entries[..count]
            .iter()
            .any(|entry| entry.kind == E820Kind::Ram as u32 && entry.addr >= 1 << 32)
    );
    let total: u64 = entries[..count]
        .iter()
        .filter(|entry| entry.kind == E820Kind::Ram as u32)
        .map(|entry| entry.size)
        .sum();
    assert_eq!(memory.total_ram_bytes().unwrap(), total);
    assert!(memory.detect_memory(&mut entries[..1]).is_err());
    // The owned state passed to a later host/payload retains the exclusion and
    // reduced usable total. The early firmware region stays permanent.
    let state = memory.into_state();
    assert_eq!(state.total_ram(), total);
    assert!(
        state
            .entries()
            .iter()
            .any(|entry| entry.kind == E820Kind::Reserved as u32
                && entry.addr <= crate::layout::EARLY_START
                && entry.addr + entry.size >= crate::layout::EARLY_END)
    );
}

#[test]
fn holes_in_the_early_firmware_region_are_rejected() {
    use crate::layout::{EARLY_END, EARLY_START, IMAGE_BASE, MICROCODE_START, PSP_START};
    for (base, size) in [
        (EARLY_START, 1),
        (MICROCODE_START, 1),
        (PSP_START + 32, 1),
        (IMAGE_BASE, 0x1000),
        (EARLY_END - 1, 1),
        (fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS + 16, 1),
    ] {
        let bytes = apob_bytes(&[(base, size, 99)]);
        let apob = Apob::parse(&bytes).unwrap().memory_map().unwrap();
        assert!(matches!(
            Memory::from_apob(&apob, 0xd0000000, Some(16 << 30), 8 << 20),
            Err(super::memory::Error::Boundary)
        ));
    }
}
