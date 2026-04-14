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
pub fn run(
    board_name: &str,
    platform: Platform,
    binary: &Path,
    disk: Option<&str>,
    memory: Option<&str>,
) -> Result<(), String> {
    let (qemu_bin, mut args) = if board_name.contains("sbsa") {
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
                    // secure=on: needed so EL3 exists (CrabEFI's RNG does SMC)
                    // virtualization=on: EL2 exists (standard for UEFI)
                    // gic-version=3: GICv3 initialized by the GicInit capability
                    //   which issues SMC FSTART_GIC_INIT to configure GICD/GICR
                    //   from EL3 (addresses from board RON `gic` config).
                    "virt,secure=on,virtualization=on,gic-version=3".to_string(),
                    // cortex-a72: ARMv8.0 without FEAT_S1PIE and other
                    // ARMv9 extensions that trap from Secure EL1 to EL3.
                    // -cpu max enables FEAT_S1PIE whose PIRE0_EL1 register
                    // writes trap to EL3 in a tight loop, stalling the
                    // Linux kernel.  cortex-a72 avoids this and is the
                    // standard QEMU virt CPU for ARM64 firmware testing.
                    "-cpu".to_string(),
                    "cortex-a72".to_string(),
                    "-nographic".to_string(),
                ];
                // AArch64: always use -bios so QEMU enters firmware boot mode,
                // which places the DTB at RAM base (0x40000000) and starts the
                // CPU at PC=0x0. Works for both ELF and raw FFS images.
                args.extend(["-bios".to_string(), binary.display().to_string()]);
                // Bochs VBE display — non-VGA PCI device (class 0x0380) with
                // MMIO registers in BAR2. No legacy VGA I/O ports needed.
                args.extend(["-device".to_string(), "bochs-display".to_string()]);
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
            Platform::X86_64 => {
                // x86 Q35: load firmware as pflash (flash ROM at top of 4GB).
                // QEMU Q35 maps pflash0 at the top of the 32-bit address space
                // with the reset vector at 0xFFFFFFF0.
                let pflash_size = 8 * 1024 * 1024; // 8 MiB flash
                                                   // x86: the raw stage .bin has boot code at the correct
                                                   // offsets within 8MB (reset vector at end, code at start).
                                                   // The FFS image has the stage segments + kernel payload.
                                                   // We overlay the FFS onto the raw binary so both the boot
                                                   // code (end of flash) and FFS data (start of flash) are
                                                   // present.
                                                   //
                                                   // binary = target/ffs/qemu-q35.ffs
                                                   // stage_bin = target/x86_64-unknown-none/{debug,release}/fstart-stage.bin
                let workspace = binary.parent().unwrap().parent().unwrap();
                let profile = if binary.to_str().unwrap_or("").contains("release")
                    || std::env::args().any(|a| a == "--release")
                {
                    "release"
                } else {
                    "debug"
                };
                let stage_bin = workspace
                    .join("x86_64-unknown-none")
                    .join(profile)
                    .join("fstart-stage.bin");
                let pflash_path = if stage_bin.exists() {
                    create_x86_pflash(binary, &stage_bin, pflash_size)?
                } else {
                    eprintln!(
                        "[fstart] warning: stage .bin not found at {}, using FFS directly",
                        stage_bin.display()
                    );
                    create_pflash_image_aligned(binary, pflash_size, true)?
                };

                // Try KVM first, fall back to TCG if /dev/kvm is absent.
                // -cpu max: under KVM exposes host features; under TCG
                // enables all emulated features.
                // KVM requires -bios (ROM mapping via EPT). pflash is backed
                // by a block device whose MMIO semantics prevent KVM from
                // fetching instructions — the ljmpl from the boot block to
                // stage code at 0xFF800000 hangs. TCG software-emulates
                // everything so pflash works fine there.
                let use_kvm = std::fs::File::open("/dev/kvm").is_ok();
                let mut args = vec![
                    "-machine".to_string(),
                    "q35".to_string(),
                    "-accel".to_string(),
                    if use_kvm { "kvm" } else { "tcg" }.to_string(),
                    "-cpu".to_string(),
                    if use_kvm { "host" } else { "max" }.to_string(),
                    "-m".to_string(),
                    "1G".to_string(),
                    "-nographic".to_string(),
                    if use_kvm { "-bios" } else { "-drive" }.to_string(),
                    if use_kvm {
                        pflash_path.display().to_string()
                    } else {
                        format!("if=pflash,format=raw,file={}", pflash_path.display())
                    },
                    // ISA debugcon for early debug output
                    "-device".to_string(),
                    "isa-debugcon,iobase=0x402,chardev=debugout".to_string(),
                    "-chardev".to_string(),
                    "file,id=debugout,path=/dev/stderr".to_string(),
                    // Bochs VBE display — non-VGA PCI device (class 0x0380)
                    // with MMIO registers in BAR2. Only added when the
                    // board RON declares a BochsDisplay child device.
                    // Q35 has a built-in VGA; use -vga none to avoid
                    // conflicts, then add bochs-display explicitly.
                    //
                    // TODO: conditionally add based on board RON devices.
                    // For now, always add it.
                    "-vga".to_string(),
                    "none".to_string(),
                    "-device".to_string(),
                    "bochs-display".to_string(),
                ];
                (find_qemu("qemu-system-x86_64"), args)
            }
        }
    };

    // Add RAM if specified
    if let Some(mem) = memory {
        args.extend(["-m".to_string(), mem.to_string()]);
    }

    // Attach disk image as NVMe (CrabEFI has an NVMe driver)
    if let Some(disk_path) = disk {
        let fmt = if disk_path.ends_with(".qcow2") {
            "qcow2"
        } else {
            "raw"
        };
        args.extend([
            "-drive".to_string(),
            format!("file={disk_path},id=hd0,if=none,format={fmt}"),
            "-device".to_string(),
            "nvme,serial=fstartdisk0,drive=hd0".to_string(),
        ]);
        eprintln!("[fstart] disk: {disk_path} (NVMe, format={fmt})");
    }

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
///
/// The `align_end` parameter controls placement within the pflash:
/// - `false` (default, RISC-V/ARM): firmware at offset 0, padding after.
/// - `true` (x86): firmware at end of flash, padding before. This is
///   required because QEMU maps pflash so the LAST byte is at the top
///   of the address space (0xFFFFFFFF on x86), and the reset vector
///   must be at the last 16 bytes.
fn create_pflash_image_aligned(
    binary: &Path,
    flash_size: usize,
    align_end: bool,
) -> Result<PathBuf, String> {
    let data = std::fs::read(binary).map_err(|e| format!("failed to read firmware binary: {e}"))?;

    if data.len() > flash_size {
        return Err(format!(
            "firmware binary ({} bytes) exceeds flash bank size ({} bytes)",
            data.len(),
            flash_size
        ));
    }

    let mut pflash = vec![0xFFu8; flash_size];
    if align_end {
        // x86: firmware at the END of flash (reset vector at last 16 bytes)
        let offset = flash_size - data.len();
        pflash[offset..].copy_from_slice(&data);
    } else {
        // ARM/RISC-V: firmware at the START of flash
        pflash[..data.len()].copy_from_slice(&data);
    }

    let pflash_path = binary.with_extension("pflash");
    std::fs::write(&pflash_path, &pflash)
        .map_err(|e| format!("failed to write pflash image: {e}"))?;

    eprintln!(
        "[fstart] pflash image: {} ({} bytes, firmware {} bytes{})",
        pflash_path.display(),
        flash_size,
        data.len(),
        if align_end { ", aligned to end" } else { "" },
    );

    Ok(pflash_path)
}

/// Convenience wrapper: firmware at start of flash (ARM/RISC-V).
fn create_pflash_image(binary: &Path, flash_size: usize) -> Result<PathBuf, String> {
    create_pflash_image_aligned(binary, flash_size, false)
}

/// Create an x86 pflash image by overlaying the FFS image onto the raw
/// stage binary.
///
/// The raw stage `.bin` (from objcopy) is a full flash-sized image with
/// boot code at the correct offsets (reset vector at end, 16/32/64-bit
/// entry code near end, main code at start). The FFS image contains the
/// stage segments + kernel payload at offset 0 (mapped to flash base).
///
/// We start with the raw binary as the base (preserving boot code at the
/// end) and overlay the FFS content at offset 0 (so the FFS anchor and
/// payload are accessible via memory-mapped flash reads).
fn create_x86_pflash(
    ffs_image: &Path,
    stage_bin: &Path,
    flash_size: usize,
) -> Result<PathBuf, String> {
    let mut pflash =
        std::fs::read(stage_bin).map_err(|e| format!("failed to read stage binary: {e}"))?;

    if pflash.len() != flash_size {
        // Pad or truncate to flash size
        pflash.resize(flash_size, 0xFF);
    }

    let ffs_data =
        std::fs::read(ffs_image).map_err(|e| format!("failed to read FFS image: {e}"))?;

    if ffs_data.len() > flash_size {
        return Err(format!(
            "FFS image ({} bytes) exceeds flash size ({} bytes)",
            ffs_data.len(),
            flash_size,
        ));
    }

    // Place FFS at offset 0x100000 (1 MiB) to avoid overwriting stage code.
    // Stage code (.text + .rodata + .ltext with code-model=large) occupies
    // ~640 KiB at the start of flash; 1 MiB gives headroom for growth.
    //
    // Flash layout (8 MiB pflash at 0xFF800000):
    //   [0x000000..0x0FFFFF] stage code XIP (.text, .rodata, .ltext)
    //   [0x100000..0x7FEFFF] FFS image (kernel payload, manifest, etc.)
    //   [0x7FF000..0x7FFFFF] boot block (.x86boot + .reset)
    //
    // Must match the board RON BootMedia base:
    //   flash_base + FFS_FLASH_OFFSET = 0xFF800000 + 0x100000 = 0xFF900000
    const FFS_FLASH_OFFSET: usize = 0x100000;
    let ffs_end = FFS_FLASH_OFFSET + ffs_data.len();
    let bootblock_start = flash_size - 0x1000; // last 4K

    if ffs_end > bootblock_start {
        return Err(format!(
            "FFS image ({} bytes) at offset {:#x} overlaps boot block at {:#x}",
            ffs_data.len(),
            FFS_FLASH_OFFSET,
            bootblock_start,
        ));
    }

    pflash[FFS_FLASH_OFFSET..ffs_end].copy_from_slice(&ffs_data);

    // Patch the FSTART_ANCHOR in the stage code region.
    //
    // The assembler patches the anchor within the FFS image (at the anchor
    // offset within the FFS). But the stage binary has its own copy of the
    // anchor in the `.fstart.anchor` section (at a fixed flash offset).
    // On x86, these are at different flash offsets, so we must copy the
    // patched anchor from the FFS to the stage's anchor location.
    //
    // This mechanism is reusable: any x86 platform with the boot block
    // architecture needs this anchor patching step.
    //
    // Find the anchor in the FFS by scanning for the FSTART01 magic.
    let anchor_magic = b"FSTART01";
    if let Some(ffs_anchor_off) = ffs_data
        .windows(anchor_magic.len())
        .position(|w| w == anchor_magic)
    {
        // Find the anchor in the stage binary (pflash offset 0..FFS_FLASH_OFFSET)
        if let Some(stage_anchor_off) = pflash[..FFS_FLASH_OFFSET]
            .windows(anchor_magic.len())
            .position(|w| w == anchor_magic)
        {
            // The anchor block is 300 bytes (AnchorBlock size).
            // Copy from FFS anchor to stage anchor.
            let anchor_size = 300;
            let ffs_src = ffs_anchor_off;
            let stage_dst = stage_anchor_off;
            if ffs_src + anchor_size <= ffs_data.len()
                && stage_dst + anchor_size <= FFS_FLASH_OFFSET
            {
                pflash[stage_dst..stage_dst + anchor_size]
                    .copy_from_slice(&ffs_data[ffs_src..ffs_src + anchor_size]);
                eprintln!(
                    "[fstart] x86 pflash: patched anchor at flash offset {:#x} (from FFS offset {:#x})",
                    stage_dst, ffs_src,
                );
            }
        }
    }

    let pflash_path = ffs_image.with_extension("pflash");
    std::fs::write(&pflash_path, &pflash)
        .map_err(|e| format!("failed to write pflash image: {e}"))?;

    eprintln!(
        "[fstart] x86 pflash: {} ({} bytes, FFS {} bytes at offset {:#x})",
        pflash_path.display(),
        flash_size,
        ffs_data.len(),
        FFS_FLASH_OFFSET,
    );

    Ok(pflash_path)
}
