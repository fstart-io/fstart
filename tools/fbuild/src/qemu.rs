use fstart_core::{BoardBuildPolicy, Platform};
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
    let accel = std::env::var("FSTART_QEMU_ACCEL").ok();
    let kvm = match accel.as_deref() {
        Some("kvm") => true,
        Some("tcg") => false,
        _ => std::fs::File::open("/dev/kvm").is_ok(),
    };
    let smp = std::env::var("FSTART_QEMU_SMP").unwrap_or_else(|_| "1".into());
    let launch = fstart_platform_qemu::host::launch::resolve(
        platform,
        build_policy.qemu_machine,
        kvm,
        &smp,
    )?;
    let qemu_bin = find_qemu(launch.program);
    let mut args = launch.args;
    use fstart_platform_qemu::host::launch::BootMedia;
    match launch.media {
        BootMedia::Bios => args.extend(["-bios".into(), binary.display().to_string()]),
        BootMedia::Mtd => args.extend([
            "-drive".into(),
            format!("file={},format=raw,if=mtd", binary.display()),
        ]),
        BootMedia::SunxiSd => {
            let sd = create_sunxi_sd_image(binary)?;
            args.extend(["-sd".into(), sd.display().to_string()]);
        }
        BootMedia::ArmVirt { bank_size } => {
            args.extend(virt_flash_args(binary, bank_size as usize)?)
        }
        BootMedia::X86Pflash { bank_size } => {
            let elf = x86_stage_elf.ok_or("x86 pflash requires the linked stage ELF")?;
            let image = create_x86_pflash_from_elf(binary, elf, bank_size as usize)?;
            args.extend(["-bios".into(), image.display().to_string()]);
        }
        BootMedia::Pflash {
            bank_size,
            secure_bank,
        } => {
            let unit = if secure_bank {
                let secure = secure_firmware
                    .ok_or("QEMU requires --secure-firmware for its secure pflash bank")?;
                let size = std::fs::metadata(secure)
                    .map_err(|e| format!("failed to read secure firmware: {e}"))?
                    .len();
                if size != bank_size {
                    return Err(format!(
                        "secure firmware is {size} bytes; expected {bank_size}"
                    ));
                }
                args.extend([
                    "-drive".into(),
                    format!("if=pflash,file={secure},format=raw,unit=0,readonly=on"),
                ]);
                1
            } else {
                0
            };
            let image = create_pflash_image(binary, bank_size as usize)?;
            args.extend([
                "-drive".into(),
                format!("if=pflash,file={},format=raw,unit={unit}", image.display()),
            ]);
        }
    };

    if launch.no_reboot {
        args.push("-no-reboot".to_string());
    }

    if launch.allow_memory_override
        && let Some(mem) = memory
    {
        args.extend(["-m".to_string(), mem.to_string()]);
    }

    // Keep one backend-free PCI function present on architecture QEMU machines
    // so every normal boot exercises ECAM enumeration and BAR assignment.
    if launch.pci_test {
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
        if platform.is_x86() {
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

    // Extra devices for one run, e.g. a swtpm-backed TPM:
    // FSTART_QEMU_EXTRA_ARGS="-chardev socket,id=chrtpm,path=... -tpmdev ..."
    if let Ok(extra) = std::env::var("FSTART_QEMU_EXTRA_ARGS") {
        args.extend(extra.split_whitespace().map(str::to_string));
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

/// Split an image across the two pflash banks declared by platform policy.
/// `-bios` only fills bank 0, so a larger image needs two explicit drives.
fn virt_flash_args(binary: &Path, bank_size: usize) -> Result<Vec<String>, String> {
    let data = std::fs::read(binary).map_err(|e| format!("failed to read firmware image: {e}"))?;
    if bank_size == 0 || bank_size.checked_mul(2).is_none() {
        return Err("invalid pflash bank size".into());
    }
    if data.len() <= bank_size {
        return Ok(vec!["-bios".to_string(), binary.display().to_string()]);
    }
    if data.len() > 2 * bank_size {
        return Err(format!(
            "firmware image ({} bytes) exceeds both virt pflash banks ({} bytes)",
            data.len(),
            2 * bank_size
        ));
    }

    let mut bank0 = vec![0xFFu8; bank_size];
    bank0.copy_from_slice(&data[..bank_size]);
    let mut bank1 = vec![0xFFu8; bank_size];
    bank1[..data.len() - bank_size].copy_from_slice(&data[bank_size..]);

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

    // A zero overlay means a mis-linked ELF or wrong base and silently
    // reproduces a flash without reset vector; fail loudly instead.
    let mut overlaid = 0u32;
    for (file_offset, start, file_size) in elf_load_segments(&elf_data)
        .map_err(|e| format!("failed to parse stage ELF {}: {e}", elf_path.display()))?
    {
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

/// `(p_offset, p_paddr, p_filesz)` of every PT_LOAD in a 32- or 64-bit ELF.
/// XIP `.data` has a RAM virtual address but a flash load address.
fn elf_load_segments(data: &[u8]) -> Result<Vec<(u64, u64, u64)>, String> {
    use object::read::elf::{FileHeader, ProgramHeader};

    fn load<Elf: FileHeader<Endian = object::Endianness>>(
        data: &[u8],
    ) -> object::read::Result<Vec<(u64, u64, u64)>> {
        let header = Elf::parse(data)?;
        let endian = header.endian()?;
        Ok(header
            .program_headers(endian, data)?
            .iter()
            .filter(|ph| ph.p_type(endian) == object::elf::PT_LOAD)
            .map(|ph| {
                (
                    ph.p_offset(endian).into(),
                    ph.p_paddr(endian).into(),
                    ph.p_filesz(endian).into(),
                )
            })
            .collect())
    }

    match object::FileKind::parse(data).map_err(|e| e.to_string())? {
        object::FileKind::Elf32 => load::<object::elf::FileHeader32<object::Endianness>>(data),
        object::FileKind::Elf64 => load::<object::elf::FileHeader64<object::Endianness>>(data),
        _ => return Err("not an ELF file".into()),
    }
    .map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pflash_split_uses_the_declared_bank_size_and_erased_tail() {
        let path =
            std::env::temp_dir().join(format!("fstart-qemu-banks-{}.bin", std::process::id()));
        std::fs::write(&path, [1, 2, 3, 4, 5, 6]).unwrap();
        let args = virt_flash_args(&path, 4).unwrap();
        assert_eq!(args.len(), 4);
        assert_eq!(
            std::fs::read(path.with_extension("bank0.pflash")).unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            std::fs::read(path.with_extension("bank1.pflash")).unwrap(),
            [5, 6, 0xff, 0xff]
        );
        assert!(virt_flash_args(&path, 2).is_err());
        assert!(virt_flash_args(&path, 0).is_err());
        assert_eq!(virt_flash_args(&path, 8).unwrap()[0], "-bios");
        for file in [
            path.with_extension("bank0.pflash"),
            path.with_extension("bank1.pflash"),
            path,
        ] {
            std::fs::remove_file(file).unwrap();
        }
    }

    #[test]
    fn x86_pflash_overlay_uses_physical_load_address() {
        let flash_base = 0xff00_0000u64;
        let paddr = flash_base + 0x20;
        // One PT_LOAD whose RAM vaddr differs from its flash paddr.
        let elf64 = {
            let mut elf = vec![0u8; 0x104];
            elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
            elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
            elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
            let ph = 64;
            elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes());
            elf[ph + 16..ph + 24].copy_from_slice(&0x0100_0000u64.to_le_bytes());
            elf[ph + 24..ph + 32].copy_from_slice(&paddr.to_le_bytes());
            elf[ph + 32..ph + 40].copy_from_slice(&4u64.to_le_bytes());
            (2, elf)
        };
        let elf32 = {
            let mut elf = vec![0u8; 0x104];
            elf[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
            elf[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
            elf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
            let ph = 52;
            elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[ph + 4..ph + 8].copy_from_slice(&0x100u32.to_le_bytes());
            elf[ph + 8..ph + 12].copy_from_slice(&0x0100_0000u32.to_le_bytes());
            elf[ph + 12..ph + 16].copy_from_slice(&(paddr as u32).to_le_bytes());
            elf[ph + 16..ph + 20].copy_from_slice(&4u32.to_le_bytes());
            (1, elf)
        };
        for (class, mut elf) in [elf64, elf32] {
            elf[0..4].copy_from_slice(b"\x7fELF");
            elf[4] = class;
            elf[5] = 1; // little-endian
            elf[6] = 1; // EV_CURRENT
            elf[0x100..0x104].copy_from_slice(b"data");
            let path = std::env::temp_dir().join(format!(
                "fstart-qemu-overlay-test-{}-{class}.elf",
                std::process::id()
            ));
            std::fs::write(&path, elf).unwrap();

            let mut pflash = vec![0xff; 0x100];
            overlay_x86_elf_segments(&path, &mut pflash, flash_base).unwrap();
            std::fs::remove_file(path).unwrap();

            assert_eq!(&pflash[0x20..0x24], b"data");
        }
    }
}
