//! Integration tests: build an FFS image, then read it back and verify.

use ed25519_dalek::{Signer, SigningKey};
use fstart_ffs::builder::{build_image, FfsImageConfig, InputFile, InputRegion, InputSegment};
use fstart_ffs::reader::FfsReader;
use fstart_types::ffs::{
    AnchorBlock, Compression, EntryContent, FileType, RegionContent, SegmentFlags, SegmentKind,
    Signature, VerificationKey, ANCHOR_SIZE,
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

/// Create stage binary data with an embedded anchor placeholder.
///
/// The builder requires the first file to contain `FSTART01` magic so it
/// can find and patch the anchor. This simulates a bootblock binary that
/// has the `#[link_section = ".fstart.anchor"]` static.
fn stage_data_with_anchor(extra: &[u8]) -> Vec<u8> {
    let placeholder = AnchorBlock::placeholder();
    let mut data = vec![0u8; ANCHOR_SIZE + extra.len()];
    placeholder.write_to(&mut data[..ANCHOR_SIZE]);
    data[ANCHOR_SIZE..].copy_from_slice(extra);
    data
}

#[test]
fn test_ro_only_round_trip() {
    let (signing_key, vk) = dev_keypair();

    // Build a simple RO-only image with one file.
    let stage_data = stage_data_with_anchor(&[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04]);

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
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
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");

    // Now read it back
    let reader = FfsReader::new(&ffs.image);

    // Scan for anchor (now embedded inside the first file's data)
    let anchor_offset = reader.scan_for_anchor().expect("should find anchor");

    // Read anchor
    let anchor = reader
        .read_anchor(anchor_offset)
        .expect("should read anchor");
    assert_eq!(anchor.key_count, 1);
    assert_eq!(anchor.valid_keys().len(), 1);
    assert_eq!(anchor.valid_keys()[0].key_id, 0);

    // Read and verify manifest
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");
    assert_eq!(manifest.regions.len(), 1);

    // Check region
    let region = &manifest.regions[0];
    assert_eq!(region.name.as_str(), "ro");
    let children = match &region.content {
        RegionContent::Container { children } => children,
        _ => panic!("expected Container"),
    };
    assert_eq!(children.len(), 1);

    // Check file entry
    let entry = &children[0];
    assert_eq!(entry.name.as_str(), "bootblock");
    let (file_type, segments, digests) = match &entry.content {
        EntryContent::File {
            file_type,
            segments,
            digests,
        } => (file_type, segments, digests),
        _ => panic!("expected File"),
    };
    assert_eq!(*file_type, FileType::StageCode);
    assert_eq!(segments.len(), 1);

    // Check segment
    let seg = &segments[0];
    assert_eq!(seg.name.as_str(), ".text");
    assert_eq!(seg.kind, SegmentKind::Code);
    assert_eq!(seg.loaded_size, stage_data.len() as u32);
    assert_eq!(seg.stored_size, stage_data.len() as u32);
    assert_eq!(seg.load_addr, 0x8000_0000);
    assert_eq!(seg.compression, Compression::None);
    assert!(seg.flags.execute);
    assert!(seg.flags.read);
    assert!(!seg.flags.write);

    // Digests should be present
    assert!(digests.sha256.is_some(), "should have SHA-256 digest");
}

#[test]
fn test_multi_segment_file() {
    let (signing_key, vk) = dev_keypair();

    let text_data = stage_data_with_anchor(&vec![0x01; 64]); // code with anchor
    let rodata = vec![0x02; 32]; // read-only data
    let data = vec![0x03; 16]; // read-write data

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
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
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let entry = FfsReader::find_entry(region, "main-stage").expect("should find file");

    let segments = match &entry.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    assert_eq!(segments.len(), 3);

    // Verify each segment
    let text_seg = &segments[0];
    assert_eq!(text_seg.name.as_str(), ".text");
    assert_eq!(text_seg.kind, SegmentKind::Code);
    let text_back = reader
        .read_segment_data(text_seg, region, entry)
        .expect("read .text");
    assert_eq!(text_back.len(), text_data.len());
    // The anchor region is patched post-build, so compare only the non-anchor suffix
    assert_eq!(&text_back[ANCHOR_SIZE..], &text_data[ANCHOR_SIZE..]);

    let rodata_seg = &segments[1];
    assert_eq!(rodata_seg.name.as_str(), ".rodata");
    assert_eq!(rodata_seg.kind, SegmentKind::ReadOnlyData);
    let rodata_back = reader
        .read_segment_data(rodata_seg, region, entry)
        .expect("read .rodata");
    assert_eq!(rodata_back, &rodata);

    let data_seg = &segments[2];
    assert_eq!(data_seg.name.as_str(), ".data");
    assert_eq!(data_seg.kind, SegmentKind::ReadWriteData);
    let data_back = reader
        .read_segment_data(data_seg, region, entry)
        .expect("read .data");
    assert_eq!(data_back, &data);
}

#[test]
fn test_multiple_files_in_ro() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 16]),
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
        }],
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader
        .read_anchor(image.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let children = match &region.content {
        RegionContent::Container { children } => children,
        _ => panic!("expected Container"),
    };
    assert_eq!(children.len(), 3);

    // Look up files by name
    let bootblock = FfsReader::find_entry(region, "bootblock").expect("find bootblock");
    assert!(matches!(
        &bootblock.content,
        EntryContent::File {
            file_type: FileType::StageCode,
            ..
        }
    ));

    let cfg = FfsReader::find_entry(region, "board.cfg").expect("find board.cfg");
    assert!(matches!(
        &cfg.content,
        EntryContent::File {
            file_type: FileType::BoardConfig,
            ..
        }
    ));

    let fdt = FfsReader::find_entry(region, "device-tree.dtb").expect("find fdt");
    assert!(matches!(
        &fdt.content,
        EntryContent::File {
            file_type: FileType::Fdt,
            ..
        }
    ));

    // File not found
    let missing = FfsReader::find_entry(region, "nonexistent");
    assert!(missing.is_err());
}

#[test]
fn test_multiple_container_regions() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![
            InputRegion::Container {
                name: "ro".to_string(),
                files: vec![InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 16]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                }],
            },
            InputRegion::Container {
                name: "rw-a".to_string(),
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
            InputRegion::Container {
                name: "rw-b".to_string(),
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
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader
        .read_anchor(image.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    // Should have 3 regions
    assert_eq!(manifest.regions.len(), 3);

    // Read rw-a
    let rw_a = FfsReader::find_region(&manifest, "rw-a").expect("find rw-a");
    let entry_a = FfsReader::find_entry(rw_a, "main-stage").expect("find main in rw-a");
    let segs_a = match &entry_a.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    let data_a = reader
        .read_segment_data(&segs_a[0], rw_a, entry_a)
        .expect("read rw-a data");
    assert_eq!(data_a, &vec![0x11; 64]);

    // Read rw-b
    let rw_b = FfsReader::find_region(&manifest, "rw-b").expect("find rw-b");
    let entry_b = FfsReader::find_entry(rw_b, "main-stage").expect("find main in rw-b");
    let segs_b = match &entry_b.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    let data_b = reader
        .read_segment_data(&segs_b[0], rw_b, entry_b)
        .expect("read rw-b data");
    assert_eq!(data_b, &vec![0x22; 64]);
}

#[test]
fn test_nvs_region() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![
            InputRegion::Container {
                name: "ro".to_string(),
                files: vec![InputFile {
                    name: "stage".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 8]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                }],
            },
            InputRegion::Raw {
                name: "nvs".to_string(),
                size: 4096,
                fill: 0xFF,
            },
        ],
    };

    let image = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&image.image);
    let anchor = reader
        .read_anchor(image.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    // Should have 2 regions: ro + nvs
    assert_eq!(manifest.regions.len(), 2);

    // NVS region
    let nvs = FfsReader::find_region(&manifest, "nvs").expect("find nvs");
    assert_eq!(nvs.size, 4096);
    match &nvs.content {
        RegionContent::Raw { fill } => assert_eq!(*fill, 0xFF),
        _ => panic!("expected Raw"),
    }

    // NVS region data should be all 0xFF
    let nvs_data = &image.image[nvs.offset as usize..(nvs.offset + nvs.size) as usize];
    assert!(nvs_data.iter().all(|&b| b == 0xFF));
}

#[test]
fn test_tampered_signature_fails_verification() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data_with_anchor(&[0xAA; 8]),
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let mut image = ffs.image;
    let anchor_off = ffs.anchor_offset;

    // Tamper with the manifest region (flip a byte somewhere in the manifest area)
    let tamper_offset = {
        let reader = FfsReader::new(&image);
        let anchor = reader.read_anchor(anchor_off).unwrap();
        anchor.manifest_offset as usize + 10 // somewhere in the manifest
    };
    image[tamper_offset] ^= 0xFF; // flip bits

    // Now reading the manifest should fail signature verification
    let reader = FfsReader::new(&image);
    let anchor = reader
        .read_anchor(anchor_off)
        .expect("anchor should still parse");
    let result = reader.read_manifest(&anchor);
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
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data_with_anchor(&[0xAA; 8]),
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        }],
    };

    // Sign with the correct key but embed the wrong key in anchor
    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");

    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("anchor should parse");
    let result = reader.read_manifest(&anchor);
    assert!(result.is_err(), "wrong key should fail verification");
}

#[test]
fn test_file_not_found() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data_with_anchor(&[0xAA; 8]),
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader.read_anchor(ffs.anchor_offset).unwrap();
    let manifest = reader.read_manifest(&anchor).unwrap();

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let result = FfsReader::find_entry(region, "nonexistent");
    assert_eq!(
        result.err(),
        Some(fstart_ffs::reader::ReaderError::FileNotFound)
    );
}

#[test]
fn test_region_not_found() {
    let (signing_key, vk) = dev_keypair();

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data_with_anchor(&[0xAA; 8]),
                    load_addr: 0x0,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            }],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader.read_anchor(ffs.anchor_offset).unwrap();
    let manifest = reader.read_manifest(&anchor).unwrap();

    let result = FfsReader::find_region(&manifest, "nonexistent");
    assert_eq!(
        result.err(),
        Some(fstart_ffs::reader::ReaderError::RegionNotFound)
    );
}

#[test]
fn test_bootblock_digest_valid_after_anchor_patch() {
    let (signing_key, vk) = dev_keypair();

    // Build image where the bootblock has an embedded anchor placeholder.
    // The builder patches the anchor post-layout, then recomputes the digest.
    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xDE, 0xAD, 0xBE, 0xEF]),
                        load_addr: 0x8000_0000,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "config".to_string(),
                    file_type: FileType::BoardConfig,
                    segments: vec![InputSegment {
                        name: "data".to_string(),
                        kind: SegmentKind::ReadOnlyData,
                        data: vec![0xBB; 32],
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::RODATA,
                    }],
                },
            ],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader
        .read_manifest(&anchor)
        .expect("should verify manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");

    // Bootblock digest should now be valid even though the anchor was patched
    let bootblock = FfsReader::find_entry(region, "bootblock").expect("find bootblock");
    reader
        .verify_entry_digests(bootblock, region)
        .expect("bootblock digest should be valid after anchor patch");

    // Non-bootblock files should also have valid digests
    let config_entry = FfsReader::find_entry(region, "config").expect("find config");
    reader
        .verify_entry_digests(config_entry, region)
        .expect("config digest should be valid");
}

// ---------------------------------------------------------------------------
// LZ4 compression tests
// ---------------------------------------------------------------------------

#[test]
fn test_lz4_compressed_segment_round_trip() {
    let (signing_key, vk) = dev_keypair();

    // Highly compressible data (repeated pattern)
    let compressible_data = vec![0xAB; 4096];

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 16]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "main-stage".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: compressible_data.clone(),
                        load_addr: 0x8010_0000,
                        compression: Compression::Lz4,
                        flags: SegmentFlags::CODE,
                    }],
                },
            ],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let entry = FfsReader::find_entry(region, "main-stage").expect("find main-stage");

    let (segments, _digests) = match &entry.content {
        EntryContent::File {
            segments, digests, ..
        } => (segments, digests),
        _ => panic!("expected File"),
    };
    assert_eq!(segments.len(), 1);

    let seg = &segments[0];
    assert_eq!(seg.compression, Compression::Lz4);
    assert_eq!(seg.loaded_size, 4096);
    // Compressed data should be smaller than original
    assert!(
        seg.stored_size < seg.loaded_size,
        "stored_size ({}) should be < loaded_size ({})",
        seg.stored_size,
        seg.loaded_size
    );
    // in_place_size should be >= loaded_size
    assert!(
        seg.in_place_size >= seg.loaded_size,
        "in_place_size ({}) should be >= loaded_size ({})",
        seg.in_place_size,
        seg.loaded_size
    );

    // Read the compressed data from the image
    let compressed_data = reader
        .read_segment_data(seg, region, entry)
        .expect("should read compressed segment");
    assert_eq!(compressed_data.len(), seg.stored_size as usize);

    // Decompress and verify it matches the original
    let mut decompressed = vec![0u8; seg.loaded_size as usize];
    let n =
        fstart_ffs::lz4::decompress_block(compressed_data, &mut decompressed).expect("decompress");
    assert_eq!(n, seg.loaded_size as usize);
    assert_eq!(decompressed, compressible_data);
}

#[test]
fn test_lz4_in_place_decompression() {
    let (signing_key, vk) = dev_keypair();

    // Use data with mixed compressibility to exercise the in-place margin
    let mut mixed_data = Vec::with_capacity(8192);
    for i in 0..8192u32 {
        // Alternating compressible and less-compressible patterns
        if (i / 256) % 2 == 0 {
            mixed_data.push(0xAA); // compressible runs
        } else {
            mixed_data.push((i & 0xFF) as u8); // varying bytes
        }
    }

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 16]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "payload".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: mixed_data.clone(),
                        load_addr: 0x8010_0000,
                        compression: Compression::Lz4,
                        flags: SegmentFlags::CODE,
                    }],
                },
            ],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let entry = FfsReader::find_entry(region, "payload").expect("find payload");

    let segments = match &entry.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    let seg = &segments[0];

    // Read compressed data from flash
    let compressed = reader
        .read_segment_data(seg, region, entry)
        .expect("read compressed data");

    // Simulate in-place decompression exactly as the runtime would:
    // 1. Allocate in_place_size buffer (simulating the load_addr region)
    // 2. Copy compressed data to the END
    // 3. Decompress from tail to head
    let buf_size = seg.in_place_size as usize;
    let mut buf = vec![0u8; buf_size];
    let src_offset = buf_size - compressed.len();
    buf[src_offset..].copy_from_slice(compressed);

    // Decompress in-place using raw pointers (same as the runtime)
    let n = unsafe {
        let src = core::slice::from_raw_parts(buf.as_ptr().add(src_offset), compressed.len());
        let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), seg.loaded_size as usize);
        fstart_ffs::lz4::decompress_block(src, dst).expect("in-place decompress should succeed")
    };

    assert_eq!(n, mixed_data.len());
    assert_eq!(&buf[..n], &mixed_data);
}

#[test]
fn test_lz4_multi_segment_with_mixed_compression() {
    let (signing_key, vk) = dev_keypair();

    let text_data = vec![0x01; 2048]; // compressible code
    let rodata = vec![0x02; 512]; // uncompressed rodata

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 8]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "stage".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![
                        InputSegment {
                            name: ".text".to_string(),
                            kind: SegmentKind::Code,
                            data: text_data.clone(),
                            load_addr: 0x8010_0000,
                            compression: Compression::Lz4,
                            flags: SegmentFlags::CODE,
                        },
                        InputSegment {
                            name: ".rodata".to_string(),
                            kind: SegmentKind::ReadOnlyData,
                            data: rodata.clone(),
                            load_addr: 0x8010_1000,
                            compression: Compression::None,
                            flags: SegmentFlags::RODATA,
                        },
                    ],
                },
            ],
        }],
    };

    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let entry = FfsReader::find_entry(region, "stage").expect("find stage");

    let segments = match &entry.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    assert_eq!(segments.len(), 2);

    // .text is LZ4-compressed
    let text_seg = &segments[0];
    assert_eq!(text_seg.compression, Compression::Lz4);
    assert!(text_seg.stored_size < text_seg.loaded_size);
    assert!(text_seg.in_place_size > 0);

    let text_compressed = reader
        .read_segment_data(text_seg, region, entry)
        .expect("read .text");
    let mut text_decompressed = vec![0u8; text_seg.loaded_size as usize];
    let n = fstart_ffs::lz4::decompress_block(text_compressed, &mut text_decompressed)
        .expect("decompress .text");
    assert_eq!(n, text_data.len());
    assert_eq!(text_decompressed, text_data);

    // .rodata is uncompressed
    let rodata_seg = &segments[1];
    assert_eq!(rodata_seg.compression, Compression::None);
    assert_eq!(rodata_seg.in_place_size, 0);

    let rodata_back = reader
        .read_segment_data(rodata_seg, region, entry)
        .expect("read .rodata");
    assert_eq!(rodata_back, &rodata);

    // Compressed files return CannotVerifyInPlace for digest verification
    // because the digest covers uncompressed data but the image has compressed
    let verify_result = reader.verify_entry_digests(entry, region);
    assert_eq!(
        verify_result.err(),
        Some(fstart_ffs::reader::ReaderError::CannotVerifyInPlace),
        "multi-segment with compression should return CannotVerifyInPlace"
    );
}

#[test]
fn test_lz4_incompressible_data() {
    let (signing_key, vk) = dev_keypair();

    // Pseudo-random data that won't compress well
    let mut data = vec![0u8; 1024];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = ((i * 17 + 131) % 256) as u8;
    }

    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: vec![
                InputFile {
                    name: "bootblock".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: stage_data_with_anchor(&[0xAA; 8]),
                        load_addr: 0x0,
                        compression: Compression::None,
                        flags: SegmentFlags::CODE,
                    }],
                },
                InputFile {
                    name: "random-blob".to_string(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: data.clone(),
                        load_addr: 0x8010_0000,
                        compression: Compression::Lz4,
                        flags: SegmentFlags::CODE,
                    }],
                },
            ],
        }],
    };

    // Should still build — LZ4 handles incompressible data (may expand slightly)
    let ffs = build_image(&config, &make_signer(&signing_key)).expect("build should succeed");
    let reader = FfsReader::new(&ffs.image);
    let anchor = reader
        .read_anchor(ffs.anchor_offset)
        .expect("should read anchor");
    let manifest = reader.read_manifest(&anchor).expect("should read manifest");

    let region = FfsReader::find_region(&manifest, "ro").expect("find ro");
    let entry = FfsReader::find_entry(region, "random-blob").expect("find random-blob");

    let segments = match &entry.content {
        EntryContent::File { segments, .. } => segments,
        _ => panic!("expected File"),
    };
    let seg = &segments[0];

    assert_eq!(seg.compression, Compression::Lz4);
    // For incompressible data, stored_size may be >= loaded_size
    // but in-place decompression should still work
    assert!(seg.in_place_size >= seg.loaded_size);

    // Verify round-trip through in-place decompression
    let compressed = reader
        .read_segment_data(seg, region, entry)
        .expect("read compressed");
    let buf_size = seg.in_place_size as usize;
    let mut buf = vec![0u8; buf_size];
    let src_offset = buf_size - compressed.len();
    buf[src_offset..].copy_from_slice(compressed);

    let n = unsafe {
        let src = core::slice::from_raw_parts(buf.as_ptr().add(src_offset), compressed.len());
        let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), seg.loaded_size as usize);
        fstart_ffs::lz4::decompress_block(src, dst).expect("in-place decompress")
    };

    assert_eq!(n, data.len());
    assert_eq!(&buf[..n], &data);
}
