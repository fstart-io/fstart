//! QEMU launcher for testing.

use std::path::Path;
use std::process::Command;

/// Run firmware in QEMU.
pub fn run(board_name: &str, binary: &Path) -> Result<(), String> {
    let (qemu_bin, args) = match board_name {
        name if name.contains("riscv64") => (
            "qemu-system-riscv64",
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-nographic".to_string(),
                "-bios".to_string(),
                binary.display().to_string(),
            ],
        ),
        name if name.contains("aarch64") => (
            "qemu-system-aarch64",
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-cpu".to_string(),
                "cortex-a72".to_string(),
                "-nographic".to_string(),
                "-bios".to_string(),
                binary.display().to_string(),
            ],
        ),
        _ => return Err(format!("no QEMU configuration for board: {board_name}")),
    };

    eprintln!("[fstart] launching: {qemu_bin} {}", args.join(" "));

    let status = Command::new(qemu_bin)
        .args(&args)
        .status()
        .map_err(|e| format!("failed to launch QEMU: {e}"))?;

    if !status.success() {
        return Err("QEMU exited with error".to_string());
    }

    Ok(())
}
