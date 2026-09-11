use fstart_core::{BoardBuildPolicy, Platform, QemuMachine};
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_qemu(name: &str) -> String {
    if let Ok(output) = Command::new("which").arg(name).output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return path;
        }
    }

    if let Ok(entries) = std::fs::read_dir("/nix/store") {
        for entry in entries.flatten() {
            let candidate = entry.path().join("bin").join(name);
            if candidate.is_file() {
                return candidate.display().to_string();
            }
        }
    }

    name.to_string()
}

pub fn run(
    build_policy: &BoardBuildPolicy,
    platform: Platform,
    binary: &Path,
    disk: Option<&str>,
    memory: Option<&str>,
    secure_firmware: Option<&str>,
    x86_stage_elf: Option<&Path>,
) -> Result<(), String> {
    let use_sbsa_ref = build_policy.qemu_machine == Some(QemuMachine::SbsaRef);
    let use_sifive_u = build_policy.qemu_machine == Some(QemuMachine::SifiveU);
    let use_orangepi_pc = build_policy.qemu_machine == Some(QemuMachine::OrangePiPc);
    let (qemu_bin, mut args) = if use_sbsa_ref {
        if platform != Platform::Aarch64 {
            return Err("QEMU sbsa-ref requires the aarch64 platform".to_string());
        }
        let secure = secure_firmware.ok_or_else(|| {
            "QEMU sbsa-ref requires --secure-firmware (a complete 256 MiB TF-A pflash image)"
                .to_string()
        })?;
        let secure_size = std::fs::metadata(secure)
            .map_err(|e| format!("failed to read SBSA secure firmware: {e}"))?
            .len();
        const SBSA_PFLASH_SIZE: u64 = 256 * 1024 * 1024;
        if secure_size != SBSA_PFLASH_SIZE {
            return Err(format!(
                "SBSA secure firmware is {secure_size} bytes; expected {SBSA_PFLASH_SIZE}"
            ));
        }
        let non_secure = create_pflash_image(binary, SBSA_PFLASH_SIZE as usize)?;
        (
            find_qemu("qemu-system-aarch64"),
            vec![
                "-M".to_string(),
                "sbsa-ref".to_string(),
                "-cpu".to_string(),
                "max".to_string(),
                "-nographic".to_string(),
                "-drive".to_string(),
                format!("if=pflash,file={secure},format=raw,unit=0,readonly=on"),
                "-drive".to_string(),
                format!("if=pflash,file={},format=raw,unit=1", non_secure.display()),
            ],
        )
    } else if use_orangepi_pc {
        if platform != Platform::Armv7 {
            return Err("QEMU orangepi-pc requires armv7".to_string());
        }
        let sd = create_sunxi_sd_image(binary)?;
        (
            find_qemu("qemu-system-arm"),
            vec![
                "-M".to_string(),
                "orangepi-pc".to_string(),
                "-nographic".to_string(),
                "-sd".to_string(),
                sd.display().to_string(),
            ],
        )
    } else if use_sifive_u {
        if platform != Platform::Riscv64 {
            return Err("QEMU sifive_u requires the riscv64 platform".to_string());
        }
        (
            find_qemu("qemu-system-riscv64"),
            vec![
                "-M".to_string(),
                "sifive_u".to_string(),
                "-m".to_string(),
                "1G".to_string(),
                "-nographic".to_string(),
                "-bios".to_string(),
                binary.display().to_string(),
            ],
        )
    } else {
        match platform {
            Platform::Riscv64 => {
                let pflash_size = 32 * 1024 * 1024;
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
            Platform::Aarch64 => {
                let mut args = vec![
                    "-machine".to_string(),
                    "virt,secure=on,virtualization=on,gic-version=3".to_string(),
                    "-cpu".to_string(),
                    "cortex-a72".to_string(),
                    "-nographic".to_string(),
                ];
                args.extend(virt_flash_args(binary)?);
                (find_qemu("qemu-system-aarch64"), args)
            }
            Platform::Armv7 => {
                let mut args = vec![
                    "-machine".to_string(),
                    "virt,highmem-ecam=off".to_string(),
                    "-cpu".to_string(),
                    "cortex-a15".to_string(),
                    "-nographic".to_string(),
                ];
                args.extend(virt_flash_args(binary)?);
                (find_qemu("qemu-system-arm"), args)
            }
            Platform::X86_64 => {
                let pflash_size = 16 * 1024 * 1024;
                // The plan build always hands over the linked stage ELF;
                // without it there is no reset vector or anchor source.
                let pflash_path = match x86_stage_elf {
                    Some(elf) => create_x86_pflash_from_elf(binary, elf, pflash_size)?,
                    None => {
                        return Err("x86 pflash requires the linked stage ELF".to_string());
                    }
                };

                let accel = std::env::var("FSTART_QEMU_ACCEL").ok();
                let use_kvm = match accel.as_deref() {
                    Some("kvm") => true,
                    Some("tcg") => false,
                    _ => std::fs::File::open("/dev/kvm").is_ok(),
                };
                let args = vec![
                    "-M".to_string(),
                    "q35".to_string(),
                    "-accel".to_string(),
                    if use_kvm { "kvm" } else { "tcg" }.to_string(),
                    "-cpu".to_string(),
                    if use_kvm { "host" } else { "max" }.to_string(),
                    "-m".to_string(),
                    "1G".to_string(),
                    "-smp".to_string(),
                    "1".to_string(),
                    "-no-reboot".to_string(),
                    "-display".to_string(),
                    "none".to_string(),
                    "-chardev".to_string(),
                    "stdio,id=char0,mux=on,signal=off".to_string(),
                    "-serial".to_string(),
                    "chardev:char0".to_string(),
                    "-mon".to_string(),
                    "chardev=char0,mode=readline".to_string(),
                    "-bios".to_string(),
                    pflash_path.display().to_string(),
                    "-vga".to_string(),
                    "none".to_string(),
                    "-device".to_string(),
                    "bochs-display".to_string(),
                ];
                (find_qemu("qemu-system-x86_64"), args)
            }
        }
    };

    if !use_sbsa_ref
        && !use_sifive_u
        && !use_orangepi_pc
        && !args.iter().any(|arg| arg == "-no-reboot")
    {
        args.push("-no-reboot".to_string());
    }

    if !use_sbsa_ref
        && !use_sifive_u
        && !use_orangepi_pc
        && let Some(mem) = memory
    {
        args.extend(["-m".to_string(), mem.to_string()]);
    }

    // Keep one backend-free PCI function present on architecture QEMU machines
    // so every normal boot exercises ECAM enumeration and BAR assignment.
    if platform != Platform::X86_64 && !use_sifive_u && !use_orangepi_pc {
        args.extend([
            "-device".to_string(),
            "virtio-scsi-pci,id=fstart-pci-test".to_string(),
        ]);
    }

    if let Some(disk_path) = disk {
        let fmt = if disk_path.ends_with(".qcow2") {
            "qcow2"
        } else {
            "raw"
        };
        if platform == Platform::X86_64 {
            let is_iso = disk_path.ends_with(".iso");
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

/// ARM `virt` machines have two 64 MiB pflash banks (0x0 and 0x0400_0000).
/// `-bios` only fills bank 0, so images that place the FFS in bank 1 (XIP
/// assemble output) must be split into two pflash drives.
const VIRT_FLASH_BANK_SIZE: usize = 64 * 1024 * 1024;

fn virt_flash_args(binary: &Path) -> Result<Vec<String>, String> {
    let data = std::fs::read(binary).map_err(|e| format!("failed to read firmware image: {e}"))?;
    if data.len() <= VIRT_FLASH_BANK_SIZE {
        return Ok(vec!["-bios".to_string(), binary.display().to_string()]);
    }
    if data.len() > 2 * VIRT_FLASH_BANK_SIZE {
        return Err(format!(
            "firmware image ({} bytes) exceeds both virt pflash banks ({} bytes)",
            data.len(),
            2 * VIRT_FLASH_BANK_SIZE
        ));
    }

    let mut bank0 = vec![0xFFu8; VIRT_FLASH_BANK_SIZE];
    bank0.copy_from_slice(&data[..VIRT_FLASH_BANK_SIZE]);
    let mut bank1 = vec![0xFFu8; VIRT_FLASH_BANK_SIZE];
    bank1[..data.len() - VIRT_FLASH_BANK_SIZE].copy_from_slice(&data[VIRT_FLASH_BANK_SIZE..]);

    let bank0_path = binary.with_extension("bank0.pflash");
    let bank1_path = binary.with_extension("bank1.pflash");
    std::fs::write(&bank0_path, &bank0)
        .map_err(|e| format!("failed to write pflash bank 0: {e}"))?;
    std::fs::write(&bank1_path, &bank1)
        .map_err(|e| format!("failed to write pflash bank 1: {e}"))?;

    eprintln!(
        "[fstart] virt pflash banks: {} + {}",
        bank0_path.display(),
        bank1_path.display()
    );
    Ok(vec![
        "-drive".to_string(),
        format!("if=pflash,file={},format=raw,unit=0", bank0_path.display()),
        "-drive".to_string(),
        format!("if=pflash,file={},format=raw,unit=1", bank1_path.display()),
    ])
}

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
        let offset = flash_size - data.len();
        pflash[offset..].copy_from_slice(&data);
    } else {
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
    let flash_end = flash_base + pflash.len() as u64;

    if elf_data.get(0..4) != Some(b"\x7fELF") || elf_data.get(4) != Some(&2) {
        return Err(format!("{} is not an ELF64 file", elf_path.display()));
    }
    if elf_data.get(5) != Some(&1) {
        return Err(format!("{} is not little-endian ELF", elf_path.display()));
    }

    let phoff = read_elf_u64(&elf_data, 32, "e_phoff")? as usize;
    let phentsize = read_elf_u16(&elf_data, 54, "e_phentsize")? as usize;
    let phnum = read_elf_u16(&elf_data, 56, "e_phnum")? as usize;

    // A zero overlay means a mis-linked ELF or wrong base and silently
    // reproduces a flash without reset vector; fail loudly instead.
    let mut overlaid = 0u32;
    for idx in 0..phnum {
        let ph = phoff
            .checked_add(idx * phentsize)
            .ok_or_else(|| "ELF program header offset overflow".to_string())?;
        if ph
            .checked_add(phentsize)
            .is_none_or(|end| end > elf_data.len())
        {
            return Err(format!("ELF program header {idx} is out of bounds"));
        }
        let p_type = read_elf_u32(&elf_data, ph, "p_type")?;
        const PT_LOAD: u32 = 1;
        if p_type != PT_LOAD {
            continue;
        }

        let file_offset = read_elf_u64(&elf_data, ph + 8, "p_offset")?;
        // XIP .data has a RAM virtual address but a flash load address.
        let start = read_elf_u64(&elf_data, ph + 24, "p_paddr")?;
        let file_size = read_elf_u64(&elf_data, ph + 32, "p_filesz")?;
        if file_size == 0 {
            continue;
        }
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
        overlaid += 1;
    }
    if overlaid == 0 {
        return Err(format!(
            "no flash PT_LOAD segments overlaid from {}",
            elf_path.display()
        ));
    }

    eprintln!(
        "[fstart] x86 pflash: overlaid flash PT_LOAD segments from {}",
        elf_path.display(),
    );
    Ok(())
}

fn read_elf_u16(data: &[u8], offset: usize, field: &str) -> Result<u16, String> {
    let bytes: [u8; 2] = data
        .get(offset..offset + 2)
        .ok_or_else(|| format!("ELF field {field} is out of bounds"))?
        .try_into()
        .expect("slice length checked");
    Ok(u16::from_le_bytes(bytes))
}

fn read_elf_u32(data: &[u8], offset: usize, field: &str) -> Result<u32, String> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("ELF field {field} is out of bounds"))?
        .try_into()
        .expect("slice length checked");
    Ok(u32::from_le_bytes(bytes))
}

fn read_elf_u64(data: &[u8], offset: usize, field: &str) -> Result<u64, String> {
    let bytes: [u8; 8] = data
        .get(offset..offset + 8)
        .ok_or_else(|| format!("ELF field {field} is out of bounds"))?
        .try_into()
        .expect("slice length checked");
    Ok(u64::from_le_bytes(bytes))
}

/// x86 pflash from the linked stage ELF: overlay its file-backed segments at
/// their linked addresses (bootblock, reset vector), then lay the FFS image
/// at its firmware offset and patch the ROM anchor copy.
fn create_x86_pflash_from_elf(
    ffs_image: &Path,
    stage_elf: &Path,
    flash_size: usize,
) -> Result<PathBuf, String> {
    let mut pflash = vec![0xFFu8; flash_size];
    let flash_base = x86_flash_base(flash_size)?;
    overlay_x86_elf_segments(stage_elf, &mut pflash, flash_base)?;
    let ffs_data =
        std::fs::read(ffs_image).map_err(|e| format!("failed to read FFS image: {e}"))?;
    finish_x86_pflash(
        pflash, ffs_image, &ffs_data, stage_elf, flash_base, flash_size,
    )
}

fn x86_flash_base(flash_size: usize) -> Result<u64, String> {
    0x1_0000_0000u64
        .checked_sub(flash_size as u64)
        .ok_or_else(|| "invalid x86 flash size".to_string())
}

/// Legacy compact-.bin fallback for boards without a stage ELF: the binary
/// must already be a full flash image.
/// Copy the finalized FFS trust policy over the stage's placeholder trust
/// block. The ELF overlay carries linked placeholder bytes, but runtime reads
/// its expected keys from linked code, so the ROM copy must carry the policy
/// the image builder finalized. Runs after the FFS copy and returns the
/// patched flash offset for the overlap check.
fn patch_x86_stage_trust(pflash: &mut [u8], ffs_data: &[u8]) -> Result<usize, String> {
    use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
    let mut placeholder = [0u8; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut placeholder);
    let trust = (0..ffs_data.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .filter_map(|offset| {
            let window = &ffs_data[offset..offset + TRUST_SIZE];
            if window == placeholder {
                return None;
            }
            TrustBlock::parse(window).filter(|block| block.key_count > 0)
        })
        .next()
        .ok_or_else(|| "no finalized trust block found in FFS image".to_string())?;
    let mut patched = [0u8; TRUST_SIZE];
    trust.write_to(&mut patched);
    let dst = (0..pflash.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .find(|&offset| pflash[offset..offset + TRUST_SIZE] == placeholder)
        .ok_or_else(|| "stage trust placeholder not found in x86 pflash".to_string())?;
    pflash[dst..dst + TRUST_SIZE].copy_from_slice(&patched);
    eprintln!("[fstart] x86 pflash: patched trust at flash offset {dst:#x}");
    Ok(dst)
}

fn finish_x86_pflash(
    mut pflash: Vec<u8>,
    ffs_image: &Path,
    ffs_data: &[u8],
    stage_elf: &Path,
    flash_base: u64,
    flash_size: usize,
) -> Result<PathBuf, String> {
    if ffs_data.len() > flash_size {
        return Err(format!(
            "FFS image ({} bytes) exceeds flash size ({} bytes)",
            ffs_data.len(),
            flash_size,
        ));
    }

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

    pflash[FFS_FLASH_OFFSET..ffs_end].copy_from_slice(ffs_data);

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
    let stage_anchor_off =
        x86_elf_symbol_flash_offset(stage_elf, "_fstart_anchor_early", flash_base, flash_size)?
            .ok_or_else(|| {
                format!(
                    "missing _fstart_anchor_early in {} while building x86 pflash",
                    stage_elf.display()
                )
            })?;

    let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
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
    // The trust search must see final bytes, and the patched site must not
    // overlap the FFS window it was just copied beside.
    let trust_dst = patch_x86_stage_trust(&mut pflash, ffs_data)?;
    let trust_end = trust_dst + fstart_core::ffs::trust::TRUST_SIZE;
    if trust_dst < ffs_end && trust_end > FFS_FLASH_OFFSET {
        return Err(format!(
            "stage trust at flash offset {trust_dst:#x} overlaps the FFS window \
             [{FFS_FLASH_OFFSET:#x}..{ffs_end:#x})",
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_pflash_overlay_uses_physical_load_address() {
        let flash_base = 0xff00_0000u64;
        let mut elf = vec![0u8; 0x104];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // ELF64
        elf[5] = 1; // little-endian
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph = 64;
        elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes());
        elf[ph + 16..ph + 24].copy_from_slice(&0x0100_0000u64.to_le_bytes());
        elf[ph + 24..ph + 32].copy_from_slice(&(flash_base + 0x20).to_le_bytes());
        elf[ph + 32..ph + 40].copy_from_slice(&4u64.to_le_bytes());
        elf[0x100..0x104].copy_from_slice(b"data");

        let path = std::env::temp_dir().join(format!(
            "fstart-qemu-overlay-test-{}.elf",
            std::process::id()
        ));
        std::fs::write(&path, elf).unwrap();

        let mut pflash = vec![0xff; 0x100];
        overlay_x86_elf_segments(&path, &mut pflash, flash_base).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(&pflash[0x20..0x24], b"data");
    }
}

/// QEMU's H3 BROM reads eGON from SD byte 0x2000; provide a real-sized SD image.
fn create_sunxi_sd_image(binary: &Path) -> Result<PathBuf, String> {
    const SIZE: usize = 32 * 1024 * 1024;
    const EGON_OFFSET: usize = 8 * 1024;
    let image = std::fs::read(binary).map_err(|e| format!("failed to read eGON image: {e}"))?;
    if image.len() > SIZE - EGON_OFFSET {
        return Err("eGON image exceeds QEMU SD image".to_string());
    }
    let path = binary.with_extension("sunxi-sd.img");
    let mut sd = vec![0u8; SIZE];
    sd[EGON_OFFSET..EGON_OFFSET + image.len()].copy_from_slice(&image);
    std::fs::write(&path, sd).map_err(|e| format!("failed to create Sunxi SD image: {e}"))?;
    Ok(path)
}
