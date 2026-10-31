// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use assembly::{AmdAssembly, BiosDirectoryFacts, Firmware, read_bios_directory};
use fstart_amdfw_format::{
    directory::{Header, fletcher32, modify_entries, write_table},
    efs::BiosDirectorySlot,
};
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
        reset_slot: None,
        reset_destination: 0x2030000,
        apob_destination: 0x2001000,
        shared_memory: Some((0x2011000, 0x8000)),
        apob_nv: None,
        psp_nv: None,
    }
}
#[test]
fn packed_vilboz_uses_sources_without_apcb_file_inputs() {
    use assembly::{FileBlob, PicassoFiles};
    let mut raw = image();
    let apcbs = apcb_toml::compile(
        include_str!("../../../../boards/google/zork/apcb/vilboz.toml"),
        apcb_toml::Profile::Vilboz,
    )
    .unwrap();
    raw.bios = apcbs
        .iter()
        .map(|a| Blob {
            key: Key::new(a.selector.kind, a.selector.subprogram, a.selector.instance),
            bytes: a.bytes.clone(),
        })
        .collect();
    let operation = AmdAssembly {
        apcbs,
        firmware: Firmware::Packed {
            files: PicassoFiles {
                flash_size: raw.flash_size,
                efs_offset: raw.efs_offset,
                partition_end: raw.partition_end,
                spi_read_mode: raw.spi_read_mode,
                spi_speed: raw.spi_speed,
                micron: raw.micron,
                soft_fuse: raw.soft_fuse,
                psp: vec![FileBlob {
                    key: raw.psp[0].key,
                    path: "signed-psp.bin".into(),
                }],
                bios: vec![],
                apob_nv: raw.apob_nv,
                psp_nv: raw.psp_nv,
                apcb: None,
                assets: vec![],
                psp_app: false,
            },
            reset_offset: None,
        },
        ffs: None,
        reset_unit: "reset".into(),
        flash_size: raw.flash_size,
        bios_directory: PicassoImage::BIOS_DIRECTORY,
        reset_destination: raw.reset_destination,
        reset_size: raw.reset_image.len() as u32,
        apob_destination: raw.apob_destination,
        shared_memory: raw.shared_memory,
        psp: Default::default(),
    };
    assert_eq!(operation.bindings().unwrap().len(), 1);
    let actual = operation
        .assemble(&[&raw.psp[0].bytes], &raw.reset_image, None)
        .unwrap();
    assert_eq!(actual.bytes, raw.pack().unwrap().bytes);
}

#[test]
fn typed_operation_packs_real_reset_input_without_a_fictitious_ffs_image() {
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
        apcb: None,
        assets: vec![],
        psp_app: false,
    };
    let operation = AmdAssembly {
        apcbs: vec![],
        firmware: Firmware::Packed {
            files,
            reset_offset: None,
        },
        ffs: None,
        reset_unit: "reset".into(),
        flash_size: raw.flash_size,
        bios_directory: PicassoImage::BIOS_DIRECTORY,
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
    let with_files = |change: fn(&mut PicassoFiles)| {
        let mut changed = operation.clone();
        if let Firmware::Packed { files, .. } = &mut changed.firmware {
            change(files);
        }
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
            security: default_security_config(),
            payload: None,
            microcode: None,
            data_assets: vec![],
            file_assets: vec![],
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
        apcbs: vec![],
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
        psp: Default::default(),
    }
}

#[test]
fn reviewed_mj11_donors_are_unchanged_by_source_replacement() {
    let (ffs, stage) = ffs_and_stage(0x25_0000);
    for (env, source, profile) in [
        (
            "FSTART_MJ11_EC0_DONOR",
            include_str!("../../../../boards/gigabyte/mj11/apcb/ec0.toml"),
            apcb_toml::Profile::Mj11Ec0F02,
        ),
        (
            "FSTART_MJ11_EC1_DONOR",
            include_str!("../../../../boards/gigabyte/mj11/apcb/ec1.toml"),
            apcb_toml::Profile::Mj11Ec1F09,
        ),
    ] {
        let Some(path) = std::env::var_os(env) else {
            eprintln!("skipping: set {env} to its reviewed vendor readout");
            continue;
        };
        let vendor = std::fs::read(path).unwrap();
        let baseline = mj11().assemble(&[&vendor], &stage, Some(&ffs)).unwrap();
        let mut plan = mj11();
        plan.apcbs = apcb_toml::compile(source, profile).unwrap();
        let generated = plan.assemble(&[&vendor], &stage, Some(&ffs)).unwrap();
        assert_eq!(generated.bytes, baseline.bytes, "{env}");
    }
}

#[test]
fn mj11_sources_replace_both_directory_levels_and_keep_slot_padding() {
    let generated = apcb_toml::Profile::Mj11Ec1F09
        .definition()
        .unwrap()
        .compile()
        .unwrap();
    let (ffs, stage) = ffs_and_stage(0x25_0000);
    let mut vendor = two_level_vendor();
    // This test adds the backup slots present in both reviewed directory levels.
    for (at, backup, size, level, cookie) in [
        (0xf_7000, 0xfc000, 0x4000, 0, *b"$BHD"),
        (0x22_9000, 0x22_c000, 0x1d00, 1, *b"$BL2"),
    ] {
        use fstart_amdfw_format::{Flash, Profile, ROM2_BASE, directory::Directory};
        let mut entries = Directory::<BiosEntry>::read_checked(
            Flash::region(&vendor, 0, ROM2_BASE, Profile::for_slot(SLOT)),
            at,
            level,
        )
        .unwrap()
        .entries
        .to_vec();
        entries.push(BiosEntry::new(
            bios::APCB_BACKUP,
            0,
            0,
            0,
            size,
            0xff00_0000u64 + backup as u64,
            u64::MAX,
        ));
        write_table(&mut vendor[at..], cookie, 0, &entries).unwrap();
    }
    for at in [0xf_8000, 0xfc000, 0x22_a000, 0x22_c000] {
        vendor[at..at + generated[0].bytes.len()].copy_from_slice(&generated[0].bytes);
    }
    let expected = mj11().assemble(&[&vendor], &stage, Some(&ffs)).unwrap();
    let mut plan = mj11();
    plan.apcbs = generated;
    assert_eq!(
        plan.assemble(&[&vendor], &stage, Some(&ffs)).unwrap().bytes,
        expected.bytes
    );
    let mut config = apcb_toml::Profile::Mj11Ec1F09.definition().unwrap();
    config.blobs[0].revision += 1;
    plan.apcbs = config.compile().unwrap();
    let mut edited = expected.bytes;
    for at in [0xf_8000, 0xfc000, 0x22_a000, 0x22_c000] {
        edited[at..at + plan.apcbs[0].bytes.len()].copy_from_slice(&plan.apcbs[0].bytes);
    }
    assert_eq!(
        plan.assemble(&[&vendor], &stage, Some(&ffs)).unwrap().bytes,
        edited
    );
    plan.apcbs[0].donor_sha256 = "0".repeat(64);
    assert!(plan.assemble(&[&vendor], &stage, Some(&ffs)).is_err());
    let mut large = apcb_toml::Profile::Mj11Ec1F09.definition().unwrap();
    large.tables.insert(
        "empty-ppr".into(),
        apcb_layout::Table::EmptyPpr { slots: 512 },
    );
    plan.apcbs = large.compile().unwrap();
    assert!(
        plan.assemble(&[&vendor], &stage, Some(&ffs))
            .unwrap_err()
            .contains("slot capacity")
    );
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
        "f40ad114b05e11aaf06812c9fbcdac7193b8db0e215381ea75f0c3aaffb6dabb"
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

#[test]
fn native_image_has_the_partition_the_compressed_reset_and_the_ffs() {
    use assembly::{ApcbFiles, FileBlob, PicassoFiles, PspOptions};
    use fstart_amdfw_format::{Flash, directory::BiosEntry};
    // An APCB template with the GPIO and one SPD sentinel.
    let mut template = vec![0; 128 + 28 + 512];
    template[..4].copy_from_slice(b"APCB");
    template[32..44].copy_from_slice(&[0xfa, 0xde, 0xdd, 0xad].repeat(3));
    template[156..162].copy_from_slice(&[0xf0, 0x05, 0xba, 0x11, 0, 0]);
    let files = PicassoFiles {
        flash_size: 0x100_0000,
        efs_offset: 0xe2_0000,
        partition_end: 0x100_0000,
        spi_read_mode: 4,
        spi_speed: 0,
        micron: false,
        soft_fuse: 0x3000_0000,
        psp: vec![FileBlob {
            key: Key::new(1, 0, 0),
            path: "psp.bin".into(),
        }],
        // The microcode goes into the FFS, not the BIOS directory.
        bios: vec![FileBlob {
            key: Key::new(bios::MICROCODE, 0, 0),
            path: "ucode.bin".into(),
        }],
        apob_nv: None,
        psp_nv: None,
        apcb: Some(ApcbFiles {
            template: "apcb.bin".into(),
            gpio: [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]],
            spds: vec!["spd-a.bin".into(), "spd-b.hex".into()],
            second_channel: false,
            strip_manufacturer: true,
        }),
        assets: vec![],
        psp_app: false,
    };
    let (ffs, stage) = ffs_and_stage(0x20_0000);
    let operation = AmdAssembly {
        apcbs: vec![],
        firmware: Firmware::Packed {
            files: files.clone(),
            reset_offset: Some(0x98_0000),
        },
        ffs: Some(ffs_window(0xb8_0000, 0x29_f000)),
        reset_unit: "reset".into(),
        flash_size: 0x100_0000,
        bios_directory: PicassoImage::BIOS_DIRECTORY,
        reset_size: 0x20_0000,
        reset_destination: 0x300_0000,
        apob_destination: 0x200_1000,
        shared_memory: None,
        psp: PspOptions {
            compress_reset: true,
            spi: None,
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
    assert_eq!(image.facts.reset_offset, 0x98_0000);
    assert_eq!(image.bytes[0xb8_0000..][..ffs.len()], ffs[..]);
    assert!(image.bytes[..0x98_0000].iter().all(|&b| b == 0xff));
    let levels = Flash::new(&image.bytes)
        .bios_directories(PicassoImage::BIOS_DIRECTORY)
        .unwrap();
    let boot = levels.boot();
    let reset = boot
        .entries
        .iter()
        .find(|e| e.kind == bios::RESET_IMAGE)
        .unwrap();
    assert!(reset.compressed() && reset.size.get() == 0x20_0000);
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
fn takes_the_vendor_microcode_from_level_2() {
    use fstart_core::board::{AmdMicrocodeConfig, AmdVendorMicrocode};
    let mut vendor = two_level_vendor();
    // The level-2 microcode patch is for Zeppelin (equivalence ID 0x8012).
    vendor[0x25_f818..0x25_f81a].copy_from_slice(&0x8012u16.to_le_bytes());
    let path = std::env::temp_dir().join(format!("fstart-amd-ucode-{}.bin", std::process::id()));
    std::fs::write(&path, &vendor).unwrap();
    let config = AmdMicrocodeConfig {
        files: Default::default(),
        vendor_images: heapless::Vec::from_slice(&[AmdVendorMicrocode {
            image: path.to_str().unwrap().try_into().unwrap(),
            bios_directory: SLOT,
            origin: 0,
        }])
        .unwrap(),
    };
    let blob = crate::assemble::amd_microcode(&config, std::path::Path::new("/"));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(blob.unwrap(), vendor[0x25_f800..0x25_f800 + 0xc80]);
}

#[test]
fn the_psp_inflates_the_reset_image() {
    use fstart_amdfw_format::{Flash, directory::BiosEntry};
    let (ffs, stage) = ffs_and_stage(0x25_0000);
    let vendor = two_level_vendor();
    let plain = mj11()
        .assemble(&[&vendor], &stage, Some(&ffs))
        .unwrap()
        .bytes;
    let mut operation = mj11();
    operation.psp = assembly::PspOptions {
        compress_reset: true,
        spi: Some((7, 1)),
    };
    let image = operation
        .assemble(&[&vendor], &stage, Some(&ffs))
        .unwrap()
        .bytes;
    let levels = Flash::new(&image).bios_directories(SLOT).unwrap();
    assert_eq!(levels.iter().count(), 2);
    for directory in levels.iter() {
        let reset = directory.entries.iter().find(|e| e.kind == 0x62).unwrap();
        assert_eq!(reset.flags & BiosEntry::COMPRESSED, BiosEntry::COMPRESSED);
    }
    assert_eq!(image[0x2_0043..0x2_0045], [7, 1]);
    // In the prefix only the EFS SPI fields and each directory's checksum
    // and reset entry flags change.
    let allowed = |i: usize| {
        matches!(i, 0x2_0043 | 0x2_0044 | 0xf_7042 | 0x22_9042)
            || (0xf_7004..0xf_7008).contains(&i)
            || (0x22_9004..0x22_9008).contains(&i)
    };
    assert!((0..0x29_4000).all(|i| image[i] == vendor[i] || allowed(i)));
    assert_eq!(image[0x29_4000..0xdb_0000], plain[0x29_4000..0xdb_0000]);
    let inflated = assembly::amd_decompress(&image[0xdb_0000..], 0x25_0000).unwrap();
    assert_eq!(inflated, plain[0xdb_0000..]);
    // An uncompressed replacement must not inherit a compressed vendor flag.
    let raw = mj11()
        .assemble(&[&image], &stage, Some(&ffs))
        .unwrap()
        .bytes;
    for directory in Flash::new(&raw).bios_directories(SLOT).unwrap().iter() {
        assert!(
            !directory
                .entries
                .iter()
                .find(|e| e.kind == 0x62)
                .unwrap()
                .compressed()
        );
    }
    assert_eq!(raw[0xdb_0000..], plain[0xdb_0000..]);
}
