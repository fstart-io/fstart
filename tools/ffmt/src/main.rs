// SPDX-License-Identifier: MIT OR Apache-2.0
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Check (or rewrite) the layout of `register_bitfields!` bodies: one bitfield
/// per line, one enum variant per line.
#[derive(Parser)]
struct Args {
    /// Rewrite the files in place instead of reporting.
    #[arg(long)]
    fix: bool,
    /// Report only; this is the default.
    #[arg(long)]
    check: bool,
    /// Files or directories to scan; defaults to the current directory.
    paths: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let roots = if args.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.paths.clone()
    };
    let mut files = Vec::new();
    let mut errors = 0;
    let mut seen = std::collections::HashSet::new();
    for root in &roots {
        if let Err(e) = collect(root, &mut seen, &mut files) {
            eprintln!("{}: {e}", root.display());
            errors += 1;
        }
    }
    files.sort();
    files.dedup();
    let mut found = 0;
    let mut rewritten = 0;
    for file in &files {
        let src = match std::fs::read_to_string(file) {
            Ok(src) => src,
            Err(e) => {
                // A file we cannot read is a check we did not perform.
                eprintln!("{}: {e}", file.display());
                errors += 1;
                continue;
            }
        };
        if !src.contains(ffmt::MACRO_NAME) {
            continue;
        }
        if args.fix {
            match ffmt::fix_source(&src) {
                Ok(Some(new)) => {
                    if !ffmt::check_source(&new).is_empty() {
                        eprintln!("{}: reflow left violations; not written", file.display());
                        found += 1;
                        continue;
                    }
                    match std::fs::write(file, new) {
                        Ok(()) => {
                            rewritten += 1;
                            println!("rewrote {}", file.display());
                        }
                        Err(e) => {
                            eprintln!("{}: {e}", file.display());
                            errors += 1;
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("{}: {e}", file.display());
                    found += 1;
                }
            }
            continue;
        }
        for violation in ffmt::check_source(&src) {
            println!(
                "{}:{}: {}",
                file.display(),
                violation.line,
                violation.message
            );
            found += 1;
        }
    }
    if args.fix {
        println!("{rewritten} file(s) rewritten, {found} left alone");
    } else if found > 0 {
        eprintln!("{found} register layout violation(s); `cargo ffmt --fix` rewrites them");
    }
    if found > 0 || errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn collect(path: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if !meta.is_dir() {
        return Err("not a file or a directory".into());
    }
    // A symlinked directory can point at one of its own parents; visiting each
    // physical directory once keeps a cycle from scanning the same file over
    // and over.
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key) {
        return Ok(());
    }
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "node_modules") {
                collect(&path, seen, out)?;
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}
