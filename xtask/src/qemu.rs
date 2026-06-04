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
    let (qemu_bin, mut args) = if board_name == "qemu-sbsa" {
        // SBSA-ref: TF-A runs first from pflash0 (secure flash at 0x0),
        // then launches fstart as BL33 from pflash1 (non-secure flash
        // at 0x10000000). Both pflash images must be exactly 256 MiB.
        //
        // TF-A binaries (bl1.bin + fip.bin) are expected in the board
        // directory. Build them with:
        //   make PLAT=qemu_sbsa all fip ARM_LINUX_KERNEL_AS_BL33=1
        let pflash_size = 256 * 1024 * 1024; // 256 MiB

        // pflash0 = TF-A (secure flash)
        let tfa_path = find_tfa_flash(binary, board_name, pflash_size)?;

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
                // AArch64 virt: use -bios for FFS images.  QEMU loads
                // the -bios file into pflash0 AND enters firmware boot
                // mode (DTB at RAM base, PSCI conduit setup, proper
                // EL3 initialization).  Using raw pflash without -bios
                // skips the PSCI conduit, which breaks TF-A BL31 boot.
                //
                // The flat binary FFS packing (preserving section
                // alignment gaps) ensures the anchor is at its
                // link-time VMA even with -bios.
                args.extend(["-bios".to_string(), binary.display().to_string()]);
                // Bochs VBE display — only for boards that use a framebuffer
                // (CrabEFI UEFI payload). Non-VGA PCI device (class 0x0380)
                // with MMIO registers in BAR2.
                if board_name.contains("uefi") {
                    args.extend(["-device".to_string(), "bochs-display".to_string()]);
                }
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
                let is_uefi = board_name.contains("uefi");

                // x86: the raw stage .bin has boot code at the correct
                // offsets within 8MB (reset vector at end, code at start).
                // The FFS image has the stage segments + kernel payload.
                // We overlay the FFS onto the raw binary so both the boot
                // code (end of flash) and FFS data (start of flash) are
                // present.
                //
                // For multi-stage boards (e.g., qemu-q35-uefi), the
                // bootblock .bin provides the reset vector; FFS contains
                // the main stage + payload data.
                //
                // binary = target/ffs/<board>.ffs
                // stage_bin = target/x86_64-unknown-none/{debug,release}/fstart-{stage}.bin
                let workspace = binary.parent().unwrap().parent().unwrap();
                let profile = if binary.to_str().unwrap_or("").contains("release")
                    || std::env::args().any(|a| a == "--release")
                {
                    "release"
                } else {
                    "debug"
                };

                // For multi-stage, the bootblock binary has the reset vector.
                // For monolithic, it's fstart-stage.bin.
                let stage_bin_name = if is_uefi {
                    "fstart-bootblock.bin"
                } else {
                    "fstart-stage.bin"
                };
                let stage_bin = workspace
                    .join("x86_64-unknown-none")
                    .join(profile)
                    .join(stage_bin_name);
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
                let accel = std::env::var("FSTART_QEMU_ACCEL").ok();
                let use_kvm = match accel.as_deref() {
                    Some("kvm") => true,
                    Some("tcg") => false,
                    _ => std::fs::File::open("/dev/kvm").is_ok(),
                };
                let default_mem = if is_uefi { "4G" } else { "1G" };
                let mut args = vec![
                    "-machine".to_string(),
                    "q35".to_string(),
                    "-accel".to_string(),
                    if use_kvm { "kvm" } else { "tcg" }.to_string(),
                    "-cpu".to_string(),
                    if use_kvm { "host" } else { "max" }.to_string(),
                    "-m".to_string(),
                    default_mem.to_string(),
                    "-smp".to_string(),
                    if board_name == "qemu-q35" { "4" } else { "1" }.to_string(),
                    "-no-reboot".to_string(),
                    "-display".to_string(),
                    "none".to_string(),
                    "-chardev".to_string(),
                    "stdio,id=char0,mux=on,signal=off".to_string(),
                    "-serial".to_string(),
                    "chardev:char0".to_string(),
                    "-mon".to_string(),
                    "chardev=char0,mode=readline".to_string(),
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
                    // with MMIO registers in BAR2. Q35 has a built-in VGA;
                    // use -vga none to avoid conflicts.
                    "-vga".to_string(),
                    "none".to_string(),
                    "-device".to_string(),
                    "bochs-display".to_string(),
                ];

                // UEFI boards: attach AHCI disk via ICH9-AHCI.
                // QEMU Q35 already provides an ICH9-AHCI controller on
                // the southbridge, but adding an explicit one ensures
                // SATA device detection in CrabEFI's PCI scan.
                if is_uefi && disk.is_none() {
                    // Check for a default disk image in the board directory
                    let board_dir = find_board_dir_by_name(binary, board_name)?;
                    let default_disk = board_dir.join("disk.img");
                    if default_disk.exists() {
                        let disk_str = default_disk.display().to_string();
                        args.extend([
                            "-drive".to_string(),
                            format!("file={disk_str},id=hd0,if=none,format=raw"),
                            "-device".to_string(),
                            "ide-hd,drive=hd0".to_string(),
                        ]);
                        eprintln!("[fstart] disk: {disk_str} (AHCI/SATA, default)");
                    }
                }

                (find_qemu("qemu-system-x86_64"), args)
            }
        }
    };

    if !args.iter().any(|arg| arg == "-no-reboot") {
        args.push("-no-reboot".to_string());
    }

    // Add RAM if specified
    if let Some(mem) = memory {
        args.extend(["-m".to_string(), mem.to_string()]);
    }

    // Attach disk image.
    // x86 Q35 UEFI: AHCI (ICH9-SATA) via ide-hd on the built-in controller.
    // AArch64/RISC-V: NVMe (CrabEFI has an NVMe driver).
    if let Some(disk_path) = disk {
        let fmt = if disk_path.ends_with(".qcow2") {
            "qcow2"
        } else {
            "raw"
        };
        if platform == Platform::X86_64 {
            let is_iso = disk_path.ends_with(".iso");
            if board_name.contains("uefi") && !is_iso {
                // Match CrabEFI's upstream GRUB/Linux CI path: expose the test
                // disk as USB mass storage. GRUB probes EFI BlockIO handles
                // aggressively; under QEMU TCG its AHCI path stalls before
                // reaching Linux, while the USB path is what CrabEFI exercises
                // for its x86_64 GRUB boot-chain CI.
                args.extend([
                    "-device".to_string(),
                    "qemu-xhci,id=xhci".to_string(),
                    "-drive".to_string(),
                    format!("file={disk_path},id=hd0,if=none,format={fmt},readonly=off"),
                    "-device".to_string(),
                    "usb-storage,drive=hd0,bus=xhci.0".to_string(),
                ]);
                eprintln!("[fstart] disk: {disk_path} (USB mass storage, format={fmt})");
            } else {
                let device_type = if is_iso { "ide-cd" } else { "ide-hd" };
                args.extend([
                    "-drive".to_string(),
                    format!(
                        "file={disk_path},id=hd0,if=none,format={fmt},readonly={}",
                        if is_iso { "on" } else { "off" }
                    ),
                    "-device".to_string(),
                    format!("{device_type},drive=hd0"),
                ]);
                eprintln!("[fstart] disk: {disk_path} (AHCI/{device_type}, format={fmt})");
            }
        } else {
            args.extend([
                "-drive".to_string(),
                format!("file={disk_path},id=hd0,if=none,format={fmt}"),
                "-device".to_string(),
                "nvme,serial=fstartdisk0,drive=hd0".to_string(),
            ]);
            eprintln!("[fstart] disk: {disk_path} (NVMe, format={fmt})");
        }
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
fn find_tfa_flash(
    fstart_binary: &Path,
    board_name: &str,
    flash_size: usize,
) -> Result<PathBuf, String> {
    let board_dir = find_board_dir_by_name(fstart_binary, board_name)?;

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

/// Find a board directory by board name, starting from a binary's location.
fn find_board_dir_by_name(binary: &Path, board_name: &str) -> Result<PathBuf, String> {
    let mut dir = binary
        .parent()
        .ok_or_else(|| "no parent directory for binary".to_string())?
        .to_path_buf();

    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                let board_dir = dir.join("boards").join(board_name);
                if board_dir.is_dir() {
                    return Ok(board_dir);
                }
                return Err(format!(
                    "board directory not found: {}",
                    board_dir.display()
                ));
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root from binary path".to_string());
        }
    }
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

fn x86_elf_symbol_flash_offset(
    elf_path: &Path,
    symbol_name: &str,
    flash_base: u64,
    flash_size: usize,
) -> Result<Option<usize>, String> {
    let elf_data = std::fs::read(elf_path)
        .map_err(|e| format!("failed to read stage ELF {}: {e}", elf_path.display()))?;
    let elf = object::File::parse(&*elf_data)
        .map_err(|e| format!("failed to parse stage ELF {}: {e}", elf_path.display()))?;
    let flash_end = flash_base + flash_size as u64;

    for sym in object::Object::symbols(&elf) {
        let Ok(name) = object::ObjectSymbol::name(&sym) else {
            continue;
        };
        let addr = object::ObjectSymbol::address(&sym);
        if name == symbol_name && addr >= flash_base && addr < flash_end {
            return Ok(Some((addr - flash_base) as usize));
        }
    }
    Ok(None)
}

fn overlay_x86_elf_segments(
    elf_path: &Path,
    pflash: &mut [u8],
    flash_base: u64,
) -> Result<(), String> {
    let elf_data = std::fs::read(elf_path)
        .map_err(|e| format!("failed to read stage ELF {}: {e}", elf_path.display()))?;
    let elf = object::File::parse(&*elf_data)
        .map_err(|e| format!("failed to parse stage ELF {}: {e}", elf_path.display()))?;
    let flash_end = flash_base + pflash.len() as u64;

    for segment in object::Object::segments(&elf) {
        let (file_offset, file_size) = object::ObjectSegment::file_range(&segment);
        if file_size == 0 {
            continue;
        }
        let start = object::ObjectSegment::address(&segment);
        let end = start
            .checked_add(file_size)
            .ok_or_else(|| "stage ELF segment address overflow".to_string())?;
        if start < flash_base || end > flash_end {
            continue;
        }

        let src_start = file_offset as usize;
        let src_end = src_start
            .checked_add(file_size as usize)
            .ok_or_else(|| "stage ELF segment file offset overflow".to_string())?;
        if src_end > elf_data.len() {
            return Err(format!(
                "stage ELF segment exceeds file size: offset {:#x}, size {:#x}",
                file_offset, file_size,
            ));
        }

        let dst_start = (start - flash_base) as usize;
        let dst_end = dst_start + file_size as usize;
        pflash[dst_start..dst_end].copy_from_slice(&elf_data[src_start..src_end]);
    }

    eprintln!(
        "[fstart] x86 pflash: overlaid flash PT_LOAD segments from {}",
        elf_path.display(),
    );
    Ok(())
}

/// Create an x86 pflash image by overlaying the FFS image onto the raw
/// stage binary.
///
/// The raw stage `.bin` (extracted from ELF PT_LOAD data) is a full flash-sized image with
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
    let mut pflash = vec![0xFFu8; flash_size];
    let flash_base = 0x1_0000_0000u64
        .checked_sub(flash_size as u64)
        .ok_or_else(|| "invalid x86 flash size".to_string())?;

    // Prefer the ELF next to the flat .bin so we can honor sparse section
    // placement. `llvm-objcopy -O binary` drops address gaps before the first
    // loadable section, so a compact .bin cannot be blindly copied at pflash
    // offset 0: for small bootblocks the reset vector would land near the start
    // of flash instead of at 0xfffffff0.
    let stage_elf = stage_bin.with_extension("");
    if stage_elf.exists() {
        overlay_x86_elf_segments(&stage_elf, &mut pflash, flash_base)?;
    } else {
        let stage_data =
            std::fs::read(stage_bin).map_err(|e| format!("failed to read stage binary: {e}"))?;
        if stage_data.len() != flash_size {
            return Err(format!(
                "x86 stage binary {} is {} bytes, but flash size is {} bytes; \
                 compact .bin images are not safe for x86 pflash placement without ELF {}",
                stage_bin.display(),
                stage_data.len(),
                flash_size,
                stage_elf.display(),
            ));
        }
        pflash.copy_from_slice(&stage_data);
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
    // Must match the Rust platform firmware-image mapping for q35:
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
    // Find the patched FFS anchor, not just any embedded stage placeholder.
    // Stage entries can contain their own placeholder anchors with the same
    // magic; the real FFS anchor has total_image_size equal to the assembled
    // image length and anchor_offset equal to its byte offset.
    let anchor_magic = b"FSTART01";
    let ffs_anchor_off =
        ffs_data
            .windows(anchor_magic.len())
            .enumerate()
            .find_map(|(offset, w)| {
                if w != anchor_magic || offset + 28 > ffs_data.len() {
                    return None;
                }
                let total_image_size = u32::from_le_bytes([
                    ffs_data[offset + 20],
                    ffs_data[offset + 21],
                    ffs_data[offset + 22],
                    ffs_data[offset + 23],
                ]) as usize;
                let anchor_offset = u32::from_le_bytes([
                    ffs_data[offset + 24],
                    ffs_data[offset + 25],
                    ffs_data[offset + 26],
                    ffs_data[offset + 27],
                ]) as usize;
                (total_image_size == ffs_data.len() && anchor_offset == offset).then_some(offset)
            });
    let ffs_anchor_off = ffs_anchor_off.ok_or_else(|| {
        format!(
            "failed to locate patched FSTART anchor in FFS image {} (ffs_data len {})",
            ffs_image.display(),
            ffs_data.len()
        )
    })?;
    // Prefer the linker symbol for the ROM-resident anchor. Scanning for
    // the magic is ambiguous because optimized code may embed the magic as
    // an immediate constant, and stage payloads can contain placeholder
    // anchors of their own.
    let stage_anchor_off =
        x86_elf_symbol_flash_offset(&stage_elf, "_fstart_anchor_early", flash_base, flash_size)?
            .ok_or_else(|| {
                format!(
                    "missing _fstart_anchor_early in {} while building x86 pflash",
                    stage_elf.display()
                )
            })?;

    // The anchor block is 300 bytes (AnchorBlock size).
    // Copy from FFS anchor to stage anchor.
    let anchor_size = 300;
    let ffs_src = ffs_anchor_off;
    let stage_dst = stage_anchor_off;
    if ffs_src + anchor_size > ffs_data.len() || stage_dst + anchor_size > pflash.len() {
        return Err(format!(
            "x86 anchor patch out of bounds: ffs_src={:#x}, stage_dst={:#x}, \
             anchor_size={}, ffs_data len={}, pflash len={}",
            ffs_src,
            stage_dst,
            anchor_size,
            ffs_data.len(),
            pflash.len()
        ));
    }
    pflash[stage_dst..stage_dst + anchor_size]
        .copy_from_slice(&ffs_data[ffs_src..ffs_src + anchor_size]);
    eprintln!(
        "[fstart] x86 pflash: patched anchor at flash offset {:#x} (from FFS offset {:#x})",
        stage_dst, ffs_src,
    );

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
