//! Exercise the exact host commands the workflows invoke.
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fbuild"))
        .args(args)
        .output()
        .unwrap()
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn checks_list_shows_the_guard_and_discovered_platform_commands() {
    let text = success(run(&["checks", "--list"]));
    assert!(
        text.lines()
            .any(|line| line == "# Check driver-amd capability features")
    );
    assert!(text.lines().any(|line| {
        line.starts_with("cargo clippy -p fstart-driver-amd --target x86_64-unknown-none")
    }));
    assert!(text.lines().any(|line| {
        line == "cargo clippy -p fstart-platform-qemu --lib --no-default-features --features host"
    }));

    // Missing manifest must fail rather than printing an incomplete list.
    let tmp = tempfile::tempdir().unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_fbuild"))
        .args(["checks", "--list"])
        .env("FSTART_WORKSPACE_ROOT", tmp.path())
        .output()
        .unwrap();
    assert!(!failed.status.success());
}

#[test]
fn size_artifact_cli_records_provenance_and_handles_legacy_baselines() {
    let tmp = tempfile::tempdir().unwrap();
    let lines = tmp.path().join("sizes.jsonl");
    let artifact = tmp.path().join("sizes.json");
    let legacy = tmp.path().join("legacy.json");
    fs::write(
        &lines,
        json!({"board":"q35","stages":{"stage":{"flat":10,"text":8,"rodata":2,"data":0,"bss":0}}})
            .to_string()
            + "\n",
    )
    .unwrap();
    success(run(&[
        "sizes",
        "merge",
        lines.to_str().unwrap(),
        artifact.to_str().unwrap(),
        "--commit",
        &"a".repeat(40),
        "--source-date-epoch",
        "1735689600",
        "--pr",
        "207",
    ]));
    let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
    assert_eq!(doc["source_date_epoch"], 1735689600u64);
    assert_eq!(doc["pr"], 207);
    assert!(
        success(run(&[
            "sizes",
            "report",
            artifact.to_str().unwrap(),
            artifact.to_str().unwrap()
        ]))
        .contains("No stage size changed.")
    );
    doc.as_object_mut().unwrap().remove("source_date_epoch");
    fs::write(&legacy, doc.to_string()).unwrap();
    let report = success(run(&[
        "sizes",
        "report",
        legacy.to_str().unwrap(),
        artifact.to_str().unwrap(),
    ]));
    assert!(report.starts_with("<!-- fstart-stage-sizes -->"));
    assert!(report.contains("not comparable"));
    assert!(!report.contains("| Board |"));
    for epoch in [None, Some("-1"), Some("false")] {
        let mut args = vec![
            "sizes",
            "merge",
            lines.to_str().unwrap(),
            artifact.to_str().unwrap(),
            "--commit",
            "abc",
        ];
        if let Some(epoch) = epoch {
            args.extend(["--source-date-epoch", epoch]);
        }
        assert!(!run(&args).status.success());
    }
}

#[test]
fn board_json_preserves_sorted_inventory_and_profile_metadata() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let boards = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_fbuild"))
            .args(args)
            .env("FSTART_WORKSPACE_ROOT", &root)
            .output()
            .unwrap()
    };
    let text = success(boards(&["boards"]));
    let json: Vec<serde_json::Value> =
        serde_json::from_str(&success(boards(&["boards", "--json"]))).unwrap();
    let names: Vec<_> = json.iter().map(|b| b["board"].as_str().unwrap()).collect();
    assert_eq!(names, text.lines().collect::<Vec<_>>());
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        json.iter()
            .any(|b| b["build_profile"]["dependency"] == "fstart-platform-qemu")
    );
}

#[test]
fn board_selection_failures_are_reported_without_masking_exit_status() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tmp = tempfile::tempdir().unwrap();
    let summary = tmp.path().join("summary.md");
    for command in ["build-boards", "boot-test"] {
        let out = Command::new(env!("CARGO_BIN_EXE_fbuild"))
            .args([command, "invalid-board-id"])
            .env("FSTART_WORKSPACE_ROOT", &root)
            .env("GITHUB_STEP_SUMMARY", &summary)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let output = String::from_utf8(out.stdout).unwrap();
        assert!(output.contains("| invalid-board-id |"));
        assert!(output.contains("Not run: board 'invalid-board-id' not found"));
        assert!(output.contains("No physical hardware was tested."));
    }
    let report = fs::read_to_string(summary).unwrap();
    assert!(report.contains("### Release builds and host library tests"));
    assert!(report.contains("### QEMU boot scenarios"));
}

#[test]
fn not_run_reports_declared_scenarios_without_creating_boot_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path().join("logs");
    let text = success(
        Command::new(env!("CARGO_BIN_EXE_fbuild"))
            .args([
                "boot-test",
                "qemu-riscv64",
                "--not-run",
                "assets unavailable",
                "--logs",
            ])
            .arg(&logs)
            .env(
                "FSTART_WORKSPACE_ROOT",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            )
            .env_remove("GITHUB_STEP_SUMMARY")
            .output()
            .unwrap(),
    );
    for scenario in ["halt", "linux", "uefi"] {
        assert!(
            text.contains(&format!(
                "| qemu-riscv64 | {scenario} | {scenario} | Not run: assets unavailable |"
            )),
            "{text}"
        );
    }
    assert!(!logs.exists());
}
