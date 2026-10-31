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
            assembly: Assembly::Unassembled {
                platform: Platform::X86_64,
            },
            stages: vec![StageBinary {
                name: "stage".into(),
                path: "/units/stage/stage.elf".into(),
                run_path: "/units/stage/stage.bin".into(),
                load_addr: 0,
            }],
            inputs: vec![],
            packed_donor: None,
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
        target
            .start_file(file.name(), zip::write::SimpleFileOptions::default())
            .unwrap();
        target
            .write_all(if file.name() == name {
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
            .assemble(None, &BTreeMap::new(), None)
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

#[test]
fn rejects_ambiguous_donor_zips() {
    let bundle = fixture();
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for name in ["a.rom", "b.bin"] {
        archive.start_file(name, options).unwrap();
        archive.write_all(&vec![0; 1 << 20]).unwrap();
    }
    let encoded = archive.finish().unwrap().into_inner();
    assert!(bundle.donor(&encoded).unwrap_err().contains("exactly one"));
}
