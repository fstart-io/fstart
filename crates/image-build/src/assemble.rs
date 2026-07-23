use fstart_core::ffs::{
    Compression, FileType, SegmentFlags, SegmentKind, Signature, VerificationKey, ANCHOR_SIZE,
    FFS_MAGIC, FFS_VERSION,
};
use fstart_core::memory::{FlashLayout, IntelIfdFlashLayout, IntelIfdRegion};
use fstart_core::{
    BoardConfig, FdtSource, FirmwareImagePolicy, Platform, RunsFrom, SocImageFormat, StageLayout,
};
use fstart_ffs::builder::{
    build_image, ExternalInputFile, FfsImageConfig, InputFile, InputRegion, InputSegment,
};
use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::fs;
use std::path::{Path, PathBuf};

use crate::StageBinary;

pub fn assemble(
    workspace_root: &Path,
    board_dir: &Path,
    config: &BoardConfig,
    stage_binaries: &[StageBinary],
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
    fit_path: Option<&str>,
) -> Result<PathBuf, String> {
    eprintln!("[fstart] assembling FFS image for: {}", config.name);

    let (signing_key, verification_key) = get_or_create_dev_keys(board_dir, config)?;

    let mut ro_files = Vec::new();

    match &config.stages {
        StageLayout::Monolithic(mono) => {
            let stage = &stage_binaries[0];

            if let Ok(segs) = parse_elf_segments(&stage.path, Compression::None) {
                log_stage_segments("stage", &stage.path, &segs);
            }

            let bin_data = fs::read(&stage.run_path)
                .map_err(|e| format!("failed to read {}: {e}", stage.run_path.display()))?;

            ro_files.push(InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".flat".to_string(),
                    kind: SegmentKind::Code,
                    data: bin_data,
                    mem_size: None,
                    load_addr: mono.load_addr,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            });
        }
        StageLayout::MultiStage(stages) => {
            for (i, stage_bin) in stage_binaries.iter().enumerate() {
                if i == 0 {
                    let bin_data = fs::read(&stage_bin.run_path).map_err(|e| {
                        format!("failed to read {}: {e}", stage_bin.run_path.display())
                    })?;
                    match parse_elf_segments(&stage_bin.path, Compression::None) {
                        Ok(segs) => log_stage_segments(&stage_bin.name, &stage_bin.path, &segs),
                        Err(err) => eprintln!(
                            "[fstart] warning: failed to parse ELF for diagnostics ({}): {}",
                            stage_bin.path.display(),
                            err,
                        ),
                    }
                    ro_files.push(InputFile {
                        name: stage_bin.name.clone(),
                        file_type: FileType::StageCode,
                        segments: vec![InputSegment {
                            name: ".flat".to_string(),
                            kind: SegmentKind::Code,
                            data: bin_data,
                            mem_size: None,
                            load_addr: stage_bin.load_addr,
                            compression: Compression::None,
                            flags: SegmentFlags::CODE,
                        }],
                    });
                } else {
                    let stage_cfg = stages
                        .iter()
                        .find(|stage| stage.name.as_str() == stage_bin.name)
                        .ok_or_else(|| {
                            format!("stage '{}' missing from board config", stage_bin.name)
                        })?;
                    let compression = stage_cfg.compression;
                    if compression != Compression::None
                        && !stage_loaded_via_stage_load(stages, &stage_bin.name)
                    {
                        return Err(format!(
                            "stage '{}' requests {:?} compression but is not loaded by the stage-load helper",
                            stage_bin.name, compression
                        ));
                    };
                    let bin_data = fs::read(&stage_bin.run_path).map_err(|e| {
                        format!("failed to read {}: {e}", stage_bin.run_path.display())
                    })?;
                    eprintln!(
                        "[fstart] {}: flat binary, {} bytes, load_addr={:#x} (from {})",
                        stage_bin.name,
                        bin_data.len(),
                        stage_bin.load_addr,
                        stage_bin.run_path.display(),
                    );
                    ro_files.push(InputFile {
                        name: stage_bin.name.clone(),
                        file_type: FileType::StageCode,
                        segments: vec![InputSegment {
                            name: ".flat".to_string(),
                            kind: SegmentKind::Code,
                            data: bin_data,
                            mem_size: None,
                            load_addr: stage_bin.load_addr,
                            compression,
                            flags: SegmentFlags::CODE,
                        }],
                    });
                }
            }
        }
    }

    if let Some(ref microcode) = config.microcode {
        assemble_microcode(microcode, board_dir, &mut ro_files)?;
    }

    if let Some(ref payload) = config.payload {
        if payload.kind == fstart_core::PayloadKind::FitImage {
            assemble_fit_payload(payload, board_dir, fit_path.or(kernel_path), &mut ro_files)?;
        } else {
            assemble_linux_payload(
                payload,
                board_dir,
                kernel_path,
                firmware_path,
                &mut ro_files,
            )?;
        }

        if let FdtSource::Override(ref dtb_name) = payload.fdt {
            let dtb_path = board_dir.join(dtb_name.as_str());
            if dtb_path.exists() {
                let dtb_data =
                    fs::read(&dtb_path).map_err(|e| format!("failed to read DTB: {e}"))?;
                let dtb_load_addr = payload.dtb_addr.unwrap_or(0);

                eprintln!(
                    "[fstart] DTB: {} ({} bytes, load_addr={:#x})",
                    dtb_path.display(),
                    dtb_data.len(),
                    dtb_load_addr,
                );

                ro_files.push(InputFile {
                    name: dtb_name.to_string(),
                    file_type: FileType::Fdt,
                    segments: vec![InputSegment {
                        name: ".fdt".to_string(),
                        kind: SegmentKind::ReadOnlyData,
                        data: dtb_data,
                        mem_size: None,
                        load_addr: dtb_load_addr,
                        compression: Compression::None,
                        flags: SegmentFlags::RODATA,
                    }],
                });
            } else {
                eprintln!("[fstart] warning: DTB not found: {}", dtb_path.display());
            }
        }
    }

    validate_flash_layout(config, board_dir)?;

    let mut image_config = FfsImageConfig {
        keys: vec![verification_key],
        regions: ffs_input_regions(config, ro_files)?,
    };

    let compressed_anchor_slots = compressed_anchor_slots(&image_config.regions)?;
    let sign = |manifest_bytes: &[u8]| sign_with_ed25519(&signing_key, manifest_bytes);
    let ffs_image = build_image_with_static_compressed_anchors(
        &mut image_config,
        &compressed_anchor_slots,
        &sign,
    )?;

    let mut image_bytes = ffs_image.image;

    if config.soc_image_format == SocImageFormat::AllwinnerEgon {
        let bb_bin_path = &stage_binaries[0].run_path;
        let bb_bin =
            fs::read(bb_bin_path).map_err(|e| format!("failed to read bootblock .bin: {e}"))?;
        if bb_bin.len() < 0x14 {
            return Err("bootblock .bin too small to read eGON header".to_string());
        }
        let bootblock_size =
            u32::from_le_bytes([bb_bin[0x10], bb_bin[0x11], bb_bin[0x12], bb_bin[0x13]]);
        if bootblock_size == 0 {
            return Err("bootblock .bin has zero eGON length — was it patched?".to_string());
        }

        if stage_binaries.len() > 1 {
            let next_name = &stage_binaries[1].name;
            let loc = ffs_image
                .file_data
                .iter()
                .find(|f| f.name == *next_name)
                .ok_or_else(|| format!("next stage '{next_name}' not found in FFS file_data"))?;

            image_bytes[0x2C..0x30].copy_from_slice(&loc.data_offset.to_le_bytes());
            image_bytes[0x30..0x34].copy_from_slice(&loc.data_size.to_le_bytes());
            let ffs_total = image_bytes.len() as u32;
            image_bytes[0x34..0x38].copy_from_slice(&ffs_total.to_le_bytes());

            eprintln!(
                "[fstart] next stage '{}': offset={:#x}, size={:#x} ({} bytes)",
                next_name, loc.data_offset, loc.data_size, loc.data_size,
            );
        }

        crate::image::egon::patch_ffs(&mut image_bytes, bootblock_size)?;
    }

    let output_dir = workspace_root.join("target").join("ffs");
    fs::create_dir_all(&output_dir).map_err(|e| format!("failed to create output dir: {e}"))?;

    let image_path = output_dir.join(format!("{}.ffs", config.name));
    fs::write(&image_path, &image_bytes).map_err(|e| format!("failed to write FFS image: {e}"))?;

    eprintln!(
        "[fstart] FFS image: {} ({} bytes)",
        image_path.display(),
        image_bytes.len()
    );
    eprintln!(
        "[fstart] anchor at offset {} ({} bytes)",
        ffs_image.anchor_offset,
        ffs_image.anchor_bytes.len(),
    );

    let stage_count = stage_binaries.len();
    eprintln!(
        "[fstart] {} stage{} packaged into FFS",
        stage_count,
        if stage_count == 1 { "" } else { "s" }
    );

    let flash_layout_files = match &config.memory.flash_layout {
        Some(FlashLayout::IntelIfd(layout)) => {
            layout.regions.iter().any(|region| region.file.is_some())
        }
        Some(FlashLayout::X86Legacy(_)) | None => false,
    };
    if config.full_flash_image || flash_layout_files {
        let full_flash = FullFlashInput {
            config,
            board_dir,
            bootblock_elf: &stage_binaries[0].path,
            bootblock_bin: &stage_binaries[0].run_path,
            ffs_data: &image_bytes,
            ffs_anchor_offset: ffs_image.anchor_offset,
            ffs_path: &image_path,
        };
        return create_full_flash_image(full_flash);
    }

    Ok(image_path)
}

#[derive(Debug, Clone, Copy)]
struct CompressedAnchorSlot {
    region_idx: usize,
    file_idx: usize,
    segment_idx: usize,
    offset: usize,
}

fn build_image_with_static_compressed_anchors<F>(
    config: &mut FfsImageConfig,
    slots: &[CompressedAnchorSlot],
    sign: &F,
) -> Result<fstart_ffs::builder::FfsImage, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    if slots.is_empty() {
        return build_image(config, sign);
    }

    let mut patched_anchor: Option<Vec<u8>> = None;
    for _ in 0..8 {
        let image = build_image(config, sign)?;
        if patched_anchor.as_deref() == Some(image.anchor_bytes.as_slice()) {
            return Ok(image);
        }
        patch_compressed_anchor_slots(&mut config.regions, slots, &image.anchor_bytes)?;
        patched_anchor = Some(image.anchor_bytes);
    }

    Err("compressed FSTART_ANCHOR patching did not converge".to_string())
}

fn compressed_anchor_slots(regions: &[InputRegion]) -> Result<Vec<CompressedAnchorSlot>, String> {
    let mut slots = Vec::new();
    for (region_idx, region) in regions.iter().enumerate() {
        let files = match region {
            InputRegion::Container { files, .. }
            | InputRegion::ContainerWithExternal { files, .. } => files,
            InputRegion::Raw { .. } | InputRegion::ExternalRaw { .. } => continue,
        };

        for (file_idx, file) in files.iter().enumerate() {
            for (segment_idx, segment) in file.segments.iter().enumerate() {
                if segment.compression == Compression::None {
                    continue;
                }
                for offset in anchor_placeholder_offsets(&segment.data) {
                    slots.push(CompressedAnchorSlot {
                        region_idx,
                        file_idx,
                        segment_idx,
                        offset,
                    });
                }
            }
        }
    }
    Ok(slots)
}

fn anchor_placeholder_offsets(data: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut offset = 0usize;
    while offset + ANCHOR_SIZE <= data.len() {
        if data[offset..offset + FFS_MAGIC.len()] == FFS_MAGIC {
            let rest = &data[offset + FFS_MAGIC.len()..offset + ANCHOR_SIZE];
            let version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
            let manifest_off = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
            let manifest_sz = u32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
            let total_sz = u32::from_le_bytes([rest[12], rest[13], rest[14], rest[15]]);
            if version == FFS_VERSION && manifest_off == 0 && manifest_sz == 0 && total_sz == 0 {
                offsets.push(offset);
            }
        }
        offset += 8;
    }
    offsets
}

fn patch_compressed_anchor_slots(
    regions: &mut [InputRegion],
    slots: &[CompressedAnchorSlot],
    anchor_bytes: &[u8],
) -> Result<(), String> {
    if anchor_bytes.len() != ANCHOR_SIZE {
        return Err(format!(
            "patched anchor has {} bytes, expected {}",
            anchor_bytes.len(),
            ANCHOR_SIZE
        ));
    }

    for slot in slots {
        let Some(region) = regions.get_mut(slot.region_idx) else {
            return Err("compressed anchor slot region index is stale".to_string());
        };
        let files = match region {
            InputRegion::Container { files, .. }
            | InputRegion::ContainerWithExternal { files, .. } => files,
            InputRegion::Raw { .. } | InputRegion::ExternalRaw { .. } => {
                return Err("compressed anchor slot points at a raw region".to_string());
            }
        };
        let Some(file) = files.get_mut(slot.file_idx) else {
            return Err("compressed anchor slot file index is stale".to_string());
        };
        let Some(segment) = file.segments.get_mut(slot.segment_idx) else {
            return Err("compressed anchor slot segment index is stale".to_string());
        };
        let end = slot.offset + ANCHOR_SIZE;
        if end > segment.data.len() {
            return Err(format!(
                "compressed anchor slot in '{}'/'{}' exceeds segment size",
                file.name, segment.name
            ));
        }
        segment.data[slot.offset..end].copy_from_slice(anchor_bytes);
    }

    Ok(())
}

fn ffs_input_regions(
    config: &BoardConfig,
    ro_files: Vec<InputFile>,
) -> Result<Vec<InputRegion>, String> {
    let Some(FlashLayout::IntelIfd(layout)) = &config.memory.flash_layout else {
        if config.full_flash_image {
            let flash_image = firmware_image_from_policy(config)?.ok_or_else(|| {
                "full_flash_image requires a firmware image build policy".to_string()
            })?;
            let flash_size = flash_image.size;
            let flash_size_u32 = u32::try_from(flash_size)
                .map_err(|_| format!("flash size {flash_size:#x} exceeds FFS u32 limits"))?;
            let (files, external_files) =
                externalize_xip_bootblock(config, ro_files, flash_size_u32)?;
            if !external_files.is_empty() {
                return Ok(vec![InputRegion::ContainerWithExternal {
                    name: "ro".to_string(),
                    files,
                    external_files,
                    size: Some(flash_size_u32),
                }]);
            }
            return Ok(vec![InputRegion::Container {
                name: "ro".to_string(),
                files,
            }]);
        }
        return Ok(vec![InputRegion::Container {
            name: "ro".to_string(),
            files: ro_files,
        }]);
    };

    let bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
    if bios.size == 0 {
        return Err("Intel IFD BIOS region must not be empty".to_string());
    }

    let mut regions = Vec::new();
    for region in &layout.regions {
        if region.size == 0 {
            continue;
        }
        regions.push(InputRegion::ExternalRaw {
            name: region.kind.as_str().to_string(),
            offset: region.offset,
            size: region.size,
            fill: 0xff,
        });
    }
    let (files, external_files) = externalize_xip_bootblock(config, ro_files, bios.size)?;

    regions.push(InputRegion::ContainerWithExternal {
        name: "ro".to_string(),
        files,
        external_files,
        size: Some(bios.size),
    });

    Ok(regions)
}

fn externalize_xip_bootblock(
    config: &BoardConfig,
    mut files: Vec<InputFile>,
    container_size: u32,
) -> Result<(Vec<InputFile>, Vec<ExternalInputFile>), String> {
    let first_stage_is_xip = match &config.stages {
        StageLayout::MultiStage(stages) => stages
            .first()
            .is_some_and(|stage| stage.runs_from == RunsFrom::Rom),
        _ => false,
    };
    if config.platform != Platform::X86_64 || !first_stage_is_xip || files.is_empty() {
        return Ok((files, Vec::new()));
    }

    let image_base = if let Some(FlashLayout::IntelIfd(layout)) = &config.memory.flash_layout {
        let bios = layout
            .bios_region()
            .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
        layout.base() + u64::from(bios.offset)
    } else {
        firmware_image_from_policy(config)?
            .and_then(|image| image.contiguous_window())
            .map(|window| window.cpu_base)
            .ok_or_else(|| {
                "x86 XIP bootblock requires a contiguous firmware image build policy".to_string()
            })?
    };
    let mut bootblock = files.remove(0);
    let bootblock_size = input_file_stored_size(&bootblock)?;
    if bootblock_size > container_size {
        return Err(format!(
            "bootblock size {bootblock_size:#x} exceeds container size {container_size:#x}"
        ));
    }
    let bootblock_offset = container_size - bootblock_size;

    let mut segment_offset = 0u64;
    for segment in &mut bootblock.segments {
        segment.load_addr = image_base + u64::from(bootblock_offset) + segment_offset;
        segment_offset += u64::try_from(segment.data.len()).map_err(|_| {
            format!(
                "file '{}' segment '{}' is too large",
                bootblock.name, segment.name
            )
        })?;
    }

    Ok((
        files,
        vec![ExternalInputFile {
            name: bootblock.name,
            file_type: bootblock.file_type,
            offset: bootblock_offset,
            segments: bootblock.segments,
        }],
    ))
}

fn firmware_image_from_policy(
    config: &BoardConfig,
) -> Result<Option<fstart_core::services::FirmwareImage>, String> {
    match config.build.firmware_image {
        FirmwareImagePolicy::None => Ok(None),
        FirmwareImagePolicy::MemoryMapped { cpu_base, size } => Ok(Some(
            fstart_core::services::FirmwareImage::single_window(cpu_base, size),
        )),
        FirmwareImagePolicy::Auto => Ok(config
            .memory
            .firmware_window()
            .map(|(base, size)| fstart_core::services::FirmwareImage::single_window(base, size))),
    }
}

fn input_file_stored_size(file: &InputFile) -> Result<u32, String> {
    file.segments.iter().try_fold(0u32, |acc, segment| {
        let len = u32::try_from(segment.data.len()).map_err(|_| {
            format!(
                "file '{}' segment '{}' is too large",
                file.name, segment.name
            )
        })?;
        acc.checked_add(len)
            .ok_or_else(|| format!("file '{}' size overflows u32", file.name))
    })
}

struct FullFlashInput<'a> {
    config: &'a BoardConfig,
    board_dir: &'a Path,
    bootblock_elf: &'a Path,
    bootblock_bin: &'a Path,
    ffs_data: &'a [u8],
    ffs_anchor_offset: usize,
    ffs_path: &'a Path,
}

/// Compose a non-x86 XIP image from the linked stage and its separate FFS
/// window. The two windows are board data: no board-name dispatch is needed.
fn create_xip_flash_image(
    config: &BoardConfig,
    bootblock_elf: &Path,
    bootblock_bin: &Path,
    ffs_data: &[u8],
    ffs_anchor_offset: usize,
    ffs_path: &Path,
) -> Result<PathBuf, String> {
    let flash_image = config
        .build
        .flash_image
        .and_then(policy_window)
        .and_then(|image| image.contiguous_window())
        .ok_or_else(|| "XIP flash image requires a contiguous flash_image policy".to_string())?;
    let ffs_image = firmware_image_from_policy(config)?
        .and_then(|image| image.contiguous_window())
        .ok_or_else(|| "XIP flash image requires a contiguous firmware image policy".to_string())?;
    let flash_end = flash_image
        .cpu_base
        .checked_add(flash_image.size)
        .ok_or_else(|| "XIP flash image window overflows".to_string())?;
    let ffs_end = ffs_image
        .cpu_base
        .checked_add(ffs_image.size)
        .ok_or_else(|| "FFS window overflows".to_string())?;
    if ffs_image.cpu_base < flash_image.cpu_base || ffs_end > flash_end {
        return Err("FFS window lies outside the composite flash image".to_string());
    }
    if ffs_data.len() > ffs_image.size as usize {
        return Err(format!(
            "FFS image ({} bytes) exceeds its firmware window ({} bytes)",
            ffs_data.len(),
            ffs_image.size
        ));
    }

    let elf_data = fs::read(bootblock_elf).map_err(|e| {
        format!(
            "failed to read bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;
    let segments = elf_load_segments(&elf_data, bootblock_elf)?;
    let mut image = vec![0xff; flash_image.size as usize];
    let mut stage_segment_count = 0;
    for segment in segments.into_iter().filter(|segment| segment.filesz != 0) {
        let segment_end = segment
            .paddr
            .checked_add(segment.filesz)
            .ok_or_else(|| "stage ELF segment address overflows".to_string())?;
        if segment.paddr < flash_image.cpu_base || segment_end > flash_end {
            continue;
        }
        let dst_start = usize::try_from(segment.paddr - flash_image.cpu_base)
            .map_err(|_| "stage ELF segment offset is too large".to_string())?;
        let size = usize::try_from(segment.filesz)
            .map_err(|_| "stage ELF segment size is too large".to_string())?;
        let src_start = usize::try_from(segment.offset)
            .map_err(|_| "stage ELF segment file offset is too large".to_string())?;
        let src_end = src_start
            .checked_add(size)
            .ok_or_else(|| "stage ELF segment file range overflows".to_string())?;
        if src_end > elf_data.len() {
            return Err("stage ELF segment extends past end of file".to_string());
        }
        image[dst_start..dst_start + size].copy_from_slice(&elf_data[src_start..src_end]);
        stage_segment_count += 1;
    }
    let ffs_start = usize::try_from(ffs_image.cpu_base - flash_image.cpu_base)
        .map_err(|_| "FFS offset is too large".to_string())?;
    if stage_segment_count == 0 {
        // A RAM-linked stage is still flashed at offset zero for its reset
        // stub to relocate. Its flat binary is already contiguous by PT_LOAD
        // address, unlike an ELF whose addresses intentionally lie in RAM.
        let stage = fs::read(bootblock_bin).map_err(|e| {
            format!(
                "failed to read RAM-linked bootblock binary {}: {e}",
                bootblock_bin.display()
            )
        })?;
        if stage.len() > ffs_start {
            return Err(format!(
                "RAM-linked stage ({} bytes) overlaps the FFS window at {ffs_start:#x}",
                stage.len()
            ));
        }
        image[..stage.len()].copy_from_slice(&stage);
        eprintln!("[fstart] XIP flash: placed RAM-linked stage at flash offset zero");
    }

    let ffs_end = ffs_start
        .checked_add(ffs_data.len())
        .ok_or_else(|| "FFS range overflows composite flash image".to_string())?;
    image[ffs_start..ffs_end].copy_from_slice(ffs_data);
    patch_stage_anchor(&mut image, ffs_data, ffs_anchor_offset)?;

    let mib = image.len() / (1024 * 1024);
    let out_path = ffs_path.with_file_name(format!("{}-{mib}m.pflash", config.name));
    fs::write(&out_path, &image).map_err(|e| {
        format!(
            "failed to write XIP flash image {}: {e}",
            out_path.display()
        )
    })?;
    eprintln!(
        "[fstart] XIP flash image: {} ({} bytes, FFS {} bytes at offset {ffs_start:#x})",
        out_path.display(),
        image.len(),
        ffs_data.len(),
    );
    Ok(out_path)
}

fn patch_stage_anchor(
    image: &mut [u8],
    ffs_data: &[u8],
    ffs_anchor_offset: usize,
) -> Result<(), String> {
    let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
    let anchor_end = ffs_anchor_offset
        .checked_add(anchor_size)
        .ok_or_else(|| "FFS anchor range overflows".to_string())?;
    let anchor = ffs_data
        .get(ffs_anchor_offset..anchor_end)
        .ok_or_else(|| "FFS anchor lies outside the FFS image".to_string())?;
    let placeholder = fstart_core::ffs::AnchorBlock::placeholder();
    let placeholder = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_core::ffs::AnchorBlock as *const u8,
            anchor_size,
        )
    };
    let offset = image
        .windows(anchor_size)
        .position(|window| window == placeholder)
        .ok_or_else(|| "stage anchor placeholder not found in composite flash image".to_string())?;
    image[offset..offset + anchor_size].copy_from_slice(anchor);
    eprintln!("[fstart] XIP flash: patched stage anchor at offset {offset:#x}");
    Ok(())
}

fn policy_window(
    policy: fstart_core::FirmwareImagePolicy,
) -> Option<fstart_core::services::FirmwareImage> {
    match policy {
        fstart_core::FirmwareImagePolicy::MemoryMapped { cpu_base, size } => Some(
            fstart_core::services::FirmwareImage::single_window(cpu_base, size),
        ),
        fstart_core::FirmwareImagePolicy::Auto | fstart_core::FirmwareImagePolicy::None => None,
    }
}

fn create_full_flash_image(input: FullFlashInput<'_>) -> Result<PathBuf, String> {
    let FullFlashInput {
        config,
        board_dir,
        bootblock_elf,
        bootblock_bin,
        ffs_data,
        ffs_anchor_offset,
        ffs_path,
    } = input;

    if let Some(FlashLayout::IntelIfd(layout)) = &config.memory.flash_layout {
        return create_intel_ifd_flash_image(
            config,
            board_dir,
            layout,
            bootblock_elf,
            bootblock_bin,
            ffs_data,
            ffs_anchor_offset,
            ffs_path,
        );
    }

    if config.build.flash_image.is_some() {
        return create_xip_flash_image(
            config,
            bootblock_elf,
            bootblock_bin,
            ffs_data,
            ffs_anchor_offset,
            ffs_path,
        );
    }

    let (flash_base, flash_size) = match &config.memory.flash_layout {
        Some(FlashLayout::X86Legacy(layout)) => (layout.base(), layout.size() as usize),
        Some(FlashLayout::IntelIfd(_)) => unreachable!("IFD layout handled above"),
        None => {
            let flash_image = firmware_image_from_policy(config)?.ok_or_else(|| {
                "full_flash_image requires a firmware image build policy".to_string()
            })?;
            let flash_window = flash_image.contiguous_window().ok_or_else(|| {
                "full_flash_image requires a contiguous firmware image window".to_string()
            })?;
            (flash_window.cpu_base, flash_image.size as usize)
        }
    };

    if ffs_data.len() > flash_size {
        return Err(format!(
            "FFS image ({} bytes) exceeds flash size ({} bytes)",
            ffs_data.len(),
            flash_size
        ));
    }

    let mut image = vec![0xffu8; flash_size];

    image[..ffs_data.len()].copy_from_slice(ffs_data);

    let elf_data = fs::read(bootblock_elf).map_err(|e| {
        format!(
            "failed to read bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;
    let load_segments = elf_load_segments(&elf_data, bootblock_elf)?;

    let mut first_flash_load: Option<usize> = None;
    for segment in load_segments {
        if segment.filesz == 0 {
            continue;
        }
        let paddr = segment.paddr;
        if paddr < flash_base {
            continue;
        }
        let off = (paddr - flash_base) as usize;
        let size = segment.filesz as usize;
        if off + size > flash_size {
            return Err(format!(
                "bootblock segment paddr={paddr:#x} size={size:#x} outside flash image"
            ));
        }
        first_flash_load = Some(first_flash_load.map_or(off, |first| first.min(off)));
        eprintln!(
            "[fstart] full flash: bootblock segment paddr={paddr:#x} -> offset={off:#x} size={size:#x}"
        );
    }

    let bootblock_data = fs::read(bootblock_bin).map_err(|e| {
        format!(
            "failed to read bootblock flat binary {}: {e}",
            bootblock_bin.display()
        )
    })?;
    if bootblock_data.len() > flash_size {
        return Err(format!(
            "bootblock flat binary is {} bytes, larger than flash size {}",
            bootblock_data.len(),
            flash_size
        ));
    }
    let xip_offset = flash_size - bootblock_data.len();
    if ffs_data.len() > xip_offset {
        return Err(format!(
            "FFS image ({} bytes) overlaps top-aligned bootblock at flash offset {xip_offset:#x}",
            ffs_data.len()
        ));
    }
    if let Some(first_flash_load) = first_flash_load {
        if xip_offset != first_flash_load {
            return Err(format!(
                "top-aligned bootblock offset {xip_offset:#x} does not match first ELF load offset {first_flash_load:#x}"
            ));
        }
    }
    image[xip_offset..xip_offset + bootblock_data.len()].copy_from_slice(&bootblock_data);
    eprintln!(
        "[fstart] full flash: bootblock flat binary -> offset={xip_offset:#x} size={:#x}",
        bootblock_data.len()
    );

    let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
    if ffs_anchor_offset + anchor_size > ffs_data.len() {
        return Err(format!(
            "FFS anchor offset {ffs_anchor_offset:#x} outside FFS image"
        ));
    }
    let placeholder = fstart_core::ffs::AnchorBlock::placeholder();
    let placeholder_bytes = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_core::ffs::AnchorBlock as *const u8,
            anchor_size,
        )
    };
    let xip_anchor = image
        .windows(placeholder_bytes.len())
        .position(|w| w == placeholder_bytes)
        .ok_or_else(|| {
            "bootblock XIP anchor placeholder not found in full flash image".to_string()
        })?;
    let mut xip_anchor_block = unsafe {
        core::ptr::read_unaligned(
            ffs_data[ffs_anchor_offset..].as_ptr() as *const fstart_core::ffs::AnchorBlock
        )
    };
    xip_anchor_block.anchor_offset = xip_anchor as u32;
    let mut anchor = vec![0u8; anchor_size];
    xip_anchor_block.write_to(&mut anchor);
    image[xip_anchor..xip_anchor + anchor_size].copy_from_slice(&anchor);
    eprintln!(
        "[fstart] full flash: patched XIP anchor at offset {xip_anchor:#x} from FFS offset {ffs_anchor_offset:#x}"
    );

    let mib = flash_size / (1024 * 1024);
    let out_path = ffs_path.with_file_name(format!("{}-{}m.pflash", config.name, mib));
    fs::write(&out_path, &image).map_err(|e| {
        format!(
            "failed to write full flash image {}: {e}",
            out_path.display()
        )
    })?;
    eprintln!(
        "[fstart] full flash image: {} ({} bytes, FFS {} bytes at offset 0)",
        out_path.display(),
        flash_size,
        ffs_data.len()
    );
    Ok(out_path)
}

#[allow(clippy::too_many_arguments)]
fn create_intel_ifd_flash_image(
    config: &BoardConfig,
    board_dir: &Path,
    layout: &IntelIfdFlashLayout,
    bootblock_elf: &Path,
    bootblock_bin: &Path,
    ffs_data: &[u8],
    ffs_anchor_offset: usize,
    ffs_path: &Path,
) -> Result<PathBuf, String> {
    let bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
    let bios_end = bios
        .offset
        .checked_add(bios.size)
        .ok_or_else(|| "Intel IFD BIOS region overflows u32".to_string())?;
    if bios_end > layout.size() {
        return Err(format!(
            "Intel IFD BIOS region [{:#x}..{:#x}) exceeds flash size {:#x}",
            bios.offset,
            bios_end,
            layout.size()
        ));
    }
    if ffs_data.len() > bios.size as usize {
        return Err(format!(
            "FFS image ({} bytes) exceeds Intel IFD BIOS region ({} bytes)",
            ffs_data.len(),
            bios.size
        ));
    }

    let mut image = vec![0xffu8; layout.size() as usize];

    for region in &layout.regions {
        let Some(file) = &region.file else {
            continue;
        };
        let path = resolve_board_path(board_dir, file.as_str());
        let data = fs::read(&path)
            .map_err(|e| format!("failed to read flash region {}: {e}", path.display()))?;
        if data.len() > region.size as usize {
            return Err(format!(
                "flash region {} file {} is {} bytes, larger than region size {}",
                region.kind.as_str(),
                path.display(),
                data.len(),
                region.size
            ));
        }
        let start = region.offset as usize;
        let end = start + region.size as usize;
        if end > image.len() {
            return Err(format!(
                "flash region {} [{:#x}..{:#x}) exceeds flash size {:#x}",
                region.kind.as_str(),
                region.offset,
                region.offset + region.size,
                layout.size()
            ));
        }
        image[start..start + data.len()].copy_from_slice(&data);
        eprintln!(
            "[fstart] flash region {}: {} ({} bytes at offset {:#x})",
            region.kind.as_str(),
            path.display(),
            data.len(),
            region.offset
        );
    }

    let bios_start = bios.offset as usize;
    image[bios_start..bios_start + ffs_data.len()].copy_from_slice(ffs_data);

    let elf_data = fs::read(bootblock_elf).map_err(|e| {
        format!(
            "failed to read bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;
    let load_segments = elf_load_segments(&elf_data, bootblock_elf)?;

    let mut first_flash_load: Option<usize> = None;
    for segment in load_segments {
        if segment.filesz == 0 {
            continue;
        }
        let paddr = segment.paddr;
        if paddr < layout.base() || paddr >= layout.end() {
            continue;
        }
        let off = (paddr - layout.base()) as usize;
        let size = segment.filesz as usize;
        if off + size > image.len() {
            return Err(format!(
                "bootblock segment paddr={paddr:#x} size={size:#x} outside Intel IFD flash image"
            ));
        }
        first_flash_load = Some(first_flash_load.map_or(off, |first| first.min(off)));
        eprintln!(
            "[fstart] Intel IFD full flash: bootblock segment paddr={paddr:#x} -> offset={off:#x} size={size:#x}"
        );
    }

    let bootblock_data = fs::read(bootblock_bin).map_err(|e| {
        format!(
            "failed to read bootblock flat binary {}: {e}",
            bootblock_bin.display()
        )
    })?;
    if bootblock_data.len() > bios.size as usize {
        return Err(format!(
            "bootblock flat binary is {} bytes, larger than BIOS region size {}",
            bootblock_data.len(),
            bios.size
        ));
    }
    let xip_offset = bios_end as usize - bootblock_data.len();
    let ffs_end = bios_start + ffs_data.len();
    if ffs_end > xip_offset {
        return Err(format!(
            "BIOS FFS image [{bios_start:#x}..{ffs_end:#x}) overlaps top-aligned bootblock at flash offset {xip_offset:#x}"
        ));
    }
    if let Some(first_flash_load) = first_flash_load {
        if xip_offset != first_flash_load {
            return Err(format!(
                "top-aligned bootblock offset {xip_offset:#x} does not match first ELF load offset {first_flash_load:#x}"
            ));
        }
    }
    image[xip_offset..xip_offset + bootblock_data.len()].copy_from_slice(&bootblock_data);
    eprintln!(
        "[fstart] Intel IFD full flash: bootblock flat binary -> offset={xip_offset:#x} size={:#x}",
        bootblock_data.len()
    );

    patch_xip_anchor(&mut image, ffs_data, ffs_anchor_offset, bios.offset)?;

    let mib = layout.size() as usize / (1024 * 1024);
    let out_path = ffs_path.with_file_name(format!("{}-{}m.pflash", config.name, mib));
    fs::write(&out_path, &image).map_err(|e| {
        format!(
            "failed to write Intel IFD flash image {}: {e}",
            out_path.display()
        )
    })?;
    eprintln!(
        "[fstart] Intel IFD full flash image: {} ({} bytes, BIOS FFS {} bytes at offset {:#x})",
        out_path.display(),
        image.len(),
        ffs_data.len(),
        bios.offset
    );
    Ok(out_path)
}

fn patch_xip_anchor(
    image: &mut [u8],
    ffs_data: &[u8],
    ffs_anchor_offset: usize,
    image_base_delta: u32,
) -> Result<(), String> {
    let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
    if ffs_anchor_offset + anchor_size > ffs_data.len() {
        return Err(format!(
            "FFS anchor offset {ffs_anchor_offset:#x} outside FFS image"
        ));
    }
    let placeholder = fstart_core::ffs::AnchorBlock::placeholder();
    let placeholder_bytes = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_core::ffs::AnchorBlock as *const u8,
            anchor_size,
        )
    };
    let xip_anchor = image
        .windows(placeholder_bytes.len())
        .position(|w| w == placeholder_bytes)
        .ok_or_else(|| {
            "bootblock XIP anchor placeholder not found in full flash image".to_string()
        })?;
    let mut xip_anchor_block = unsafe {
        core::ptr::read_unaligned(
            ffs_data[ffs_anchor_offset..].as_ptr() as *const fstart_core::ffs::AnchorBlock
        )
    };
    xip_anchor_block.anchor_offset = (xip_anchor as u32)
        .checked_sub(image_base_delta)
        .ok_or_else(|| "XIP anchor lies before BIOS image base".to_string())?;
    let mut anchor = vec![0u8; anchor_size];
    xip_anchor_block.write_to(&mut anchor);
    image[xip_anchor..xip_anchor + anchor_size].copy_from_slice(&anchor);
    eprintln!(
        "[fstart] full flash: patched XIP anchor at offset {xip_anchor:#x} \
         (image-relative {:#x}) from FFS offset {ffs_anchor_offset:#x}",
        xip_anchor_block.anchor_offset
    );
    Ok(())
}

fn validate_flash_layout(config: &BoardConfig, board_dir: &Path) -> Result<(), String> {
    let Some(FlashLayout::IntelIfd(layout)) = &config.memory.flash_layout else {
        return Ok(());
    };

    let _bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;

    let aperture_end = layout
        .base()
        .checked_add(u64::from(layout.size()))
        .ok_or_else(|| "Intel IFD flash aperture overflows u64".to_string())?;
    for region in &layout.regions {
        let region_end = region
            .offset
            .checked_add(region.size)
            .ok_or_else(|| format!("Intel IFD region {} overflows u32", region.kind.as_str()))?;
        if region_end > layout.size() {
            return Err(format!(
                "Intel IFD region {} [{:#x}..{:#x}) exceeds flash size {:#x}",
                region.kind.as_str(),
                region.offset,
                region_end,
                layout.size()
            ));
        }
        let mapped_start = layout.base() + u64::from(region.offset);
        let mapped_end = layout.base() + u64::from(region_end);
        if mapped_start < layout.base() || mapped_end > aperture_end {
            return Err(format!(
                "Intel IFD region {} maps outside flash aperture",
                region.kind.as_str()
            ));
        }
    }

    let descriptor = layout
        .regions
        .iter()
        .find(|region| region.kind == IntelIfdRegion::Descriptor)
        .and_then(|region| region.file.as_ref().map(|file| (region, file)));
    if let Some((_region, file)) = descriptor {
        let path = resolve_board_path(board_dir, file.as_str());
        let data = fs::read(&path)
            .map_err(|e| format!("failed to read Intel descriptor {}: {e}", path.display()))?;
        let parsed = parse_intel_ifd(&data)?;
        if parsed.flash_size != layout.size() {
            return Err(format!(
                "Intel descriptor {} flash size is {:#x}, but board metadata declares {:#x}",
                path.display(),
                parsed.flash_size,
                layout.size()
            ));
        }
        for region in &layout.regions {
            let Some(idx) = region.kind.flreg_index() else {
                continue;
            };
            let Some((offset, size)) = parsed.regions.get(idx).copied().flatten() else {
                if region.size == 0 {
                    continue;
                }
                return Err(format!(
                    "Intel descriptor {} has no FLREG{} for configured {} region",
                    path.display(),
                    idx,
                    region.kind.as_str()
                ));
            };
            if offset != region.offset || size != region.size {
                return Err(format!(
                    "Intel descriptor {} FLREG{} ({}) is offset={offset:#x} size={size:#x}, \
                     but board metadata declares offset={:#x} size={:#x}",
                    path.display(),
                    idx,
                    region.kind.as_str(),
                    region.offset,
                    region.size
                ));
            }
        }
        eprintln!(
            "[fstart] Intel descriptor layout validated: {}",
            path.display()
        );
    }

    Ok(())
}

fn resolve_board_path(board_dir: &Path, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        board_dir.join(path)
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedIntelIfd {
    flash_size: u32,
    regions: [Option<(u32, u32)>; 16],
}

fn parse_intel_ifd(data: &[u8]) -> Result<ParsedIntelIfd, String> {
    let sig_offset = data
        .windows(4)
        .enumerate()
        .step_by(4)
        .find_map(|(offset, bytes)| {
            let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (value == 0x0ff0_a55a).then_some(offset)
        })
        .ok_or_else(|| "Intel flash descriptor signature 0x0ff0a55a not found".to_string())?;

    if sig_offset + 8 > data.len() {
        return Err("Intel flash descriptor too small for FLMAP0".to_string());
    }
    let flmap0 = u32::from_le_bytes([
        data[sig_offset + 4],
        data[sig_offset + 5],
        data[sig_offset + 6],
        data[sig_offset + 7],
    ]);

    let fcba = ((flmap0 & 0xff) << 4) as usize;
    let component_count = ((flmap0 >> 8) & 0x3) + 1;
    if fcba + 4 > data.len() {
        return Err(format!(
            "Intel flash descriptor FCBA {fcba:#x} outside descriptor file"
        ));
    }
    let flcomp = u32::from_le_bytes([data[fcba], data[fcba + 1], data[fcba + 2], data[fcba + 3]]);
    let mut flash_size = 1u32 << (19 + (flcomp & 0x7));
    if component_count > 1 {
        flash_size = flash_size.saturating_add(1u32 << (19 + ((flcomp >> 3) & 0x7)));
    }

    let frba = (((flmap0 >> 16) & 0xff) << 4) as usize;
    if frba + 4 > data.len() {
        return Err(format!(
            "Intel flash descriptor FRBA {frba:#x} outside descriptor file"
        ));
    }

    let mut regions = [None; 16];
    for (idx, slot) in regions.iter_mut().enumerate() {
        let off = frba + idx * 4;
        if off + 4 > data.len() {
            break;
        }
        let flreg = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let base = (flreg & 0x7fff) << 12;
        let limit = ((flreg >> 16) & 0x7fff) << 12 | 0xfff;
        if limit >= base {
            *slot = Some((base, limit - base + 1));
        }
    }

    Ok(ParsedIntelIfd {
        flash_size,
        regions,
    })
}

fn assemble_microcode(
    microcode: &fstart_core::board::MicrocodeConfig,
    board_dir: &Path,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    match microcode {
        fstart_core::board::MicrocodeConfig::Intel(config) => {
            let mut blob = Vec::new();
            for file in &config.files {
                let path = Path::new(file.as_str());
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    board_dir.join(path)
                };
                let data = fs::read(&path).map_err(|e| {
                    format!("failed to read Intel microcode {}: {e}", path.display())
                })?;
                eprintln!(
                    "[fstart] Intel microcode: {} ({} bytes)",
                    path.display(),
                    data.len()
                );
                blob.extend_from_slice(&data);
            }

            if blob.is_empty() {
                return Err("Intel microcode config did not include any bytes".to_string());
            }

            eprintln!(
                "[fstart] Intel microcode blob: {} bytes (early={}, mp={})",
                blob.len(),
                config.early,
                config.mp
            );
            ro_files.push(InputFile {
                name: "cpu_microcode_blob.bin".to_string(),
                file_type: FileType::CpuMicrocode,
                segments: vec![InputSegment {
                    name: ".microcode".to_string(),
                    kind: SegmentKind::ReadOnlyData,
                    data: blob,
                    mem_size: None,
                    load_addr: 0,
                    compression: Compression::None,
                    flags: SegmentFlags::RODATA,
                }],
            });
        }
    }

    Ok(())
}

fn stage_loaded_via_stage_load(stages: &[fstart_core::StageConfig], stage_name: &str) -> bool {
    stages.iter().any(|stage| {
        stage
            .build
            .load_next_stage
            .as_ref()
            .is_some_and(|next_stage| next_stage.as_str() == stage_name)
    })
}

fn assemble_fit_payload(
    payload: &fstart_core::PayloadConfig,
    board_dir: &Path,
    kernel_override: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    let fit_parse = payload
        .fit_parse
        .unwrap_or(fstart_core::FitParseMode::Buildtime);

    let fit_path = kernel_override.map(PathBuf::from).or_else(|| {
        payload
            .fit_file
            .as_ref()
            .map(|f| board_dir.join(f.as_str()))
    });

    let fit_path = match fit_path {
        Some(p) => p,
        None => {
            eprintln!(
                "[fstart] warning: FIT image: no fit_file specified and no --kernel override"
            );
            return Ok(());
        }
    };

    if !fit_path.exists() {
        eprintln!(
            "[fstart] warning: FIT image not found: {}",
            fit_path.display()
        );
        return Ok(());
    }

    let fit_data = fs::read(&fit_path).map_err(|e| format!("failed to read FIT image: {e}"))?;
    eprintln!(
        "[fstart] FIT image: {} ({} bytes)",
        fit_path.display(),
        fit_data.len(),
    );

    let fit = fstart_boot::fit::FitImage::parse(&fit_data)
        .map_err(|e| format!("failed to parse FIT image: {e:?}"))?;

    if let Some(desc) = fit.description() {
        eprintln!("[fstart] FIT description: {desc}");
    }

    let config_name = payload.fit_config.as_ref().map(|s| s.as_str());

    match fit_parse {
        fstart_core::FitParseMode::Runtime => {
            eprintln!("[fstart] FIT mode: runtime (embedding whole .itb in FFS)");

            ro_files.push(InputFile {
                name: "fit_image".to_string(),
                file_type: FileType::FitImage,
                segments: vec![InputSegment {
                    name: ".fit".to_string(),
                    kind: SegmentKind::ReadOnlyData,
                    data: fit_data,
                    mem_size: None,
                    load_addr: 0, // parsed in-place, not loaded to fixed address
                    compression: Compression::None,
                    flags: SegmentFlags::RODATA,
                }],
            });
        }
        fstart_core::FitParseMode::Buildtime => {
            eprintln!("[fstart] FIT mode: buildtime (extracting components)");

            let boot = fit
                .resolve_boot_images(config_name)
                .map_err(|e| format!("failed to resolve FIT config: {e:?}"))?;

            eprintln!(
                "[fstart] FIT config: {}",
                boot.config.description().unwrap_or(boot.config.name())
            );

            let kernel_data = boot
                .kernel
                .data()
                .map_err(|e| format!("failed to read kernel from FIT: {e:?}"))?;
            let kernel_load = boot
                .kernel
                .load_addr()
                .unwrap_or(payload.kernel_load_addr.unwrap_or(0));

            eprintln!(
                "[fstart] FIT kernel: '{}' ({} bytes, load={:#x})",
                boot.kernel.name(),
                kernel_data.len(),
                kernel_load,
            );

            ro_files.push(InputFile {
                name: boot.kernel.name().to_string(),
                file_type: FileType::Payload,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: kernel_data.to_vec(),
                    mem_size: None,
                    load_addr: kernel_load,
                    compression: Compression::Lz4,
                    flags: SegmentFlags::CODE,
                }],
            });

            if let Some(ref rd) = boot.ramdisk {
                if let Ok(rd_data) = rd.data() {
                    let rd_load = rd.load_addr().unwrap_or(0);
                    eprintln!(
                        "[fstart] FIT ramdisk: '{}' ({} bytes, load={:#x})",
                        rd.name(),
                        rd_data.len(),
                        rd_load,
                    );

                    ro_files.push(InputFile {
                        name: rd.name().to_string(),
                        file_type: FileType::Data,
                        segments: vec![InputSegment {
                            name: ".data".to_string(),
                            kind: SegmentKind::ReadOnlyData,
                            data: rd_data.to_vec(),
                            mem_size: None,
                            load_addr: rd_load,
                            compression: Compression::Lz4,
                            flags: SegmentFlags::RODATA,
                        }],
                    });
                }
            }

            if let Some(ref fdt_img) = boot.fdt {
                if let Ok(fdt_data) = fdt_img.data() {
                    let fdt_load = fdt_img.load_addr().unwrap_or(payload.dtb_addr.unwrap_or(0));
                    eprintln!(
                        "[fstart] FIT fdt: '{}' ({} bytes, load={:#x})",
                        fdt_img.name(),
                        fdt_data.len(),
                        fdt_load,
                    );

                    ro_files.push(InputFile {
                        name: fdt_img.name().to_string(),
                        file_type: FileType::Fdt,
                        segments: vec![InputSegment {
                            name: ".fdt".to_string(),
                            kind: SegmentKind::ReadOnlyData,
                            data: fdt_data.to_vec(),
                            mem_size: None,
                            load_addr: fdt_load,
                            compression: Compression::None,
                            flags: SegmentFlags::RODATA,
                        }],
                    });
                }
            }
        }
    }

    add_firmware_blob(payload, board_dir, None, ro_files)?;

    Ok(())
}

fn assemble_linux_payload(
    payload: &fstart_core::PayloadConfig,
    board_dir: &Path,
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    add_firmware_blob(payload, board_dir, firmware_path, ro_files)?;

    let kernel_file = kernel_path.map(PathBuf::from).or_else(|| {
        payload
            .kernel_file
            .as_ref()
            .map(|kf| board_dir.join(kf.as_str()))
    });

    if let Some(ref k_path) = kernel_file {
        if k_path.exists() {
            let kernel_data =
                fs::read(k_path).map_err(|e| format!("failed to read kernel blob: {e}"))?;
            // Kernel blobs are stored verbatim and the platform jumps to the
            // configured kernel_addr, so an ELF (vmlinux) would have its ELF
            // header executed — a silent hang at boot. Require flat images.
            if payload.kind == fstart_core::PayloadKind::LinuxBoot
                && kernel_data.starts_with(b"\x7fELF")
            {
                return Err(format!(
                    "kernel blob {} is an ELF (vmlinux); Linux payloads are loaded \
                     verbatim at kernel_addr, so pass a flat kernel image instead \
                     (Image / zImage / bzImage), or use a FIT payload",
                    k_path.display()
                ));
            }
            let kernel_load_addr = payload.kernel_load_addr.unwrap_or(0);
            let kernel_name = payload
                .kernel_file
                .as_ref()
                .map(|kf| kf.to_string())
                .unwrap_or_else(|| "kernel".to_string());

            eprintln!(
                "[fstart] kernel blob: {} ({} bytes, load_addr={:#x})",
                k_path.display(),
                kernel_data.len(),
                kernel_load_addr,
            );

            ro_files.push(InputFile {
                name: kernel_name,
                file_type: FileType::Payload,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: kernel_data,
                    mem_size: None,
                    load_addr: kernel_load_addr,
                    compression: payload.compression,
                    flags: SegmentFlags::CODE,
                }],
            });
        } else {
            return Err(format!("kernel blob not found: {}", k_path.display()));
        }
    }

    Ok(())
}

fn add_firmware_blob(
    payload: &fstart_core::PayloadConfig,
    board_dir: &Path,
    firmware_path: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    let fw_file = firmware_path.map(PathBuf::from).or_else(|| {
        payload
            .firmware
            .as_ref()
            .map(|fw| board_dir.join(fw.file.as_str()))
    });

    if let Some(ref fw_path) = fw_file {
        if fw_path.exists() {
            let fw_data =
                fs::read(fw_path).map_err(|e| format!("failed to read firmware blob: {e}"))?;
            let fw_load_addr = payload
                .firmware
                .as_ref()
                .map(|fw| fw.load_addr)
                .unwrap_or(0);
            let fw_name = payload
                .firmware
                .as_ref()
                .map(|fw| fw.file.to_string())
                .unwrap_or_else(|| "firmware".to_string());

            eprintln!(
                "[fstart] firmware blob: {} ({} bytes, load_addr={:#x})",
                fw_path.display(),
                fw_data.len(),
                fw_load_addr,
            );

            ro_files.push(InputFile {
                name: fw_name,
                file_type: FileType::Firmware,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: fw_data,
                    mem_size: None,
                    load_addr: fw_load_addr,
                    compression: Compression::Lz4,
                    flags: SegmentFlags::CODE,
                }],
            });
        } else {
            eprintln!(
                "[fstart] warning: firmware blob not found: {}",
                fw_path.display()
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ElfLoadSegment {
    offset: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

fn elf_load_segments(elf_data: &[u8], elf_path: &Path) -> Result<Vec<ElfLoadSegment>, String> {
    if elf_data.len() < 16 {
        return Err(format!("ELF {} is too short", elf_path.display()));
    }

    match elf_data[4] {
        elf::ELFCLASS32 => elf_load_segments_for::<elf::FileHeader32<object::Endianness>>(elf_data),
        elf::ELFCLASS64 => elf_load_segments_for::<elf::FileHeader64<object::Endianness>>(elf_data),
        class => {
            return Err(format!(
                "unsupported ELF class {class} in {}",
                elf_path.display()
            ));
        }
    }
    .map_err(|e| format!("failed to parse ELF {}: {e}", elf_path.display()))
}

fn elf_load_segments_for<Elf>(elf_data: &[u8]) -> Result<Vec<ElfLoadSegment>, object::Error>
where
    Elf: FileHeader,
{
    let elf = ElfFile::<Elf>::parse(elf_data)?;
    let endian = elf.elf_header().endian()?;
    Ok(elf
        .elf_program_headers()
        .iter()
        .filter(|phdr| phdr.p_type(endian) == elf::PT_LOAD)
        .map(|phdr| ElfLoadSegment {
            offset: phdr.p_offset(endian).into(),
            paddr: phdr.p_paddr(endian).into(),
            filesz: phdr.p_filesz(endian).into(),
            memsz: phdr.p_memsz(endian).into(),
            flags: phdr.p_flags(endian),
        })
        .collect())
}

fn parse_elf_segments(
    elf_path: &Path,
    compression: Compression,
) -> Result<Vec<InputSegment>, String> {
    let elf_data =
        fs::read(elf_path).map_err(|e| format!("failed to read {}: {e}", elf_path.display()))?;

    let mut segments = Vec::new();

    for phdr in elf_load_segments(&elf_data, elf_path)? {
        if phdr.memsz == 0 {
            continue;
        }

        let p_flags = phdr.flags;
        let is_exec = p_flags & elf::PF_X != 0;
        let is_write = p_flags & elf::PF_W != 0;

        let (kind, name, flags) = if phdr.filesz == 0 {
            (SegmentKind::Bss, ".bss", SegmentFlags::DATA)
        } else if is_exec {
            (SegmentKind::Code, ".text", SegmentFlags::CODE)
        } else if is_write {
            (SegmentKind::ReadWriteData, ".data", SegmentFlags::DATA)
        } else {
            (SegmentKind::ReadOnlyData, ".rodata", SegmentFlags::RODATA)
        };

        let data = if phdr.filesz > 0 {
            let start = phdr.offset as usize;
            let end = start + phdr.filesz as usize;
            if end > elf_data.len() {
                return Err(format!(
                    "PT_LOAD at {:#x} extends past EOF in {}",
                    phdr.paddr,
                    elf_path.display(),
                ));
            }
            elf_data[start..end].to_vec()
        } else {
            Vec::new()
        };

        let mem_size = if phdr.memsz != phdr.filesz {
            Some(phdr.memsz)
        } else {
            None
        };

        let seg_compression = if phdr.filesz == 0 {
            Compression::None
        } else {
            compression
        };

        segments.push(InputSegment {
            name: name.to_string(),
            kind,
            data,
            mem_size,
            load_addr: phdr.paddr,
            compression: seg_compression,
            flags,
        });
    }

    if segments.is_empty() {
        return Err(format!(
            "no PT_LOAD segments found in {}",
            elf_path.display()
        ));
    }

    Ok(segments)
}

fn log_stage_segments(stage_name: &str, elf_path: &Path, segments: &[InputSegment]) {
    let total_file: usize = segments.iter().map(|s| s.data.len()).sum();
    let total_mem: u64 = segments
        .iter()
        .map(|s| s.mem_size.unwrap_or(s.data.len() as u64))
        .sum();
    eprintln!(
        "[fstart] {stage_name}: {} PT_LOAD segment{}, {} bytes stored, {} bytes memory (from {})",
        segments.len(),
        if segments.len() == 1 { "" } else { "s" },
        total_file,
        total_mem,
        elf_path.display(),
    );
    for seg in segments {
        let mem = seg.mem_size.unwrap_or(seg.data.len() as u64);
        let comp = match seg.compression {
            Compression::None => "",
            Compression::Lz4 => " lz4",
        };
        eprintln!(
            "[fstart]   {} load={:#x} file={} mem={}{comp}",
            seg.name,
            seg.load_addr,
            seg.data.len(),
            mem,
        );
    }
}

fn get_or_create_dev_keys(
    board_dir: &Path,
    _config: &fstart_core::BoardConfig,
) -> Result<(ed25519_dalek::SigningKey, VerificationKey), String> {
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    let keys_dir = board_dir.join("keys");
    let privkey_path = keys_dir.join("dev-signing.key");
    let pubkey_path = keys_dir.join("dev-signing.pub");

    if privkey_path.exists() && pubkey_path.exists() {
        let privkey_bytes =
            fs::read(&privkey_path).map_err(|e| format!("failed to read private key: {e}"))?;
        if privkey_bytes.len() != 32 {
            return Err(format!(
                "invalid private key size: {} (expected 32)",
                privkey_bytes.len()
            ));
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&privkey_bytes);
        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();

        let vk = VerificationKey::ed25519(0, verifying_key.to_bytes());
        eprintln!(
            "[fstart] loaded existing dev keys from {}",
            keys_dir.display()
        );
        return Ok((signing_key, vk));
    }

    eprintln!("[fstart] generating new dev Ed25519 key pair...");
    fs::create_dir_all(&keys_dir).map_err(|e| format!("failed to create keys dir: {e}"))?;

    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    fs::write(&privkey_path, signing_key.as_bytes())
        .map_err(|e| format!("failed to write private key: {e}"))?;

    fs::write(&pubkey_path, verifying_key.as_bytes())
        .map_err(|e| format!("failed to write public key: {e}"))?;

    eprintln!("[fstart] saved dev keys to {}", keys_dir.display());

    let vk = VerificationKey::ed25519(0, verifying_key.to_bytes());
    Ok((signing_key, vk))
}

fn sign_with_ed25519(
    signing_key: &ed25519_dalek::SigningKey,
    message: &[u8],
) -> Result<Signature, String> {
    use ed25519_dalek::Signer;

    let sig = signing_key.sign(message);
    Ok(Signature::ed25519(0, sig.to_bytes()))
}
