use crate::assemble::AssembledImage;
use fstart_image_build::run::{RunArtifact, RunDisk, RunMedia, RunRecipe};
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

fn artifact<'a>(built: &'a AssembledImage, artifact: &RunArtifact) -> Result<&'a Path, String> {
    match artifact {
        RunArtifact::Step(step) => built
            .artifacts
            .get(step)
            .map(PathBuf::as_path)
            .ok_or_else(|| format!("run recipe attaches image step {step}, which was not built")),
        RunArtifact::Stage => built
            .stages
            .first()
            .map(|stage| stage.run_path.as_path())
            .ok_or_else(|| "run recipe attaches a stage, but none was built".into()),
    }
}

/// Launch QEMU exactly as `recipe` describes: machine, CPU and devices
/// are the platform's data; this function only renders argv and performs
/// the media attachment transforms (padding, bank splits, card
/// placement) the recipe parameterises.
pub fn run(
    recipe: &RunRecipe,
    built: &AssembledImage,
    external: &std::collections::BTreeMap<String, std::path::PathBuf>,
    disk: Option<&str>,
    memory: Option<&str>,
) -> Result<(), String> {
    let mut args: Vec<String> = vec!["-machine".into(), recipe.machine.clone()];
    if recipe.accel_kvm {
        let accel = std::env::var("FSTART_QEMU_ACCEL").ok();
        let use_kvm = match accel.as_deref() {
            Some("kvm") => true,
            Some("tcg") => false,
            _ => std::fs::File::open("/dev/kvm").is_ok(),
        };
        args.extend([
            "-accel".into(),
            if use_kvm { "kvm" } else { "tcg" }.into(),
            "-cpu".into(),
            recipe
                .cpu
                .clone()
                .unwrap_or_else(|| if use_kvm { "host" } else { "max" }.into()),
        ]);
    } else if let Some(cpu) = &recipe.cpu {
        args.extend(["-cpu".into(), cpu.clone()]);
    }
    if let Some(memory) = memory.or(recipe.memory.as_deref()) {
        args.extend(["-m".into(), memory.to_string()]);
    }
    if let Some(smp) = &recipe.smp {
        // `FSTART_QEMU_SMP` selects the vCPU count so MP/SMM flight
        // plans can be exercised with APs.
        let smp = std::env::var("FSTART_QEMU_SMP").unwrap_or_else(|_| smp.clone());
        args.extend(["-smp".into(), smp]);
    }
    if recipe.nographic {
        args.push("-nographic".into());
    }
    for media in &recipe.media {
        match media {
            RunMedia::Bios { artifact: kind } => {
                args.extend(["-bios".into(), artifact(built, kind)?.display().to_string()]);
            }
            RunMedia::Pflash {
                artifact: kind,
                unit,
                size,
                readonly,
            } => {
                let path = create_pflash_image(artifact(built, kind)?, *size as usize)?;
                args.extend([
                    "-drive".into(),
                    format!(
                        "if=pflash,file={},format=raw,unit={unit}{}",
                        path.display(),
                        if *readonly { ",readonly=on" } else { "" }
                    ),
                ]);
            }
            RunMedia::ExternalPflash { name, unit, size } => {
                let path = external.get(name).ok_or_else(|| {
                    format!("this machine requires input {name}, a complete {size}-byte pflash image (--input {name}=PATH)")
                })?;
                let actual = std::fs::metadata(path)
                    .map_err(|e| format!("failed to read {name}: {e}"))?
                    .len();
                if actual != *size {
                    return Err(format!("{name} is {actual} bytes; expected {size}"));
                }
                args.extend([
                    "-drive".into(),
                    format!(
                        "if=pflash,file={},format=raw,unit={unit},readonly=on",
                        path.display()
                    ),
                ]);
            }
            RunMedia::SplitPflash {
                artifact: kind,
                bank_size,
            } => {
                args.extend(split_pflash_args(
                    artifact(built, kind)?,
                    *bank_size as usize,
                )?);
            }
            RunMedia::SdCard {
                artifact: kind,
                offset,
                size,
            } => {
                let sd = create_sd_image(artifact(built, kind)?, *offset as usize, *size as usize)?;
                args.extend(["-sd".into(), sd.display().to_string()]);
            }
            RunMedia::Mtd { artifact: kind } => {
                args.extend([
                    "-drive".into(),
                    format!(
                        "file={},format=raw,if=mtd",
                        artifact(built, kind)?.display()
                    ),
                ]);
            }
        }
    }
    for device in &recipe.devices {
        args.extend(["-device".into(), device.clone()]);
    }
    args.extend(recipe.args.iter().cloned());

    if let Some(disk_path) = disk {
        let fmt = if disk_path.ends_with(".qcow2") {
            "qcow2"
        } else {
            "raw"
        };
        match recipe.disk {
            Some(RunDisk::Ide) => {
                let is_iso = disk_path.ends_with(".iso");
                let device_type = if is_iso { "ide-cd" } else { "ide-hd" };
                args.extend([
                    "-drive".into(),
                    format!(
                        "file={disk_path},id=hd0,if=none,format={fmt},readonly={}",
                        if is_iso { "on" } else { "off" }
                    ),
                    "-device".into(),
                    format!("{device_type},drive=hd0"),
                ]);
                eprintln!("[fstart] disk: {disk_path} (AHCI/{device_type}, format={fmt})");
            }
            Some(RunDisk::Nvme) => {
                args.extend([
                    "-drive".into(),
                    format!("file={disk_path},id=hd0,if=none,format={fmt}"),
                    "-device".into(),
                    "nvme,serial=fstartdisk0,drive=hd0".into(),
                ]);
                eprintln!("[fstart] disk: {disk_path} (NVMe, format={fmt})");
            }
            None => return Err("this board's run recipe has no disk attachment".into()),
        }
    }

    // Extra devices for one run, e.g. a swtpm-backed TPM:
    // FSTART_QEMU_EXTRA_ARGS="-chardev socket,id=chrtpm,path=... -tpmdev ..."
    if let Ok(extra) = std::env::var("FSTART_QEMU_EXTRA_ARGS") {
        args.extend(extra.split_whitespace().map(str::to_string));
    }

    let qemu_bin = find_qemu(&recipe.qemu);
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

/// Machines with two pflash banks (ARM `virt`: 64 MiB at 0x0 and
/// 0x0400_0000) only fill bank 0 from `-bios`, so images that place the
/// FFS in bank 1 (XIP assemble output) must be split into two drives.
fn split_pflash_args(binary: &Path, bank_size: usize) -> Result<Vec<String>, String> {
    let data = std::fs::read(binary).map_err(|e| format!("failed to read firmware image: {e}"))?;
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

/// An SD card image carrying the boot image at the boot ROM's offset
/// (the Allwinner BROM reads eGON from byte 0x2000), real-sized.
fn create_sd_image(binary: &Path, offset: usize, size: usize) -> Result<PathBuf, String> {
    let image = std::fs::read(binary).map_err(|e| format!("failed to read boot image: {e}"))?;
    if offset.checked_add(image.len()).is_none_or(|end| end > size) {
        return Err("boot image exceeds QEMU SD image".to_string());
    }
    let path = binary.with_extension("sd.img");
    let mut sd = vec![0u8; size];
    sd[offset..offset + image.len()].copy_from_slice(&image);
    std::fs::write(&path, sd).map_err(|e| format!("failed to create SD image: {e}"))?;
    Ok(path)
}
