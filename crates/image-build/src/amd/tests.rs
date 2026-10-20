// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use assembly::{AmdAssembly, BiosDirectoryFacts, Firmware, read_bios_directory};
use fstart_amdfw::{directory::fletcher32, efs::BiosDirectorySlot};
use zerocopy::FromBytes;
fn image() -> PicassoImage {
    PicassoImage {
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
        reset_destination: 0x2030000,
        apob_destination: 0x2001000,
        shared_memory: Some((0x2011000, 0x8000)),
        apob_nv: None,
        psp_nv: None,
    }
}
#[test]
fn typed_operation_packs_real_reset_input_without_a_fictitious_ffs_image() {
    use crate::build_plan::FileBinding;
    use assembly::{FileBlob, PicassoFiles};
    let raw = image();
    let files = PicassoFiles {
        flash_size: raw.flash_size,
        efs_offset: raw.efs_offset,
        partition_end: raw.partition_end,
        spi_read_mode: raw.spi_read_mode,
        spi_speed: raw.spi_speed,
        micron: raw.micron,
        soft_fuse: raw.soft_fuse,
        psp: vec![FileBlob {
            key: raw.psp[0].key,
            path: "explicit/psp.bin".into(),
        }],
        bios: vec![FileBlob {
            key: raw.bios[0].key,
            path: "explicit/apcb.bin".into(),
        }],
        apob_nv: raw.apob_nv,
        psp_nv: raw.psp_nv,
    };
    let operation = AmdAssembly {
        firmware: Firmware::Packed {
            configuration: FileBinding {
                environment: "IMAGE_CONFIG".into(),
                path: "image.json".into(),
                capacity: 65536,
            },
            files,
        },
        ffs: None,
        reset_unit: "reset".into(),
        flash_size: raw.flash_size,
        bios_directory: PicassoImage::BIOS_DIRECTORY,
        reset_destination: raw.reset_destination,
        reset_size: raw.reset_image.len() as u32,
        apob_destination: raw.apob_destination,
        shared_memory: raw.shared_memory,
    };
    let Firmware::Packed { files, .. } = &operation.firmware else {
        unreachable!()
    };
    let json = serde_json::to_vec(files).unwrap();
    let sources = [
        json.as_slice(),
        raw.psp[0].bytes.as_slice(),
        raw.bios[0].bytes.as_slice(),
    ];
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
        ["IMAGE_CONFIG", "PSP_00_00_00", "BIOS_60_00_00"]
    );
    assert!(
        operation
            .assemble(&sources, &raw.reset_image[..0x1ffff], None)
            .is_err()
    );
    let with_files = |change: fn(&mut PicassoFiles)| {
        let mut changed = operation.clone();
        if let Firmware::Packed { files, .. } = &mut changed.firmware {
            change(files);
        }
        changed
    };
    assert!(
        with_files(|f| f.soft_fuse ^= 1)
            .assemble(&sources, &raw.reset_image, None)
            .unwrap_err()
            .contains("configuration changed")
    );
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
        .chunks_exact(24)
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
    let entry = 0x22_9010 + index * 24;
    image[entry + 16..entry + 24].copy_from_slice(&destination.to_le_bytes());
    let checksum = fletcher32(&image[0x22_9008..0x22_9010 + 5 * 24]);
    image[0x22_9004..0x22_9008].copy_from_slice(&checksum.to_le_bytes());
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    put(0x2_0000, &efs);
    let mut directory = |offset: usize, cookie: &[u8; 4], entries: &[(u8, u8, u32, u64, u64)]| {
        let count = (entries.len() as u32).to_le_bytes();
        let mut table = [*cookie, [0; 4], count, [0; 4]].concat();
        for &(kind, flags, size, source, destination) in entries {
            table.extend([kind, 0, flags, 0]);
            table.extend(size.to_le_bytes());
            table.extend(source.to_le_bytes());
            table.extend(destination.to_le_bytes());
        }
        let checksum = fletcher32(&table[8..]);
        table[4..8].copy_from_slice(&checksum.to_le_bytes());
        put(offset, &table);
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
    use fstart_core::ffs::{AnchorBlock, trust::TrustBlock};
    let pattern = |len: usize, seed: usize| -> Vec<u8> {
        (0..len).map(|i| ((i * seed) >> 4) as u8).collect()
    };
    let mut ffs = pattern(0x2000, 7);
    let mut locator = AnchorBlock::placeholder();
    locator.manifest_offset.set(0x100);
    locator.manifest_size.set(0x20);
    locator.write_to(&mut ffs[0x40..]);
    let mut trust = TrustBlock::placeholder();
    trust.set_key_count(1);
    trust.write_to(&mut ffs[0x400..]);
    let mut stage = pattern(reset_size, 13);
    AnchorBlock::placeholder().write_to(&mut stage[0x1000..]);
    TrustBlock::placeholder().write_to(&mut stage[0x2000..]);
    (ffs, stage)
}

fn ffs_window(offset: u32, size: u32) -> assembly::FfsWindow {
    use fstart_core::*;
    assembly::FfsWindow {
        assembly: crate::build_plan::FfsAssembly {
            platform: Platform::X86_64,
            memory: vec![],
            flash: None,
            bootstrap: vec![],
            stages: StageLayout::MultiStage(heapless::Vec::new()),
            security: dev_security_config("unused"),
            payload: None,
            microcode: None,
            data_assets: vec![],
            full_flash_image: false,
            soc_image_format: SocImageFormat::None,
            boot_hart_id: 0,
            build: BoardBuildPolicy::default(),
        },
        offset,
        size,
    }
}

/// MJ11's layout with its vendor image.
fn mj11() -> AmdAssembly {
    AmdAssembly {
        firmware: Firmware::Vendor {
            image: crate::build_plan::FileBinding {
                environment: "VENDOR_FLASH".into(),
                path: "vendor.rom".into(),
                capacity: 0x100_0000,
            },
            prefix_size: 0x29_4000,
        },
        ffs: Some(ffs_window(0x29_4000, 0xdb_0000 - 0x29_4000)),
        reset_unit: "reset".into(),
        flash_size: 0x100_0000,
        bios_directory: SLOT,
        reset_size: 0x25_0000,
        reset_destination: 0x76db_0000,
        apob_destination: 0x3000_0000,
        shared_memory: None,
    }
}

#[test]
fn composes_the_pinned_image() {
    let (ffs, stage) = ffs_and_stage(0x25_0000);
    let vendor = two_level_vendor();
    let image = mj11().assemble(&[&vendor], &stage, Some(&ffs)).unwrap();
    assert!(image.is_whole_flash(0x100_0000));
    let image = image.bytes;
    assert_eq!(image[..0x29_4000], vendor[..0x29_4000]);
    assert_eq!(image[0x29_4000..0x29_6000], ffs);
    // The stage got the FFS's locator.
    assert_eq!(image[0xdb_1000..0xdb_1028], ffs[0x40..0x68]);
    assert_eq!(
        hex(&fstart_crypto::digest::hash_sha256(&image)),
        "021a94e0a1b68bfabee168c750b1e6c1dd48c5b607f8c77729a286d37a475488"
    );
    // The vendor image must describe the layout.
    let mut other = mj11();
    other.reset_destination = 0x7600_0000;
    assert!(other.assemble(&[&vendor], &stage, Some(&ffs)).is_err());
    assert!(
        mj11()
            .assemble(&[&vendor], &stage[1..], Some(&ffs))
            .is_err()
    );
    assert!(mj11().assemble(&[&vendor], &stage, None).is_err());
}
