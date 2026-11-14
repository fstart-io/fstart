//! Exercise the real shell orchestration without building firmware or running QEMU.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::Duration,
};
use tempfile::TempDir;

// The cargo shim re-enters this test executable, so no second scripting runtime
// or separately compiled mock tool is needed. Normal cargo test invocation is a
// no-op; only the subprocess receives MOCK_ARGV.
#[test]
fn mock_cargo() {
    let Ok(argv) = env::var("MOCK_ARGV") else {
        return;
    };
    let argv: Vec<_> = argv.lines().collect();
    let args = &argv[argv.iter().position(|arg| *arg == "--").unwrap() + 1..];
    use std::io::Write;
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env::var_os("MOCK_CALLS").unwrap())
        .unwrap();
    writeln!(
        calls,
        "{}",
        json!({"args": args, "smp": env::var("FSTART_QEMU_SMP").ok()})
    )
    .unwrap();
    let ready = PathBuf::from(env::var_os("MOCK_READY").unwrap());
    let delay = |key| {
        thread::sleep(Duration::from_secs_f64(
            env::var(key)
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap(),
        ))
    };
    let status = |key| {
        env::var(key)
            .unwrap_or_else(|_| "0".into())
            .parse::<i32>()
            .unwrap()
    };
    match args[0] {
        "build" => {
            delay("MOCK_BUILD_DELAY");
            let code = status("MOCK_BUILD_STATUS");
            if code != 0 {
                println!("mock build failed");
                std::process::exit(code);
            }
            fs::write(&ready, "").unwrap();
            if env::var_os("MOCK_BUILD_MARKER").is_some() {
                println!("ramstage: ready for payload");
            }
        }
        "qemu" => {
            if !ready.exists() {
                delay("MOCK_BUILD_DELAY");
                fs::write(&ready, "").unwrap();
            }
            if env::var_os("MOCK_NO_BOOT_MARKERS").is_none() {
                for marker in [
                    "ramstage: ready for payload",
                    "mp: initialization complete (2 CPUs)",
                    "2 CPUs relocated",
                    "SMM: permanent SMI returned on all 2 CPUs",
                    "mp: initialization complete (4 CPUs)",
                    "SMM: selected AMD64 revision 0x00020064; 4 CPUs relocated",
                    "SMM: permanent SMI enabled and SMRAM locked",
                    "SMM: global SMI enable locked and verified",
                    "SMM: permanent SMI returned on all 4 CPUs",
                    "FSTART_CI_BOOT_SUCCESS",
                    "UROOT_BOOT_SUCCESS",
                    "qemu-sbsa ramstage: ready for payload",
                    "PCI root ready (",
                ] {
                    println!("{marker}");
                }
                std::io::stdout().flush().unwrap();
            }
            delay("MOCK_BOOT_DELAY");
            std::process::exit(status("MOCK_BOOT_STATUS"));
        }
        other => panic!("unexpected mock command {other}"),
    }
}

struct Matrix {
    root: TempDir,
}
impl Matrix {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in ["bin", "assets", "x86-assets"] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        let cargo = root.path().join("bin/cargo");
        fs::write(&cargo, "#!/usr/bin/env bash\nexport MOCK_ARGV=\"$(printf '%s\\n' \"$@\")\"\nexec \"$MOCK_TEST_BINARY\" --exact mock_cargo --nocapture\n").unwrap();
        fs::set_permissions(cargo, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }
    fn script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ci/qemu-boot-tests.sh")
    }
    fn list(&self, filters: &[&str]) -> Vec<String> {
        let out = Command::new("bash")
            .arg(Self::script())
            .arg("--list")
            .args(filters)
            .current_dir(self.root.path())
            .env("MOCK_TEST_BINARY", env::current_exe().unwrap())
            .env("MOCK_CALLS", self.path("calls.jsonl"))
            .env("QEMU_BOOT_LOG_DIR", self.path("logs"))
            .env("QEMU_BOOT_X86_ASSETS", self.path("x86-assets"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }
    fn run(&self, board: &str, payload: &str, overrides: &[(&str, &str)]) -> (Output, Vec<Value>) {
        let mut paths = vec![self.path("bin")];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap()));
        let output = Command::new("bash")
            .arg(Self::script())
            .arg(self.path("assets"))
            .args(["--board", board, "--payload", payload])
            .current_dir(self.root.path())
            .env("PATH", env::join_paths(paths).unwrap())
            .env("MOCK_TEST_BINARY", env::current_exe().unwrap())
            .env("MOCK_CALLS", self.path("calls.jsonl"))
            .env("MOCK_READY", self.path("ready"))
            .env("QEMU_BOOT_LOG_DIR", self.path("logs"))
            .env("QEMU_BOOT_X86_ASSETS", self.path("x86-assets"))
            .env("QEMU_BUILD_TIMEOUT", "5s")
            .env("QEMU_BOOT_TIMEOUT", "1s")
            .envs(overrides.iter().copied())
            .output()
            .unwrap();
        let calls = fs::read_to_string(self.path("calls.jsonl"))
            .unwrap_or_else(|e| panic!("{e}: {output:?}"))
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (output, calls)
    }
}
fn passed(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed, 0 failed"));
}
fn commands(calls: &[Value]) -> Vec<&str> {
    calls
        .iter()
        .map(|c| c["args"][0].as_str().unwrap())
        .collect()
}

#[test]
fn cold_build_does_not_consume_boot_timeout() {
    let (out, calls) = Matrix::new().run("qemu-q35-i686", "halt", &[("MOCK_BUILD_DELAY", "2")]);
    passed(&out);
    assert_eq!(commands(&calls), ["build", "qemu"]);
}
#[test]
fn build_failure_does_not_run_or_reuse_stale_boot_log() {
    let m = Matrix::new();
    fs::create_dir(m.path("logs")).unwrap();
    let log = m.path("logs/qemu-q35-i686-halt.log");
    fs::write(&log, "ramstage: ready for payload\n").unwrap();
    let (out, calls) = m.run("qemu-q35-i686", "halt", &[("MOCK_BUILD_STATUS", "17")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("build (exit 17;"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("mock build failed"));
    assert_eq!(commands(&calls), ["build"]);
    assert_eq!(fs::read_to_string(log).unwrap(), "");
}
#[test]
fn build_has_its_own_deadline() {
    let (out, calls) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[("QEMU_BUILD_TIMEOUT", "0.5s"), ("MOCK_BUILD_DELAY", "2")],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("build (exit 124;"));
    assert_eq!(commands(&calls), ["build"]);
}
#[test]
fn boot_deadline_rejects_missing_markers() {
    let (out, calls) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[
            ("QEMU_BOOT_TIMEOUT", "0.5s"),
            ("MOCK_BOOT_DELAY", "2"),
            ("MOCK_NO_BOOT_MARKERS", "1"),
        ],
    );
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("(exit 124;"));
    assert!(!stdout.contains("build (exit"));
    assert_eq!(commands(&calls), ["build", "qemu"]);
}
#[test]
fn halt_marker_passes_when_qemu_is_stopped_by_timeout() {
    let (out, _) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[("QEMU_BOOT_TIMEOUT", "0.5s"), ("MOCK_BOOT_DELAY", "2")],
    );
    passed(&out);
}
#[test]
fn marker_does_not_hide_run_failure() {
    let (out, _) = Matrix::new().run("qemu-q35-i686", "halt", &[("MOCK_BOOT_STATUS", "9")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("(exit 9;"));
}
#[test]
fn build_output_cannot_supply_boot_marker() {
    let (out, _) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[("MOCK_BUILD_MARKER", "1"), ("MOCK_NO_BOOT_MARKERS", "1")],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("(exit 0;"));
}
#[test]
fn variant_uses_payload_kind_and_preserves_smp() {
    let (out, calls) = Matrix::new().run("qemu-q35", "halt-smp4-hotplug", &[]);
    passed(&out);
    for call in &calls {
        let args = call["args"].as_array().unwrap();
        assert_eq!(
            args[args.iter().position(|a| a == "--payload").unwrap() + 1],
            "halt"
        );
    }
    assert_eq!(calls[1]["smp"], "4,maxcpus=8");
}
#[test]
fn image_and_runtime_inputs_remain_on_qemu_command() {
    for (board, payload, extra) in [
        (
            "qemu-riscv64",
            "linux",
            vec!["--kernel", "Image-riscv64", "--firmware", "fw_dynamic.bin"],
        ),
        ("qemu-q35", "uefi-disk", vec!["--disk", "disk.img"]),
        (
            "qemu-sbsa",
            "halt",
            vec!["--secure-firmware", "sbsa-secure.pflash"],
        ),
    ] {
        let m = Matrix::new();
        fs::write(m.path("assets/sbsa-secure.pflash"), "").unwrap();
        fs::write(m.path("x86-assets/disk.img"), "").unwrap();
        let (out, calls) = m.run(board, payload, &[]);
        passed(&out);
        let expected: Vec<_> = [
            "--board",
            board,
            "--release",
            "--payload",
            payload.split('-').next().unwrap(),
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            calls[0]["args"],
            json!([vec!["build".into()], expected.clone()].concat())
        );
        let extra: Vec<_> = extra
            .into_iter()
            .map(|v| {
                if v.starts_with("--") {
                    v.into()
                } else {
                    m.path(&format!(
                        "{}/{v}",
                        if v == "disk.img" {
                            "x86-assets"
                        } else {
                            "assets"
                        }
                    ))
                    .display()
                    .to_string()
                }
            })
            .collect();
        assert_eq!(
            calls[1]["args"],
            json!([vec!["qemu".into()], expected, extra].concat())
        );
    }
}
#[test]
fn boot_stops_once_marker_is_logged_and_log_is_quiet() {
    // The guest would idle until the 60 s deadline; the marker and one quiet
    // second end the boot instead.
    let start = std::time::Instant::now();
    let (out, _) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[
            ("QEMU_BOOT_TIMEOUT", "60s"),
            ("MOCK_BOOT_DELAY", "60"),
            ("QEMU_BOOT_QUIET", "1"),
        ],
    );
    passed(&out);
    assert!(start.elapsed() < Duration::from_secs(10));
}
#[test]
fn quiet_log_without_marker_still_waits_for_deadline() {
    let (out, _) = Matrix::new().run(
        "qemu-q35-i686",
        "halt",
        &[
            ("QEMU_BOOT_TIMEOUT", "3s"),
            ("MOCK_BOOT_DELAY", "10"),
            ("MOCK_NO_BOOT_MARKERS", "1"),
            ("QEMU_BOOT_QUIET", "1"),
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("(exit 124;"));
}
#[test]
fn list_prints_every_combination_without_building() {
    let m = Matrix::new();
    let out = m.list(&[]);
    // The combinations CI shards over, in script order. Asset-gated entries
    // (uefi-disk, qemu-sbsa, orangepi-r1 linux) are listed even though no
    // assets exist here, and listing builds and boots nothing, creates no log
    // directory and copies no DTB.
    assert_eq!(
        out,
        [
            "qemu-q35 halt",
            "qemu-q35 halt-smp4",
            "qemu-q35 halt-smp4-multisocket",
            "qemu-q35 halt-smp4-hotplug",
            "qemu-q35 uefi",
            "qemu-q35 uefi-disk",
            "qemu-q35-i686 halt",
            "qemu-q35-i686 halt-smp2",
            "qemu-riscv64 halt",
            "qemu-riscv64 linux",
            "qemu-riscv64 uefi",
            "qemu-sifive-u halt",
            "qemu-sifive-u linux",
            "qemu-aarch64 halt",
            "qemu-aarch64 linux",
            "qemu-aarch64 uefi",
            "qemu-armv7 halt",
            "qemu-armv7 linux",
            "qemu-sbsa halt",
            "orangepi-r1 halt",
            "orangepi-r1 linux",
        ]
    );
    assert!(!m.path("calls.jsonl").exists());
    assert!(!m.path("logs").exists());
    assert!(!m.path("boards").exists());
}
#[test]
fn list_honours_filters() {
    assert_eq!(
        Matrix::new().list(&["--board", "qemu-q35", "--payload", "uefi"]),
        ["qemu-q35 uefi"]
    );
}
