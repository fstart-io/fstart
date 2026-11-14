use super::*;
use fstart_image_build::boot_test::{Asset, Smp};

// The runner's fbuild re-enters this test executable, which acts as a mock
// fbuild in `mock_fbuild` when MOCK_ARGV is set; otherwise that test does
// nothing.
#[test]
fn mock_fbuild() {
    let Ok(argv) = std::env::var("MOCK_ARGV") else {
        return;
    };
    let env = |key: &str| std::env::var(key).ok();
    let number = |key: &str| env(key).map_or(0.0, |v| v.parse::<f64>().unwrap());
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env("MOCK_CALLS").unwrap())
        .unwrap();
    writeln!(
        calls,
        "{}",
        serde_json::json!({
            "args": argv.lines().collect::<Vec<_>>(),
            "smp": env("FSTART_QEMU_SMP"),
        })
    )
    .unwrap();
    let mut out = io::stdout().lock();
    if argv.starts_with("build") {
        thread::sleep(Duration::from_secs_f64(number("MOCK_BUILD_DELAY")));
        if env("MOCK_BUILD_MARKER").is_some() {
            writeln!(out, "ramstage: ready for payload").unwrap();
        }
        let status = number("MOCK_BUILD_STATUS") as i32;
        if status != 0 {
            writeln!(out, "mock build failed").unwrap();
        }
        out.flush().unwrap();
        std::process::exit(status);
    }
    if env("MOCK_NO_MARKERS").is_none() {
        writeln!(out, "ramstage: ready for payload\nPCI root ready (2 buses)").unwrap();
        out.flush().unwrap();
    }
    thread::sleep(Duration::from_secs_f64(number("MOCK_BOOT_DELAY")));
    std::process::exit(number("MOCK_BOOT_STATUS") as i32);
}

struct Mock {
    dir: tempfile::TempDir,
    runner: Runner,
}

impl Mock {
    fn new(env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls.jsonl");
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let exe = std::env::current_exe().unwrap();
        let mut runner = Runner::new(None, dir.path().join("logs")).unwrap();
        runner.build_timeout = Duration::from_secs(5);
        runner.boot_timeout = Duration::from_secs(1);
        runner.quiet = Duration::from_millis(200);
        runner.fbuild = Box::new(move |args| {
            let mut command = Command::new(&exe);
            command
                .args([
                    "--exact",
                    "boot_test::tests::mock_fbuild",
                    "--nocapture",
                    "--quiet",
                ])
                .env("MOCK_ARGV", args.join("\n"))
                .env("MOCK_CALLS", &calls)
                .envs(env.iter().map(|(k, v)| (k, v)));
            command
        });
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        Self { dir, runner }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    fn boot(&self, test: &BootTest) -> Verdict {
        self.runner.boot("qemu-q35", test)
    }

    /// Each call's arguments, joined by spaces.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.path("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let call: serde_json::Value = serde_json::from_str(line).unwrap();
                let args: Vec<_> = call["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_string())
                    .collect();
                args.join(" ")
            })
            .collect()
    }
}

fn halt() -> BootTest {
    BootTest::new("halt", "halt").expect("ramstage: ready for payload")
}

fn failed(verdict: Verdict, why: &str) {
    match verdict {
        Verdict::Fail(reason) => assert!(reason.contains(why), "{reason}"),
        other => panic!("{other:?}, expected a failure with {why}"),
    }
}

fn passed(verdict: Verdict) {
    assert!(matches!(verdict, Verdict::Pass), "{verdict:?}");
}

#[test]
fn builds_then_boots_with_the_selected_smp() {
    let mock = Mock::new(&[]);
    let test = BootTest {
        name: "halt-smp4".into(),
        ..halt()
    }
    .smp(Smp::cpus(4).max_cpus(8));
    passed(mock.boot(&test));
    assert_eq!(
        mock.calls(),
        [
            "build --board qemu-q35 --release --payload halt",
            "run --board qemu-q35 --release --payload halt",
        ]
    );
    let calls = fs::read_to_string(mock.path("calls.jsonl")).unwrap();
    assert!(calls.contains(r#""smp":"4,maxcpus=8""#), "{calls}");
}

#[test]
fn a_cold_build_does_not_consume_the_boot_deadline() {
    let mock = Mock::new(&[("MOCK_BUILD_DELAY", "1.5")]);
    passed(mock.boot(&halt()));
}

#[test]
fn a_failed_build_boots_nothing_and_leaves_no_stale_log() {
    let mock = Mock::new(&[("MOCK_BUILD_STATUS", "17")]);
    let log = mock.path("logs/qemu-q35-halt.log");
    fs::write(&log, "ramstage: ready for payload\n").unwrap();
    failed(mock.boot(&halt()), "build exit status: 17");
    assert_eq!(mock.calls().len(), 1);
    assert!(!log.exists());
}

#[test]
fn the_build_has_its_own_deadline() {
    let mut mock = Mock::new(&[("MOCK_BUILD_DELAY", "5")]);
    mock.runner.build_timeout = Duration::from_millis(500);
    failed(mock.boot(&halt()), "build timed out");
    assert_eq!(mock.calls().len(), 1);
}

#[test]
fn missing_lines_fail_at_the_deadline() {
    let mock = Mock::new(&[("MOCK_NO_MARKERS", "1"), ("MOCK_BOOT_DELAY", "10")]);
    failed(mock.boot(&halt()), "timed out; missing");
}

#[test]
fn a_quiet_boot_with_every_line_stops_before_the_deadline() {
    let mut mock = Mock::new(&[("MOCK_BOOT_DELAY", "60")]);
    mock.runner.boot_timeout = Duration::from_secs(60);
    let start = Instant::now();
    let test = halt().expect("PCI root ready (");
    passed(mock.boot(&test));
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn a_boot_stopped_at_the_deadline_passes_with_every_line() {
    let mut mock = Mock::new(&[("MOCK_BOOT_DELAY", "10")]);
    mock.runner.quiet = Duration::from_secs(60);
    passed(mock.boot(&halt()));
}

#[test]
fn printed_lines_do_not_hide_a_failed_run() {
    let mock = Mock::new(&[("MOCK_BOOT_STATUS", "9")]);
    failed(mock.boot(&halt()), "exit status: 9");
}

#[test]
fn the_build_output_cannot_supply_a_line() {
    let mock = Mock::new(&[("MOCK_BUILD_MARKER", "1"), ("MOCK_NO_MARKERS", "1")]);
    failed(mock.boot(&halt()), "missing");
}

#[test]
fn a_rejected_line_fails() {
    let mock = Mock::new(&[]);
    let test = halt().reject("PCI root ready");
    failed(mock.boot(&test), "printed");
}

#[test]
fn assets_are_run_inputs_from_the_asset_directory() {
    let test = BootTest::new("linux", "linux")
        .input(RunInput::Kernel(Asset::Armv7Linux))
        .input(RunInput::PayloadFile(
            "board.dtb".into(),
            Asset::OrangePiR1Dtb,
        ))
        .expect("ramstage: ready for payload");

    // Without assets the test is skipped before anything is built.
    let mock = Mock::new(&[]);
    assert!(matches!(mock.boot(&test), Verdict::Skip(_)));
    assert!(mock.calls().is_empty());

    let mut mock = Mock::new(&[]);
    let assets = mock.path("assets");
    mock.runner.assets = Some(assets.clone());
    failed(mock.boot(&test), "no boot asset");

    fs::create_dir_all(assets.join("armv7")).unwrap();
    for asset in [Asset::Armv7Linux, Asset::OrangePiR1Dtb] {
        fs::write(assets.join(asset.path()), "").unwrap();
    }
    passed(mock.boot(&test));
    let a = assets.display();
    assert_eq!(
        mock.calls().last().unwrap(),
        &format!(
            "run --board qemu-q35 --release --payload linux --kernel {a}/armv7/zImage \
             --payload-file board.dtb={a}/armv7/sun8i-h2-plus-orangepi-r1.dtb"
        )
    );
}

#[test]
fn a_boards_tests_have_unique_names_known_payloads_and_expectations() {
    assert!(validate(&[halt()]).is_ok());
    for tests in [
        vec![halt(), halt()],
        vec![BootTest::new("bios", "bios").expect("x")],
        vec![BootTest::new("halt", "halt")],
    ] {
        assert!(validate(&tests).is_err(), "{tests:?}");
    }
}
