#![cfg(feature = "std")]
use ed25519_dalek::{Signer, SigningKey};
use fstart_core::ffs::{
    ANCHOR_SIZE, AnchorBlock, Compression, FileType, SegmentFlags, SegmentKind, Signature,
    VerificationKey,
};
use fstart_ffs::{ManifestView, builder::*, root::*};

fn descriptor() -> BootstrapDescriptor {
    BootstrapDescriptor {
        role: BootstrapRole::Mainstage,
        compression: Compression::None,
        offset: 0x1020,
        stored_size: 4,
        loaded_size: 4,
        load_addr: 0x80000000,
        entry_offset: 0,
        scratch_size: 0,
        stored_digest: [0x31; 32],
        loaded_digest: [0x31; 32],
    }
}
#[test]
fn root_wire_golden_and_reserved_bytes() {
    let root = Root {
        security_version: 0x0102030405060708,
        image_family: [0x21; 16],
        key_id: 7,
        descriptors: [Some(descriptor()), None],
        directory: DirectoryRef {
            offset: 0x22334455,
            size: 28,
            digest: [0x41; 32],
        },
        signature: [0x51; 64],
    };
    let b = root.encode();
    assert_eq!(
        &b[..24],
        b"FSTARTBR\x01\x00\x40\x00\x00\x02\x00\x00\x08\x07\x06\x05\x04\x03\x02\x01"
    );
    assert_eq!(
        &b[64..80],
        b"\x02\x00\x00\x00\x00\x00\x00\x00\x20\x10\x00\x00\x00\x00\x00\x00"
    );
    assert_eq!(&b[224..384], &[0; 160]);
    assert_eq!(&b[384..392], &0x22334455u64.to_le_bytes());
    assert_eq!(
        &b[432..448],
        b"\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    );
    assert_eq!(Root::parse(&b).unwrap(), root);
    for index in [44, 48, 63, 67, 68, 184, 223, 224, 436, 447] {
        let mut bad = b;
        bad[index] ^= 0x80;
        assert!(Root::parse(&bad).is_err(), "byte {index}");
    }
    for len in 0..ROOT_SIZE {
        assert!(Root::parse(&b[..len]).is_err());
    }
    let mut bad = descriptor();
    bad.offset = u64::MAX;
    assert!(bad.validate(u64::MAX).is_err());
    bad = descriptor();
    bad.entry_offset = bad.loaded_size;
    assert!(bad.validate(u64::MAX).is_err());
}

fn fixture() -> (FfsImage, VerificationKey) {
    let key = SigningKey::from_bytes(&[0x19; 32]);
    let vk = VerificationKey::ed25519(0, key.verifying_key().to_bytes());
    let mut initial = vec![0; ANCHOR_SIZE + 160];
    AnchorBlock::placeholder().write_to(&mut initial);
    initial[ANCHOR_SIZE..ANCHOR_SIZE + 8].copy_from_slice(b"FSTPIN01");
    let segment = |data, compression, load_addr| InputSegment {
        name: ".flat".into(),
        kind: SegmentKind::Code,
        data,
        mem_size: None,
        load_addr,
        compression,
        flags: SegmentFlags::CODE,
    };
    let config = FfsImageConfig {
        keys: vec![vk],
        regions: vec![InputRegion::Container {
            name: "ro".into(),
            files: vec![
                InputFile {
                    name: "bootblock".into(),
                    file_type: FileType::StageCode,
                    segments: vec![segment(initial, Compression::None, 0)],
                },
                InputFile {
                    name: "mainstage".into(),
                    file_type: FileType::StageCode,
                    segments: vec![segment(vec![0x37; 4096], Compression::Lz4, 0x80000000)],
                },
            ],
        }],
    };
    let policy = BootRootConfig {
        image_family: [0x44; 16],
        security_version: 5,
        bootstrap: vec![("mainstage".into(), BootstrapRole::Mainstage)],
    };
    let signer = |b: &[u8]| Ok(Signature::ed25519(0, key.sign(b).to_bytes()));
    let mut built = build_image_with_root(&config, &policy, &signer).unwrap();
    finalize_initial_stage(
        &mut built,
        "bootblock",
        |initial, root| {
            initial[ANCHOR_SIZE..].copy_from_slice(&root.descriptors[0].unwrap().encode());
            Ok(())
        },
        &signer,
    )
    .unwrap();
    (built, vk)
}

#[test]
fn signed_root_directory_and_compressed_bytes_authenticate_exact_buffers() {
    let (built, key) = fixture();
    let reader = fstart_ffs::FfsReader::new(&built.image);
    let anchor = reader.read_anchor(built.anchor_offset).unwrap();
    assert_eq!(anchor.manifest_size, 512);
    let trust = reader.read_trust().unwrap();
    assert_eq!(trust.image_family, [0x44; 16]);
    let bytes =
        &built.image[anchor.manifest_offset as usize..anchor.manifest_offset as usize + 512];
    let keys = [key];
    let mut policy = RootPolicy {
        image_family: trust.image_family,
        minimum_security_version: 5,
        image_size: built.image.len() as u64,
        max_directory_size: 65536,
        keys: &keys,
    };
    let authenticated = authenticate_root(&policy, bytes).unwrap();
    policy.minimum_security_version = 6;
    assert!(authenticate_root(&policy, bytes).is_err());
    policy.minimum_security_version = 5;
    policy.image_family = [0; 16];
    assert!(authenticate_root(&policy, bytes).is_err());
    policy.image_family = trust.image_family;
    let mut corrupt = bytes.to_vec();
    corrupt[120] ^= 1;
    assert!(authenticate_root(&policy, &corrupt).is_err());
    let d = authenticated.descriptors()[0].unwrap();
    assert_eq!(d.compression, Compression::Lz4);
    assert!(d.scratch_size >= d.stored_size + d.loaded_size);
    let stored = &built.image[d.offset as usize..(d.offset + d.stored_size) as usize];
    verify_digest(stored, &d.stored_digest).unwrap();
    let mut loaded = vec![0; d.loaded_size as usize];
    assert_eq!(
        fstart_ffs::lz4::decompress_block(stored, &mut loaded).unwrap(),
        4096
    );
    verify_digest(&loaded, &d.loaded_digest).unwrap();
    loaded[0] ^= 1;
    assert!(verify_digest(&loaded, &d.loaded_digest).is_err());
    let reference = authenticated.directory();
    let directory =
        &built.image[reference.offset as usize..(reference.offset + reference.size) as usize];
    reference.verify_bytes(directory).unwrap();
    let view = ManifestView::parse(directory).unwrap();
    assert!(view.find_file_by_type(FileType::StageCode).is_err());
    let file = view.find_file_by_name("mainstage").unwrap();
    assert_eq!(file.segments()[0].stored_digest(), d.stored_digest);
    assert_eq!(file.segments()[0].loaded_digest(), d.loaded_digest);
    let owned = reader.read_manifest(&anchor).unwrap();
    let region = fstart_ffs::FfsReader::find_region(&owned, "ro").unwrap();
    let initial = fstart_ffs::FfsReader::find_entry(region, "bootblock").unwrap();
    reader.verify_entry_digests(initial, region).unwrap();
    let mut bad = directory.to_vec();
    bad.push(0);
    assert!(ManifestView::parse(&bad).is_err());
    assert!(reference.verify_bytes(&bad).is_err());
    // First entry starts at header28+region24; digest flags at byte23.
    let mut bad = directory.to_vec();
    bad[28 + 24 + 23] = 0;
    assert!(ManifestView::parse(&bad).is_err());
}

#[test]
fn directory_empty_golden_revision() {
    let golden = *b"FSMZ\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x1c\x00\x00\x00\x00\x00\x00\x00";
    assert_eq!(ManifestView::parse(&golden).unwrap().summary().regions, 0);
    let mut old = golden;
    old[4] = 1;
    assert!(ManifestView::parse(&old).is_err());
}

#[test]
fn lz4_rejects_trailing_input() {
    let mut output = [0; 3];
    assert_eq!(
        fstart_ffs::lz4::decompress_block(b"\x30abc", &mut output),
        Ok(3)
    );
    assert!(fstart_ffs::lz4::decompress_block(b"\x30abc\x00", &mut output).is_err());
}

#[test]
fn populated_directory_golden_round_trip() {
    let golden = include_bytes!("fixtures/directory-v2.bin");
    let view = ManifestView::parse(golden).unwrap();
    let file = view.find_file_by_name("file").unwrap();
    assert_eq!(file.file_type().unwrap(), FileType::StageCode);
    assert_eq!(file.segments()[0].initialized_size(), 3);
    assert_eq!(file.segments()[0].load_addr(), 0x80000000);
    verify_digest(b"abc", &file.segments()[0].stored_digest()).unwrap();
    let owned = view.to_owned_manifest().unwrap();
    assert_eq!(
        fstart_ffs::manifest::encode_manifest(&owned).unwrap(),
        golden
    );
}
