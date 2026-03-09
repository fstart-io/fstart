//! QEMU launcher for testing.

use fstart_types::Platform;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run firmware in QEMU.
///
/// For both monolithic and FFS builds, `binary` is a flat binary (raw
/// firmware or FFS image). RISC-V uses pflash to load it at flash base
/// (0x20000000, XIP); AArch64 uses `-bios` to load it at flash (0x0, XIP).
pub fn run(platform: Platform, binary: &Path) -> Result<(), String> {
    let (qemu_bin, args) = match platform {
        Platform::Riscv64 => {
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
            ("qemu-system-riscv64", args)
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
            ("qemu-system-aarch64", args)
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
            ("qemu-system-arm", args)
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
