// SPDX-License-Identifier: MIT OR Apache-2.0
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ffmt-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, mode: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ffmt"))
            .arg(mode)
            .arg(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

const PACKED: &str =
    "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];";

#[test]
fn check_fix_and_recheck() {
    let dir = Directory::new();
    let path = dir.0.join("registers.rs");
    std::fs::write(&path, PACKED).unwrap();
    assert!(!dir.run("--check").status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PACKED);
    assert!(dir.run("--fix").status.success());
    assert!(dir.run("--check").status.success());
    let once = std::fs::read(&path).unwrap();
    assert!(dir.run("--fix").status.success());
    assert_eq!(std::fs::read(&path).unwrap(), once);
}

#[test]
fn refusal_leaves_the_entire_file_unchanged() {
    let dir = Directory::new();
    let src = format!(
        "{PACKED}\nregister_bitfields![u32, S [A OFFSET(0) NUMBITS(1) [], /* ambiguous */ B OFFSET(1) NUMBITS(1) []]];"
    );
    let path = dir.0.join("registers.rs");
    std::fs::write(&path, &src).unwrap();
    assert!(!dir.run("--fix").status.success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), src);
}

#[test]
fn nonexistent_path_is_an_error() {
    let dir = Directory::new();
    let out = Command::new(env!("CARGO_BIN_EXE_ffmt"))
        .arg("--check")
        .arg(dir.0.join("missing.rs"))
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[cfg(unix)]
#[test]
fn skips_symlinked_files_directories_and_cycles() {
    use std::os::unix::fs::symlink;
    let dir = Directory::new();
    let outside = Directory::new();
    let path = outside.0.join("registers.rs");
    std::fs::write(&path, PACKED).unwrap();
    symlink(&outside.0, dir.0.join("external")).unwrap();
    symlink(&path, dir.0.join("file.rs")).unwrap();
    symlink(&dir.0, dir.0.join("cycle")).unwrap();
    assert!(dir.run("--check").status.success());
    assert!(dir.run("--fix").status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PACKED);
    let out = Command::new(env!("CARGO_BIN_EXE_ffmt"))
        .arg("--fix")
        .arg(dir.0.join("file.rs"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), PACKED);
}
