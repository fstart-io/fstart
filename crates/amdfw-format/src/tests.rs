// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use directory::{BiosEntry, PspEntry, bios, fletcher32};
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
    directory::write_table(&mut image[offset..], cookie, info, entries).unwrap();
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
    assert!(matches!(
        check(&image),
        Err(Error::AncillaryPointer(0x33800))
    ));
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

/// A Milan "Two 16M in 32MB" half: the EFS at 0x20000, `$BHD` with the
/// APOB, the reset image and an L2 with two microcode patches.
fn milan_half() -> Vec<u8> {
    let mut half = vec![0xff; HALF_SIZE];
    let mut efs = Efs::erased();
    efs.generation.set(0xffff_fffc);
    efs.new_psp_directory.set(0x30000);
    efs.bios_directory_mut(Slot::Family19h).set(0x31000);
    half[0x20000..0x20060].copy_from_slice(efs.as_bytes());
    let reset = BiosEntry::new(bios::RESET_IMAGE, 0, 0, 3, 0x8000, 0x38000, 0x75ff_8000);
    put::<PspEntry>(&mut half, 0x30000, *b"$PSP", 1 << 29, &[]);
    put(
        &mut half,
        0x31000,
        *b"$BHD",
        1 << 29,
        &[
            BiosEntry::new(bios::APOB, 0, 0, 0, 0, 0, 0x3000_0000),
            reset,
            BiosEntry::new(bios::LEVEL2, 0, 0, 0, 0x1000, 0x32000, u64::MAX),
        ],
    );
    put(
        &mut half,
        0x32000,
        *b"$BL2",
        1 << 29,
        &[
            BiosEntry::new(bios::MICROCODE, 0, 0, 0, 0x15c0, 0x33000, u64::MAX),
            BiosEntry::new(bios::MICROCODE, 0, 1, 0, 0x15c0, 0x35000, u64::MAX),
            reset,
        ],
    );
    half
}

fn sp3(bytes: &[u8], slot: Slot) -> Flash<'_> {
    Flash::region(bytes, 0, 0xff00_0000, Profile::for_slot(slot))
}

#[test]
fn level2_first_lookup() {
    let half = milan_half();
    let flash = sp3(&half, Slot::Family19h);
    // Rome's boot ROM takes a Milan EFS, which has no Rome BIOS directory.
    assert!(sp3(&half, Slot::Family17hModel30).efs_checked().is_ok());
    assert!(matches!(
        sp3(&half, Slot::Family17hModel30).bios_directories_checked(Slot::Family17hModel30),
        Err(Error::MissingDirectory)
    ));
    let dirs = flash.bios_directories_checked(Slot::Family19h).unwrap();
    assert_eq!(dirs.level1.header.address_mode(), 1);
    let (dir, apob) = dirs.find(bios::APOB, None, None).unwrap();
    assert_eq!(apob.destination(), 0x3000_0000);
    assert_eq!(dir.offset, 0x31000, "only in L1");
    let (dir, reset) = dirs.find(bios::RESET_IMAGE, None, None).unwrap();
    assert_eq!(dir.offset, 0x32000, "L2 first");
    assert_eq!(dir.range(reset), Ok(0x38000..0x40000));
    assert!(reset.reset() && reset.copy() && !reset.read_only());
    let microcode: Vec<_> = dirs
        .all(bios::MICROCODE)
        .map(|(d, e)| d.range(e).unwrap().start)
        .collect();
    assert_eq!(microcode, [0x33000, 0x35000]);
    assert!(dirs.find(bios::MICROCODE, Some(1), Some(0)).is_some());
    assert!(dirs.find(bios::MICROCODE, Some(2), None).is_none());
    assert_eq!(flash.psp_directories().unwrap().level1.entries.len(), 0);
}

#[test]
fn entry_attributes() {
    let mut psp = PspEntry::new(1, 0, 3, 0x100, 0x1000);
    psp.attributes[0] |= 2 | 4;
    assert_eq!((psp.instance(), psp.rom_id(), psp.writable()), (3, 2, true));
    let mut bios = BiosEntry::new(bios::APCB, 1, 2, BiosEntry::READ_ONLY, 0x100, 0x1000, 5);
    bios.subprogram |= 1 << 3;
    assert_eq!(
        (bios.instance(), bios.subprogram(), bios.rom_id()),
        (2, 1, 1)
    );
    assert!(bios.read_only() && !bios.reset());
    assert_eq!(bios.destination(), 5);
}

/// A `2PSP` at 0x3f000 naming the `$PSP` at 0x30000 for Milan.
#[test]
fn combo_directory_selects_by_psp_id() {
    let mut half = milan_half();
    let mut combo = b"2PSP".to_vec();
    combo.extend([0; 4]);
    combo.extend(2u32.to_le_bytes());
    combo.extend([0; 20]);
    for (id, dir) in [(0xbc0b_0000u32, 0x3e000u64), (0xbc0b_0d00, 0x30000)] {
        combo.extend(0u32.to_le_bytes());
        combo.extend(id.to_le_bytes());
        combo.extend(dir.to_le_bytes());
    }
    let sum = fletcher32(&combo[8..]);
    combo[4..8].copy_from_slice(&sum.to_le_bytes());
    half[0x3f000..0x3f000 + combo.len()].copy_from_slice(&combo);
    half[0x20014..0x20018].copy_from_slice(&0x3f000u32.to_le_bytes());
    let flash = sp3(&half, Slot::Family19h);
    let parsed = directory::Combo::read(flash, 0x3f000).unwrap().unwrap();
    assert_eq!(parsed.lookup_mode(), 0);
    assert_eq!(parsed.entries().count(), 2);
    assert_eq!(flash.psp_directories().unwrap().level1.offset, 0x30000);
    // Rome's entry points at erased flash.
    assert_eq!(
        sp3(&half, Slot::Family17hModel30).psp_directories().err(),
        Some(Error::Cookie(0x3e000))
    );
    assert_eq!(
        sp3(&half, Slot::Family17hModel00).psp_directories().err(),
        Some(Error::ComboSelection)
    );
    half[0x3f000 + 0x24] ^= 1;
    assert_eq!(
        sp3(&half, Slot::Family19h).psp_directories().err(),
        Some(Error::Checksum(0x3f000))
    );
}

#[test]
fn boot_rom_takes_the_first_accepted_half() {
    let milan = milan_half();
    let halves = [0, HALF_SIZE];
    let booted =
        |flash: &[u8], slot| booted_half(flash, 0xff00_0000, Profile::for_slot(slot), &halves);
    let mut flash = vec![0xff; 2 * HALF_SIZE];
    flash[HALF_SIZE..].copy_from_slice(&milan);
    assert_eq!(booted(&flash, Slot::Family19h), Some(HALF_SIZE));
    // Naples searches the lower half only.
    assert_eq!(
        booted_half(
            &flash,
            0xff00_0000,
            Profile::for_slot(Slot::Family17hModel00),
            &[0]
        ),
        None
    );
    flash[..HALF_SIZE].copy_from_slice(&milan);
    assert_eq!(booted(&flash, Slot::Family17hModel30), Some(0));
    // Naples ignores the generation word.
    assert_eq!(
        booted_half(
            &flash,
            0xff00_0000,
            Profile::for_slot(Slot::Family17hModel00),
            &[0]
        ),
        Some(0)
    );
}

/// A donor half, if the SP3 analysis workspace is next to this tree.
fn donor(name: &str) -> Option<Vec<u8>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sp3/firmware/donors");
    std::fs::read(dir.join(std::format!("{name}.bin"))).ok()
}

#[test]
fn donor_directories() {
    use std::eprintln;
    // (donor, BIOS slot, reset flash, size, destination, microcode count)
    let rows = [
        (
            "naples-s8026-v202",
            Slot::Family17hModel00,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "rome-s8030-v404",
            Slot::Family17hModel30,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "rome-s8026-v202",
            Slot::Family17hModel30,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "milan-s8030-v404",
            Slot::Family19h,
            0xce_0000,
            0x32_0000,
            0x75ce_0000,
            4,
        ),
    ];
    for (name, slot, flash, size, destination, microcode) in rows {
        let Some(half) = donor(name) else {
            eprintln!("skipping {name}: donor absent");
            continue;
        };
        let naples = slot == Slot::Family17hModel00;
        let image = sp3(&half, slot);
        let dirs = image.bios_directories_checked(slot).unwrap();
        assert!(dirs.level2.is_some(), "{name}");
        let (dir, reset) = dirs.find(bios::RESET_IMAGE, None, None).unwrap();
        assert_eq!(dir.range(reset), Ok(flash..flash + size), "{name}");
        assert_eq!(reset.destination(), destination, "{name}");
        let (_, apob) = dirs.find(bios::APOB, None, None).unwrap();
        assert_eq!(apob.destination(), 0x3000_0000, "{name}");
        assert_eq!(dirs.all(bios::MICROCODE).count(), microcode, "{name}");
        let nv = dirs
            .find(bios::APOB_NV, None, None)
            .map(|(d, e)| d.range(e).unwrap());
        assert_eq!(nv, naples.then_some(0x24_e000..0x25_e000), "{name}");
        let pmu = dirs.find(bios::PMU_INSTRUCTIONS, Some(1), Some(u8::from(!naples)));
        assert!(pmu.is_some(), "{name}");
        let psp = image.psp_directories().unwrap();
        assert!(psp.level2.is_some(), "{name}");
        assert!(psp.find(directory::psp::SOFT_FUSE, None, None).is_some());
    }
    // 2P donors only have to parse.
    for (name, slot) in [
        ("naples2p-knppd32-1801", Slot::Family17hModel00),
        ("rome2p-s8253-v203", Slot::Family17hModel30),
        ("milan2p-s8253-v203", Slot::Family19h),
    ] {
        if let Some(half) = donor(name) {
            let dirs = sp3(&half, slot).bios_directories_checked(slot).unwrap();
            assert!(dirs.find(bios::RESET_IMAGE, None, None).is_some(), "{name}");
        }
    }
}

#[test]
fn fletcher_known_values() {
    // The SP3 analysis workspace's values; 2000 bytes cross the 359-word
    // fold.
    let data: Vec<u8> = (0..=255).collect();
    assert_eq!(fletcher32(&data), 0x7555_3fc0);
    let data: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
    assert_eq!(fletcher32(&data), 0xc496_b05b);
}

const SLOT: Slot = Slot::Family17hModel00;

fn bios_file(kind: u8, instance: u8, size: u32, source: u64) -> BiosEntry {
    BiosEntry::new(kind, 0, instance, 0, size, source, u64::MAX)
}

/// A 1-MiB flash: the EFS at 0x20000, level 1 at 0x30000 with a microcode
/// and the pointer to level 2 at 0x40000 with two other microcodes.
fn one_mib_flash() -> Vec<u8> {
    let mut flash = vec![0xffu8; 0x10_0000];
    let mut efs = Efs::erased();
    efs.bios_directory_mut(SLOT).set(0xfff3_0000);
    // The probe 0xfff20000.
    flash[0x2_0000..0x2_0060].copy_from_slice(efs.as_bytes());
    put(
        &mut flash,
        0x3_0000,
        directory::BIOS_COOKIE,
        0,
        &[
            bios_file(bios::MICROCODE, 0, 0x10, 0xfff5_0000),
            bios_file(bios::LEVEL2, 0, 0x400, 0xfff4_0000),
        ],
    );
    put(
        &mut flash,
        0x4_0000,
        directory::BIOS_LEVEL2_COOKIE,
        0,
        &[
            bios_file(bios::APOB, 0, 0, 0),
            // A flash offset instead of a mapped address.
            bios_file(bios::MICROCODE, 0, 0x10, 0x5_1000),
            bios_file(bios::MICROCODE, 1, 0x10, 0xfff5_2000),
        ],
    );
    flash
}

#[test]
fn the_efs_is_at_a_probe_offset_of_the_window() {
    let image = one_mib_flash();
    let flash = Flash::new(&image);
    // 0xff020000 is below the 1-MiB flash, 0xfff20000 0x20000 into it.
    assert_eq!(flash.efs().map(|(offset, _)| offset), Some(0x2_0000));
    assert_eq!(flash.address(0x2_0000), 0xfff2_0000);
    assert_eq!(flash.offset(0xfff2_0000), Some(0x2_0000));
    assert_eq!(flash.offset(0x2_0000), Some(0x2_0000));
}

#[test]
fn the_psp_boots_from_level_2() {
    let image = one_mib_flash();
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
    assert!(flash.bios_directories(Slot::Family17hModel10).is_none());
}

#[test]
fn corrupt_referenced_level_2_is_rejected() {
    let mut image = one_mib_flash();
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
