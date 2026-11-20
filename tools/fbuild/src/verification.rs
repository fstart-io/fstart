//! End-of-command reports for the firmware CI steps and local runs.

use crate::manifest_for;
use fbuild::boot_test::{Runner, Verdict};
use fbuild::{board_manifest, build_board, resolved_image::ResolvedImage};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

/// Render the same table in the terminal and in the GitHub job summary. A
/// reporting error must neither hide a test failure nor fail a passing run.
struct Summary(String);

impl Summary {
    fn new(title: &str, columns: &[&str]) -> Self {
        let mut report = Self(format!(
            "\n### {title}\n\nNo physical hardware was tested.\n\n"
        ));
        report.row(columns);
        report.row(&vec!["---"; columns.len()]);
        report
    }

    fn row(&mut self, cells: &[&str]) {
        self.0.push('|');
        for cell in cells {
            let cell = cell
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\\', "&#92;")
                .replace('`', "&#96;")
                .replace('[', "&#91;")
                .replace('|', "&#124;")
                .replace(['\r', '\n'], " ");
            self.0.push_str(&format!(" {cell} |"));
        }
        self.0.push('\n');
    }

    fn print(self) {
        println!("{}", self.0);
        if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY")
            && let Err(e) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| writeln!(file, "{}", self.0))
        {
            eprintln!("warning: cannot write GITHUB_STEP_SUMMARY: {e}");
        }
    }
}

/// Link the selected boards and run their host library tests in order.
pub fn build_boards(boards: &[String], size_dir: Option<PathBuf>) -> Result<(), String> {
    let mut summary = Summary::new(
        "Release builds and host library tests",
        &[
            "Board",
            "Release link",
            "Payload",
            "Completed units (target; outputs)",
            "Host library test command",
            "Stage sizes",
        ],
    );
    let size_dir = size_dir
        .map(|dir| {
            std::fs::create_dir_all(&dir)
                .and_then(|()| std::path::absolute(&dir))
                .map_err(|e| format!("--size-dir {}: {e}", dir.display()))
        })
        .transpose();
    let size_dir = match size_dir {
        Ok(dir) => dir,
        Err(e) => {
            for board in boards {
                summary.row(&[board, "Not run", "—", "—", "Not run", &e]);
            }
            summary.print();
            return Err(e);
        }
    };
    let github = std::env::var_os("GITHUB_ACTIONS").is_some();
    let mut failed = Vec::new();
    for board in boards {
        if github {
            println!("::group::{board}");
        }
        let start = Instant::now();
        let mut units = Vec::new();
        let mut payload = "—".to_string();
        let mut link = "Not run".into();
        let mut host = "Not run".into();
        let mut sizes = size_dir
            .as_ref()
            .map_or("Not requested", |_| "Not run")
            .to_string();
        let result = (|| {
            let manifest = manifest_for(board).inspect_err(|e| link = format!("Not run: {e}"))?;
            let root = build_board::workspace_root_pub()
                .inspect_err(|e| link = format!("Not run: {e}"))?;
            let selection = fbuild::payload::BuildArgs::default().selection(None);
            let resolved = ResolvedImage::load(&root, &manifest, selection)
                .inspect_err(|e| link = format!("Not run: resolve plan: {e}"))?;
            payload = resolved.payload().into();
            let build = resolved
                .build_with_units(&root, &manifest, true, |unit, artifacts| {
                    let kinds = artifacts.keys().map(String::as_str).collect::<Vec<_>>();
                    units.push(format!(
                        "{} ({}; {})",
                        unit.name,
                        unit.target,
                        kinds.join(", ")
                    ));
                })
                .inspect_err(|e| link = format!("Fail: {e}"))?;
            link = "Pass".into();
            if let Some(dir) = &size_dir {
                let filename = format!("stage-sizes-{board}.jsonl");
                fbuild::sizes::append(&dir.join(&filename), &manifest.board, &build.stages)
                    .inspect_err(|e| sizes = format!("Fail: {e}"))?;
                sizes = format!("Written: {filename}");
            }
            fbuild::board_tool::test_library(&manifest)
                .inspect_err(|e| host = format!("Fail: {e}"))?;
            host = "Pass".into();
            Ok::<_, String>(())
        })();
        if github {
            println!("::endgroup::");
        }
        let seconds = start.elapsed().as_secs();
        match result {
            Ok(()) => println!("PASS {board} ({seconds}s)"),
            Err(e) => {
                println!("FAIL {board} ({seconds}s): {e}");
                if github {
                    println!("::error title={board}::{e}");
                }
                failed.push(board.as_str());
            }
        }
        let units = if units.is_empty() {
            "—".into()
        } else {
            units.join("; ")
        };
        summary.row(&[board, &link, &payload, &units, &host, &sizes]);
    }
    summary.0.push_str(concat!(
        "\nSee the command log (the **Build and test boards** step in CI) for compiler diagnostics ",
        "and host test counts, which may be zero. Local artifacts and Cargo JSON logs are under ",
        "`target/fstart-build/<board>/release/`. This command does not run QEMU.\n",
    ));
    summary.print();
    println!(
        "{} of {} boards passed",
        boards.len() - failed.len(),
        boards.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed: {}", failed.join(" ")))
    }
}

/// List, run, or report why the selected boards' QEMU scenarios did not run.
pub fn boot_test(
    boards: &[String],
    tests: &[String],
    assets: Option<PathBuf>,
    logs: Option<PathBuf>,
    list: bool,
    not_run: Option<&str>,
) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let mut summary = Summary::new(
        "QEMU boot scenarios",
        &["Board", "Scenario", "Payload", "Result", "Logs"],
    );
    let selected = if boards.is_empty() {
        board_manifest::discover(&root)?
            .into_iter()
            .filter(|board| board.build_profile.is_some())
            .collect()
    } else {
        let manifests: Vec<_> = boards.iter().map(|board| manifest_for(board)).collect();
        if let Some(err) = manifests.iter().find_map(|board| board.as_ref().err()) {
            // Explicit selections are validated before any boot, as in --list.
            if !list {
                for (id, manifest) in boards.iter().zip(&manifests) {
                    let reason = manifest
                        .as_ref()
                        .err()
                        .map_or("invalid board selection", String::as_str);
                    summary.row(&[id, "—", "—", &format!("Not run: {reason}"), "—"]);
                }
                summary.print();
            }
            return Err(err.clone());
        }
        manifests.into_iter().collect::<Result<Vec<_>, _>>()?
    };
    let runner = Runner {
        assets: assets
            .map(|dir| std::path::absolute(&dir).map_err(|e| format!("--assets: {e}")))
            .transpose()?,
        logs: logs.unwrap_or_else(|| root.join("target/boot-tests")),
        ..Default::default()
    };
    let mut failures = 0;
    let mut plan_errors = 0;
    let mut setup_error = None;
    for board in &selected {
        let id = &board.board;
        if let Some(e) = &setup_error {
            summary.row(&[id, "—", "—", &format!("Not run: {e}"), "—"]);
            continue;
        }
        let declared = match fbuild::boot_test::load(&root, board) {
            Ok(declared) => declared,
            Err(e) => {
                if let Some(reason) = not_run {
                    summary.row(&[
                        id,
                        "—",
                        "—",
                        &format!("Not run: {reason}; could not load boot plan: {e}"),
                        "—",
                    ]);
                } else {
                    println!("FAIL {id} ({e})");
                    summary.row(&[
                        id,
                        "—",
                        "—",
                        &format!("Not run: could not load boot plan: {e}"),
                        "—",
                    ]);
                    plan_errors += 1;
                }
                continue;
            }
        };
        let chosen: Vec<_> = declared
            .iter()
            .filter(|test| tests.is_empty() || tests.contains(&test.name))
            .collect();
        if list {
            for test in chosen {
                println!("{id} {}", test.name);
            }
            continue;
        }
        let mut results = if not_run.is_some() {
            Vec::new().into_iter()
        } else {
            match runner.run(&root, board, &chosen) {
                Ok(results) => results.into_iter(),
                Err(e) => {
                    setup_error = Some(e);
                    Vec::new().into_iter()
                }
            }
        };
        if declared.is_empty() {
            let reason = setup_error
                .as_deref()
                .unwrap_or("plan declares no QEMU boot scenarios");
            summary.row(&[id, "—", "—", &format!("Not run: {reason}"), "—"]);
        }
        for test in &declared {
            let (status, logs) = if !tests.is_empty() && !tests.contains(&test.name) {
                ("Not run: excluded by --test".into(), "—".into())
            } else if let Some(reason) = not_run.or(setup_error.as_deref()) {
                (format!("Not run: {reason}"), "—".into())
            } else {
                let result = results.next().expect("one result per selected test");
                let status = match result.verdict {
                    Verdict::Pass => "Pass".into(),
                    Verdict::Skip(why) => format!("Skipped: {why}"),
                    Verdict::Fail(why) => {
                        failures += 1;
                        format!("Fail: {why}")
                    }
                };
                let logs = result
                    .logs
                    .iter()
                    .filter_map(|path| path.file_name())
                    .map(|name| name.to_string_lossy())
                    .collect::<Vec<_>>();
                (
                    status,
                    if logs.is_empty() {
                        "—".into()
                    } else {
                        logs.join(", ")
                    },
                )
            };
            summary.row(&[id, &test.name, &test.payload, &status, &logs]);
        }
    }
    if !list {
        let logs = std::path::absolute(&runner.logs).unwrap_or(runner.logs);
        summary.0.push_str(&format!("\nLogs listed above are relative to `{}`. In CI, look for the **Boot test logs** artifact link below when logs were uploaded.\n", logs.display()));
        summary.print();
    }
    if let Some(e) = setup_error {
        return Err(e);
    }
    match (failures, plan_errors) {
        (0, 0) => Ok(()),
        (n, 0) => Err(format!("{n} boot test(s) failed")),
        (0, n) => Err(format!("{n} boot plan(s) could not be loaded")),
        (n, p) => Err(format!(
            "{n} boot test(s) failed; {p} boot plan(s) could not be loaded"
        )),
    }
}
