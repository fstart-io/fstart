//! Runs the QEMU boot tests of a board's run recipe
//! (`fstart_image_build::boot_test`): `fbuild boot-test`.
//!
//! A test boots the image its payload selects the way `fbuild qemu` boots
//! it, the recipe rendered by [`crate::qemu::command`], with the test's boot
//! assets bound by the names `fbuild qemu` takes. It passes when every
//! expected line is in QEMU's output, no rejected one is, and QEMU did not
//! fail on its own. Most guests idle once they are done, so the runner stops
//! QEMU when every expected line is there and the output has been quiet for
//! a while, or at the deadline. A measured boot runs with a fresh swtpm, and
//! its guest's event log must replay to the PCRs it reads (`tpm_log`).

use crate::board_manifest::BoardManifest;
use crate::payload::{AssembleArgs, PayloadChoice, PlanArgs};
use crate::plan_executor::Resolved;
use clap::ValueEnum;
use fstart_crypto::pcr;
use fstart_image_build::boot_test::{Bank, Binding, BootTest};
use fstart_image_build::run::RunRecipe;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The boot tests of `board`: its default plan's run recipe's.
pub fn load(root: &Path, board: &BoardManifest) -> Result<Vec<BootTest>, String> {
    let resolved = Resolved::load(root, board, PlanArgs::default().selection(None))?;
    match &resolved.plan.run {
        Some(recipe) => {
            validate(recipe).map_err(|e| format!("{}: {e}", board.board))?;
            Ok(recipe.tests.clone())
        }
        None => Ok(Vec::new()),
    }
}

/// What the recipe's types cannot express.
fn validate(recipe: &RunRecipe) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    for test in &recipe.tests {
        let name = &test.name;
        PayloadChoice::from_str(&test.payload, false)
            .map_err(|_| format!("boot test {name}: unknown payload {}", test.payload))?;
        if !names.insert(name) {
            return Err(format!("boot test {name}: duplicate name"));
        }
        if test.expect.is_empty() {
            return Err(format!("boot test {name}: expects nothing"));
        }
        if test.measured.is_some() && recipe.tpm.is_none() {
            return Err(format!("boot test {name}: the machine has no TPM device"));
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

/// How the runner boots and judges.
pub struct Runner {
    /// The boot assets; tests that need one are skipped without them.
    pub assets: Option<PathBuf>,
    pub logs: PathBuf,
    pub boot_timeout: Duration,
    /// How long QEMU's output must be quiet after the last expected line.
    pub quiet: Duration,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            assets: None,
            logs: PathBuf::from("target/boot-tests"),
            boot_timeout: Duration::from_secs(120),
            quiet: Duration::from_secs(10),
        }
    }
}

/// One selected scenario's verdict and the logs this invocation produced.
pub struct TestResult {
    pub verdict: Verdict,
    pub logs: Vec<PathBuf>,
}

impl Runner {
    /// Run `board`'s `tests`, returning results in test order.
    pub fn run(
        &self,
        root: &Path,
        board: &BoardManifest,
        tests: &[&BootTest],
    ) -> Result<Vec<TestResult>, String> {
        fs::create_dir_all(&self.logs)
            .map_err(|e| format!("create {}: {e}", self.logs.display()))?;
        Ok(tests
            .iter()
            .map(|test| self.run_one(root, board, test))
            .collect())
    }

    fn run_one(&self, root: &Path, board: &BoardManifest, test: &BootTest) -> TestResult {
        let name = &test.name;
        let board_name = &board.board;
        let log = self.logs.join(format!("{board_name}-{name}.log"));
        // Do not report stale logs from a prior invocation when a scenario
        // skips, or fails before QEMU or swtpm starts.
        let logs: Vec<_> = ["log", "swtpm-setup.log", "swtpm.log", "tpm"]
            .into_iter()
            .map(|ext| log.with_extension(ext))
            .filter(|path| match fs::remove_file(path) {
                Ok(()) => true,
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            })
            .collect();
        github(&format!("::group::{board_name} {name}"));
        let verdict = self.boot(root, board, test, &log);
        github("::endgroup::");
        match &verdict {
            Verdict::Pass => println!("PASS {board_name} {name}"),
            Verdict::Skip(why) => println!("SKIP {board_name} {name} ({why})"),
            Verdict::Fail(why) => {
                println!("FAIL {board_name} {name} ({why})");
                github(&format!("::error title={board_name} {name}::{why}"));
            }
        }
        TestResult {
            verdict,
            logs: logs.into_iter().filter(|path| path.is_file()).collect(),
        }
    }

    /// The boot asset `binding` names.
    fn asset(&self, binding: &Binding) -> Result<PathBuf, Verdict> {
        let Some(assets) = &self.assets else {
            return Err(Verdict::Skip("needs --assets".into()));
        };
        let path = assets.join(binding.asset().path());
        if path.is_file() {
            Ok(path)
        } else {
            Err(Verdict::Fail(format!("no boot asset {}", path.display())))
        }
    }

    fn boot(&self, root: &Path, board: &BoardManifest, test: &BootTest, log: &Path) -> Verdict {
        let mut assemble = AssembleArgs::default();
        let mut supplied = BTreeMap::new();
        let mut disk = None;
        for binding in &test.bindings {
            let path = match self.asset(binding) {
                Ok(path) => path,
                Err(verdict) => return verdict,
            };
            match binding {
                Binding::Input(name, _) => {
                    supplied.insert(name.clone(), path);
                }
                Binding::PayloadFile(name, _) => {
                    assemble
                        .payload_files
                        .push((name.clone(), path.display().to_string()));
                }
                Binding::Disk(_) => disk = Some(path),
            }
        }

        let payload = PayloadChoice::from_str(&test.payload, false).ok();
        let built = Resolved::load(root, board, PlanArgs::default().selection(payload)).and_then(
            |resolved| crate::board_tool::qemu_image(board, &resolved, true, supplied, &assemble),
        );
        let built = match built {
            Ok(built) => built,
            Err(e) => return Verdict::Fail(format!("build: {e}")),
        };
        // The guest may write its disk: boot a copy, so tests stay
        // independent and read-only assets (a Nix store path) work.
        let disk = match disk {
            Some(source) => {
                let copy = log.with_extension("disk");
                let copied = fs::copy(&source, &copy)
                    .and_then(|_| fs::set_permissions(&copy, fs::Permissions::from_mode(0o644)));
                if let Err(e) = copied {
                    return Verdict::Fail(format!("copy {}: {e}", source.display()));
                }
                Some(copy.display().to_string())
            }
            None => None,
        };
        let swtpm = match &test.measured {
            Some(measured) => match crate::swtpm::Swtpm::start(&measured.tpm) {
                Ok(swtpm) => Some(swtpm),
                Err(e) => return Verdict::Fail(format!("swtpm: {e}")),
            },
            None => None,
        };
        let socket = swtpm.as_ref().map(crate::swtpm::Swtpm::socket);
        let launch = crate::qemu::Launch {
            disk: disk.as_deref(),
            smp: test.smp.as_deref(),
            tpm: socket.as_deref(),
            ..Default::default()
        };
        let command = crate::qemu::command(&built.recipe, &built.image, &built.external, &launch);
        let outcome = match command {
            Ok(command) => {
                let deadline = test.timeout.map_or(self.boot_timeout, Duration::from_secs);
                supervise(command, log, deadline, Some((&test.expect, self.quiet)))
            }
            Err(e) => return Verdict::Fail(e),
        };
        if let Some(swtpm) = swtpm {
            swtpm.keep_logs(log);
        }
        if disk.is_some() {
            let _ = fs::remove_file(log.with_extension("disk"));
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => return Verdict::Fail(format!("run: {e}")),
        };
        let output = fs::read(log).unwrap_or_default();
        match judge(test, &outcome, &String::from_utf8_lossy(&output)) {
            Ok(Some(report)) => {
                let _ = fs::write(log.with_extension("tpm"), report);
                Verdict::Pass
            }
            Ok(None) => Verdict::Pass,
            Err(why) => fail_with_log(&why, log),
        }
    }
}

/// Whether `output` of a boot that ended in `outcome` passes `test`; the
/// TPM report of a measured boot.
fn judge(test: &BootTest, outcome: &Outcome, output: &str) -> Result<Option<String>, String> {
    let missing: Vec<_> = test
        .expect
        .iter()
        .filter(|line| !output.contains(line.as_str()))
        .collect();
    if !missing.is_empty() {
        return Err(format!("{outcome}; missing {missing:?}"));
    }
    if let Outcome::Exited(status) = outcome
        && !status.success()
    {
        return Err(outcome.to_string());
    }
    let rejected: Vec<_> = test
        .reject
        .iter()
        .filter(|line| output.contains(line.as_str()))
        .collect();
    if !rejected.is_empty() {
        return Err(format!("printed {rejected:?}"));
    }
    let Some(measured) = &test.measured else {
        return Ok(None);
    };
    let banks: Vec<_> = measured.tpm.banks.iter().map(|bank| alg(*bank)).collect();
    crate::tpm_log::verify(output, &measured.events, Some(&banks))
        .map(Some)
        .map_err(|e| format!("TPM log: {e}"))
}

/// The QEMU runner's judgment; tests not selected are reported by the CLI.
#[derive(Debug)]
pub enum Verdict {
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

/// Run `command` with its output in `log`, until it exits, `deadline`
/// passes, or, given `(expect, quiet)`, every `expect` line is in the log and
/// it has not grown for `quiet`.
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
        .spawn()?;
    let start = Instant::now();
    let (mut size, mut grown) = (0, Instant::now());
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome::Exited(status));
        }
        if start.elapsed() >= deadline {
            break Outcome::TimedOut;
        }
        if let Some((expect, quiet)) = done {
            let now = fs::metadata(log)?.len();
            if now != size {
                (size, grown) = (now, Instant::now());
            } else if grown.elapsed() >= quiet {
                let text = fs::read(log)?;
                let text = String::from_utf8_lossy(&text);
                if expect.iter().all(|line| text.contains(line.as_str())) {
                    break Outcome::Done;
                }
            }
        }
        thread::sleep(POLL);
    };
    child.kill()?;
    child.wait()?;
    Ok(outcome)
}

#[cfg(test)]
#[path = "boot_test_tests.rs"]
mod tests;
