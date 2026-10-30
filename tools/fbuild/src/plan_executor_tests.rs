use super::*;
use fstart_image_build::build_plan::ArtifactBinding;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn arbitrary_named_units_share_selections_and_content_bound_artifacts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let scratch = Scratch(
        std::env::temp_dir().join(format!("fstart common executor {}", std::process::id())),
    );
    fs::create_dir_all(&scratch.0).unwrap();
    let mut plan = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap();
    plan.units[0].name = "launch".into();
    plan.stages = vec!["launch".into()];
    let mut producer = plan.units[0].clone();
    producer.name = "digest-maker".into();
    producer.target = "x86_64-unknown-none".into();
    producer.entry = "x86_64".into();
    // This synthetic producer owns its vocabulary; it is not a QEMU unit.
    producer.cfg_schema.entries = vec!["x86_64".into()];
    producer.features = vec!["different-feature".into()];
    producer.rustflags = vec!["-Crelocation-model=static".into()];
    if let UnitOutput::Executable { expectations, .. } = &mut producer.output {
        expectations.architecture = fstart_image_build::elf::Architecture::X86_64;
    }
    plan.units[0].bindings = vec![ArtifactBinding {
        producer: producer.name.clone(),
        artifact: "flat".into(),
        environment: "LINKED_PAYLOAD".into(),
    }];
    // Intentionally consumer-first. Neither names nor output formats imply order.
    plan.units.push(producer);
    let resolved = Resolved { plan };
    assert_eq!(
        resolved
            .plan
            .selection("launch")
            .unwrap()
            .iter()
            .map(|u| u.name.as_str())
            .collect::<Vec<_>>(),
        ["digest-maker", "launch"]
    );
    assert_eq!(resolved.plan.selection("digest-maker").unwrap().len(), 1);
    assert!(resolved.selected(Some("ramstage")).is_err());
    let launch = resolved.selected(Some("launch")).unwrap();
    assert!(
        resolved
            .prepare(&scratch.0, &board, launch, &BTreeMap::new(), true)
            .is_err()
    );
    let artifact = scratch.0.join("current.bin");
    fs::write(&artifact, b"first generation").unwrap();
    let artifacts = BTreeMap::from([(
        "digest-maker".into(),
        BTreeMap::from([("flat".into(), artifact.clone())]),
    )]);
    let first = resolved
        .prepare(&scratch.0, &board, launch, &artifacts, true)
        .unwrap();
    assert!(
        first
            .cfgs
            .contains(&"fstart_entry=\"aarch64-relocate\"".into())
    );
    assert_eq!(
        first.environment()["LINKED_PAYLOAD"],
        artifact.display().to_string()
    );
    let producer = resolved.selected(Some("digest-maker")).unwrap();
    let producer_selection = resolved
        .prepare(&scratch.0, &board, producer, &BTreeMap::new(), true)
        .unwrap();
    assert!(
        producer_selection
            .cfgs
            .contains(&"fstart_entry=\"x86_64\"".into())
    );
    assert!(
        !producer_selection
            .environment()
            .contains_key("LINKED_PAYLOAD")
    );
    assert_ne!(producer_selection.args, first.args);
    // IDE receives this Selection too; check/build only change the Cargo verb.
    for build in [false, true] {
        let command = first.command(build);
        let args = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[2..], &first.args);
        assert_eq!(&first.check_command()[3..], &first.args);
    }
    fs::write(&artifact, b"second generation").unwrap();
    let second = resolved
        .prepare(&scratch.0, &board, launch, &artifacts, true)
        .unwrap();
    assert_ne!(
        first.directory, second.directory,
        "producer bytes must invalidate the consuming compiler directory"
    );
    let mut changed = resolved.clone();
    changed.plan.units[1].features.push("new-input".into());
    assert_ne!(
        resolved
            .artifact_dir(&scratch.0, &board.board, true)
            .unwrap(),
        changed
            .artifact_dir(&scratch.0, &board.board, true)
            .unwrap()
    );
}

#[test]
fn compiler_files_are_snapshotted_content_bound_and_namespace_isolated() {
    use fstart_image_build::build_plan::FileBinding;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart compiler files {}", std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let source = scratch.0.join("source.bin");
    fs::write(&source, b"first").unwrap();
    let mut plan = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut other = plan.units[0].clone();
    other.name = "other".into();
    plan.units[0].file_bindings.push(FileBinding {
        environment: "FSTART_TEST_INPUT".into(),
        path: source.display().to_string(),
        capacity: 8,
    });
    plan.units.push(other);
    plan.order().unwrap();
    let resolved = Resolved { plan };
    let unit = &resolved.plan.units[0];
    let first = resolved
        .prepare(&scratch.0, &board, unit, &BTreeMap::new(), true)
        .unwrap();
    let snapshot = PathBuf::from(&first.environment()["FSTART_TEST_INPUT"]);
    assert_ne!(snapshot, source);
    assert_eq!(fs::read(&snapshot).unwrap(), b"first");
    assert!(fs::metadata(&snapshot).unwrap().permissions().readonly());
    let records: serde_json::Value =
        serde_json::from_slice(&fs::read(first.directory.join("compiler-inputs.json")).unwrap())
            .unwrap();
    assert_eq!(records[0]["bytes"], 5);
    assert_eq!(
        records[0]["sha256"],
        format!("{:x}", Sha256::digest(b"first"))
    );
    let cached = resolved
        .prepare(&scratch.0, &board, unit, &BTreeMap::new(), true)
        .unwrap();
    assert_eq!(first.directory, cached.directory);
    fs::write(&source, b"second").unwrap();
    let second = resolved
        .prepare(&scratch.0, &board, unit, &BTreeMap::new(), true)
        .unwrap();
    assert_ne!(first.directory, second.directory);
    assert_eq!(fs::read(&snapshot).unwrap(), b"first");
    assert_eq!(
        fs::read(&second.environment()["FSTART_TEST_INPUT"]).unwrap(),
        b"second"
    );
    let isolated = resolved
        .prepare(
            &scratch.0,
            &board,
            &resolved.plan.units[1],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
    assert!(!isolated.environment().contains_key("FSTART_TEST_INPUT"));
    assert!(isolated.check_command().starts_with(&[
        "env".into(),
        "-u".into(),
        "FSTART_TEST_INPUT".into()
    ]));
    let current = PathBuf::from(&second.environment()["FSTART_TEST_INPUT"]);
    // Deliberate local-cache corruption must not be silently overwritten/reused.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&current, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(&current, b"corrupt").unwrap();
    let mut permissions = fs::metadata(&current).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&current, permissions).unwrap();
    assert!(
        resolved
            .prepare(&scratch.0, &board, unit, &BTreeMap::new(), true)
            .is_err()
    );
    assert_eq!(fs::read(&current).unwrap(), b"corrupt");
    fs::write(&source, b"too-large-for-bound").unwrap();
    assert!(
        resolved
            .prepare(&scratch.0, &board, unit, &BTreeMap::new(), true)
            .is_err()
    );
}

#[test]
fn unsupported_required_input_is_rejected_by_input_validation_itself() {
    let mut plan = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap();
    plan.inputs.push(fstart_image_build::build_plan::InputFile {
        name: "calibration".into(),
        default: Some("must-not-be-ignored.bin".into()),
        capacity: 4096,
    });
    let resolved = Resolved { plan };
    assert!(
        resolved
            .validate_inputs(Path::new("."), None, None, None)
            .unwrap_err()
            .contains("unknown or duplicate payload input")
    );
}
