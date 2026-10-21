// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::{
    directory::{BIOS_COOKIE, BIOS_LEVEL2_COOKIE, BiosEntry, Header, bios, fletcher32},
    efs::{BiosDirectorySlot, Efs},
    *,
};
use std::{vec, vec::Vec};
use zerocopy::IntoBytes;

const SLOT: BiosDirectorySlot = BiosDirectorySlot::Family17hModel00;

fn put_directory(flash: &mut [u8], offset: usize, cookie: [u8; 4], entries: &[BiosEntry]) {
    let mut table = Header {
        cookie,
        checksum: 0.into(),
        count: (entries.len() as u32).into(),
        additional: 0.into(),
    }
    .as_bytes()
    .to_vec();
    entries.iter().for_each(|e| table.extend(e.as_bytes()));
    let checksum = fletcher32(&table[8..]);
    table[4..8].copy_from_slice(&checksum.to_le_bytes());
    flash[offset..offset + table.len()].copy_from_slice(&table);
}

fn file(kind: u8, instance: u8, size: u32, source: u64) -> BiosEntry {
    BiosEntry::new(kind, 0, instance, 0, size, source, u64::MAX)
}

/// A 1-MiB flash: the EFS at 0x20000, level 1 at 0x30000 with a microcode
/// and the pointer to level 2 at 0x40000 with two other microcodes.
fn flash() -> Vec<u8> {
    let mut flash = vec![0xffu8; 0x10_0000];
    let mut efs = Efs::erased();
    efs.bios_directory_mut(SLOT).set(0xfff3_0000);
    // The probe 0xfff20000.
    flash[0x2_0000..0x2_0060].copy_from_slice(efs.as_bytes());
    put_directory(
        &mut flash,
        0x3_0000,
        BIOS_COOKIE,
        &[
            file(bios::MICROCODE, 0, 0x10, 0xfff5_0000),
            file(bios::LEVEL2, 0, 0x400, 0xfff4_0000),
        ],
    );
    put_directory(
        &mut flash,
        0x4_0000,
        BIOS_LEVEL2_COOKIE,
        &[
            file(bios::APOB, 0, 0, 0),
            // A flash offset instead of a mapped address.
            file(bios::MICROCODE, 0, 0x10, 0x5_1000),
            file(bios::MICROCODE, 1, 0x10, 0xfff5_2000),
        ],
    );
    flash
}

#[test]
fn the_efs_is_at_a_probe_offset_of_the_window() {
    let image = flash();
    let flash = Flash::new(&image);
    // 0xff020000 is below the 1-MiB flash, 0xfff20000 0x20000 into it.
    assert_eq!(flash.efs().map(|(offset, _)| offset), Some(0x2_0000));
    assert_eq!(flash.address(0x2_0000), 0xfff2_0000);
    assert_eq!(flash.offset(0xfff2_0000), Some(0x2_0000));
    assert_eq!(flash.offset(0x2_0000), Some(0x2_0000));
}

#[test]
fn the_psp_boots_from_level_2() {
    let image = flash();
    let flash = Flash::new(&image);
    let levels = flash.bios_directories(SLOT).unwrap();
    assert_eq!(levels.level1.offset, 0x3_0000);
    assert_eq!(levels.boot().offset, 0x4_0000);
    assert_eq!(levels.iter().count(), 2);
    let microcode: Vec<_> = flash
        .bios_files(SLOT, bios::MICROCODE)
        .map(|data| data.as_ptr())
        .collect();
    assert_eq!(
        microcode,
        [image[0x5_1000..].as_ptr(), image[0x5_2000..].as_ptr()]
    );
    // No directory for another SoC generation.
    assert!(
        flash
            .bios_directories(BiosDirectorySlot::Family17hModel10)
            .is_none()
    );
}

#[test]
fn corrupt_referenced_level_2_is_rejected() {
    let mut image = flash();
    image[0x4_0004] ^= 1;
    let flash = Flash::new(&image);
    assert!(matches!(
        flash.bios_directories_checked(SLOT),
        Err(Error::Checksum(0x4_0000))
    ));
    assert_eq!(flash.bios_files(SLOT, bios::MICROCODE).count(), 0);
}

#[test]
fn fletcher_reference_vectors() {
    assert_eq!(fletcher32(&[0; 8]), 0xffff_ffff);
    assert_eq!(fletcher32(&[1, 0, 2, 0, 3, 0, 4, 0]), 0x0014_000a);
    assert_eq!(fletcher32(&[0xff; 1024]), 0xffff_ffff);
}
