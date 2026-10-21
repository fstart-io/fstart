// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use directory::{BiosEntry, Header, PspEntry, bios, fletcher32};
use efs::{BiosDirectorySlot as Slot, Efs};
use std::{vec, vec::Vec};
use zerocopy::{FromBytes, IntoBytes};

fn put<E: Entry + IntoBytes>(
    image: &mut [u8],
    offset: usize,
    cookie: [u8; 4],
    info: u32,
    entries: &[E],
) {
    let mut bytes = Header {
        cookie,
        checksum: 0.into(),
        count: (entries.len() as u32).into(),
        additional: info.into(),
    }
    .as_bytes()
    .to_vec();
    for entry in entries {
        bytes.extend(entry.as_bytes());
    }
    let sum = fletcher32(&bytes[8..]);
    bytes[4..8].copy_from_slice(&sum.to_le_bytes());
    image[offset..offset + bytes.len()].copy_from_slice(&bytes);
}
fn image(slot: Slot) -> Vec<u8> {
    let mut image = vec![0xff; 0x80000];
    let mut efs = Efs::erased();
    efs.generation.set(0xffff_fffc);
    efs.bios_directory_mut(slot).set(0x31000);
    efs.new_psp_directory.set(0x30000);
    image[0x20000..0x20060].copy_from_slice(efs.as_bytes());
    put::<PspEntry>(
        &mut image,
        0x30000,
        *b"$PSP",
        1 << 29,
        &[
            PspEntry::new(1, 0, 0, 0x100, 0x34000),
            PspEntry::new(0x0b, 0, 0, u32::MAX, 1 << 28),
        ],
    );
    put(
        &mut image,
        0x31000,
        *b"$BHD",
        1 << 29,
        &[
            BiosEntry::new(bios::APOB, 0, 0, 0, 0, 0, 0x30000000),
            BiosEntry::new(bios::RESET_IMAGE, 0, 0, 3, 0x8000, 0x38000, 0x75ff8000),
            BiosEntry::new(bios::LEVEL2, 0, 0, 0, 0x1000, 0x32000, u64::MAX),
        ],
    );
    put(
        &mut image,
        0x32000,
        *b"$BL2",
        1 << 29,
        &[
            BiosEntry::new(bios::MICROCODE, 0, 0, 0, 0x100, 0x33000, u64::MAX),
            BiosEntry::new(bios::RESET_IMAGE, 0, 0, 3, 0x8000, 0x38000, 0x75ff8000),
        ],
    );
    image
}
#[test]
fn modern_slots_and_full_prefix_closure() {
    for slot in [Slot::Family17hModel30, Slot::Family19h] {
        let image = image(slot);
        let flash = Flash::new(&image).with_profile(Profile::for_slot(slot));
        let dirs = flash.bios_directories_checked(slot).unwrap();
        assert_eq!(dirs.boot().offset, 0x32000);
        assert!(
            flash
                .validate_prefix(slot, 0x35000, Some(0x38000..0x40000))
                .is_ok()
        );
        // BIOS fits, but a PSP blob would be erased.
        assert!(matches!(
            flash.validate_prefix(slot, 0x34000, Some(0x38000..0x40000)),
            Err(Error::OutsidePrefix { start: 0x34000, .. })
        ));
    }
}
#[test]
fn ancillary_blob_is_kept_only_below_the_directories() {
    let slot = Slot::Family17hModel00;
    let mut image = image(slot);
    let set_xhci = |image: &mut Vec<u8>, at: u32| {
        let mut efs = Efs::read_from_bytes(&image[0x20000..0x20060]).unwrap();
        efs.xhci.set(at);
        image[0x20000..0x20060].copy_from_slice(efs.as_bytes());
    };
    let check = |image: &Vec<u8>| {
        Flash::new(image)
            .with_profile(Profile::for_slot(slot))
            .validate_prefix(slot, 0x35000, Some(0x38000..0x40000))
    };
    // Between the EFS and the first directory, as the vendor images put it.
    set_xhci(&mut image, 0x21000);
    assert!(check(&image).is_ok());
    // Past a directory its length is unbounded.
    set_xhci(&mut image, 0x33800);
    assert!(matches!(check(&image), Err(Error::AncillaryPointer(0x33800))));
}
#[test]
fn present_invalid_l2_is_not_absent() {
    let mut image = image(Slot::Family19h);
    image[0x32004] ^= 1;
    assert!(matches!(
        Flash::new(&image).bios_directories_checked(Slot::Family19h),
        Err(Error::Checksum(0x32000))
    ));
}
#[test]
fn physical_image_table_slot_and_entry_override_modes() {
    let bytes = vec![0; 0x100000];
    let flash = Flash::region(
        &bytes,
        0x10000,
        0xff000000,
        Profile::for_slot(Slot::Family19h),
    );
    assert_eq!(flash.resolve(0xff002000, 0, 0), Ok(0x12000));
    assert_eq!(flash.resolve(0x2000, 1, 0), Ok(0x12000));
    assert_eq!(flash.resolve(0x2000, 2, 0x40000), Ok(0x42000));
    assert_eq!(flash.resolve(0x2000, 3, 0), Err(Error::AddressMode(3)));
    assert_eq!(
        flash.with_slot(0x50000).resolve(3 << 62 | 0x2000, 0, 0),
        Ok(0x52000)
    );
    assert_eq!(flash.resolve(2 << 62 | 0x2000, 1, 0x40000), Ok(0x42000));
    assert!(flash.resolve(u64::MAX, 0, 0).is_err());
}
#[test]
fn header_v1_uses_its_own_mode_field_and_extent() {
    let mut image = image(Slot::Family19h);
    put(
        &mut image,
        0x32000,
        *b"$BL2",
        1 << 31 | 2 << 24 | 1,
        &[BiosEntry::new(
            bios::MICROCODE,
            0,
            0,
            0,
            0x100,
            0x1000,
            u64::MAX,
        )],
    );
    let dirs = Flash::new(&image)
        .bios_directories_checked(Slot::Family19h)
        .unwrap();
    let dir = dirs.boot();
    assert_eq!(dir.header.version(), 1);
    assert_eq!(dir.header.address_mode(), 2);
    assert_eq!(dir.extent(), Ok(0x32000..0x33000));
    assert_eq!(dir.range(&dir.entries[0]), Ok(0x33000..0x33100));
}
#[test]
fn explicit_origin_selects_firmware_region_in_32_mib_rom() {
    let region = image(Slot::Family19h);
    let mut rom = vec![0xff; 32 << 20];
    rom[16 << 20..(16 << 20) + region.len()].copy_from_slice(&region);
    let flash = Flash::region(
        &rom,
        16 << 20,
        0xff000000,
        Profile::for_slot(Slot::Family19h),
    );
    assert_eq!(flash.efs_checked().unwrap().0, (16 << 20) + 0x20000);
    assert_eq!(
        flash
            .bios_directories_checked(Slot::Family19h)
            .unwrap()
            .boot()
            .offset,
        (16 << 20) + 0x32000
    );
}
#[test]
fn generation_word_is_checked() {
    let mut image = image(Slot::Family19h);
    image[0x20024..0x20028].copy_from_slice(&0xffff_fffe_u32.to_le_bytes());
    assert!(
        Flash::new(&image)
            .with_profile(Profile::for_slot(Slot::Family17hModel30))
            .efs_checked()
            .is_ok()
    );
    assert!(matches!(
        Flash::new(&image)
            .with_profile(Profile::for_slot(Slot::Family19h))
            .efs_checked(),
        Err(Error::NoEfs)
    ));
}
#[test]
fn other_efs_bios_slots_are_preserved() {
    let mut image = image(Slot::Family19h);
    image[0x20018..0x2001c].copy_from_slice(&0x35000_u32.to_le_bytes());
    put(
        &mut image,
        0x35000,
        *b"$BHD",
        1 << 29,
        &[BiosEntry::new(
            bios::MICROCODE,
            0,
            0,
            0,
            0x100,
            0x37000,
            u64::MAX,
        )],
    );
    assert!(matches!(
        Flash::new(&image).validate_prefix(Slot::Family19h, 0x37000, Some(0x38000..0x40000)),
        Err(Error::OutsidePrefix { start: 0x37000, .. })
    ));
}

#[test]
fn psp_bios_table_links_are_traversed_and_recovery_is_not_guessed() {
    let mut image = image(Slot::Family19h);
    put(
        &mut image,
        0x30000,
        *b"$PSP",
        1 << 29,
        &[PspEntry::new(
            directory::psp::BIOS_TABLE,
            0,
            0,
            0x1000,
            0x34000,
        )],
    );
    put(
        &mut image,
        0x34000,
        *b"$BL2",
        1 << 29,
        &[BiosEntry::new(
            bios::MICROCODE,
            0,
            0,
            0,
            0x100,
            0x36000,
            u64::MAX,
        )],
    );
    assert!(matches!(
        Flash::new(&image).validate_prefix(Slot::Family19h, 0x35000, Some(0x38000..0x40000)),
        Err(Error::OutsidePrefix { start: 0x36000, .. })
    ));
    put(
        &mut image,
        0x30000,
        *b"$PSP",
        1 << 29,
        &[PspEntry::new(
            directory::psp::RECOVERY_A,
            0,
            0,
            0x1000,
            0x34000,
        )],
    );
    assert_eq!(
        Flash::new(&image).validate_prefix(Slot::Family19h, image.len(), None),
        Err(Error::UnsupportedTopology(0x48))
    );
}

#[test]
fn combo_branch_closure_is_not_limited_to_running_generation() {
    let mut image = image(Slot::Family19h);
    image[0x20014..0x20018].copy_from_slice(&0x35000_u32.to_le_bytes());
    let mut combo = b"2PSP".to_vec();
    combo.extend([0; 4]);
    combo.extend(2u32.to_le_bytes());
    combo.extend([0; 20]);
    for (id, pointer) in [(0xbc0b_0d00_u32, 0x30000_u64), (0xbc0b_0000, 0x36000)] {
        combo.extend(0u32.to_le_bytes());
        combo.extend(id.to_le_bytes());
        combo.extend(pointer.to_le_bytes());
    }
    let sum = fletcher32(&combo[8..]);
    combo[4..8].copy_from_slice(&sum.to_le_bytes());
    image[0x35000..0x35000 + combo.len()].copy_from_slice(&combo);
    put(
        &mut image,
        0x36000,
        *b"$PSP",
        1 << 29,
        &[PspEntry::new(1, 0, 0, 0x100, 0x37000)],
    );
    let flash = Flash::new(&image).with_profile(Profile::for_slot(Slot::Family19h));
    assert_eq!(flash.psp_directories().unwrap().boot().offset, 0x30000);
    assert!(matches!(
        flash.validate_prefix(Slot::Family19h, 0x37000, Some(0x38000..0x40000)),
        Err(Error::OutsidePrefix { start: 0x37000, .. })
    ));
}
