//! QEMU launcher for testing.

use std::path::Path;
use std::process::Command;

/// Run firmware in QEMU.
///
/// For monolithic builds, `binary` is the ELF — QEMU loads it via `-bios`.
/// For multi-stage FFS images, `binary` is the raw FFS image — we use
/// `-device loader` to load it at the correct memory address.
pub fn run(board_name: &str, binary: &Path) -> Result<(), String> {
    let ext = binary.extension().and_then(|e| e.to_str()).unwrap_or("");
    let is_ffs = ext == "ffs";

    let (qemu_bin, args) = match board_name {
        name if name.contains("riscv64") => {
            let mut args = vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-nographic".to_string(),
            ];
            if is_ffs {
                // FFS image: load as raw binary at the flash base address.
                // The bootblock is at offset 0 of the image, and QEMU's virt
                // machine starts executing at 0x80000000.
                // Use -bios none and -device loader to place the raw image.
                args.extend([
                    "-bios".to_string(),
                    "none".to_string(),
                    "-device".to_string(),
                    format!(
                        "loader,file={},addr=0x80000000,force-raw=on",
                        binary.display()
                    ),
                ]);
            } else {
                // ELF: QEMU parses and loads it at the correct addresses.
                args.extend(["-bios".to_string(), binary.display().to_string()]);
            }
            ("qemu-system-riscv64", args)
        }
        name if name.contains("aarch64") => {
            let mut args = vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-cpu".to_string(),
                "cortex-a72".to_string(),
                "-nographic".to_string(),
            ];
            if is_ffs {
                args.extend([
                    "-bios".to_string(),
                    "none".to_string(),
                    "-device".to_string(),
                    format!(
                        "loader,file={},addr=0x00000000,force-raw=on",
                        binary.display()
                    ),
                ]);
            } else {
                args.extend(["-bios".to_string(), binary.display().to_string()]);
            }
            ("qemu-system-aarch64", args)
        }
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
