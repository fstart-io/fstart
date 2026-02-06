//! Integration tests: build an FFS image, then read it back and verify.

use ed25519_dalek::{Signer, SigningKey};
use fstart_ffs::builder::{build_image, FfsImageConfig, InputFile, InputSegment, RegionConfig};
use fstart_ffs::reader::FfsReader;
use fstart_types::ffs::{
    Compression, FileType, RegionRole, SegmentFlags, SegmentKind, Signature, VerificationKey,
};
use rand_core::OsRng;

/// Generate a fresh Ed25519 key pair for testing.
fn dev_keypair() -> (SigningKey, VerificationKey) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let vk = VerificationKey::ed25519(0, verifying_key.to_bytes());
    (signing_key, vk)
}

/// Signing function that uses the given key.
fn make_signer(signing_key: &SigningKey) -> impl Fn(&[u8]) -> Result<Signature, String> + '_ {
    move |message: &[u8]| {
        let sig = signing_key.sign(message);
        Ok(Signature::ed25519(0, sig.to_bytes()))
    }
}

#[test]
fn test_ro_only_round_trip() {
    let (signing_key, vk) = dev_keypair();

    // Build a simple RO-only image with one file
    let stage_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "bootblock".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data.clone(),
                    load_addr: 0x8000_0000,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");

    // Now read it back
    let reader = FfsReader::new(&ffs.image);

    // Scan for anchor
    let anchor_offset = reader.scan_for_anchor().expect("should find anchor");
    assert_eq!(anchor_offset, 0, "anchor should be at offset 0");

    // Read anchor
    let anchor = reader
        .read_anchor(anchor_offset)
        .expect("should read anchor");
    assert_eq!(anchor.keys.len(), 1);
    assert_eq!(anchor.keys[0].key_id, 0);

    // Verify ro_region_base is persisted in the anchor
    let ro_base = anchor.ro_region_base;
    assert_eq!(
        ro_base, ffs.ro_region_base,
        "anchor should persist ro_region_base"
    );

    // Read and verify RO manifest
    let manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read RO manifest");
    assert_eq!(manifest.region, RegionRole::Ro);
    assert_eq!(manifest.entries.len(), 1);
    assert!(manifest.rw_slots.is_empty());
    assert!(manifest.nvs.is_none());

    // Check file entry
    let file = &manifest.entries[0];
    assert_eq!(file.name.as_str(), "bootblock");
    assert_eq!(file.file_type, FileType::StageCode);
    assert_eq!(file.segments.len(), 1);

    // Check segment
    let seg = &file.segments[0];
    assert_eq!(seg.name.as_str(), ".text");
    assert_eq!(seg.kind, SegmentKind::Code);
    assert_eq!(seg.loaded_size, stage_data.len() as u32);
    assert_eq!(seg.stored_size, stage_data.len() as u32);
    assert_eq!(seg.load_addr, 0x8000_0000);
    assert_eq!(seg.compression, Compression::None);
    assert!(seg.flags.execute);
    assert!(seg.flags.read);
    assert!(!seg.flags.write);

    // Read segment data back — offset is relative to RO region base
    let seg_data = reader
        .read_segment_data(seg, ro_base)
        .expect("should read segment data");
    assert_eq!(seg_data, &stage_data);

    // Digests should be present (builder computes SHA-256 with the 'all' feature)
    assert!(file.digests.sha256.is_some(), "should have SHA-256 digest");
}

#[test]
fn test_multi_segment_file() {
    let (signing_key, vk) = dev_keypair();

    let text_data = vec![0x01; 64]; // code
    let rodata = vec![0x02; 32]; // read-only data
    let data = vec![0x03; 16]; // read-write data

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "main-stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![
                    InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: text_data.clone(),
                        load_addr: 0x8000_0000,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    },
                    InputSegment {
                        name: ".rodata".to_string(),
                        kind: SegmentKind::ReadOnlyData,
                        data: rodata.clone(),
                        load_addr: 0x8000_1000,
                        compression: Compression::None,
                        flags: SegmentFlags::RODATA,
                    },
                    InputSegment {
                        name: ".data".to_string(),
                        kind: SegmentKind::ReadWriteData,
                        data: data.clone(),
                        load_addr: 0x8000_2000,
                        compression: Compression::None,
                        flags: SegmentFlags::DATA,
                    },
                ],
            }],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let ro_base = ffs.ro_region_base;
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader.read_anchor(0).expect("should read anchor");
    let manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read manifest");

    let file = FfsReader::find_file(&manifest, "main-stage").expect("should find file");
    assert_eq!(file.segments.len(), 3);

    // Verify each segment
    let text_seg = &file.segments[0];
    assert_eq!(text_seg.name.as_str(), ".text");
    assert_eq!(text_seg.kind, SegmentKind::Code);
    let text_back = reader
        .read_segment_data(text_seg, ro_base)
        .expect("read .text");
    assert_eq!(text_back, &text_data);

    let rodata_seg = &file.segments[1];
    assert_eq!(rodata_seg.name.as_str(), ".rodata");
    assert_eq!(rodata_seg.kind, SegmentKind::ReadOnlyData);
    let rodata_back = reader
        .read_segment_data(rodata_seg, ro_base)
        .expect("read .rodata");
    assert_eq!(rodata_back, &rodata);

    let data_seg = &file.segments[2];
    assert_eq!(data_seg.name.as_str(), ".data");
    assert_eq!(data_seg.kind, SegmentKind::ReadWriteData);
    let data_back = reader
        .read_segment_data(data_seg, ro_base)
        .expect("read .data");
    assert_eq!(data_back, &data);
}

#[test]
fn test_multiple_files_in_ro() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: vec![0xAA; 16],
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "board.cfg".to_string(),
                    file_type: FileType::BoardConfig,
                    segments: vec![InputSegment {
                        name: "config".to_string(),
                        kind: SegmentKind::ReadOnlyData,
                        data: vec![0xBB; 32],
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::RODATA,
                    }],
                },
                InputFile {
                    name: "device-tree.dtb".to_string(),
                    file_type: FileType::Fdt,
                    segments: vec![InputSegment {
                        name: "fdt".to_string(),
                        kind: SegmentKind::ReadOnlyData,
                        data: vec![0xCC; 64],
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::RODATA,
                    }],
                },
            ],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader.read_anchor(0).expect("should read anchor");
    let manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read manifest");

    assert_eq!(manifest.entries.len(), 3);

    // Look up files by name
    let bootblock = FfsReader::find_file(&manifest, "bootblock").expect("find bootblock");
    assert_eq!(bootblock.file_type, FileType::StageCode);

    let cfg = FfsReader::find_file(&manifest, "board.cfg").expect("find board.cfg");
    assert_eq!(cfg.file_type, FileType::BoardConfig);

    let fdt = FfsReader::find_file(&manifest, "device-tree.dtb").expect("find fdt");
    assert_eq!(fdt.file_type, FileType::Fdt);

    // File not found
    let missing = FfsReader::find_file(&manifest, "nonexistent");
    assert!(missing.is_err());
}

#[test]
fn test_ro_with_rw_slot() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "bootblock".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 16],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![RegionConfig {
            role: RegionRole::Rw,
            files: vec![InputFile {
                name: "main-stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xDD; 128],
                    load_addr: 0x8000_0000,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        }],
        nvs_size: None,
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader.read_anchor(0).expect("should read anchor");
    let ro_manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read RO manifest");

    // RO manifest should point to one RW slot
    assert_eq!(ro_manifest.rw_slots.len(), 1);
    let rw_ptr = &ro_manifest.rw_slots[0];
    assert_eq!(rw_ptr.role, RegionRole::Rw);

    // Read and verify the RW manifest
    let rw_manifest = reader
        .read_rw_manifest(rw_ptr, &anchor)
        .expect("should read RW manifest");
    assert_eq!(rw_manifest.region, RegionRole::Rw);
    assert_eq!(rw_manifest.entries.len(), 1);

    let rw_file = FfsReader::find_file(&rw_manifest, "main-stage").expect("find main-stage");
    assert_eq!(rw_file.file_type, FileType::StageCode);

    // Read RW segment data using the slot's region_base
    let rw_seg = &rw_file.segments[0];
    let rw_data = reader
        .read_segment_data(rw_seg, rw_ptr.region_base)
        .expect("read RW segment");
    assert_eq!(rw_data, &vec![0xDD; 128]);
}

#[test]
fn test_ro_with_rw_ab_slots() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "bootblock".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 16],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![
            RegionConfig {
                role: RegionRole::RwA,
                files: vec![InputFile {
                    name: "main-stage".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: vec![0x11; 64],
                        load_addr: 0x8000_0000,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                }],
            },
            RegionConfig {
                role: RegionRole::RwB,
                files: vec![InputFile {
                    name: "main-stage".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: vec![0x22; 64],
                        load_addr: 0x8000_0000,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                }],
            },
        ],
        nvs_size: None,
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader.read_anchor(0).expect("should read anchor");
    let ro_manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read RO manifest");

    // Should have 2 RW slots
    assert_eq!(ro_manifest.rw_slots.len(), 2);

    // Read slot A
    let slot_a = FfsReader::find_rw_slot(&ro_manifest, RegionRole::RwA).expect("find slot A");
    let manifest_a = reader.read_rw_manifest(slot_a, &anchor).expect("read RW-A");
    assert_eq!(manifest_a.region, RegionRole::RwA);
    let file_a = &manifest_a.entries[0];
    let data_a = reader
        .read_segment_data(&file_a.segments[0], slot_a.region_base)
        .expect("read A data");
    assert_eq!(data_a, &vec![0x11; 64]);

    // Read slot B
    let slot_b = FfsReader::find_rw_slot(&ro_manifest, RegionRole::RwB).expect("find slot B");
    let manifest_b = reader.read_rw_manifest(slot_b, &anchor).expect("read RW-B");
    assert_eq!(manifest_b.region, RegionRole::RwB);
    let file_b = &manifest_b.entries[0];
    let data_b = reader
        .read_segment_data(&file_b.segments[0], slot_b.region_base)
        .expect("read B data");
    assert_eq!(data_b, &vec![0x22; 64]);
}

#[test]
fn test_nvs_region() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 8],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![],
        nvs_size: Some(4096),
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader.read_anchor(0).expect("should read anchor");
    let manifest = reader
        .read_ro_manifest(&anchor)
        .expect("should read manifest");

    // NVS pointer should be present
    let nvs = manifest.nvs.as_ref().expect("should have NVS pointer");
    assert_eq!(nvs.size, 4096);

    // NVS region should be filled with 0xFF (erased flash)
    let nvs_data = &image.image[nvs.offset as usize..(nvs.offset + nvs.size) as usize];
    assert!(nvs_data.iter().all(|&b| b == 0xFF));
}

#[test]
fn test_tampered_signature_fails_verification() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 8],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    let mut image = build_image(&config, &make_signer(&signing_key))
        .expect("build should succeed")
        .image;

    // Tamper with the manifest region (flip a byte somewhere in the manifest area)
    let anchor_len = {
        let reader = FfsReader::new(&image);
        let anchor = reader.read_anchor(0).unwrap();
        anchor.ro_manifest_offset as usize + 10 // somewhere in the manifest
    };
    image[anchor_len] ^= 0xFF; // flip bits

    // Now reading the manifest should fail signature verification
    let reader = FfsReader::new(&image);
    let anchor = reader.read_anchor(0).expect("anchor should still parse");
    let result = reader.read_ro_manifest(&anchor);
    assert!(
        result.is_err(),
        "tampered manifest should fail verification"
    );
}

#[test]
fn test_wrong_key_fails_verification() {
    let (signing_key, _vk) = dev_keypair();
    let (_other_key, wrong_vk) = dev_keypair(); // different key!

    let config = FfsImageConfig {
        keys: vec![wrong_vk], // embed wrong key in anchor
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 8],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    // Sign with the correct key but embed the wrong key in anchor
    let image = build_image(&config, &make_signer(&signing_key))
        .expect("build should succeed")
        .image;

    let reader = FfsReader::new(&image);
    let anchor = reader.read_anchor(0).expect("anchor should parse");
    let result = reader.read_ro_manifest(&anchor);
    assert!(result.is_err(), "wrong key should fail verification");
}

#[test]
fn test_file_not_found() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        ro_region: RegionConfig {
            role: RegionRole::Ro,
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: vec![0xAA; 8],
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        },
        rw_regions: vec![],
        nvs_size: None,
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build");
    let reader = FfsReader::new(&image.image);
    let anchor = reader.read_anchor(0).unwrap();
    let manifest = reader.read_ro_manifest(&anchor).unwrap();

    let result = FfsReader::find_file(&manifest, "nonexistent");
    assert_eq!(
        result.err(),
        Some(fstart_ffs::reader::ReaderError::FileNotFound)
    );
}
