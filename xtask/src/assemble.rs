//! FFS image assembly — `cargo xtask assemble`.
//!
//! Reads a board config, collects built binaries, and assembles them into
//! a signed FFS firmware image using the fstart-ffs builder.
//!
//! Stage ELFs are parsed directly — each PT_LOAD segment becomes a separate
//! FFS segment with its own load address, kind, and flags. This avoids
//! `llvm-objcopy` entirely and correctly preserves `.data` (initialized
//! statics), `.rodata`, and BSS information.

use fstart_ffs::builder::{build_image, FfsImageConfig, InputFile, InputRegion, InputSegment};
use fstart_types::ffs::{
    Compression, FileType, SegmentFlags, SegmentKind, Signature, VerificationKey,
};
use fstart_types::{FdtSource, SocImageFormat, StageLayout};
use goblin::elf::{program_header, Elf};
use std::fs;
use std::path::{Path, PathBuf};

/// Assemble an FFS image for the given board.
///
/// 1. Builds all stages (via `cargo xtask build`).
/// 2. Packages stage binaries into an FFS image.
/// 3. Signs the manifest with the board's dev key pair.
pub fn assemble(board_name: &str) -> Result<PathBuf, String> {
    assemble_impl(board_name, false, None, None)
}

/// Assemble with explicit release flag (used by `run` for multi-stage boards).
pub fn assemble_release(board_name: &str, release: bool) -> Result<PathBuf, String> {
    assemble_impl(board_name, release, None, None)
}

/// Assemble with full options: release flag and optional kernel/firmware paths.
///
/// If `kernel`/`firmware` are `None`, falls back to paths from the board RON
/// `payload` config (resolved relative to the board directory). If neither
/// is available, no external blobs are added to the image.
pub fn assemble_with_opts(
    board_name: &str,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
) -> Result<PathBuf, String> {
    assemble_impl(board_name, release, kernel, firmware)
}

fn assemble_impl(
    board_name: &str,
    release: bool,
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
) -> Result<PathBuf, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let board_dir = workspace_root.join("boards").join(board_name);
    let board_ron = board_dir.join("board.ron");

    if !board_ron.exists() {
        return Err(format!("board config not found: {}", board_ron.display()));
    }

    eprintln!("[fstart] loading board config: {}", board_ron.display());
    let config = fstart_codegen::ron_loader::load_board_config(&board_ron)?;

    eprintln!("[fstart] assembling FFS image for: {}", config.name);

    // Build all stages first
    let build_result = crate::build_board::build(board_name, release)?;

    // Read the public key (or generate a dev key pair if not present)
    let (signing_key, verification_key) = get_or_create_dev_keys(&board_dir, &config)?;

    // Build the list of input files from the built stages.
    //
    // Each stage ELF is parsed directly: PT_LOAD segments become FFS
    // segments with correct load addresses, kinds (code/rodata/data/bss),
    // and permission flags. No llvm-objcopy needed.
    let mut ro_files = Vec::new();

    match &config.stages {
        StageLayout::Monolithic(mono) => {
            let stage = &build_result.stages[0];

            // Log the ELF segment breakdown for diagnostics.
            if let Ok(segs) = parse_elf_segments(&stage.path, Compression::None) {
                log_stage_segments("stage", &stage.path, &segs);
            }

            // Use the flat binary (.bin) to preserve alignment gaps
            // between sections (e.g., .text -> .fstart.anchor -> .rodata).
            // ELF segment parsing packs segments contiguously, shifting the
            // anchor relative to its link-time VMA — fatal for XIP boards
            // that read the anchor via volatile at the linked address.
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
        StageLayout::MultiStage(_stages) => {
            for (i, stage_bin) in build_result.stages.iter().enumerate() {
                if i == 0 {
                    // The bootblock must be uncompressed — it executes
                    // directly from flash/SRAM and contains the anchor
                    // placeholder that gets patched in-place.
                    //
                    // Use the flat binary (.bin) rather than ELF segment
                    // parsing. The flat binary preserves alignment gaps
                    // between sections (e.g., .text -> .fstart.anchor ->
                    // .rodata), which is critical for XIP boards where
                    // the firmware reads the anchor at its link-time VMA.
                    // ELF segment parsing packs segments contiguously,
                    // losing alignment padding and causing the anchor to
                    // shift relative to its link address.
                    let bin_data = fs::read(&stage_bin.run_path).map_err(|e| {
                        format!("failed to read {}: {e}", stage_bin.run_path.display())
                    })?;
                    log_stage_segments(&stage_bin.name, &stage_bin.path, &{
                        // Still parse ELF for the log message (segment breakdown)
                        parse_elf_segments(&stage_bin.path, Compression::None).unwrap_or_default()
                    });
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
                    // Subsequent stages are stored as flat binaries (the
                    // objcopy .bin).  The bootblock copies this blob
                    // directly to load_addr — no FFS parsing or LZ4
                    // decompression required.
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
                            compression: Compression::None,
                            flags: SegmentFlags::CODE,
                        }],
                    });
                }
            }
        }
    }

    // Add payload blobs to the FFS image.
    //
    // Resolution order for paths:
    //   1. CLI flags (--kernel, --firmware)
    //   2. Board RON payload config (payload.firmware.file, payload.kernel_file,
    //      payload.fit_file) resolved relative to the board directory
    //   3. Skip — no external blob added
    if let Some(ref payload) = config.payload {
        // Handle FIT image payloads
        if payload.kind == fstart_types::PayloadKind::FitImage {
            assemble_fit_payload(payload, &board_dir, kernel_path, &mut ro_files)?;
        } else {
            // LinuxBoot / other payload types: add firmware + kernel blobs
            assemble_linux_payload(
                payload,
                &board_dir,
                kernel_path,
                firmware_path,
                &mut ro_files,
            )?;
        }

        // Resolve DTB blob path from FdtSource::Override
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

    let image_config = FfsImageConfig {
        keys: vec![verification_key],
        regions: vec![InputRegion::Container {
            name: "ro".to_string(),
            files: ro_files,
        }],
    };

    // Build the image with signing
    let ffs_image = build_image(&image_config, &move |manifest_bytes| {
        sign_with_ed25519(&signing_key, manifest_bytes)
    })?;

    let mut image_bytes = ffs_image.image;

    // Allwinner eGON: the FFS assembler reads stage ELFs, so the eGON
    // header (length, checksum, SPL signature) is unpatched. Read the
    // bootblock size from the standalone .bin (which was already patched
    // by `build_board`) and apply the same eGON patching to the FFS.
    if config.soc_image_format == SocImageFormat::AllwinnerEgon {
        let bb_bin_path = &build_result.stages[0].run_path;
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

        // Patch next-stage offset/size into the eGON header BEFORE the
        // checksum is computed. The bootblock reads these at runtime via
        // volatile reads from SRAM to find and copy the next stage.
        if build_result.stages.len() > 1 {
            let next_name = &build_result.stages[1].name;
            let loc = ffs_image
                .file_data
                .iter()
                .find(|f| f.name == *next_name)
                .ok_or_else(|| format!("next stage '{next_name}' not found in FFS file_data"))?;

            // Offset 0x2C: next_stage_offset (from FFS image start).
            image_bytes[0x2C..0x30].copy_from_slice(&loc.data_offset.to_le_bytes());
            // Offset 0x30: next_stage_size.
            image_bytes[0x30..0x34].copy_from_slice(&loc.data_size.to_le_bytes());
            // Offset 0x34: ffs_total_size — used by subsequent stages to
            // locate the FFS anchor at ffs_total_size - ANCHOR_SIZE.
            let ffs_total = image_bytes.len() as u32;
            image_bytes[0x34..0x38].copy_from_slice(&ffs_total.to_le_bytes());

            eprintln!(
                "[fstart] next stage '{}': offset={:#x}, size={:#x} ({} bytes)",
                next_name, loc.data_offset, loc.data_size, loc.data_size,
            );
        }

        crate::build_board::patch_allwinner_egon_ffs(&mut image_bytes, bootblock_size)?;
    }

    // Write the FFS image
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

    // Log the stage files in the image
    let stage_count = build_result.stages.len();
    eprintln!(
        "[fstart] {} stage{} packaged into FFS",
        stage_count,
        if stage_count == 1 { "" } else { "s" }
    );

    Ok(image_path)
}

// ============================================================================
// Payload assembly helpers
// ============================================================================

/// Assemble a FIT image payload into FFS entries.
///
/// Depending on `fit_parse` mode:
/// - **Buildtime**: Parse the FIT, extract kernel (and ramdisk), embed them
///   as separate FFS entries with load addresses from the FIT metadata.
/// - **Runtime**: Embed the whole .itb as a single `FileType::FitImage` entry.
fn assemble_fit_payload(
    payload: &fstart_types::PayloadConfig,
    board_dir: &Path,
    kernel_override: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    let fit_parse = payload
        .fit_parse
        .unwrap_or(fstart_types::FitParseMode::Buildtime);

    // Resolve the FIT file path
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

    // Parse the FIT with the same parser used at runtime
    let fit = fstart_fit::FitImage::parse(&fit_data)
        .map_err(|e| format!("failed to parse FIT image: {e:?}"))?;

    if let Some(desc) = fit.description() {
        eprintln!("[fstart] FIT description: {desc}");
    }

    let config_name = payload.fit_config.as_ref().map(|s| s.as_str());

    match fit_parse {
        fstart_types::FitParseMode::Runtime => {
            // Embed the whole FIT as a single FFS entry
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
        fstart_types::FitParseMode::Buildtime => {
            // Extract components from the FIT and embed as separate entries
            eprintln!("[fstart] FIT mode: buildtime (extracting components)");

            let boot = fit
                .resolve_boot_images(config_name)
                .map_err(|e| format!("failed to resolve FIT config: {e:?}"))?;

            eprintln!(
                "[fstart] FIT config: {}",
                boot.config.description().unwrap_or(boot.config.name())
            );

            // Extract kernel
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
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            });

            // Extract ramdisk if present
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
                            compression: Compression::None,
                            flags: SegmentFlags::RODATA,
                        }],
                    });
                }
            }

            // Extract FDT if present in FIT
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

    // Add firmware blob (SBI/ATF) — always separate from FIT
    add_firmware_blob(payload, board_dir, None, ro_files)?;

    Ok(())
}

/// Assemble a LinuxBoot payload into FFS entries (firmware + kernel blobs).
fn assemble_linux_payload(
    payload: &fstart_types::PayloadConfig,
    board_dir: &Path,
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    // Add firmware blob
    add_firmware_blob(payload, board_dir, firmware_path, ro_files)?;

    // Resolve kernel blob path
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
                    // TODO: make compression configurable per-board via RON
                    // (e.g. payload.compression field). For now LZ4 is always
                    // used — the runtime decompressor is enabled whenever FFS
                    // is active, and smaller images benefit all targets.
                    compression: Compression::Lz4,
                    flags: SegmentFlags::CODE,
                }],
            });
        } else {
            eprintln!(
                "[fstart] warning: kernel blob not found: {}",
                k_path.display()
            );
        }
    }

    Ok(())
}

/// Add the firmware blob (SBI/ATF) to FFS entries.
fn add_firmware_blob(
    payload: &fstart_types::PayloadConfig,
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
                    // Firmware blobs (SBI/ATF) are loaded to their
                    // final address uncompressed.  LZ4 decompression
                    // writes to the destination with multi-byte stores
                    // that may be unaligned — this triggers alignment
                    // faults on targets where the load address falls in
                    // Device-mapped memory (e.g., AArch64 secure SRAM
                    // at 0x0E000000 mapped as Device-nGnRnE).
                    compression: Compression::None,
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

// ============================================================================
// ELF parsing — replaces llvm-objcopy
// ============================================================================

/// Parse an ELF file into FFS input segments, one per PT_LOAD.
///
/// Follows the coreboot cbfstool payload model: each PT_LOAD program header
/// becomes a separate segment with its own load address and type. This
/// avoids the ROM→RAM address gap that makes flat binaries enormous.
///
/// - `p_paddr` is used as the load address (like coreboot and u-boot).
/// - `p_filesz` bytes of data are extracted from the ELF.
/// - `p_memsz > p_filesz` produces a BSS tail (the loader zero-fills it).
/// - Pure BSS segments (`p_filesz == 0`) become `SegmentKind::Bss`.
/// - Segment kind and flags are derived from `p_flags` (PF_X, PF_W, PF_R).
fn parse_elf_segments(
    elf_path: &Path,
    compression: Compression,
) -> Result<Vec<InputSegment>, String> {
    let elf_data =
        fs::read(elf_path).map_err(|e| format!("failed to read {}: {e}", elf_path.display()))?;

    let elf = Elf::parse(&elf_data)
        .map_err(|e| format!("failed to parse ELF {}: {e}", elf_path.display()))?;

    let mut segments = Vec::new();

    for phdr in &elf.program_headers {
        // Only process PT_LOAD segments with nonzero memory footprint
        if phdr.p_type != program_header::PT_LOAD {
            continue;
        }
        if phdr.p_memsz == 0 {
            continue;
        }

        let p_flags = phdr.p_flags;
        let is_exec = p_flags & program_header::PF_X != 0;
        let is_write = p_flags & program_header::PF_W != 0;

        // Determine segment kind and name from ELF flags, matching
        // the coreboot PAYLOAD_SEGMENT_CODE / DATA / BSS classification.
        let (kind, name, flags) = if phdr.p_filesz == 0 {
            // Pure BSS — no file content, just zero-fill
            (SegmentKind::Bss, ".bss", SegmentFlags::DATA)
        } else if is_exec {
            (SegmentKind::Code, ".text", SegmentFlags::CODE)
        } else if is_write {
            (SegmentKind::ReadWriteData, ".data", SegmentFlags::DATA)
        } else {
            (SegmentKind::ReadOnlyData, ".rodata", SegmentFlags::RODATA)
        };

        // Extract file data (p_filesz bytes at p_offset)
        let data = if phdr.p_filesz > 0 {
            let start = phdr.p_offset as usize;
            let end = start + phdr.p_filesz as usize;
            if end > elf_data.len() {
                return Err(format!(
                    "PT_LOAD at {:#x} extends past EOF in {}",
                    phdr.p_paddr,
                    elf_path.display(),
                ));
            }
            elf_data[start..end].to_vec()
        } else {
            Vec::new()
        };

        // mem_size tracks the BSS tail: when p_memsz > p_filesz the
        // loader must zero-fill the remaining bytes after the file data.
        let mem_size = if phdr.p_memsz != phdr.p_filesz {
            Some(phdr.p_memsz)
        } else {
            None
        };

        // BSS segments have no stored content — never compress them.
        // Other segments use the caller's requested compression.
        let seg_compression = if phdr.p_filesz == 0 {
            Compression::None
        } else {
            compression
        };

        segments.push(InputSegment {
            name: name.to_string(),
            kind,
            data,
            mem_size,
            load_addr: phdr.p_paddr,
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

/// Log the parsed segments for a stage.
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

// ============================================================================
// Crypto helpers
// ============================================================================

/// Get or create a dev Ed25519 key pair for signing.
///
/// In a real production setup, the private key would be stored securely
/// (HSM, etc.) and only the public key would be distributed. For
/// development, we generate an ephemeral key pair and store it in the
/// board's `keys/` directory.
fn get_or_create_dev_keys(
    board_dir: &Path,
    _config: &fstart_types::BoardConfig,
) -> Result<(ed25519_dalek::SigningKey, VerificationKey), String> {
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    let keys_dir = board_dir.join("keys");
    let privkey_path = keys_dir.join("dev-signing.key");
    let pubkey_path = keys_dir.join("dev-signing.pub");

    if privkey_path.exists() && pubkey_path.exists() {
        // Load existing keys
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

    // Generate new dev key pair
    eprintln!("[fstart] generating new dev Ed25519 key pair...");
    fs::create_dir_all(&keys_dir).map_err(|e| format!("failed to create keys dir: {e}"))?;

    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    // Save private key (32 bytes raw)
    fs::write(&privkey_path, signing_key.as_bytes())
        .map_err(|e| format!("failed to write private key: {e}"))?;

    // Save public key (32 bytes raw)
    fs::write(&pubkey_path, verifying_key.as_bytes())
        .map_err(|e| format!("failed to write public key: {e}"))?;

    eprintln!("[fstart] saved dev keys to {}", keys_dir.display());

    let vk = VerificationKey::ed25519(0, verifying_key.to_bytes());
    Ok((signing_key, vk))
}

/// Sign manifest bytes with Ed25519.
fn sign_with_ed25519(
    signing_key: &ed25519_dalek::SigningKey,
    message: &[u8],
) -> Result<Signature, String> {
    use ed25519_dalek::Signer;

    let sig = signing_key.sign(message);
    Ok(Signature::ed25519(0, sig.to_bytes()))
}
