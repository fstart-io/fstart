use super::*;
use fstart_image_build::boot_test::Asset;
use fstart_image_build::plan::BuildSelection;
use fstart_platform_qemu::facts::VirtMachine;

fn halt() -> BootTest {
    BootTest::new("halt", "halt").expect("ramstage: ready for payload")
}

fn exited(code: i32) -> Outcome {
    use std::os::unix::process::ExitStatusExt;
    Outcome::Exited(ExitStatus::from_raw(code << 8))
}

fn recipe(machine: VirtMachine) -> RunRecipe {
    fstart_platform_qemu::host::resolve(machine, BuildSelection::default())
        .unwrap()
        .run
        .unwrap()
}

#[test]
fn the_qemu_machines_declare_valid_tests() {
    for machine in [
        VirtMachine::Q35,
        VirtMachine::Q35ProtectedMode,
        VirtMachine::Riscv64,
        VirtMachine::SifiveU,
        VirtMachine::Aarch64,
        VirtMachine::Armv7,
        VirtMachine::Sbsa,
    ] {
        let recipe = recipe(machine);
        assert!(!recipe.tests.is_empty(), "{machine:?}");
        validate(&recipe).unwrap();
    }
}

#[test]
fn every_plan_of_a_machine_carries_the_same_tests() {
    let halt = recipe(VirtMachine::Q35).tests;
    let uefi = fstart_platform_qemu::host::resolve(
        VirtMachine::Q35,
        BuildSelection {
            payload: Some("uefi".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .run
    .unwrap()
    .tests;
    assert_eq!(halt, uefi);
}

#[test]
fn a_boards_tests_have_unique_names_known_payloads_expectations_and_a_tpm() {
    let mut recipe = recipe(VirtMachine::Armv7);
    for tests in [
        vec![halt(), halt()],
        vec![BootTest::new("bios", "bios").expect("x")],
        vec![BootTest::new("halt", "halt")],
    ] {
        recipe.tests = tests;
        assert!(validate(&recipe).is_err(), "{:?}", recipe.tests);
    }
    let mut measured = self::recipe(VirtMachine::Q35);
    measured.tpm = None;
    assert!(validate(&measured).unwrap_err().contains("no TPM device"));
}

#[test]
fn every_line_must_appear_and_no_rejected_one() {
    let test = halt().expect("PCI root ready").reject("FFS mount");
    let output = "ramstage: ready for payload\nPCI root ready (2 devices)\n";
    assert!(judge(&test, &Outcome::Done, output).is_ok());
    assert!(judge(&test, &Outcome::TimedOut, output).is_ok());
    let missing = judge(&test, &Outcome::TimedOut, "ramstage: ready for payload\n");
    assert!(missing.unwrap_err().contains("timed out; missing"));
    let rejected = judge(&test, &Outcome::Done, &format!("{output}FFS mount\n"));
    assert!(rejected.unwrap_err().contains("printed"));
}

#[test]
fn printed_lines_do_not_hide_a_failed_run() {
    let output = "ramstage: ready for payload\n";
    assert!(judge(&halt(), &exited(0), output).is_ok());
    assert!(judge(&halt(), &exited(9), output).is_err());
}

#[test]
fn a_measured_boot_needs_a_log_that_replays() {
    let test = recipe(VirtMachine::Q35)
        .tests
        .into_iter()
        .find(|test| test.measured.is_some())
        .unwrap();
    let output = test.expect.join("\n");
    assert!(
        judge(&test, &Outcome::Done, &output)
            .unwrap_err()
            .contains("TPM log")
    );
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

#[test]
fn a_quiet_boot_with_every_line_stops_before_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("boot.log");
    let start = Instant::now();
    let expect = ["ready".to_string()];
    let outcome = supervise(
        shell("echo ready; sleep 60"),
        &log,
        Duration::from_secs(30),
        Some((&expect, Duration::from_millis(300))),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Done), "{outcome}");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(fs::read_to_string(&log).unwrap(), "ready\n");
}

#[test]
fn a_boot_without_every_line_runs_to_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("boot.log");
    let expect = ["ready".to_string(), "never".to_string()];
    let outcome = supervise(
        shell("echo ready; sleep 60"),
        &log,
        Duration::from_secs(1),
        Some((&expect, Duration::from_millis(100))),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::TimedOut), "{outcome}");
}

#[test]
fn an_exit_ends_the_boot() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("boot.log");
    let outcome = supervise(
        shell("echo out; echo err >&2; exit 3"),
        &log,
        Duration::from_secs(30),
        None,
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Exited(status) if status.code() == Some(3)));
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.contains("out") && text.contains("err"));
}

#[test]
fn assets_come_from_the_asset_directory() {
    let binding = Binding::Input("kernel".into(), Asset::Armv7Linux);
    let runner = Runner::default();
    assert!(matches!(runner.asset(&binding), Err(Verdict::Skip(_))));

    let dir = tempfile::tempdir().unwrap();
    let runner = Runner {
        assets: Some(dir.path().into()),
        ..Default::default()
    };
    assert!(
        matches!(runner.asset(&binding), Err(Verdict::Fail(why)) if why.contains("no boot asset"))
    );
    fs::create_dir_all(dir.path().join("armv7")).unwrap();
    fs::write(dir.path().join("armv7/zImage"), "").unwrap();
    assert_eq!(
        runner.asset(&binding).ok(),
        Some(dir.path().join("armv7/zImage"))
    );
}
