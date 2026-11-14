//! Exercise the exact host commands invoked by the workflows and TPM shell gate.
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
fn tpm_verifier_cli_keeps_failure_details_in_stdout_and_returns_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let serial = tmp.path().join("serial.log");
    fs::write(&serial, "TPMTEST_LOG_BEGIN\n!!!!\nTPMTEST_LOG_END\n").unwrap();
    let out = run(&[
        "verify-tpm-log",
        serial.to_str().unwrap(),
        "--banks",
        "sha1,sha256",
        "--expect",
        "EV_IPL@4",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL: invalid base64 event log"));
}

#[cfg(unix)]
#[test]
fn seabios_serial_port_record_is_a_little_endian_u64() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    let src = tmp.path().join("source");
    let out = tmp.path().join("out");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&src).unwrap();
    let make = bin.join("make");
    fs::write(&make, "#!/usr/bin/env bash\nexit 0\n").unwrap();
    fs::set_permissions(make, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/payloads/seabios/build.sh");
    success(
        Command::new("bash")
            .arg(script)
            .arg(src)
            .arg(&out)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(out.join("sercon-port")).unwrap(),
        0x3f8u64.to_le_bytes()
    );
}
