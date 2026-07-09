use fstart_core::Platform;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_qemu(name: &str) -> String {
    if let Ok(output) = Command::new("which").arg(name).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
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
    _board_name: &str,
    platform: Platform,
    binary: &Path,
    disk: Option<&str>,
    memory: Option<&str>,
) -> Result<(), String> {
    let (qemu_bin, mut args) = match platform {
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
            let args = vec![
                "-machine".to_string(),
                "virt,secure=on,virtualization=on,gic-version=3".to_string(),
                "-cpu".to_string(),
                "cortex-a72".to_string(),
                "-nographic".to_string(),
                "-bios".to_string(),
                binary.display().to_string(),
            ];
            (find_qemu("qemu-system-aarch64"), args)
        }
        Platform::Armv7 => {
            let args = vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-cpu".to_string(),
                "cortex-a15".to_string(),
                "-nographic".to_string(),
                "-bios".to_string(),
                binary.display().to_string(),
            ];
            (find_qemu("qemu-system-arm"), args)
        }
        Platform::X86_64 => {
            let pflash_size = 16 * 1024 * 1024;
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
    };

    if !args.iter().any(|arg| arg == "-no-reboot") {
        args.push("-no-reboot".to_string());
    }

    if let Some(mem) = memory {
        args.extend(["-m".to_string(), mem.to_string()]);
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

fn create_x86_pflash(
    ffs_image: &Path,
    stage_bin: &Path,
    flash_size: usize,
) -> Result<PathBuf, String> {
    let mut pflash = vec![0xFFu8; flash_size];
    let flash_base = 0x1_0000_0000u64
        .checked_sub(flash_size as u64)
        .ok_or_else(|| "invalid x86 flash size".to_string())?;

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
        x86_elf_symbol_flash_offset(&stage_elf, "_fstart_anchor_early", flash_base, flash_size)?
            .ok_or_else(|| {
                format!(
                    "missing _fstart_anchor_early in {} while building x86 pflash",
                    stage_elf.display()
                )
            })?;

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
