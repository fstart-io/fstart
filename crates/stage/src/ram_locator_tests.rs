//! Run with RUSTFLAGS='--cfg fstart_stage_env="ram"' and
//! --features ffs,fstart-ffs/std --lib ram_locator_tests.
//! The real publish-once statics start fresh in this isolated test process.
#![cfg(all(fstart_stage_env = "ram", feature = "ffs"))]

use crate::{
    anchor, directory,
    fixed_helpers::MemoryMappedFfs,
    heap::{boxed::Box, vec},
};
use fstart_core::{
    ffs::{
        Compression, DigestSet, EntryContent, FileType, ImageManifest, Region, RegionContent,
        RegionEntry, Segment, SegmentFlags, SegmentKind, locator::LocatorBlock,
    },
    hstr, hvec,
    services::{ServiceError, boot_media::MemoryMapped, ffs_context},
};
use fstart_crypto::digest::hash_sha256;
use fstart_ffs::root::DirectoryRef;

#[test]
fn ram_import_is_required_before_mount_and_asset_consumers_work_after_it() {
    let microcode = [0xa5; 16];
    let vbt = [0x39; 24];
    let digest = hash_sha256(&vbt);
    let manifest = ImageManifest {
        regions: hvec([Region {
            name: hstr("ro"),
            offset: 0,
            size: 128,
            content: RegionContent::Container {
                children: hvec([RegionEntry {
                    name: hstr("vbt"),
                    offset: 64,
                    size: 24,
                    content: EntryContent::File {
                        file_type: FileType::Data,
                        digests: DigestSet {
                            sha256: Some(digest),
                            sha3_256: None,
                        },
                        segments: hvec([Segment {
                            name: hstr(".data"),
                            kind: SegmentKind::ReadOnlyData,
                            offset: 0,
                            stored_size: 24,
                            initialized_size: 24,
                            loaded_size: 24,
                            stored_digest: digest,
                            loaded_digest: digest,
                            in_place_size: 0,
                            load_addr: 0,
                            compression: Compression::None,
                            flags: SegmentFlags::RODATA,
                        }]),
                    },
                }]),
            },
        }]),
    };
    // Supply a known directory as predecessor-authenticated input, without
    // inventing a signature or exercising root authentication in this test.
    let encoded = fstart_ffs::manifest::encode_manifest(&manifest).unwrap();
    let reference = DirectoryRef {
        offset: 256,
        size: encoded.len() as u64,
        digest: hash_sha256(&encoded),
    };
    let mut bytes = vec![0; 1536];
    bytes[8..24].copy_from_slice(&microcode);
    bytes[64..88].copy_from_slice(&vbt);
    bytes[256..256 + encoded.len()].copy_from_slice(&encoded);
    let image: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let locator = LocatorBlock {
        total_image_size: image.len() as u32,
        manifest_offset: 1024,
        manifest_size: 512,
        microcode_offset: 8,
        microcode_size: 16,
        ..LocatorBlock::placeholder()
    };
    let media = MemoryMappedFfs::new(image.as_ptr() as u64, image.len());
    assert!(anchor::fstart_anchor_bytes().is_empty());
    assert!(matches!(media.mount(), Err(ServiceError::NotInitialized)));
    assert!(
        ffs_context::memory_mapped().is_none(),
        "failed mount must not publish"
    );

    // SAFETY: isolated single-threaded predecessor import, known immutable
    // image/directory, exactly as the Intel metadata phases require.
    unsafe {
        anchor::install_locator(locator, image.len() as u64).unwrap();
        let source = MemoryMapped::from_raw_addr(image.as_ptr() as u64, image.len());
        directory::install_directory_context(&source, reference).unwrap();
    }
    assert!(
        ffs_context::memory_mapped().is_none(),
        "import does not mount implicitly"
    );
    media.mount().unwrap();
    let context = ffs_context::memory_mapped().expect("mounted inherited context");
    // SAFETY: recorded image and inherited locator are immutable statics.
    let inherited = unsafe { context.anchor_bytes() };
    let reference = unsafe { fstart_core::ffs::AnchorRef::read_volatile(inherited) }.unwrap();
    let reader = fstart_ffs::FfsReader::new(unsafe { context.image_bytes() });
    assert_eq!(
        reader.intel_microcode(reference),
        Some(microcode.as_slice())
    );
    assert_eq!(
        ffs_context::read_verified_asset("vbt"),
        Some(vbt.as_slice())
    );
}
