// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use assembly::{AmdAssembly, BiosDirectoryFacts, Firmware, read_bios_directory};
use fstart_amdfw_format::{
    directory::{Header, fletcher32, modify_entries, write_table},
    efs::BiosDirectorySlot,
};
use zerocopy::FromBytes;
/// The slot Picasso's PSP takes the BIOS directory from.
const PICASSO: BiosDirectorySlot = BiosDirectorySlot::Family17hModel10;

fn image() -> SingleLevelImage {
    SingleLevelImage {
        bios_directory: PICASSO,
        flash_size: 0x1000000,
        efs_offset: 0xe20000,
        partition_end: 0x1000000,
        spi_read_mode: 4,
        spi_speed: 0,
        micron: false,
        soft_fuse: 0x30000000,
        psp: vec![Blob {
            key: Key::new(0, 0, 0),
            bytes: vec![0x12; 257],
        }],
        bios: vec![Blob {
            key: Key::new(0x60, 0, 0),
            bytes: vec![0x34; 4097],
        }],
        reset_image: vec![0x56; 0x20000],
        reset_slot: None,
        reset_destination: 0x2030000,
        apob_destination: 0x2001000,
        shared_memory: Some((0x2011000, 0x8000)),
        apob_nv: None,
        psp_nv: None,
    }
}
#[test]
fn typed_operation_packs_real_reset_input_without_a_fictitious_ffs_image() {
    use assembly::{FileBlob, PspFiles};
    let raw = image();
    let files = PspFiles {
        flash_size: raw.flash_size,
        efs_offset: raw.efs_offset,
        partition_end: raw.partition_end,
        spi_read_mode: Some(raw.spi_read_mode),
        spi_speed: Some(raw.spi_speed),
        micron: raw.micron,
        soft_fuse: Some(raw.soft_fuse),
        two_level: false,
        generation_mask: None,
        espi: None,
        psp: vec![FileBlob {
            key: raw.psp[0].key,
            path: "explicit/psp.bin".into(),
            level: Default::default(),
        }],
        bios: vec![FileBlob {
            key: raw.bios[0].key,
            path: "explicit/apcb.bin".into(),
            level: Default::default(),
        }],
        fixed: vec![],
        apob_nv: raw.apob_nv,
        psp_nv: raw.psp_nv,
        apcb: None,
        apcb_overlay: Default::default(),
        assets: vec![],
        psp_app: false,
    };
    let operation = AmdAssembly {
        firmware: Firmware::Packed {
            files,
            reset_offset: None,
        },
        ffs: None,
        nvram: None,
        stage_unit: "stage".into(),
        flash_size: raw.flash_size,
        bios_directory: PICASSO,
        reset_destination: raw.reset_destination,
        reset_size: raw.reset_image.len() as u32,
        apob_destination: raw.apob_destination,
        shared_memory: raw.shared_memory,
        psp: Default::default(),
    };
    let sources = [raw.psp[0].bytes.as_slice(), raw.bios[0].bytes.as_slice()];
    let packed = operation
        .assemble(&sources, &raw.reset_image, None)
        .unwrap();
    let expected = raw.pack().unwrap();
    assert_eq!(packed.offset, 0xe20000);
    assert!(!packed.is_whole_flash(raw.flash_size));
    assert_eq!(packed.bytes, expected.bytes);
    assert_eq!(packed.facts.efs, 0xe20000);
    assert_eq!(
        operation
            .bindings()
            .unwrap()
            .iter()
            .map(|file| file.environment.as_str())
            .collect::<Vec<_>>(),
        ["PSP_00_00_00", "BIOS_60_00_00"]
    );
    assert!(
        operation
            .assemble(&sources, &raw.reset_image[..0x1ffff], None)
            .is_err()
    );
    let with_files = |change: fn(&mut PspFiles)| {
        let mut changed = operation.clone();
        let Firmware::Packed { files, .. } = &mut changed.firmware;
        change(files);
        changed
    };
    assert!(
        with_files(|f| f.bios.push(f.bios[0].clone()))
            .bindings()
            .is_err()
    );
    assert!(
        with_files(|f| f.bios[0].key.subprogram = 1)
            .bindings()
            .is_err()
    );
}

#[test]
fn directories_and_copy_to_dram() {
    let image = image();
    let packed = image.pack().unwrap();
    assert_eq!(packed.psp_directory, 0xe30000); // initial 64-KiB quirk
    let start = (packed.psp_directory - packed.offset) as usize;
    let h = Header::read_from_bytes(&packed.bytes[start..start + 16]).unwrap();
    assert_eq!(h.cookie, PSP_COOKIE);
    assert_eq!(h.count.get(), 2);
    assert_eq!(
        h.checksum.get(),
        fletcher32(&packed.bytes[start + 8..start + 16 + 2 * 16])
    );
    let start = (packed.bios_directory - packed.offset) as usize;
    let h = Header::read_from_bytes(&packed.bytes[start..start + 16]).unwrap();
    let count = h.count.get() as usize;
    assert_eq!(count, 6); // PSB, APCB, backup APCB, APOB, reset image, shared memory
    assert_eq!(
        h.checksum.get(),
        fletcher32(&packed.bytes[start + 8..start + 16 + count * 24])
    );
    let entries: Vec<_> = packed.bytes[start + 16..start + 16 + count * 24]
        .as_chunks::<24>()
        .0
        .iter()
        .map(|b| BiosEntry::read_from_bytes(b).unwrap())
        .collect();
    let bin = entries.iter().find(|e| e.kind == 0x62).unwrap();
    assert_eq!(bin.destination.get(), 0x2030000);
    assert_eq!(bin.size.get(), 0x20000);
    assert_eq!(bin.flags, 3);
    let src = (bin.source.get() - 0xff000000 - u64::from(packed.offset)) as usize;
    assert_eq!(&packed.bytes[src..src + 0x20000], &image.reset_image);
    let apcb = entries.iter().find(|e| e.kind == 0x60).unwrap();
    let backup = entries.iter().find(|e| e.kind == 0x68).unwrap();
    assert_eq!(apcb.source, backup.source);
    assert_eq!(apcb.size.get(), 0x2000);
    let shared = entries.iter().find(|e| e.kind == 0x6b).unwrap();
    assert_eq!(shared.source.get(), u64::MAX);
    assert_eq!(
        &packed.bytes[0x14..0x18],
        &(0xff000000u32 + packed.psp_directory).to_le_bytes()
    );
    assert_eq!(&packed.bytes[0x24..0x28], &[0xff; 4]);
}
#[test]
fn bounds_and_duplicates_fail_closed() {
    let mut i = image();
    i.partition_end = i.efs_offset + 0x100;
    assert!(i.pack().is_err());
    let mut i = image();
    i.psp.push(i.psp[0].clone());
    assert!(i.pack().is_err());
    let mut i = image();
    i.apob_nv = Some(FlashRegion {
        offset: 0xe40000,
        size: 0x1000,
    });
    assert!(i.pack().is_err());
    let mut i = image();
    i.reset_image.pop();
    assert!(i.pack().is_err());
}
#[test]
fn apcb_subprograms_cannot_alias_generated_backup_selectors() {
    let mut i = image();
    i.bios.push(Blob {
        key: Key::new(0x60, 1, 0),
        bytes: vec![0x78; 0x1000],
    });
    assert!(i.pack().unwrap_err().contains("APCB selector"));
    i.bios[1].key.kind = 0x68;
    assert!(i.pack().unwrap_err().contains("APCB selector"));
}
#[test]
fn nv_physical_addresses_and_reserved_bits() {
    let mut i = image();
    i.apob_nv = Some(FlashRegion {
        offset: 0x200000,
        size: 0x1000,
    });
    i.psp_nv = Some(FlashRegion {
        offset: 0x202000,
        size: 0x1000,
    });
    let p = i.pack().unwrap();
    let s = (p.psp_directory - p.offset) as usize;
    let e = PspEntry::read_from_bytes(&p.bytes[s + 32..s + 48]).unwrap();
    assert_eq!(e.kind, 4);
    assert_eq!(e.address.get(), 0xff202000);
    assert_eq!(e.attributes, [0; 2]); // no Gen2 ROM Armor attributes
}
#[test]
fn config_is_strict_and_optional_files_are_not_read() {
    use std::path::Path;
    let text = "FIRMWARE_LOCATION blobs\nSOC_NAME Picasso\nPSP_SMUFW1_SUB2_FILE pco\nPSP_MP2FW1_FILE missing\nPSP_PMUI_FILE_SUB1_INS4 pmu\n";
    let files = config::parse(text, Path::new("root"), None, config::Options::default()).unwrap();
    assert_eq!(
        files.psp,
        vec![(Key::new(8, 2, 0), Path::new("root/blobs/pco").to_path_buf())]
    );
    assert_eq!(files.bios[0].0, Key::new(0x64, 1, 4));
    assert!(
        config::parse(
            &text.replace("Picasso", "Renoir"),
            Path::new("root"),
            None,
            config::Options::default()
        )
        .is_err()
    );
    assert!(
        config::parse(
            &(text.to_string() + "UNKNOWN_FILE x\n"),
            Path::new("root"),
            None,
            config::Options::default()
        )
        .is_err()
    );
}
#[test]
fn packs_the_pinned_image() {
    let blob = |kind, subprogram, instance, size: usize| Blob {
        key: Key::new(kind, subprogram, instance),
        bytes: (0..size).map(|i| (i as u8) ^ kind).collect(),
    };
    let mut i = image();
    i.psp = vec![
        blob(0, 0, 0, 0x240),
        blob(1, 0, 0, 0x1234),
        blob(4, 0, 0, 0x800),
        blob(8, 1, 0, 0x300),
        blob(0x30, 0, 0, 0x999),
        blob(0x52, 0, 0, 0x101),
    ];
    i.bios = vec![
        blob(5, 0, 0, 0x340),
        blob(0x60, 0, 0, 0x1d00),
        blob(0x60, 0, 1, 0x1100),
        blob(0x64, 0, 1, 0x3c10),
        blob(0x65, 0, 1, 0x330),
        blob(0x66, 0, 0, 0xc80),
        blob(0x6a, 0, 0, 0x20),
    ];
    i.apob_nv = Some(FlashRegion {
        offset: 0x20_0000,
        size: 0x1_0000,
    });
    let packed = i.pack().unwrap();
    assert_eq!(
        (packed.offset, packed.psp_directory, packed.bios_directory),
        (0xe2_0000, 0xe3_0000, 0xe3_5000)
    );
    assert_eq!(packed.bytes.len(), 0x4_0000);
    let hex: String = fstart_crypto::digest::hash_sha256(&packed.bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "7bbb2d0868c36b3e1cc016dec3802261125d20f1965b7d7cfc4afe07f946979f"
    );
}

const SLOT: BiosDirectorySlot = BiosDirectorySlot::Family17hModel00;

#[test]
fn reads_the_mj11_reset_image_entry() {
    let facts = read_bios_directory(&two_level_vendor(), 0x29_4000, SLOT).unwrap();
    assert_eq!(
        facts,
        BiosDirectoryFacts {
            efs: 0x2_0000,
            reset_offset: 0xdb_0000,
            reset_size: 0x25_0000,
            reset_destination: 0x76db_0000,
            apob_destination: 0x3000_0000,
        }
    );
    // The level-2 microcode at 0x25f800 would be cut off by a smaller
    // prefix.
    assert!(read_bios_directory(&two_level_vendor(), 0x26_0000, SLOT).is_err());
    assert!(read_bios_directory(&two_level_vendor(), 0x26_0480, SLOT).is_ok());
}

/// Point level 2's entry `index` to `destination` and fix its checksum.
fn set_level2_destination(image: &mut [u8], index: usize, destination: u64) {
    let mut i = 0;
    modify_entries::<BiosEntry>(image, 0x22_9000, |e| {
        if i == index {
            e.destination.set(destination);
        }
        i += 1;
    })
    .unwrap();
}

#[test]
fn a_psp_blob_beyond_the_vendor_prefix_is_rejected() {
    let mut image = two_level_vendor();
    let entry = PspEntry::new(1, 0, 0, 0x1000, 0xff30_0000);
    write_table(&mut image[0x5_7000..], PSP_COOKIE, 0, &[entry]).unwrap();
    assert!(
        read_bios_directory(&image, 0x29_4000, SLOT)
            .unwrap_err()
            .contains("OutsidePrefix")
    );
}

#[test]
fn the_psp_boots_from_level_2() {
    let mut image = two_level_vendor();
    // The APOB of level 2, not level 1's.
    set_level2_destination(&mut image, 1, 0x3100_0000);
    let facts = read_bios_directory(&image, 0x29_4000, SLOT).unwrap();
    assert_eq!(facts.apob_destination, 0x3100_0000);
    // The levels must agree on the reset image.
    set_level2_destination(&mut image, 2, 0x7600_0000);
    assert!(read_bios_directory(&image, 0x29_4000, SLOT).is_err());
}

/// An MJ11-like vendor image: the EFS, a level-1 BIOS directory that
/// points to a level 2 and contents up to the prefix end.
fn two_level_vendor() -> Vec<u8> {
    let mut image = vec![0xffu8; 0x100_0000];
    image[..0x29_4000]
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = ((i * 31) >> 8) as u8);
    let mut put =
        |offset: usize, bytes: &[u8]| image[offset..offset + bytes.len()].copy_from_slice(bytes);
    let efs: Vec<u8> = [0x55aa_55aa, 0, 0, 0, 0, 0xff05_7000, 0xff0f_7000]
        .into_iter()
        .chain([u32::MAX; 5])
        .flat_map(u32::to_le_bytes)
        .collect();
    put(0x2_0000, &[0xff; 0x60]);
    put(0x2_0000, &efs);
    // The EFS's PSP pointer is a reachable dependency too.
    write_table::<PspEntry>(&mut image[0x5_7000..], PSP_COOKIE, 0, &[]).unwrap();
    let mut directory = |offset: usize, cookie: &[u8; 4], entries: &[(u8, u8, u32, u64, u64)]| {
        let entries: Vec<_> = entries
            .iter()
            .map(|&(kind, flags, size, source, destination)| {
                BiosEntry::new(kind, 0, 0, flags, size, source, destination)
            })
            .collect();
        write_table(&mut image[offset..], *cookie, 0, &entries).unwrap();
    };
    directory(
        0xf_7000,
        b"$BHD",
        &[
            (0x60, 0, 0x4000, 0xff0f_8000, u64::MAX),
            (0x61, 0, 0, 0, 0x3000_0000),
            (0x62, 3, 0x25_0000, 0xffdb_0000, 0x76db_0000),
            (0x70, 0, 0x400, 0xff22_9000, u64::MAX),
        ],
    );
    directory(
        0x22_9000,
        b"$BL2",
        &[
            (0x60, 0, 0x1d00, 0xff22_a000, u64::MAX),
            (0x61, 0, 0, 0, 0x3000_0000),
            (0x62, 3, 0x25_0000, 0xffdb_0000, 0x76db_0000),
            (0x63, 0, 0x2_0000, 0xff22_e000, u64::MAX),
            (0x66, 0, 0xc80, 0xff25_f800, u64::MAX),
        ],
    );
    image
}

/// An FFS with its locator and a finalized trust block, and a stage with
/// the placeholders of both.
fn ffs_and_stage(reset_size: usize) -> (Vec<u8>, Vec<u8>) {
    use fstart_core::ffs::{LocatorBlock, trust::TrustBlock};
    let pattern = |len: usize, seed: usize| -> Vec<u8> {
        (0..len).map(|i| ((i * seed) >> 4) as u8).collect()
    };
    let mut ffs = pattern(0x2000, 7);
    let mut locator = LocatorBlock::placeholder();
    locator.root_offset.set(0x100);
    locator.root_size.set(0x20);
    locator.write_to(&mut ffs[0x40..]);
    let mut trust = TrustBlock::placeholder();
    trust.set_key_count(1);
    trust.write_to(&mut ffs[0x400..]);
    let mut stage = pattern(reset_size, 13);
    LocatorBlock::placeholder().write_to(&mut stage[0x1000..]);
    TrustBlock::placeholder().write_to(&mut stage[0x2000..]);
    (ffs, stage)
}

fn ffs_window(offset: u32, size: u32) -> assembly::FfsWindow {
    assembly::FfsWindow {
        step: "ffs".into(),
        offset,
        size,
    }
}

#[test]
fn native_image_has_the_partition_the_compressed_reset_and_the_ffs() {
    for (reset_size, reset_offset) in [(0x20_0000, 0x98_0000), (0x40_0000, 0x78_0000)] {
        check_native_image(reset_size, reset_offset);
    }
}

fn check_native_image(reset_size: u32, reset_offset: u32) {
    use assembly::{ApcbFiles, FileBlob, PspFiles, PspOptions};
    use fstart_amdfw_format::{Flash, directory::BiosEntry};
    // An APCB v2 template with PSP/0x60 GPIO and MEMORY/0x30 SPD entries.
    let mut template = Vec::new();
    template.extend_from_slice(b"APCB");
    template.extend_from_slice(&0x20u16.to_le_bytes());
    template.extend_from_slice(&0x20u16.to_le_bytes());
    template.extend_from_slice(&0u32.to_le_bytes());
    template.extend_from_slice(&1u32.to_le_bytes());
    template.resize(0x20, 0);
    let psp_at = template.len();
    template.extend_from_slice(b"PSPG");
    template.extend_from_slice(&0x1701u16.to_le_bytes());
    template.extend_from_slice(&16u16.to_le_bytes());
    template.extend_from_slice(&1u16.to_le_bytes());
    template.extend_from_slice(&[0; 6]);
    let mut psp_data = vec![0u8; 16];
    psp_data[0..2].copy_from_slice(&3u16.to_le_bytes());
    template.extend_from_slice(&0x1701u16.to_le_bytes());
    template.extend_from_slice(&0x60u16.to_le_bytes());
    template.extend_from_slice(&((16 + psp_data.len()) as u16).to_le_bytes());
    template.extend_from_slice(&[0u8; 10]);
    template.extend_from_slice(&psp_data);
    let psp_size = (template.len() - psp_at) as u32;
    template[psp_at + 12..psp_at + 16].copy_from_slice(&psp_size.to_le_bytes());
    let mem_at = template.len();
    template.extend_from_slice(b"MEMG");
    template.extend_from_slice(&0x1704u16.to_le_bytes());
    template.extend_from_slice(&16u16.to_le_bytes());
    template.extend_from_slice(&1u16.to_le_bytes());
    template.extend_from_slice(&[0; 6]);
    let mut mem_data = vec![0u8; 4 + 28 + 512];
    mem_data[0..4].copy_from_slice(&1u32.to_le_bytes());
    template.extend_from_slice(&0x1704u16.to_le_bytes());
    template.extend_from_slice(&0x30u16.to_le_bytes());
    template.extend_from_slice(&((16 + mem_data.len()) as u16).to_le_bytes());
    template.extend_from_slice(&[0u8; 10]);
    template.extend_from_slice(&mem_data);
    let mem_size = (template.len() - mem_at) as u32;
    template[mem_at + 12..mem_at + 16].copy_from_slice(&mem_size.to_le_bytes());
    let total = template.len() as u32;
    template[8..12].copy_from_slice(&total.to_le_bytes());
    fstart_amdfw_format::apcb::fix_checksum(&mut template).unwrap();
    let files = PspFiles {
        flash_size: 0x100_0000,
        efs_offset: 0xe2_0000,
        partition_end: 0x100_0000,
        spi_read_mode: Some(4),
        spi_speed: Some(0),
        micron: false,
        soft_fuse: Some(0x3000_0000),
        two_level: false,
        generation_mask: None,
        espi: None,
        psp: vec![FileBlob {
            key: Key::new(1, 0, 0),
            path: "psp.bin".into(),
            level: Default::default(),
        }],
        // The microcode goes into the FFS, not the BIOS directory.
        bios: vec![FileBlob {
            key: Key::new(bios::MICROCODE, 0, 0),
            path: "ucode.bin".into(),
            level: Default::default(),
        }],
        fixed: vec![],
        apob_nv: None,
        psp_nv: None,
        apcb: Some(ApcbFiles {
            template: "apcb.bin".into(),
            gpio: [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]],
            spds: vec!["spd-a.bin".into(), "spd-b.hex".into()],
            second_channel: false,
            strip_manufacturer: true,
        }),
        apcb_overlay: Default::default(),
        assets: vec![],
        psp_app: false,
    };
    let (ffs, stage) = ffs_and_stage(reset_size as usize);
    let operation = AmdAssembly {
        firmware: Firmware::Packed {
            files: files.clone(),
            reset_offset: Some(reset_offset),
        },
        ffs: Some(ffs_window(0xb8_0000, 0x29_f000)),
        nvram: None,
        stage_unit: "stage".into(),
        flash_size: 0x100_0000,
        bios_directory: PICASSO,
        reset_size,
        reset_destination: 0x300_0000,
        apob_destination: 0x200_1000,
        shared_memory: None,
        psp: PspOptions {
            compress_reset: true,
        },
    };
    assert_eq!(
        operation
            .bindings()
            .unwrap()
            .iter()
            .map(|file| file.environment.as_str())
            .collect::<Vec<_>>(),
        [
            "PSP_01_00_00",
            "BIOS_66_00_00",
            "APCB_TEMPLATE",
            "APCB_SPD_00",
            "APCB_SPD_01"
        ]
    );
    let spd_b = "ab ".repeat(512);
    let sources: [&[u8]; 5] = [
        &[0x11; 0x400],
        &[0x22; 0xc80],
        &template,
        &[0xaa; 512],
        spd_b.as_bytes(),
    ];
    let image = operation.assemble(&sources, &stage, Some(&ffs)).unwrap();
    assert!(image.is_whole_flash(0x100_0000));
    assert_eq!(image.facts.reset_offset, reset_offset);
    assert_eq!(image.bytes[0xb8_0000..][..ffs.len()], ffs[..]);
    assert!(
        image.bytes[..reset_offset as usize]
            .iter()
            .all(|&b| b == 0xff)
    );
    let levels = Flash::new(&image.bytes).bios_directories(PICASSO).unwrap();
    let boot = levels.boot();
    let reset = boot
        .entries
        .iter()
        .find(|e| e.kind == bios::RESET_IMAGE)
        .unwrap();
    assert!(reset.compressed() && reset.size.get() == reset_size);
    assert_eq!(
        reset.destination.get() + u64::from(reset.size.get()) - 16,
        0x300_0000 + u64::from(reset_size) - 16
    );
    assert!(!boot.entries.iter().any(|e| e.kind == bios::MICROCODE));
    let apcbs: Vec<_> = boot
        .entries
        .iter()
        .filter(|e: &&BiosEntry| e.kind == bios::APCB)
        .map(|e| boot.data(e).unwrap())
        .collect();
    assert_eq!(apcbs.len(), 2);
    let backups = boot.entries.iter().filter(|e| e.kind == bios::APCB_BACKUP);
    assert!(
        backups
            .zip(&apcbs)
            .all(|(e, apcb)| boot.data(e).unwrap() == *apcb)
    );
    assert_eq!(apcbs[0][156], 0xaa);
    assert_eq!(apcbs[1][156], 0xab);
    // The PSP's windows may not overlap.
    let mut overlapping = operation.clone();
    overlapping.ffs = Some(ffs_window(0x99_0000, 0x10_0000));
    assert!(overlapping.validate().is_err());
}

#[test]
fn native_settings_seed_changes_only_the_reserved_extent() {
    use crate::options::Seed;
    use fstart_options::{Source, varstore};

    let mut plan = offset_addressed(0x100_0000, BiosDirectorySlot::Family19h);
    let Firmware::Packed {
        files,
        reset_offset,
    } = &mut plan.firmware;
    files.partition_end = 0x80_0000;
    *reset_offset = Some(0xd0_0000);
    plan.reset_size = 0x30_0000;
    let (ffs, stage) = ffs_and_stage(plan.reset_size as usize);
    plan.ffs = Some(ffs_window(0x80_0000, 0x4c_0000));
    let store = super::FlashRegion {
        offset: 0xcc_0000,
        size: 0x4_0000,
    };
    plan.nvram = Some(store);
    let sources: [&[u8]; 3] = [&[0x11; 0x400], &[0x22; 0x400], &[0x33; 0x400]];
    let plain = plan.assemble(&sources, &stage, Some(&ffs)).unwrap();
    let mut seed = Seed {
        offset: store.offset,
        bytes: varstore::store(
            store.size as usize,
            &[varstore::Variable::option("s8040_pcie3_bifurcation", 3)],
        )
        .unwrap(),
    };
    let seeded = plan
        .assemble_seeded(&sources, &stage, Some(&ffs), Some(&seed))
        .unwrap();
    let start = store.offset as usize;
    let end = start + store.size as usize;
    assert!(plain.bytes[start..end].iter().all(|&b| b == 0xff));
    assert_eq!(seeded.bytes[..start], plain.bytes[..start]);
    assert_eq!(seeded.bytes[end..], plain.bytes[end..]);
    assert_eq!(
        varstore::VarstoreOptions::new(&seeded.bytes[start..end]).read("s8040_pcie3_bifurcation"),
        Ok(Some(3))
    );

    seed.offset -= 0x1000;
    assert!(
        plan.assemble_seeded(&sources, &stage, Some(&ffs), Some(&seed))
            .is_err()
    );
    plan.nvram.as_mut().unwrap().offset = seed.offset;
    assert!(plan.validate().is_err());
    plan.nvram.as_mut().unwrap().offset = end as u32;
    assert!(plan.validate().is_err());
    plan.nvram.as_mut().unwrap().offset = 0x7f_0000;
    assert!(plan.validate().is_err());
}

/// A Siena-like native image: two levels addressed by flash offset, the
/// SoC's generation word, a 64 MiB part and the reset image after the
/// partition, without an FFS.
fn offset_addressed(flash_size: u32, slot: BiosDirectorySlot) -> AmdAssembly {
    use super::{Key, levels::Level};
    let file = |kind, level, path: &str| assembly::FileBlob {
        key: Key::new(kind, 0, 0),
        path: path.into(),
        level,
    };
    AmdAssembly {
        firmware: Firmware::Packed {
            files: assembly::PspFiles {
                flash_size,
                efs_offset: 0x2_0000,
                partition_end: 0xa0_0000,
                spi_read_mode: None,
                spi_speed: None,
                micron: false,
                soft_fuse: None,
                two_level: true,
                generation_mask: Some(1),
                espi: Some([0x0e, 0xff, 0xff, 0xff]),
                psp: vec![file(0, Level::One, "key"), file(1, Level::Both, "boot")],
                bios: vec![file(bios::APCB_BACKUP, Level::Both, "apcb")],
                fixed: vec![assembly::FixedEntry {
                    psp: false,
                    key: Key::new(0x63, 0, 0),
                    level: Level::Two,
                    content: assembly::Fixed::Erased(0xb_0000),
                }],
                apob_nv: None,
                psp_nv: None,
                apcb: None,
                apcb_overlay: Default::default(),
                assets: vec![],
                psp_app: false,
            },
            reset_offset: Some(0xa0_0000),
        },
        ffs: None,
        nvram: None,
        stage_unit: "stage".into(),
        flash_size,
        bios_directory: slot,
        reset_size: 0x60_0000,
        reset_destination: 0x75a0_0000,
        apob_destination: 0x7590_0000,
        shared_memory: None,
        psp: Default::default(),
    }
}

#[test]
fn offset_addressed_images_span_large_parts_with_the_socs_generation_word() {
    let plan = offset_addressed(0x400_0000, BiosDirectorySlot::Family19h);
    let sources: Vec<Vec<u8>> = plan
        .bindings()
        .unwrap()
        .iter()
        .map(|b| vec![b.path.len() as u8; 0x800 + b.path.len()])
        .collect();
    let sources: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
    let stage = vec![0xa5; 0x60_0000];
    let image = plan.assemble(&sources, &stage, None).unwrap();
    assert!(image.is_whole_flash(0x400_0000));
    assert_eq!(
        image.bytes[0x2_0024..0x2_0028],
        0xffff_fffeu32.to_le_bytes()
    );
    assert_eq!(image.facts.reset_offset, 0xa0_0000);
    assert_eq!(image.facts.apob_destination, 0x7590_0000);
    assert_eq!(image.bytes[0xa0_0000..0x100_0000], stage);
    assert!(image.bytes[0x100_0000..].iter().all(|&b| b == 0xff));
    // The boot ROMs that read physical addresses map 16 MiB at most.
    assert!(
        offset_addressed(0x200_0000, BiosDirectorySlot::Family17hModel10)
            .validate()
            .is_err()
    );
    assert!(offset_addressed(0x200_0000, SLOT).validate().is_err());
}
