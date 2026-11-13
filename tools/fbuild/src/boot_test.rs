//! Runs the QEMU boot tests a board's platform declares in its host plan
//! (`fstart_image_build::boot_test`): `fbuild boot-test`.
//!
//! A test passes when every expected line is in QEMU's output, no rejected
//! one is, and QEMU did not fail on its own. Most guests idle once they are
//! done, so the runner stops QEMU when every expected line is there and the
//! output has been quiet for a while, or at the deadline. A test with a TPM
//! boots with a fresh swtpm, and its guest's event log must replay to the
//! PCRs it reads (`tpm_log`).

use crate::board_manifest::BoardManifest;
use crate::payload::PayloadChoice;
use clap::ValueEnum;
use fstart_crypto::pcr;
use fstart_image_build::boot_test::{Bank, BootTest, RunInput, Tpm, TpmFamily};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The boot tests of `board`, from its platform's host plan.
pub fn load(root: &Path, board: &BoardManifest) -> Result<Vec<BootTest>, String> {
    let source = crate::profile_source::load_profile(root, board)?;
    let tests = crate::host_plan::boot_tests(root, &source, board)?;
    validate(&tests).map_err(|e| format!("{}: {e}", board.board))?;
    Ok(tests)
}

/// What the plan's types cannot express.
fn validate(tests: &[BootTest]) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    for test in tests {
        let name = &test.name;
        PayloadChoice::from_str(&test.payload, false)
            .map_err(|_| format!("boot test {name}: unknown payload {}", test.payload))?;
        if !names.insert(name) {
            return Err(format!("boot test {name}: duplicate name"));
        }
        if test.expect.is_empty() {
            return Err(format!("boot test {name}: expects nothing"));
        }
    }
    Ok(())
}

fn alg(bank: Bank) -> u16 {
    match bank {
        Bank::Sha1 => pcr::ALG_SHA1,
        Bank::Sha256 => pcr::ALG_SHA256,
        Bank::Sha384 => pcr::ALG_SHA384,
        Bank::Sha512 => pcr::ALG_SHA512,
    }
}

/// `fbuild` with these arguments.
pub type Fbuild = Box<dyn Fn(&[String]) -> Command>;

/// How the runner builds, boots and judges.
pub struct Runner {
    /// The boot assets; tests that need one are skipped without it.
    pub assets: Option<PathBuf>,
    pub logs: PathBuf,
    pub build_timeout: Duration,
    pub boot_timeout: Duration,
    /// How long QEMU's output must be quiet after the last expected line.
    pub quiet: Duration,
    pub fbuild: Fbuild,
}

impl Runner {
    pub fn new(assets: Option<PathBuf>, logs: PathBuf) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("fbuild executable: {e}"))?;
        Ok(Self {
            assets,
            logs,
            build_timeout: Duration::from_secs(15 * 60),
            boot_timeout: Duration::from_secs(120),
            quiet: Duration::from_secs(10),
            fbuild: Box::new(move |args| {
                let mut command = Command::new(&exe);
                command.args(args);
                command
            }),
        })
    }

    /// Run `board`'s `tests`. Returns the number that failed.
    pub fn run(&self, board: &str, tests: &[&BootTest]) -> Result<usize, String> {
        fs::create_dir_all(&self.logs)
            .map_err(|e| format!("create {}: {e}", self.logs.display()))?;
        let mut failures = 0;
        for test in tests {
            let name = &test.name;
            github(&format!("::group::{board} {name}"));
            let verdict = self.boot(board, test);
            github("::endgroup::");
            match verdict {
                Verdict::Pass => println!("PASS {board} {name}"),
                Verdict::Skip(why) => println!("SKIP {board} {name} ({why})"),
                Verdict::Fail(why) => {
                    println!("FAIL {board} {name} ({why})");
                    github(&format!("::error title={board} {name}::{why}"));
                    failures += 1;
                }
            }
        }
        Ok(failures)
    }

    fn boot(&self, board: &str, test: &BootTest) -> Verdict {
        let assets = match (&self.assets, test.inputs.is_empty()) {
            (_, true) => None,
            (Some(dir), false) => Some(dir),
            (None, false) => return Verdict::Skip("needs --assets".into()),
        };
        let asset = |input: &RunInput| -> Result<String, String> {
            let path = assets.expect("checked above").join(input.asset().path());
            if path.is_file() {
                Ok(path.display().to_string())
            } else {
                Err(format!("no boot asset {}", path.display()))
            }
        };

        let log = self.logs.join(format!("{board}-{}.log", test.name));
        let build_log = log.with_extension("build.log");
        let _ = fs::remove_file(&log);
        let mut args = vec![
            "--board".to_string(),
            board.into(),
            "--release".into(),
            "--payload".into(),
            test.payload.clone(),
        ];

        // Compile before the boot deadline starts; the run below only
        // assembles the image with its run-time inputs.
        let build = [vec!["build".into()], args.clone()].concat();
        match supervise((self.fbuild)(&build), &build_log, self.build_timeout, None) {
            Ok(Outcome::Exited(status)) if status.success() => {}
            Ok(outcome) => return fail_with_log(&format!("build {outcome}"), &build_log),
            Err(e) => return Verdict::Fail(format!("build: {e}")),
        }

        for input in &test.inputs {
            let path = match asset(input) {
                Ok(path) => path,
                Err(e) => return Verdict::Fail(e),
            };
            match input {
                RunInput::Kernel(_) => args.extend(["--kernel".into(), path]),
                RunInput::Firmware(_) => args.extend(["--firmware".into(), path]),
                RunInput::SecureFirmware(_) => args.extend(["--secure-firmware".into(), path]),
                RunInput::PayloadFile(name, _) => {
                    args.extend(["--payload-file".into(), format!("{name}={path}")]);
                }
                // The guest may write its disk: boot a copy, so tests stay
                // independent and read-only assets (a Nix store path) work.
                RunInput::Disk(_) => {
                    let disk = log.with_extension("disk");
                    let _ = fs::remove_file(&disk);
                    let copied = fs::copy(&path, &disk).and_then(|_| {
                        fs::set_permissions(&disk, fs::Permissions::from_mode(0o644))
                    });
                    if let Err(e) = copied {
                        return Verdict::Fail(format!("copy {path}: {e}"));
                    }
                    args.extend(["--disk".into(), disk.display().to_string()]);
                }
            }
        }

        let mut command = (self.fbuild)(&[vec!["run".into()], args].concat());
        command.env(
            "FSTART_QEMU_SMP",
            test.smp.map_or_else(|| "1".into(), |smp| smp.qemu_arg()),
        );
        let swtpm = match &test.tpm {
            Some(tpm) => match Swtpm::start(tpm, &log.with_extension("swtpm")) {
                Ok(swtpm) => {
                    command.env("FSTART_QEMU_EXTRA_ARGS", swtpm.qemu_args());
                    Some(swtpm)
                }
                Err(e) => return Verdict::Fail(format!("swtpm: {e}")),
            },
            None => None,
        };
        let deadline = test.timeout.map_or(self.boot_timeout, Duration::from_secs);
        let outcome = supervise(command, &log, deadline, Some((&test.expect, self.quiet)));
        drop(swtpm);
        let _ = fs::remove_file(log.with_extension("disk"));
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => return Verdict::Fail(format!("run: {e}")),
        };

        let output = fs::read(&log).unwrap_or_default();
        let output = String::from_utf8_lossy(&output);
        let missing: Vec<_> = test
            .expect
            .iter()
            .filter(|line| !output.contains(line.as_str()))
            .collect();
        if !missing.is_empty() {
            return fail_with_log(&format!("{outcome}; missing {missing:?}"), &log);
        }
        if let Outcome::Exited(status) = outcome
            && !status.success()
        {
            return fail_with_log(&format!("{outcome}"), &log);
        }
        let rejected: Vec<_> = test
            .reject
            .iter()
            .filter(|line| output.contains(line.as_str()))
            .collect();
        if !rejected.is_empty() {
            return fail_with_log(&format!("printed {rejected:?}"), &log);
        }
        if let Some(tpm) = &test.tpm {
            let banks: Vec<_> = tpm.banks.iter().map(|bank| alg(*bank)).collect();
            return match crate::tpm_log::verify(&output, &tpm.events, Some(&banks)) {
                Ok(report) => {
                    let _ = fs::write(log.with_extension("tpm"), report);
                    Verdict::Pass
                }
                Err(e) => fail_with_log(&format!("TPM log: {e}"), &log),
            };
        }
        Verdict::Pass
    }
}

#[derive(Debug)]
enum Verdict {
    Pass,
    Skip(String),
    Fail(String),
}

/// Fail, showing the end of `log`: CI keeps the logs only as an artifact.
fn fail_with_log(why: &str, log: &Path) -> Verdict {
    if let Ok(text) = fs::read(log) {
        let text = String::from_utf8_lossy(&text);
        let lines: Vec<_> = text.lines().collect();
        let mut stderr = io::stderr().lock();
        for line in &lines[lines.len().saturating_sub(40)..] {
            let _ = writeln!(stderr, "{line}");
        }
    }
    Verdict::Fail(format!("{why}; {}", log.display()))
}

fn github(line: &str) {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        println!("{line}");
    }
}

#[derive(Debug)]
enum Outcome {
    Exited(ExitStatus),
    /// Stopped at the deadline.
    TimedOut,
    /// Stopped once every expected line was printed.
    Done,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(status) => write!(f, "{status}"),
            Self::TimedOut => f.write_str("timed out"),
            Self::Done => f.write_str("stopped when done"),
        }
    }
}

const POLL: Duration = Duration::from_millis(100);

/// Run `command` in its own process group with its output in `log`, until it
/// exits, `deadline` passes, or, given `(expect, quiet)`, every `expect` line
/// is in the log and it has not grown for `quiet`. The whole group is
/// stopped: `fbuild run` leaves QEMU behind otherwise.
fn supervise(
    mut command: Command,
    log: &Path,
    deadline: Duration,
    done: Option<(&[String], Duration)>,
) -> io::Result<Outcome> {
    let file = fs::File::create(log)?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .process_group(0)
        .spawn()?;
    let start = Instant::now();
    let (mut size, mut grown) = (0, Instant::now());
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome::Exited(status));
        }
        if start.elapsed() >= deadline {
            stop(&mut child)?;
            return Ok(Outcome::TimedOut);
        }
        if let Some((expect, quiet)) = done {
            let now = fs::metadata(log)?.len();
            if now != size {
                (size, grown) = (now, Instant::now());
            } else if grown.elapsed() >= quiet {
                let text = fs::read(log)?;
                let text = String::from_utf8_lossy(&text);
                if expect.iter().all(|line| text.contains(line.as_str())) {
                    stop(&mut child)?;
                    return Ok(Outcome::Done);
                }
            }
        }
        thread::sleep(POLL);
    }
}

/// SIGTERM the child's process group, SIGKILL it if it lingers.
fn stop(child: &mut Child) -> io::Result<()> {
    use rustix::process::{Pid, Signal, kill_process_group};
    let group = Pid::from_child(child);
    let _ = kill_process_group(group, Signal::TERM);
    for _ in 0..100 {
        if child.try_wait()?.is_some() {
            break;
        }
        thread::sleep(POLL);
    }
    let _ = kill_process_group(group, Signal::KILL);
    child.wait().map(drop)
}

/// A fresh software TPM in `state`, which keeps its logs; stopped on drop.
struct Swtpm {
    state: PathBuf,
    child: Child,
}

impl Swtpm {
    fn start(tpm: &Tpm, state: &Path) -> io::Result<Self> {
        let _ = fs::remove_dir_all(state);
        fs::create_dir_all(state)?;
        let tpm2 = tpm.family == TpmFamily::Tpm20;
        let mut setup = Command::new("swtpm_setup");
        setup.args(["--tpmstate".as_ref(), state.as_os_str()]);
        if tpm2 {
            let banks: Vec<_> = tpm.banks.iter().map(|bank| bank.name()).collect();
            setup.args(["--tpm2", "--pcr-banks", &banks.join(",")]);
        }
        let setup = setup
            .args(["--createek", "--overwrite"])
            .stdout(fs::File::create(state.join("setup.log"))?)
            .stderr(Stdio::null())
            .status()?;
        if !setup.success() {
            return Err(io::Error::other(format!("swtpm_setup {setup}")));
        }
        let mut swtpm = Command::new("swtpm");
        swtpm.arg("socket");
        if tpm2 {
            swtpm.arg("--tpm2");
        }
        let arg = |prefix: &str, path: PathBuf| {
            let mut arg = OsString::from(prefix);
            arg.push(path);
            arg
        };
        let child = swtpm
            .arg("--tpmstate")
            .arg(arg("dir=", state.into()))
            .arg("--ctrl")
            .arg(arg("type=unixio,path=", state.join("sock")))
            .arg("--log")
            .arg(arg("level=1,file=", state.join("swtpm.log")))
            .stdin(Stdio::null())
            .spawn()?;
        let swtpm = Self {
            state: state.into(),
            child,
        };
        let start = Instant::now();
        while !swtpm.state.join("sock").exists() {
            if start.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other("no control socket"));
            }
            thread::sleep(POLL);
        }
        Ok(swtpm)
    }

    fn qemu_args(&self) -> String {
        format!(
            "-chardev socket,id=chrtpm,path={} -tpmdev emulator,id=tpm0,chardev=chrtpm \
             -device tpm-tis,tpmdev=tpm0",
            self.state.join("sock").display()
        )
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
#[path = "boot_test_tests.rs"]
mod tests;
