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
        },
    )
    .unwrap();
    plan.units[0].name = "launch".into();
    plan.stages = vec!["launch".into()];
    let mut producer = plan.units[0].clone();
    producer.name = "digest-maker".into();
    producer.target = "x86_64-unknown-none".into();
    producer.entry = "x86_64".into();
    producer.environment = "car".into();
    // This synthetic producer owns its vocabulary; it is not a QEMU unit.
    producer.cfg_schema.entries = vec!["x86_64".into()];
    producer.cfg_schema.environments = vec!["car".into()];
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
fn unsupported_required_input_is_rejected_by_input_validation_itself() {
    let mut plan = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
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
