//! QEMU launcher for testing.

use fstart_types::Platform;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Find a QEMU binary by name.
///
/// Search order:
/// 1. `$PATH` (standard lookup)
/// 2. `/nix/store/*/bin/<name>` (NixOS systems where QEMU isn't on PATH)
fn find_qemu(name: &str) -> String {
    // Try PATH first.
    if let Ok(output) = Command::new("which").arg(name).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }

    // Search nix store.
    if let Ok(entries) = std::fs::read_dir("/nix/store") {
        for entry in entries.flatten() {
            let candidate = entry.path().join("bin").join(name);
            if candidate.is_file() {
                return candidate.display().to_string();
            }
        }
    }

    // Fall back to bare name (will fail with a clear error).
    name.to_string()
}

/// Run firmware in QEMU.
///
/// For both monolithic and FFS builds, `binary` is a flat binary (raw
/// firmware or FFS image). RISC-V uses pflash to load it at flash base
/// (0x20000000, XIP); AArch64 uses `-bios` to load it at flash (0x0, XIP).
/// SBSA uses two pflash images: pflash0 (TFA) and pflash1 (fstart).
///
/// `board_name` selects the QEMU machine: `"sifive-unmatched"` uses the
/// `sifive_u` machine (FU740 emulation with 5 harts); all other RISC-V
/// boards use the generic `virt` machine.
pub fn run(board_name: &str, platform: Platform, binary: &Path) -> Result<(), String> {
    let (qemu_bin, args) = if board_name.contains("sbsa") {
        // SBSA-ref: TF-A runs first from pflash0 (secure flash at 0x0),
        // then launches fstart as BL33 from pflash1 (non-secure flash
        // at 0x10000000). Both pflash images must be exactly 256 MiB.
        //
        // TF-A binaries (bl1.bin + fip.bin) are expected in the board
        // directory. Build them with:
        //   make PLAT=qemu_sbsa all fip ARM_LINUX_KERNEL_AS_BL33=1
        let pflash_size = 256 * 1024 * 1024; // 256 MiB

        // pflash0 = TF-A (secure flash)
        let tfa_path = find_tfa_flash(binary, pflash_size)?;

        // pflash1 = fstart firmware (non-secure flash)
        let fstart_pflash = create_pflash_image(binary, pflash_size)?;

        let args = vec![
            "-machine".to_string(),
            "sbsa-ref".to_string(),
            "-m".to_string(),
            "1G".to_string(),
            "-nographic".to_string(),
            "-pflash".to_string(),
            tfa_path.display().to_string(),
            "-pflash".to_string(),
            fstart_pflash.display().to_string(),
        ];
        (find_qemu("qemu-system-aarch64"), args)
    } else {
        match platform {
            Platform::Riscv64 => {
                // Select QEMU machine based on board name.
                // sifive-unmatched uses sifive_u (FU740: 5 harts, SiFive UART).
                // All other RISC-V boards use the generic virt machine.
                if board_name == "sifive-unmatched" {
                    // sifive_u: use -bios to load fstart firmware. QEMU's
                    // boot ROM at 0x1000 sets a0=hartid, a1=DTB, then jumps
                    // to 0x80000000 where our firmware is loaded.
                    //
                    // The firmware binary is loaded at DRAM base (0x80000000)
                    // by QEMU's -bios option.
                    let args = vec![
                        "-machine".to_string(),
                        "sifive_u".to_string(),
                        "-m".to_string(),
                        "1G".to_string(),
                        "-nographic".to_string(),
                        "-bios".to_string(),
                        binary.display().to_string(),
                    ];
                    (find_qemu("qemu-system-riscv64"), args)
                } else {
                    // RISC-V virt: load firmware into pflash bank 0 at 0x20000000
                    // (XIP).  The MROM trampoline at 0x1000 sets a0=mhartid,
                    // a1=DTB addr, then jumps to flash base.
                    //
                    // Flash bank 0 is 32 MiB; QEMU pflash requires the backing
                    // image to match exactly, so we pad the firmware binary with
                    // 0xFF (erased NOR flash state).
                    let pflash_size = 32 * 1024 * 1024; // 32 MiB — VIRT_FLASH / 2
                    let pflash_path = create_pflash_image(binary, pflash_size)?;

                    let args = vec![
                        "-machine".to_string(),
                        "virt".to_string(),
                        "-nographic".to_string(),
                        "-bios".to_string(),
                        "none".to_string(),
                        "-drive".to_string(),
                        format!("if=pflash,file={},format=raw,unit=0", pflash_path.display()),
                    ];
                    (find_qemu("qemu-system-riscv64"), args)
                }
            }
            Platform::Aarch64 => {
                let mut args = vec![
                    "-machine".to_string(),
                    // secure=on: enable TrustZone so secure SRAM at 0x0E000000 exists
                    //   (BL31 is loaded there)
                    // virtualization=on: enable EL2 so BL31 can ERET to Linux at EL2
                    // gic-version=3: ATF BL31 is built with QEMU_USE_GIC_DRIVER=QEMU_GICV3
                    "virt,secure=on,virtualization=on,gic-version=3".to_string(),
                    // Use max CPU so all ARMv8/v9 extensions are available —
                    // avoids SIGILL from userspace built with newer features.
                    "-cpu".to_string(),
                    "max".to_string(),
                    "-nographic".to_string(),
                ];
                // AArch64: always use -bios so QEMU enters firmware boot mode,
                // which places the DTB at RAM base (0x40000000) and starts the
                // CPU at PC=0x0. Works for both ELF and raw FFS images.
                args.extend(["-bios".to_string(), binary.display().to_string()]);
                (find_qemu("qemu-system-aarch64"), args)
            }
            Platform::Armv7 => {
                let args = vec![
                    "-machine".to_string(),
                    "virt".to_string(),
                    "-cpu".to_string(),
                    "cortex-a15".to_string(),
                    "-nographic".to_string(),
                    // ARMv7: always use -bios so QEMU enters firmware boot mode,
                    // which places the DTB at RAM base (0x40000000) and starts the
                    // CPU at PC=0x0. Same as AArch64.
                    "-bios".to_string(),
                    binary.display().to_string(),
                ];
                (find_qemu("qemu-system-arm"), args)
            }
        }
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

/// Locate or build the TF-A secure flash image for SBSA.
///
/// Looks for pre-built `bl1.bin` and `fip.bin` in the board directory,
/// combines them into a single 256 MiB pflash0 image. BL1 goes at offset
/// 0x0 and FIP at offset 0x12000 (matching TF-A's `qemu_sbsa` platform
/// layout).
///
/// Falls back to checking for a pre-assembled `tfa.bin` in the board
/// directory if individual BL files are missing.
fn find_tfa_flash(fstart_binary: &Path, flash_size: usize) -> Result<PathBuf, String> {
    // The board directory is two levels up from the binary (target/.../*.bin)
    // but we can also find it via the workspace.
    let board_dir = find_board_dir(fstart_binary)?;

    // Check for pre-assembled tfa.bin first
    let tfa_bin = board_dir.join("tfa.bin");
    if tfa_bin.exists() {
        let data = std::fs::read(&tfa_bin)
            .map_err(|e| format!("failed to read {}: {e}", tfa_bin.display()))?;
        if data.len() == flash_size {
            eprintln!("[fstart] TF-A flash: {} (pre-assembled)", tfa_bin.display());
            return Ok(tfa_bin);
        }
        // Pad to flash size
        return create_pflash_image(&tfa_bin, flash_size);
    }

    // Assemble from bl1.bin + fip.bin
    let bl1_path = board_dir.join("bl1.bin");
    let fip_path = board_dir.join("fip.bin");

    if !bl1_path.exists() || !fip_path.exists() {
        return Err(format!(
            "TF-A binaries not found for SBSA board.\n\
             Expected one of:\n  \
               {}\n  \
               {} + {}\n\n\
             Build TF-A with:\n  \
               cd <trusted-firmware-a>\n  \
               make PLAT=qemu_sbsa all fip ARM_LINUX_KERNEL_AS_BL33=1\n  \
               cp build/qemu_sbsa/release/bl1.bin {}\n  \
               cp build/qemu_sbsa/release/fip.bin {}",
            tfa_bin.display(),
            bl1_path.display(),
            fip_path.display(),
            board_dir.display(),
            board_dir.display(),
        ));
    }

    let bl1 = std::fs::read(&bl1_path)
        .map_err(|e| format!("failed to read {}: {e}", bl1_path.display()))?;
    let fip = std::fs::read(&fip_path)
        .map_err(|e| format!("failed to read {}: {e}", fip_path.display()))?;

    // TF-A qemu_sbsa layout: BL1 at 0x0, FIP at 0x12000
    let fip_offset = 0x12000usize;
    if bl1.len() > fip_offset {
        return Err(format!(
            "bl1.bin ({} bytes) is too large for FIP offset {:#x}",
            bl1.len(),
            fip_offset,
        ));
    }
    if fip_offset + fip.len() > flash_size {
        return Err(format!(
            "bl1.bin + fip.bin exceeds flash size ({:#x})",
            flash_size,
        ));
    }

    let mut pflash = vec![0xFFu8; flash_size];
    pflash[..bl1.len()].copy_from_slice(&bl1);
    pflash[fip_offset..fip_offset + fip.len()].copy_from_slice(&fip);

    let out_path = board_dir.join("tfa.pflash");
    std::fs::write(&out_path, &pflash).map_err(|e| format!("failed to write TF-A pflash: {e}"))?;

    eprintln!(
        "[fstart] TF-A pflash: {} (bl1={} bytes, fip={} bytes at {:#x})",
        out_path.display(),
        bl1.len(),
        fip.len(),
        fip_offset,
    );

    Ok(out_path)
}

/// Find the board directory from a binary path.
///
/// Walks up from the binary looking for the workspace root, then resolves
/// the board name from the binary path or the binary's parent directories.
fn find_board_dir(binary: &Path) -> Result<PathBuf, String> {
    // Walk up looking for workspace Cargo.toml
    let mut dir = binary
        .parent()
        .ok_or_else(|| "no parent directory for binary".to_string())?
        .to_path_buf();

    let workspace_root = loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                break dir;
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root from binary path".to_string());
        }
    };

    // Find the board name from the FSTART_BOARD_RON env or scan boards/
    // For SBSA, the board directory is boards/qemu-sbsa/
    let boards_dir = workspace_root.join("boards");
    for entry in
        std::fs::read_dir(&boards_dir).map_err(|e| format!("failed to read boards/: {e}"))?
    {
        let entry = entry.map_err(|e| format!("readdir error: {e}"))?;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Match SBSA board by checking if binary path contains the board name
            if name_str.contains("sbsa")
                && binary.to_string_lossy().contains(&format!("{name_str}"))
            {
                return Ok(entry.path());
            }
        }
    }

    // Fallback: use the first sbsa board directory
    for entry in
        std::fs::read_dir(&boards_dir).map_err(|e| format!("failed to read boards/: {e}"))?
    {
        let entry = entry.map_err(|e| format!("readdir error: {e}"))?;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let name = entry.file_name();
            if name.to_string_lossy().contains("sbsa") {
                return Ok(entry.path());
            }
        }
    }

    Err("could not find SBSA board directory".to_string())
}

/// Create a QEMU pflash image by padding a firmware binary to the exact
/// flash bank size. Padding uses 0xFF (the erased state of NOR flash).
fn create_pflash_image(binary: &Path, flash_size: usize) -> Result<PathBuf, String> {
    let data = std::fs::read(binary).map_err(|e| format!("failed to read firmware binary: {e}"))?;

    if data.len() > flash_size {
        return Err(format!(
            "firmware binary ({} bytes) exceeds flash bank size ({} bytes)",
            data.len(),
            flash_size
        ));
    }

    let mut pflash = vec![0xFFu8; flash_size];
    pflash[..data.len()].copy_from_slice(&data);

    let pflash_path = binary.with_extension("pflash");
    std::fs::write(&pflash_path, &pflash)
        .map_err(|e| format!("failed to write pflash image: {e}"))?;

    eprintln!(
        "[fstart] pflash image: {} ({} bytes, firmware {} bytes)",
        pflash_path.display(),
        flash_size,
        data.len()
    );

    Ok(pflash_path)
}
