// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use fstart_amdfw_format::{
    Flash, Profile,
    directory::{Directory, Levels},
};

const SLOTS: [BiosDirectorySlot; 3] = [
    BiosDirectorySlot::Family17hModel00,
    BiosDirectorySlot::Family17hModel30,
    BiosDirectorySlot::Family19h,
];

fn file(kind: u8, instance: u8, level: Level, size: usize) -> Entry {
    Entry {
        key: Key::new(kind, 0, instance),
        level,
        content: Content::File((0..size).map(|i| (i as u8) ^ kind ^ instance).collect()),
    }
}

fn image(slot: BiosDirectorySlot) -> TwoLevelImage {
    TwoLevelImage {
        slot,
        flash_size: 0x100_0000,
        efs_offset: 0x2_0000,
        partition_end: 0x60_0000,
        spi: Some((7, 1)),
        psp: vec![
            file(0x00, 0, Level::One, 0x240),
            file(0x01, 0, Level::Both, 0x2_0000),
            file(0x09, 0, Level::Two, 0x340),
            Entry {
                key: Key::new(psp::SOFT_FUSE, 0, 0),
                level: Level::Two,
                content: Content::Value(1),
            },
            Entry {
                key: Key::new(0x22, 0, 0),
                level: Level::Two,
                content: Content::Erased(0x1000),
            },
        ],
        bios: vec![
            file(bios::RTM_PUBKEY, 0, Level::Both, 0x340),
            file(bios::APCB, 0, Level::Two, 0x1d00),
            file(bios::APCB_BACKUP, 0, Level::Both, 0x1d00),
            file(bios::PMU_DATA, 1, Level::Both, 0x330),
            file(bios::PMU_INSTRUCTIONS, 1, Level::Both, 0x3c10),
            file(bios::PMU_INSTRUCTIONS, 4, Level::One, 0x3c10),
            Entry {
                key: Key::new(bios::APOB_NV, 0, 0),
                level: Level::Two,
                content: Content::Erased(0x2_0000),
            },
        ],
        reset: Reset {
            offset: 0xdb_0000,
            size: 0x25_0000,
            destination: 0x76db_0000,
            compressed: false,
        },
        apob_destination: 0x3000_0000,
    }
}

/// The packed partition in an erased 16 MiB flash.
fn flash(image: &TwoLevelImage) -> Vec<u8> {
    let packed = image.pack().unwrap();
    let mut flash = vec![0xff; 0x100_0000];
    flash[packed.offset as usize..][..packed.bytes.len()].copy_from_slice(&packed.bytes);
    flash
}

fn kinds<E: fstart_amdfw_format::directory::Entry>(dir: &Directory<'_, E>) -> Vec<(u8, u8)> {
    dir.entries
        .iter()
        .map(|e| (e.kind(), e.instance()))
        .collect()
}

#[test]
fn the_boot_roms_find_both_levels_of_every_generation() {
    for slot in SLOTS {
        let image = image(slot);
        let bytes = flash(&image);
        let flash = Flash::new(&bytes).with_profile(Profile::for_slot(slot));
        let (efs, header) = flash.efs_checked().unwrap();
        assert_eq!(efs, 0x2_0000, "{slot:?}");
        let psp = flash.psp_directories().unwrap();
        let psp2 = psp.level2.unwrap();
        assert_eq!(
            kinds(&psp.level1),
            [(0x00, 0), (0x01, 0), (psp::LEVEL2, 0)],
            "{slot:?}"
        );
        assert_eq!(
            kinds(&psp2),
            [(0x01, 0), (0x09, 0), (psp::SOFT_FUSE, 0), (0x22, 0)]
        );
        // A file of both levels is stored once.
        let boot_loader = |dir: &Directory<'_, PspEntry>| {
            dir.range(dir.entries.iter().find(|e| e.kind == 1).unwrap())
                .unwrap()
        };
        assert_eq!(boot_loader(&psp.level1), boot_loader(&psp2));
        let (_, fuse) = psp.find(psp::SOFT_FUSE, None, None).unwrap();
        assert_eq!((fuse.size.get(), fuse.address.get()), (u32::MAX, 1));
        let (dir, nv) = psp.find(0x22, None, None).unwrap();
        assert!(dir.data(nv).unwrap().iter().all(|&b| b == 0xff));
        assert_eq!(dir.range(nv).unwrap().start % 0x1000, 0);

        let bios: Levels<'_, BiosEntry> = flash.bios_directories_checked(slot).unwrap();
        let bios2 = bios.level2.unwrap();
        assert_eq!(
            kinds(&bios.level1),
            [
                (bios::RTM_PUBKEY, 0),
                (bios::APOB, 0),
                (bios::RESET_IMAGE, 0),
                (bios::PMU_INSTRUCTIONS, 1),
                (bios::PMU_DATA, 1),
                (bios::PMU_INSTRUCTIONS, 4),
                (bios::APCB_BACKUP, 0),
                (bios::LEVEL2, 0),
            ]
        );
        assert_eq!(
            kinds(&bios2),
            [
                (bios::RTM_PUBKEY, 0),
                (bios::APCB, 0),
                (bios::APOB, 0),
                (bios::RESET_IMAGE, 0),
                (bios::APOB_NV, 0),
                (bios::PMU_INSTRUCTIONS, 1),
                (bios::PMU_DATA, 1),
                (bios::APCB_BACKUP, 0),
            ]
        );
        for dir in [bios.level1, bios2] {
            let reset = dir.entries.iter().find(|e| e.kind == 0x62).unwrap();
            assert_eq!(dir.range(reset).unwrap(), 0xdb_0000..0x100_0000);
            assert_eq!(reset.destination(), 0x76db_0000);
            assert_eq!(reset.flags, BiosEntry::RESET | BiosEntry::COPY);
        }
        let (dir, pmu) = bios.find(bios::PMU_INSTRUCTIONS, Some(4), None).unwrap();
        assert_eq!(dir.offset, bios.level1.offset);
        assert_eq!(dir.data(pmu).unwrap()[1], 1 ^ 0x64 ^ 4);
        assert_eq!(
            bios.find(bios::APOB, None, None).unwrap().1.destination(),
            0x3000_0000
        );

        // Generation specifics of the EFS and the directory headers.
        let expected = match slot {
            BiosDirectorySlot::Family17hModel00 => {
                (u32::MAX, 0xff00_0000 + bios.level1.offset as u32, 0)
            }
            BiosDirectorySlot::Family17hModel30 => (0xffff_fffe, bios.level1.offset as u32, 0),
            _ => (0xffff_fffc, bios.level1.offset as u32, 1),
        };
        assert_eq!(header.generation.get(), expected.0);
        assert_eq!(header.bios_directory(slot), Some(u64::from(expected.1)));
        assert_eq!(psp2.header.address_mode(), expected.2);
        // Everything but the reset image is in the partition.
        flash
            .validate_prefix(slot, 0x60_0000, Some(0xdb_0000..0x100_0000))
            .unwrap();
    }
}

#[test]
fn spi_settings_go_where_the_generation_reads_them() {
    let zp = flash(&image(BiosDirectorySlot::Family17hModel00));
    assert_eq!(zp[0x2_0043..0x2_0045], [7, 1]);
    let milan = flash(&image(BiosDirectorySlot::Family19h));
    assert_eq!(milan[0x2_0047..0x2_0049], [7, 1]);
    assert_eq!(milan[0x2_0043..0x2_0045], [0xff, 0xff]);
}

#[test]
fn reserved_duplicate_and_misplaced_entries_fail_closed() {
    let slot = BiosDirectorySlot::Family17hModel00;
    let mut i = image(slot);
    i.psp.push(file(0x01, 0, Level::Two, 0x10));
    assert!(i.pack().unwrap_err().contains("duplicate"));
    // The same key in different levels is fine.
    let mut i = image(slot);
    i.bios.push(file(bios::APCB, 0, Level::One, 0x4000));
    i.pack().unwrap();
    let mut i = image(slot);
    i.bios.push(file(bios::RESET_IMAGE, 0, Level::Both, 0x10));
    assert!(i.pack().is_err());
    let mut i = image(slot);
    i.bios[0].content = Content::Value(1);
    assert!(i.pack().is_err());
    let mut i = image(slot);
    i.bios.push(Entry {
        key: Key::new(0x67, 0, 0),
        level: Level::Two,
        content: Content::Erased(0x800),
    });
    assert!(i.pack().is_err());
    let mut i = image(slot);
    i.partition_end = 0x4_0000;
    assert!(i.pack().unwrap_err().contains("partition"));
    let mut i = image(slot);
    i.reset.offset = 0x5f_0000;
    assert!(i.pack().is_err());
    let mut i = image(slot);
    i.reset.destination = 0x76db_1000;
    assert!(i.pack().is_err());
}

#[test]
fn apcbs_own_their_sectors_and_the_efs_is_where_the_boot_rom_looks() {
    let slot = BiosDirectorySlot::Family17hModel00;
    let bytes = flash(&image(slot));
    let flash = Flash::new(&bytes).with_profile(Profile::for_slot(slot));
    let bios = flash.bios_directories_checked(slot).unwrap();
    let files: Vec<_> = bios
        .iter()
        .flat_map(|dir| {
            dir.entries
                .iter()
                .filter_map(move |e| Some((e.kind, dir.range(e).ok()?)))
        })
        .filter(|(kind, _)| *kind != bios::RESET_IMAGE)
        .collect();
    for (kind, apcb) in files
        .iter()
        .filter(|(k, _)| matches!(*k, bios::APCB | bios::APCB_BACKUP))
    {
        let sectors = apcb.start..apcb.end.next_multiple_of(0x1000);
        assert_eq!(apcb.start % 0x1000, 0, "{kind:#x}");
        assert!(
            files
                .iter()
                .filter(|(_, other)| other != apcb && other.len() != 0)
                .all(|(_, other)| other.end <= sectors.start || other.start >= sectors.end),
            "{kind:#x} shares a sector"
        );
    }
    let mut i = image(slot);
    i.efs_offset = 0x1000;
    assert!(i.pack().unwrap_err().contains("EFS"));
    // A smaller flash ends the 16 MiB window.
    let mut i = image(slot);
    i.flash_size = 0x80_0000;
    i.efs_offset = 0x2_0000;
    i.reset.offset = 0x5b_0000;
    assert!(i.pack().is_err());
    i.efs_offset = 0x42_0000;
    i.partition_end = 0x5b_0000;
    i.pack().unwrap();
}
