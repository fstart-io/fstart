#![cfg(feature = "std")]

use ed25519_dalek::{Signer, SigningKey};
use fstart_core::ffs::{
    ANCHOR_SIZE, AnchorBlock, Compression, FileType, SegmentFlags, SegmentKind, Signature,
    VerificationKey,
    locator::{LOCATOR_SIZE, LocatorBlock},
    trust::{TRUST_SIZE, TrustBlock, TrustRef},
};
use fstart_ffs::{builder::*, root::*};

fn config(key_count: u8) -> (FfsImageConfig, SigningKey) {
    let signer = SigningKey::from_bytes(&[0x31; 32]);
    let keys = (0..key_count)
        .map(|id| {
            let key = SigningKey::from_bytes(&[0x31 + id; 32]);
            VerificationKey::ed25519(id, key.verifying_key().to_bytes())
        })
        .collect();
    let mut initial = vec![0; ANCHOR_SIZE];
    AnchorBlock::placeholder().write_to(&mut initial);
    let mut main = vec![0x37; 8192];
    TrustBlock::placeholder().write_to(&mut main[64..64 + TRUST_SIZE]);
    let segment = |data, compression, load_addr| InputSegment {
        name: ".flat".into(),
        kind: SegmentKind::Code,
        data,
        mem_size: None,
        load_addr,
        compression,
        flags: SegmentFlags::CODE,
    };
    (
        FfsImageConfig {
            keys,
            regions: vec![InputRegion::Container {
                name: "ro".into(),
                files: vec![
                    InputFile {
                        name: "initial".into(),
                        file_type: FileType::StageCode,
                        segments: vec![segment(initial, Compression::None, 0)],
                    },
                    InputFile {
                        name: "main".into(),
                        file_type: FileType::StageCode,
                        segments: vec![segment(main, Compression::Lz4, 0x80000000)],
                    },
                ],
            }],
        },
        signer,
    )
}

#[test]
fn compression_uses_final_constant_policy_and_deterministic_input_identity() {
    let mut sizes = Vec::new();
    let mut identities = Vec::new();
    for count in [1, 4] {
        let (config, signer) = config(count);
        let root_config = BootRootConfig {
            image_family: [count; 16],
            security_version: 19,
            bootstrap: vec![("main".into(), BootstrapRole::Mainstage)],
        };
        let sign = |b: &[u8]| Ok(Signature::ed25519(0, signer.sign(b).to_bytes()));
        let built = build_image_with_root(&config, &root_config, &sign).unwrap();
        let repeat = build_image_with_root(&config, &root_config, &sign).unwrap();
        assert_eq!(built.image, repeat.image);
        let locator = fstart_ffs::FfsReader::new(&built.image)
            .read_anchor(built.anchor_offset)
            .unwrap();
        let root = authenticate_root(
            &RootPolicy {
                image_family: root_config.image_family,
                minimum_security_version: 0,
                image_size: built.image.len() as u64,
                max_directory_size: 65536,
                keys: &config.keys,
            },
            &built.image
                [locator.manifest_offset as usize..locator.manifest_offset as usize + ROOT_SIZE],
        )
        .unwrap();
        let descriptor = root.descriptors()[0].unwrap();
        sizes.push(descriptor.stored_size);
        let stored = &built.image
            [descriptor.offset as usize..(descriptor.offset + descriptor.stored_size) as usize];
        verify_digest(stored, &descriptor.stored_digest).unwrap();
        #[repr(align(8))]
        struct Aligned([u8; 8192]);
        let mut loaded = Aligned([0; 8192]);
        assert_eq!(
            fstart_ffs::lz4::decompress_block(stored, &mut loaded.0).unwrap(),
            8192
        );
        verify_digest(&loaded.0, &descriptor.loaded_digest).unwrap();
        // SAFETY: owned aligned immutable decoded bytes, used here as test data,
        // not as a source of runtime trust authority.
        let trust = unsafe { TrustRef::read_volatile(&loaded.0[64..64 + TRUST_SIZE]) }.unwrap();
        assert_eq!(trust.valid_keys().len(), count as usize);
        assert_eq!(trust.image_family(), [count; 16]);
        assert_eq!(trust.minimum_security_version(), 0); // NOT signed version 19
        let identity = built
            .patched_inputs
            .iter()
            .find(|p| p.file == "main")
            .unwrap();
        assert_eq!(identity.sha256, descriptor.loaded_digest);
        identities.push(identity.trust_sha256);
        let directory = root.directory();
        directory
            .verify_bytes(
                &built.image
                    [directory.offset as usize..(directory.offset + directory.size) as usize],
            )
            .unwrap();
        let mut finalized_input = config.clone();
        let InputRegion::Container { files, .. } = &mut finalized_input.regions[0] else {
            unreachable!()
        };
        files[1].segments[0].data = loaded.0.to_vec();
        assert_eq!(
            build_image_with_root(&finalized_input, &root_config, &sign)
                .unwrap()
                .image,
            built.image
        );
    }
    assert_ne!(sizes[0], sizes[1], "fixture must change compressed extent");
    assert_ne!(identities[0], identities[1]);
}

#[test]
fn host_policy_inspection_accepts_unaligned_images_and_checks_the_wire_header() {
    #[repr(align(8))]
    struct Storage([u8; TRUST_SIZE + 16]);
    let mut storage = Storage([0; TRUST_SIZE + 16]);
    let mut policy = TrustBlock::placeholder();
    policy.key_count = 1;
    policy.keys[0] = VerificationKey::ed25519(7, [0x42; 32]);
    policy.image_family = [0x39; 16];
    policy.minimum_security_version = 0x0102_0304_0506_0708;
    policy.write_to(&mut storage.0[9..9 + TRUST_SIZE]);
    let image = &storage.0[1..];
    assert_ne!(image.as_ptr().align_offset(8), 0);
    let parsed = fstart_ffs::FfsReader::new(image).read_trust().unwrap();
    assert_eq!(parsed.image_family, policy.image_family);
    assert_eq!(
        parsed.minimum_security_version,
        policy.minimum_security_version
    );
    assert_eq!(parsed.valid_keys()[0].key_lo, policy.keys[0].key_lo);
    let mut encoded = [0; TRUST_SIZE];
    parsed.write_to(&mut encoded);
    assert_eq!(&encoded, &storage.0[9..9 + TRUST_SIZE]);
    assert!(TrustBlock::parse(&encoded[..TRUST_SIZE - 1]).is_none());
    for field in [8, 12, 24, 28] {
        let mut bad = encoded;
        bad[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(TrustBlock::parse(&bad).is_none());
    }
}

#[test]
fn stale_prepatched_policy_cannot_survive_a_new_packaging_selection() {
    let (mut config, signer) = config(1);
    let mut stale = TrustBlock::placeholder();
    stale.key_count = 1;
    stale.keys[0] = config.keys[0];
    stale.minimum_security_version = 99;
    let InputRegion::Container { files, .. } = &mut config.regions[0] else {
        unreachable!()
    };
    stale.write_to(&mut files[1].segments[0].data[64..64 + TRUST_SIZE]);
    let sign = |b: &[u8]| Ok(Signature::ed25519(0, signer.sign(b).to_bytes()));
    let error = build_image(&config, &sign).err().unwrap();
    assert!(error.contains("stale or malformed constant policy"));
}

#[test]
fn compressed_locator_is_a_packaging_error_not_an_iteration_fallback() {
    let (mut config, signer) = config(1);
    let InputRegion::Container { files, .. } = &mut config.regions[0] else {
        unreachable!()
    };
    let mut bytes = [0; LOCATOR_SIZE];
    LocatorBlock::placeholder().write_to(&mut bytes);
    files[1].segments[0].data[512..512 + LOCATOR_SIZE].copy_from_slice(&bytes);
    let sign = |b: &[u8]| Ok(Signature::ed25519(0, signer.sign(b).to_bytes()));
    let error = build_image(&config, &sign).err().unwrap();
    assert!(error.contains("compressed file 'main' contains a mutable locator"));
}
