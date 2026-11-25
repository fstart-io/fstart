//! Format/signing fixtures only: not CPU-authentic updates or a board profile.
use super::*;
use crate::intel_fit::{self, Plan, Resolved};
use ed25519_dalek::{Signer, SigningKey};
use fstart_core::ffs::{
    EntryContent, LocatorBlock, RegionContent,
    trust::{TRUST_SIZE, TrustBlock},
};
use fstart_ffs::builder::{ExternalInputFile, FfsImage};
use std::{cell::Cell, fs};
const SIZE: usize = 0x100000;
const BASE: u64 = 0xfff00000;
const PREFIX: usize = 0x20008;
fn put(data: &mut [u8], offset: usize, word: u32) {
    data[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
}
fn sum(data: &[u8]) -> u32 {
    data.as_chunks::<4>()
        .0
        .iter()
        .fold(0u32, |s, b| s.wrapping_add(u32::from_le_bytes(*b)))
}
fn record(legacy: bool, extended: bool) -> Vec<u8> {
    let mut bytes = vec![0; if legacy { 2048 } else { 1024 }];
    put(&mut bytes, 0, 1);
    put(&mut bytes, 4, 0x80000001);
    put(&mut bytes, 12, 0x506e3);
    put(&mut bytes, 20, 1);
    put(&mut bytes, 24, 2);
    let base = if legacy {
        2048
    } else if extended {
        992
    } else {
        1024
    };
    if !legacy {
        put(&mut bytes, 28, base as u32 - 48);
        put(&mut bytes, 32, 1024);
    }
    let check = 0u32.wrapping_sub(sum(&bytes[..base]));
    put(&mut bytes, 16, check);
    if extended {
        put(&mut bytes, base, 1);
        put(&mut bytes, base + 20, 0x906e9);
        put(&mut bytes, base + 24, 8);
        put(
            &mut bytes,
            base + 28,
            0x506e3u32
                .wrapping_add(2)
                .wrapping_add(check)
                .wrapping_sub(0x906e9 + 8),
        );
        let check = 0u32.wrapping_sub(sum(&bytes[base..]));
        put(&mut bytes, base + 4, check);
    }
    bytes
}
fn segment(data: Vec<u8>, code: bool, address: u64) -> InputSegment {
    InputSegment {
        name: ".flat".into(),
        kind: if code {
            SegmentKind::Code
        } else {
            SegmentKind::ReadOnlyData
        },
        data,
        mem_size: None,
        load_addr: address,
        compression: Compression::None,
        flags: if code {
            SegmentFlags::CODE
        } else {
            SegmentFlags::RODATA
        },
    }
}
struct Fixture {
    config: FfsImageConfig,
    plan: Plan,
    signer: SigningKey,
}
impl Fixture {
    fn new() -> Self {
        let signer = SigningKey::from_bytes(&[0x42; 32]);
        let mut code = record(false, false);
        code.extend(record(false, true));
        let mut files = vec![InputFile {
            name: intel_fit::MICROCODE.into(),
            file_type: FileType::CpuMicrocode,
            segments: vec![segment(code, false, 0)],
        }];
        let plan = Plan::prepare(&mut files).unwrap();
        let mut initial = vec![0xff; 4096];
        LocatorBlock::placeholder().write_to(&mut initial[0x100..]);
        TrustBlock::placeholder().write_to(&mut initial[0x200..0x200 + TRUST_SIZE]);
        let config = FfsImageConfig {
            keys: vec![VerificationKey::ed25519(
                0,
                signer.verifying_key().to_bytes(),
            )],
            regions: vec![
                InputRegion::Raw {
                    name: "mutable-prefix".into(),
                    size: PREFIX as u32,
                    fill: 0xff,
                },
                InputRegion::ContainerWithExternal {
                    name: "ro".into(),
                    files,
                    external_files: vec![ExternalInputFile {
                        name: "bootblock".into(),
                        file_type: FileType::StageCode,
                        offset: (SIZE - PREFIX - initial.len()) as u32,
                        segments: vec![segment(initial, true, 0xfffff000)],
                    }],
                    size: Some((SIZE - PREFIX) as u32),
                },
            ],
        };
        Self {
            config,
            plan,
            signer,
        }
    }
    fn build(&self, calls: &Cell<u32>) -> Result<(FfsImage, Resolved), String> {
        let mut resolved = None;
        let built = build_image_with_root_and_fixups(
            &self.config,
            &BootRootConfig {
                image_family: [0x42; 16],
                ..Default::default()
            },
            &[FileAlignment {
                file: intel_fit::NAME.into(),
                alignment: 16,
            }],
            |files| {
                let (patches, fit) = self.plan.resolve(
                    files,
                    fstart_core::services::FirmwareImage::single_window(BASE, SIZE as u64),
                    "bootblock",
                )?;
                resolved = Some(fit);
                Ok(patches)
            },
            &|message| {
                calls.set(calls.get() + 1);
                Ok(Signature::ed25519(0, self.signer.sign(message).to_bytes()))
            },
        )?;
        Ok((built, resolved.unwrap()))
    }
    fn bios(&self, built: &FfsImage) -> Vec<u8> {
        let mut bios = vec![0xff; SIZE];
        bios[..built.image.len()].copy_from_slice(&built.image);
        for file in &built.external_files {
            bios[file.offset as usize..file.offset as usize + file.data.len()]
                .copy_from_slice(&file.data);
        }
        bios
    }
}
#[test]
fn signed_fit_uses_exact_ffs_owned_records_and_sealed_reset_copy() {
    let fixture = Fixture::new();
    let calls = Cell::new(0);
    let (built, resolved) = fixture.build(&calls).unwrap();
    assert_eq!(calls.get(), 1);
    let bios = fixture.bios(&built);
    resolved.verify(&bios).unwrap();
    let report = intel_fit::inspect(&bios, BASE).unwrap();
    let reader = fstart_ffs::FfsReader::new(&bios);
    let anchor = reader.read_anchor(built.anchor_offset).unwrap();
    let manifest = reader.read_manifest(&anchor).unwrap();
    let code = built
        .file_data
        .iter()
        .find(|f| f.name == intel_fit::MICROCODE)
        .unwrap();
    assert_eq!(
        report.microcode,
        vec![
            BASE + u64::from(code.data_offset),
            BASE + u64::from(code.data_offset) + 1024
        ]
    );
    assert_eq!(anchor.microcode_offset.get(), code.data_offset);
    assert_eq!(anchor.microcode_size.get(), code.data_size);
    let reset_anchor = LocatorBlock::parse(
        &built.external_files[0].data[0x100..0x100 + fstart_core::ffs::LOCATOR_SIZE],
    )
    .unwrap();
    assert_eq!(reset_anchor.microcode_offset, anchor.microcode_offset);
    assert_eq!(reset_anchor.anchor_offset.get(), SIZE as u32 - 4096 + 0x100);
    // The reset image's locator lies past the inline FFS it describes.
    assert_eq!(
        reset_anchor.total_image_size.get(),
        anchor.total_image_size.get()
    );
    assert!(reset_anchor.anchor_offset.get() >= reset_anchor.total_image_size.get());
    assert!(
        fstart_ffs::FfsReader::new(&built.image)
            .scan_for_anchor()
            .is_ok()
    );
    let mut count = 0;
    for region in &manifest.regions {
        if let RegionContent::Container { children } = &region.content {
            for entry in children {
                if let EntryContent::File { file_type, .. } = entry.content
                    && file_type == FileType::CpuMicrocode
                {
                    count += 1;
                }
                reader.verify_entry_digests(entry, region).unwrap();
            }
        }
    }
    assert_eq!(count, 1); // No second microcode payload store.
    let InputRegion::ContainerWithExternal { external_files, .. } = &fixture.config.regions[1]
    else {
        unreachable!()
    };
    assert_eq!(
        &external_files[0].segments[0].data[0xfc0..0xfc8],
        &[0xff; 8]
    );

    // Actual production compositor must consume sealed bytes, not re-read the
    // cached unpatched .bin. No descriptor/ME/GbE or deployment keys are used.
    let dir = std::env::temp_dir().join(format!("fstart-fit-compose-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let elf = dir.join("reset.elf");
    let object = object::write::Object::new(
        object::BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    fs::write(&elf, object.write().unwrap()).unwrap();
    let config = BoardConfig {
        name: "fit-fixture".try_into().unwrap(),
        platform: fstart_core::Platform::X86_64,
        memory: fstart_core::MemoryMap {
            regions: heapless::Vec::new(),
            flash_layout: None,
            car: None,
        },
        stages: StageLayout::MultiStage(heapless::Vec::new()),
        security: fstart_core::SecurityConfig {
            signing_algorithm: fstart_core::SignatureAlgorithm::Ed25519,
            required_digests: heapless::Vec::new(),
        },
        payload: None,
        microcode: None,
        data_assets: heapless::Vec::new(),
        file_assets: heapless::Vec::new(),
        full_flash_image: true,
        build: fstart_core::BoardBuildPolicy {
            firmware_image: fstart_core::FirmwareImagePolicy::memory_mapped(BASE, SIZE as u64),
            ..Default::default()
        },
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    };
    let missing_bin = dir.join("must-not-read.bin");
    let path = dir.join("fixture.ffs");
    let files = crate::files::HostFiles::default();
    let input = FullFlashInput {
        files: &files,
        config: &config,
        bootblock_elf: &elf,
        bootblock_bin: &missing_bin,
        ffs_data: &built.image,
        trust_bytes: &built.trust_bytes,
        ffs_anchor_offset: built.anchor_offset,
        ffs_path: &path,
        finalized_initial: Some(&built.external_files[0]),
        fit: Some(&resolved),
    };
    let (out, _) = create_full_flash_image(input).unwrap();
    assert_eq!(fs::read(out).unwrap(), bios);
    // The same BIOS lives at a nonzero physical SPI offset in an IFD image.
    // FIT addresses must remain CPU addresses; no OEM region bytes are inputs.
    use fstart_core::memory::{IntelIfdRegion, IntelIfdRegionConfig};
    let descriptor = IntelIfdRegionConfig {
        kind: IntelIfdRegion::Descriptor,
        offset: 0,
        size: 4096,
    };
    let layout = IntelIfdFlashLayout::new(
        fstart_core::ConstVec::new(descriptor)
            .push(descriptor)
            .push(IntelIfdRegionConfig {
                kind: IntelIfdRegion::Bios,
                offset: SIZE as u32,
                size: SIZE as u32,
            }),
    );
    let mut ifd_config = config.clone();
    ifd_config.memory.flash_layout = Some(FlashLayout::IntelIfd(layout));
    let (out, placement) = create_full_flash_image(FullFlashInput {
        config: &ifd_config,
        ..input
    })
    .unwrap();
    // The BIOS region holds the same bytes as the BIOS-only image.
    assert_eq!(placement.ffs_offset, SIZE as u64);
    assert_eq!(&fs::read(&out).unwrap()[SIZE..], bios.as_slice());
    fs::write(&path, &built.image).unwrap();
    crate::inspect::inspect(path.to_str().unwrap()).unwrap();
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn fit_audit_rejects_valid_records_outside_the_owned_ffs_file() {
    let fixture = Fixture::new();
    let (built, resolved) = fixture.build(&Cell::new(0)).unwrap();
    let mut bios = fixture.bios(&built);
    let report = intel_fit::inspect(&bios, BASE).unwrap();
    let offset = (report.table_address - BASE) as usize;
    let alien = record(false, false);
    bios[0x90000..0x90400].copy_from_slice(&alien);
    bios[offset + 16..offset + 24].copy_from_slice(&(BASE + 0x90000).to_le_bytes());
    bios[offset + 15] = 0;
    bios[offset + 15] = 0u8.wrapping_sub(
        bios[offset..offset + 48]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
    );
    assert!(intel_fit::inspect(&bios, BASE).is_ok()); // Structurally valid is NOT ownership.
    assert!(resolved.verify(&bios).is_err());
    let mut bios = fixture.bios(&built);
    bios[SIZE - 64..SIZE - 56].fill(0xff);
    assert!(resolved.verify(&bios).is_err());
    let mut bios = fixture.bios(&built);
    bios[SIZE - 1] ^= 1; // Ordinary reset-code byte, not the FIT pointer.
    assert!(intel_fit::inspect(&bios, BASE).is_ok());
    assert!(resolved.verify(&bios).is_err());
}
#[test]
fn invalid_fit_sources_fail_before_any_signature() {
    let mut fixture = Fixture::new();
    let calls = Cell::new(0);
    let InputRegion::ContainerWithExternal { external_files, .. } = &mut fixture.config.regions[1]
    else {
        unreachable!()
    };
    external_files[0].segments[0].data[0xfc0] = 0;
    assert!(fixture.build(&calls).is_err());
    assert_eq!(calls.get(), 0);
    let mut fixture = Fixture::new();
    let InputRegion::ContainerWithExternal { files, .. } = &mut fixture.config.regions[1] else {
        unreachable!()
    };
    files[0].segments[0].data[1024 + 48] ^= 1;
    assert!(fixture.build(&calls).is_err());
    assert_eq!(calls.get(), 0);
    let mut files = vec![InputFile {
        name: intel_fit::MICROCODE.into(),
        file_type: FileType::CpuMicrocode,
        segments: vec![segment(record(false, false), false, 0)],
    }];
    files[0].segments[0].data.extend([1, 2, 3]);
    assert!(Plan::prepare(&mut files).is_err());
    assert_eq!(files.len(), 1);
}
#[test]
fn shared_record_parser_checks_legacy_defaults_and_each_extended_alias() {
    let mut bytes = record(true, false);
    bytes.extend(record(false, true));
    let records = fstart_core::intel_microcode::records(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records[1].offset, 2048);
    assert_eq!(records[0].revision(), 0x80000001);
    assert!(records[0].matches(0x506e3, 2));
    assert!(records[1].matches(0x906e9, 8));
    assert!(!records[1].matches(0x906e9, 2));
    let ext = 2048 + 992;
    bytes[ext + 28] ^= 1; // Corrupt alias checksum, then restore table checksum.
    put(&mut bytes, ext + 4, 0);
    let check = 0u32.wrapping_sub(sum(&bytes[ext..]));
    put(&mut bytes, ext + 4, check);
    assert_eq!(
        fstart_core::intel_microcode::records(&bytes)
            .nth(1)
            .unwrap()
            .unwrap_err(),
        fstart_core::intel_microcode::Error::Extended
    );
}
