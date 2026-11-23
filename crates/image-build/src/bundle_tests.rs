use super::*;
use fstart_core::Platform;

fn fixture() -> Bundle {
    let files = MemoryFiles::default();
    files
        .write(Path::new("/units/stage/stage.bin"), &[0; 4096])
        .unwrap();
    Bundle {
        manifest: Manifest {
            format_version: VERSION,
            board: "test-board".into(),
            payload: "halt".into(),
            revision: "test".into(),
            board_dir: "/workspace/boards/test/board".into(),
            plan: BuildPlan {
                payload: "halt".into(),
                platform: Platform::X86_64,
                units: vec![],
                stages: vec!["stage".into()],
                images: vec![],
                inputs: vec![],
                run: None,
                options: Default::default(),
                update: None,
            },
            stages: vec![StageBinary {
                name: "stage".into(),
                path: "/units/stage/stage.elf".into(),
                run_path: "/units/stage/stage.bin".into(),
                load_addr: 0,
            }],
            inputs: vec![],
            packed_donor: None,
            bundled_payload: None,
            files: BTreeMap::new(),
        },
        files,
    }
}

fn rewrite(bytes: &[u8], name: &str, replacement: &[u8]) -> Vec<u8> {
    let mut source = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut target = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..source.len() {
        let mut file = source.by_index(index).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        let member = file.name().unwrap().into_owned();
        target
            .start_file(&member, zip::write::SimpleFileOptions::default())
            .unwrap();
        target
            .write_all(if member == name {
                replacement
            } else {
                &contents
            })
            .unwrap();
    }
    target.finish().unwrap().into_inner()
}

#[test]
fn compressed_round_trip_preserves_binary_identity() {
    let mut bundle = fixture();
    let encoded = bundle.encode().unwrap();
    assert!(encoded.len() < 4096);
    let decoded = Bundle::decode(&encoded).unwrap();
    assert_eq!(decoded.manifest.board, "test-board");
    assert_eq!(
        decoded
            .files
            .read(Path::new("/units/stage/stage.bin"))
            .unwrap(),
        [0; 4096]
    );
    assert!(
        decoded
            .assemble(&Supplied::default())
            .unwrap_err()
            .contains("binaries only")
    );
}

#[test]
fn rejects_changed_bytes_even_with_valid_zip_crc() {
    let encoded = fixture().encode().unwrap();
    let changed = rewrite(&encoded, "files/units/stage/stage.bin", &[1; 4096]);
    assert!(Bundle::decode(&changed).err().unwrap().contains("checksum"));
}

#[test]
fn rejects_unknown_formats_and_expansion_limits() {
    let mut bundle = fixture();
    let encoded = bundle.encode().unwrap();
    let mut manifest = serde_json::to_value(&bundle.manifest).unwrap();
    manifest["format_version"] = 99.into();
    let changed = rewrite(
        &encoded,
        "manifest.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );
    assert!(
        Bundle::decode(&changed)
            .err()
            .unwrap()
            .contains("unsupported bundle format")
    );
    manifest["format_version"] = VERSION.into();
    manifest["files"]["/units/stage/stage.bin"]["size"] = (MAX_FILE + 1).into();
    let changed = rewrite(
        &encoded,
        "manifest.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );
    assert!(Bundle::decode(&changed).is_err());
}

fn donor_zip(images: &[(&str, u8)]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, fill) in images {
        archive.start_file(*name, options).unwrap();
        archive.write_all(&vec![*fill; 1 << 20]).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

#[test]
fn rejects_ambiguous_donor_zips() {
    let bundle = fixture();
    let encoded = donor_zip(&[("a.rom", 0), ("b.bin", 1)]);
    assert!(bundle.donor(&encoded).unwrap_err().contains("different"));
    assert!(bundle.donor(&donor_zip(&[])).is_err());
}

#[test]
fn rejects_donor_zips_with_too_many_copies() {
    let bundle = fixture();
    let copies: Vec<(String, u8)> = (0..=MAX_DONOR_COPIES)
        .map(|i| (format!("copy{i}.rom"), 7))
        .collect();
    let names: Vec<(&str, u8)> = copies.iter().map(|(n, f)| (n.as_str(), *f)).collect();
    let error = bundle.donor(&donor_zip(&names)).unwrap_err();
    assert!(error.contains("too many"), "{error}");
}

#[test]
fn accepts_identical_donor_copies() {
    // Vendor updates ship one copy per flash tool (flashabl.rom, image.bin).
    let bundle = fixture();
    let encoded = donor_zip(&[("x/flashabl.rom", 7), ("x/SPI_UPD/image.bin", 7)]);
    assert_eq!(bundle.donor(&encoded).unwrap(), vec![7; 1 << 20]);
}

#[test]
fn a_bundled_payload_fills_its_input_slot() {
    let mut bundle = fixture();
    bundle.manifest.inputs = vec![InputFile {
        name: "kernel".into(),
        default: None,
        capacity: 1 << 20,
    }];
    let none = Supplied::default();
    let missing = bundle.assemble(&none).unwrap_err();
    assert!(missing.contains("missing input kernel"), "{missing}");
    bundle.manifest.bundled_payload = Some(BundledPayload {
        name: "SeaBIOS".into(),
        input: "kernel".into(),
        path: "/payloads/seabios/bios.bin.elf".into(),
        files: vec![],
    });
    // Past the input check; this fixture has no image contract.
    let unassembled = bundle.assemble(&none).unwrap_err();
    assert!(unassembled.contains("binaries only"), "{unassembled}");
}

fn update_bundle_fixture(with_packed_donor: bool) -> Bundle {
    use crate::build_plan::{
        BootImageFormat, CargoTarget, CompilationUnit, FfsAssembly, FirmwareUpdateContract,
        UnitOutput,
    };
    use crate::elf::{Architecture, Descriptor, Expectations};
    use crate::plan::Span;
    use fstart_core::{
        BoardBuildPolicy, Compression, FirmwareImagePolicy, MemoryRegion, RegionKind, RunsFrom,
        StageBuildConfig, StageConfig, StageLayout, default_security_config, hstr, hvec,
    };

    let files = MemoryFiles::default();
    let stage_bytes = vec![0x90u8; 4096];
    files
        .write(Path::new("/units/ramstage/ramstage.bin"), &stage_bytes)
        .unwrap();
    files
        .write(Path::new("/units/ramstage/ramstage.elf"), &stage_bytes)
        .unwrap();

    let span = Span {
        base: 0x0020_0000,
        size: 0x0010_0000,
    };
    let unit = CompilationUnit {
        name: "ramstage".into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        features: vec![],
        build_std: Some("core,alloc".into()),
        rustflags: vec![],
        release_only: true,
        linker_script: Some("ENTRY(_start)".into()),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations: Expectations {
                architecture: Architecture::X86_64,
                elf64: true,
                little_endian: true,
                stored: vec![span],
                runtime: vec![span],
                identity_mapping: true,
                entry: Some(span.base),
                copy: None,
                descriptor: Some(Descriptor {
                    section: ".fstart.layout".into(),
                    start_symbol: "_fstart_layout_start".into(),
                    end_symbol: "_fstart_layout_end".into(),
                    bytes: vec![],
                    reservation: span,
                }),
                stored_flags: None,
                symbols: BTreeMap::new(),
            },
            load_address: span.base,
            flat_capacity: span.size,
        },
    };
    let ffs = FfsAssembly {
        platform: Platform::X86_64,
        memory: vec![MemoryRegion {
            name: hstr("ram"),
            base: 0x0010_0000,
            size: 0x0100_0000,
            kind: RegionKind::Ram,
        }],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::MultiStage(hvec([StageConfig {
            name: hstr("ramstage"),
            build: StageBuildConfig::default(),
            load_addr: span.base,
            stack_size: 0x1000,
            heap_size: Some(0x1000),
            runs_from: RunsFrom::Ram,
            compression: Compression::None,
            data_addr: None,
        }])),
        security: default_security_config(),
        payload: None,
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        boot_image_format: BootImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(0xff80_0000, 0x0004_0000),
            ..Default::default()
        },
        fw_version: 1,
        security_version: 1,
        rw_slot: true,
        omit_stages: vec![],
        image_family: None,
    };
    Bundle {
        manifest: Manifest {
            format_version: VERSION,
            board: "demo-ab".into(),
            payload: "uefi".into(),
            revision: "rev-test".into(),
            board_dir: "/workspace/boards/demo/ab".into(),
            plan: BuildPlan {
                payload: "uefi".into(),
                platform: Platform::X86_64,
                units: vec![unit],
                stages: vec!["ramstage".into()],
                images: vec![ffs.into()],
                inputs: vec![],
                run: None,
                options: Default::default(),
                update: Some(FirmwareUpdateContract {
                    firmware_guid: [
                        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
                        0xdd, 0xee, 0xff, 0x01,
                    ],
                    slot_size: 0x0004_0000,
                    ab: true,
                    lowest_supported_version: 1,
                    rmap_region: "RW_SLOT".into(),
                    capsule_trust_provisioned: true,
                }),
            },
            stages: vec![StageBinary {
                name: "ramstage".into(),
                path: "/units/ramstage/ramstage.elf".into(),
                run_path: "/units/ramstage/ramstage.bin".into(),
                load_addr: span.base,
            }],
            inputs: vec![],
            packed_donor: with_packed_donor.then_some(PackedDonor { tables: vec![] }),
            bundled_payload: None,
            files: BTreeMap::new(),
        },
        files,
    }
}

fn test_ed25519_key_der() -> (Vec<u8>, String) {
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
    let pkcs8 = key.to_pkcs8_der().unwrap().as_bytes().to_vec();
    let pub_hex = hex_encode(key.verifying_key().as_bytes());
    (pkcs8, pub_hex)
}

#[test]
fn update_only_assembles_donor_free_rw_slot_and_signed_capsule() {
    let bundle = update_bundle_fixture(true);
    assert!(!bundle.manifest.update_requires_donor());

    // Full mode still demands the vendor donor image.
    let full_err = bundle.assemble(&Supplied::default()).unwrap_err();
    assert!(full_err.contains("vendor BIOS"), "{full_err}");

    // UpdateOnly requires an explicit Ed25519 key (no silent throwaway key).
    let no_key_err = bundle
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            ..Default::default()
        })
        .unwrap_err();
    assert!(no_key_err.contains("--signing-key"), "{no_key_err}");

    // UpdateOnly skips the donor and produces padded RW slot + signed .cap.
    let (ed25519_der, ed25519_pub_hex) = test_ed25519_key_der();
    let (_, cert_der) = crate::pkcs7_sign::dev_capsule_key_and_cert().unwrap();
    let cert_pem =
        pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, &cert_der).unwrap();
    let outputs = bundle
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            signing_key: Some(&ed25519_der),
            fw_version: Some(7),
            security_version: Some(3),
            capsule_signing: Some(CapsuleSigning {
                rsa_key: crate::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM.as_bytes(),
                cert: cert_pem.as_bytes(),
                monotonic_count: 42,
                lowest_supported_version: Some(2),
            }),
            ..Default::default()
        })
        .unwrap();

    let rw_slot = outputs.get("demo-ab-rw-slot.bin").expect("rw-slot.bin");
    assert_eq!(rw_slot.len(), 0x0004_0000);
    let root = fstart_ffs::root::Root::parse(&rw_slot[..fstart_ffs::root::ROOT_SIZE]).unwrap();
    assert_eq!(root.fw_version, 7);
    assert_eq!(root.security_version, 3);
    assert_eq!(
        *rw_slot.last().unwrap(),
        fstart_ffs::slot_state::ERASED_BYTE
    );

    let cap = outputs.get("demo-ab.cap").expect("signed .cap");
    assert_eq!(&cap[..16], &crate::capsule::EFI_FMP_CAPSULE_GUID);

    let report: serde_json::Value =
        serde_json::from_slice(outputs.get("assembly-report.json").unwrap()).unwrap();
    assert_eq!(report["assembly_mode"], "update-only");
    assert_eq!(report["update"]["fw_version"], 7);
    assert_eq!(report["update"]["security_version"], 3);
    assert_eq!(report["update"]["capsule_trust_provisioned"], true);
    assert_eq!(report["update"]["ephemeral_signing_key"], false);
    assert_eq!(report["update"]["ed25519_public_key"], ed25519_pub_hex);
    assert_eq!(report["update"]["capsule_signed"], true);
    assert_eq!(report["update"]["monotonic_count"], 42);
    assert_eq!(report["update"]["lowest_supported_version"], 2);
}

#[test]
fn full_mode_with_update_contract_emits_ffs_rw_slot_and_capsule() {
    let bundle = update_bundle_fixture(false);
    let (ed25519_der, ed25519_pub_hex) = test_ed25519_key_der();
    let (_, cert_der) = crate::pkcs7_sign::dev_capsule_key_and_cert().unwrap();

    // Capsule signing in Full mode also rejects a throwaway Ed25519 key.
    let no_key_err = bundle
        .assemble(&Supplied {
            capsule_signing: Some(CapsuleSigning {
                rsa_key: crate::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM.as_bytes(),
                cert: &cert_der,
                monotonic_count: 1,
                lowest_supported_version: None,
            }),
            ..Default::default()
        })
        .unwrap_err();
    assert!(no_key_err.contains("--signing-key"), "{no_key_err}");

    // Full mode with explicit Ed25519 key + capsule credentials emits all outputs.
    let outputs = bundle
        .assemble(&Supplied {
            signing_key: Some(&ed25519_der),
            fw_version: Some(3),
            capsule_signing: Some(CapsuleSigning {
                rsa_key: crate::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM.as_bytes(),
                cert: &cert_der,
                monotonic_count: 5,
                lowest_supported_version: Some(2),
            }),
            ..Default::default()
        })
        .unwrap();
    assert!(outputs.contains_key("demo-ab.ffs"));
    assert!(outputs.contains_key("demo-ab-rw-slot.bin"));
    assert!(outputs.contains_key("demo-ab.cap"));
    let report: serde_json::Value =
        serde_json::from_slice(outputs.get("assembly-report.json").unwrap()).unwrap();
    assert_eq!(report["assembly_mode"], "full");
    assert_eq!(report["update"]["ed25519_public_key"], ed25519_pub_hex);
    assert_eq!(report["update"]["ephemeral_signing_key"], false);
    assert_eq!(report["update"]["capsule_signed"], true);
}

#[test]
fn update_only_rejects_non_update_bundle_settings_and_bad_version_floors() {
    let plain = fixture();
    let (ed25519_der, _) = test_ed25519_key_der();
    let err = plain
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            signing_key: Some(&ed25519_der),
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("update-only"), "{err}");

    let ab = update_bundle_fixture(false);
    let err = ab
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            signing_key: Some(&ed25519_der),
            settings: crate::options::Settings {
                options: vec![("serial_baud".into(), "115200".into())],
                secure_boot: None,
            },
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("settings/NVRAM"), "{err}");

    let (_, cert_der) = crate::pkcs7_sign::dev_capsule_key_and_cert().unwrap();
    // lsv > fw_version is rejected.
    let err = ab
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            signing_key: Some(&ed25519_der),
            fw_version: Some(3),
            capsule_signing: Some(CapsuleSigning {
                rsa_key: crate::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM.as_bytes(),
                cert: &cert_der,
                monotonic_count: 1,
                lowest_supported_version: Some(4),
            }),
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("exceeds firmware version"), "{err}");

    // fw_version < contract.lowest_supported_version is rejected.
    let err = ab
        .assemble(&Supplied {
            mode: AssemblyMode::UpdateOnly,
            signing_key: Some(&ed25519_der),
            fw_version: Some(0),
            capsule_signing: Some(CapsuleSigning {
                rsa_key: crate::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM.as_bytes(),
                cert: &cert_der,
                monotonic_count: 1,
                lowest_supported_version: Some(0),
            }),
            ..Default::default()
        })
        .unwrap_err();
    assert!(
        err.contains("below the board's lowest_supported_version"),
        "{err}"
    );
}

#[test]
fn update_requires_donor_when_ffs_consumes_blobs_dir_asset() {
    let mut bundle = update_bundle_fixture(true);
    assert!(!bundle.manifest.update_requires_donor());
    if let crate::build_plan::ImageOperation::Ffs(ffs) =
        &mut bundle.manifest.plan.images[0].operation
    {
        ffs.file_assets.push(fstart_core::board::FileAsset {
            name: heapless::String::try_from("ucode.bin").unwrap(),
            path: heapless::String::try_from(format!("{BLOBS_DIR}/ucode.bin").as_str()).unwrap(),
        });
    }
    assert!(bundle.manifest.update_requires_donor());
}
