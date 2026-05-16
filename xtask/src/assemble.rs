//! FFS image assembly — `cargo xtask assemble`.
//!
//! Reads a board config, collects built binaries, and assembles them into
//! a signed FFS firmware image using the fstart-ffs builder.
//!
//! Stage flat binaries (`.bin` files produced by `llvm-objcopy`) are
//! embedded as single FFS segments. This preserves alignment gaps between
//! sections (e.g., `.text` → `.fstart.anchor` → `.rodata`) which is
//! critical for XIP boards that read the anchor at its link-time VMA.
//! ELF parsing is retained only for diagnostic logging.

use fstart_acpi::device::AcpiDevice;
use fstart_device_registry::DriverInstance;
use fstart_driver_ite8721f::LpcBaseProvider;
use fstart_ffs::builder::{
    build_image, ExternalInputFile, FfsImageConfig, InputFile, InputRegion, InputSegment,
};
use fstart_services::device::{BusDevice, Device};
use fstart_types::device::BusAddress;
use fstart_types::ffs::{
    Compression, FileType, SegmentFlags, SegmentKind, Signature, VerificationKey,
};
use fstart_types::memory::{FlashLayout, IntelIfdFlashLayout, IntelIfdRegion};
use fstart_types::{BoardConfig, FdtSource, Platform, RunsFrom, SocImageFormat, StageLayout};
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
    assemble_with_opts_and_acpi_check(board_name, release, kernel, firmware, false)
}

pub fn assemble_with_opts_and_acpi_check(
    board_name: &str,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
    acpi_check: bool,
) -> Result<PathBuf, String> {
    let image = assemble_impl(board_name, release, kernel, firmware)?;
    if acpi_check {
        dry_run_acpi_check(board_name)?;
    }
    Ok(image)
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
    // Each stage is packaged as a flat binary (.bin from objcopy) to
    // preserve alignment gaps between sections.  ELF parsing is
    // retained only for diagnostic logging.
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
        StageLayout::MultiStage(stages) => {
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
                    // Still parse ELF for the log message (segment breakdown)
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
                    // Subsequent stages are stored as flat binaries (the
                    // objcopy .bin). The StageLoad path loads the FFS segment
                    // to load_addr and supports LZ4 in-place decompression, so
                    // ramstages loaded via StageLoad can be stored compressed.
                    // LoadNextStage users (e.g. tiny SoC bootblocks) copy raw
                    // bytes and jump directly, so those stages must remain
                    // uncompressed.
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
                            "stage '{}' requests {:?} compression but is not loaded via StageLoad",
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

    // Add payload blobs to the FFS image.
    //
    // Resolution order for paths:
    //   1. CLI flags (--kernel, --firmware)
    //   2. Board RON payload config (payload.firmware.file, payload.kernel_file,
    //      payload.fit_file) resolved relative to the board directory
    //   3. Skip — no external blob added
    if let Some(ref microcode) = config.microcode {
        assemble_microcode(microcode, &board_dir, &mut ro_files)?;
    }

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

    validate_flash_layout(&config, &board_dir)?;

    let image_config = FfsImageConfig {
        keys: vec![verification_key],
        regions: ffs_input_regions(&config, ro_files)?,
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

    let flash_layout_files = match &config.memory.flash_layout {
        Some(FlashLayout::IntelIfd(layout)) => {
            layout.regions.iter().any(|region| region.file.is_some())
        }
        None => false,
    };
    if config.full_flash_image || flash_layout_files {
        create_full_flash_image(
            &config,
            &board_dir,
            &build_result.stages[0].path,
            &build_result.stages[0].run_path,
            &image_bytes,
            ffs_image.anchor_offset,
            &image_path,
        )?;
    }

    Ok(image_path)
}

fn ffs_input_regions(
    config: &BoardConfig,
    ro_files: Vec<InputFile>,
) -> Result<Vec<InputRegion>, String> {
    let Some(FlashLayout::IntelIfd(layout)) = &config.memory.flash_layout else {
        if config.full_flash_image {
            let flash_size = config
                .memory
                .flash_size
                .ok_or_else(|| "full_flash_image requires memory.flash_size".to_string())?;
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
    if config.platform != Platform::X86_64
        || !first_stage_is_xip
        || files.first().is_none_or(|file| file.name != "bootblock")
    {
        return Ok((files, Vec::new()));
    }

    let image_base = config
        .memory
        .flash_base
        .ok_or_else(|| "x86 XIP bootblock requires memory.flash_base".to_string())?;
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

fn create_full_flash_image(
    config: &BoardConfig,
    board_dir: &Path,
    bootblock_elf: &Path,
    bootblock_bin: &Path,
    ffs_data: &[u8],
    ffs_anchor_offset: usize,
    ffs_path: &Path,
) -> Result<PathBuf, String> {
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

    let flash_base = config
        .memory
        .flash_base
        .ok_or_else(|| "full_flash_image requires memory.flash_base".to_string())?;
    let flash_size = config
        .memory
        .flash_size
        .ok_or_else(|| "full_flash_image requires memory.flash_size".to_string())?
        as usize;

    if ffs_data.len() > flash_size {
        return Err(format!(
            "FFS image ({} bytes) exceeds flash size ({} bytes)",
            ffs_data.len(),
            flash_size
        ));
    }

    let mut image = vec![0xffu8; flash_size];

    // Keep the FFS blob at flash offset 0. Anchor offsets are defined from the
    // firmware image base, and board BootMedia scans memory.flash_base..+size.
    image[..ffs_data.len()].copy_from_slice(ffs_data);

    let elf_data = fs::read(bootblock_elf).map_err(|e| {
        format!(
            "failed to read bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;
    let elf = Elf::parse(&elf_data).map_err(|e| {
        format!(
            "failed to parse bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;

    let mut first_flash_load: Option<usize> = None;
    for phdr in &elf.program_headers {
        if phdr.p_type != program_header::PT_LOAD || phdr.p_filesz == 0 {
            continue;
        }
        let paddr = phdr.p_paddr;
        if paddr < flash_base {
            continue;
        }
        let off = (paddr - flash_base) as usize;
        let size = phdr.p_filesz as usize;
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

    // Copy the patched anchor from the FFS blob into the XIP bootblock's own
    // anchor section. The builder patches the anchor inside the FFS-stage file;
    // the CPU reads the linked XIP copy at its physical flash address.
    let anchor_size = fstart_types::ffs::ANCHOR_SIZE;
    if ffs_anchor_offset + anchor_size > ffs_data.len() {
        return Err(format!(
            "FFS anchor offset {ffs_anchor_offset:#x} outside FFS image"
        ));
    }
    let placeholder = fstart_types::ffs::AnchorBlock::placeholder();
    let placeholder_bytes = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_types::ffs::AnchorBlock as *const u8,
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
            ffs_data[ffs_anchor_offset..].as_ptr() as *const fstart_types::ffs::AnchorBlock
        )
    };
    // The FFS copy of the anchor lives near offset 0, but real hardware jumps
    // into the top-aligned XIP bootblock copy. Pre-Rust x86 code has only the
    // linked anchor address, so the XIP anchor must describe its full-flash
    // offset to reconstruct `memory.flash_base` correctly.
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
    if bios_end > layout.size {
        return Err(format!(
            "Intel IFD BIOS region [{:#x}..{:#x}) exceeds flash size {:#x}",
            bios.offset, bios_end, layout.size
        ));
    }
    if ffs_data.len() > bios.size as usize {
        return Err(format!(
            "FFS image ({} bytes) exceeds Intel IFD BIOS region ({} bytes)",
            ffs_data.len(),
            bios.size
        ));
    }

    let mut image = vec![0xffu8; layout.size as usize];

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
                layout.size
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
    let elf = Elf::parse(&elf_data).map_err(|e| {
        format!(
            "failed to parse bootblock ELF {}: {e}",
            bootblock_elf.display()
        )
    })?;

    let mut first_flash_load: Option<usize> = None;
    for phdr in &elf.program_headers {
        if phdr.p_type != program_header::PT_LOAD || phdr.p_filesz == 0 {
            continue;
        }
        let paddr = phdr.p_paddr;
        if paddr < layout.base || paddr >= layout.end() {
            continue;
        }
        let off = (paddr - layout.base) as usize;
        let size = phdr.p_filesz as usize;
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

    let mib = layout.size as usize / (1024 * 1024);
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
    let anchor_size = fstart_types::ffs::ANCHOR_SIZE;
    if ffs_anchor_offset + anchor_size > ffs_data.len() {
        return Err(format!(
            "FFS anchor offset {ffs_anchor_offset:#x} outside FFS image"
        ));
    }
    let placeholder = fstart_types::ffs::AnchorBlock::placeholder();
    let placeholder_bytes = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_types::ffs::AnchorBlock as *const u8,
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
            ffs_data[ffs_anchor_offset..].as_ptr() as *const fstart_types::ffs::AnchorBlock
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

    let bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
    let expected_bios_base = layout.base + u64::from(bios.offset);
    if config.memory.flash_base != Some(expected_bios_base)
        || config.memory.flash_size != Some(u64::from(bios.size))
    {
        return Err(format!(
            "memory.flash_base/flash_size must describe the Intel IFD BIOS region: \
             expected base={expected_bios_base:#x} size={:#x}, got base={:?} size={:?}",
            bios.size, config.memory.flash_base, config.memory.flash_size
        ));
    }

    let aperture_end = layout
        .base
        .checked_add(u64::from(layout.size))
        .ok_or_else(|| "Intel IFD flash aperture overflows u64".to_string())?;
    for region in &layout.regions {
        let region_end = region
            .offset
            .checked_add(region.size)
            .ok_or_else(|| format!("Intel IFD region {} overflows u32", region.kind.as_str()))?;
        if region_end > layout.size {
            return Err(format!(
                "Intel IFD region {} [{:#x}..{:#x}) exceeds flash size {:#x}",
                region.kind.as_str(),
                region.offset,
                region_end,
                layout.size
            ));
        }
        let mapped_start = layout.base + u64::from(region.offset);
        let mapped_end = layout.base + u64::from(region_end);
        if mapped_start < layout.base || mapped_end > aperture_end {
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
        let parsed = parse_ifd_regions(&data)?;
        for region in &layout.regions {
            let Some(idx) = region.kind.flreg_index() else {
                continue;
            };
            let Some((offset, size)) = parsed.get(idx).copied().flatten() else {
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
                     but board RON declares offset={:#x} size={:#x}",
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

fn parse_ifd_regions(data: &[u8]) -> Result<[Option<(u32, u32)>; 16], String> {
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
    Ok(regions)
}

fn resolve_board_path(board_dir: &Path, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        board_dir.join(path)
    }
}

// ============================================================================
// Microcode assembly helpers
// ============================================================================

fn assemble_microcode(
    microcode: &fstart_types::board::MicrocodeConfig,
    board_dir: &Path,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    match microcode {
        fstart_types::board::MicrocodeConfig::Intel(config) => {
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

fn stage_loaded_via_stage_load(stages: &[fstart_types::StageConfig], stage_name: &str) -> bool {
    stages.iter().any(|stage| {
        stage.capabilities.iter().any(|cap| {
            matches!(
                cap,
                fstart_types::Capability::StageLoad { next_stage } if next_stage.as_str() == stage_name
            )
        })
    })
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
                    compression: Compression::Lz4,
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
                            compression: Compression::Lz4,
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
                    // LZ4 compression — the x86 FFS-to-RAM copy ensures
                    // decompression runs on fast RAM, not flash XIP.
                    // TODO: make compression configurable per-board via RON.
                    compression: Compression::Lz4,
                    flags: SegmentFlags::CODE,
                }],
            });
        } else {
            return Err(format!("kernel blob not found: {}", k_path.display()));
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

struct DryLpcBus {
    base: u16,
}

impl LpcBaseProvider for DryLpcBus {
    fn lpc_base(&self) -> u16 {
        self.base
    }
}

fn dry_run_acpi_check(board_name: &str) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let board_ron = workspace_root
        .join("boards")
        .join(board_name)
        .join("board.ron");
    let parsed = fstart_codegen::ron_loader::load_parsed_board(&board_ron)?;

    if parsed.config.acpi.is_none() {
        eprintln!("[fstart] ACPI check: board has no acpi config, skipping");
        return Ok(());
    }

    let mut dsdt_aml = Vec::new();
    let mut extra_tables: Vec<Vec<u8>> = Vec::new();

    for (idx, inst) in parsed.driver_instances.iter().enumerate() {
        let dev = &parsed.config.devices[idx];
        if !dev.enabled {
            continue;
        }

        match inst {
            DriverInstance::IntelPineview(cfg) if cfg.acpi_name.is_some() => {
                let driver = fstart_driver_intel_pineview::IntelPineview::new(cfg)
                    .map_err(|e| format!("ACPI check: failed to construct {}: {e:?}", dev.name))?;
                dsdt_aml.extend(driver.dsdt_aml(cfg));
                extra_tables.extend(driver.extra_tables(cfg));
            }
            DriverInstance::IntelIch7(cfg) if cfg.acpi_name.is_some() => {
                let driver = fstart_driver_intel_ich7::IntelIch7::new(cfg)
                    .map_err(|e| format!("ACPI check: failed to construct {}: {e:?}", dev.name))?;
                dsdt_aml.extend(driver.dsdt_aml(cfg));
                extra_tables.extend(driver.extra_tables(cfg));
            }
            DriverInstance::Ite8721f(cfg) if cfg.acpi_name.is_some() => {
                let base = match dev.bus {
                    Some(BusAddress::Lpc(base)) => base,
                    _ => 0x2e,
                };
                let bus = DryLpcBus { base };
                let driver = fstart_driver_ite8721f::Ite8721f::new_on_bus(cfg, &bus)
                    .map_err(|e| format!("ACPI check: failed to construct {}: {e:?}", dev.name))?;
                dsdt_aml.extend(driver.dsdt_aml(cfg));
                extra_tables.extend(driver.extra_tables(cfg));
            }
            _ => {}
        }
    }

    let table_set = fstart_acpi::platform::assemble(
        0x0010_0000,
        &fstart_acpi::platform::FadtConfig::default(),
        &[],
        &dsdt_aml,
        &extra_tables,
    );
    let dsdt = extract_dsdt(&table_set)?;

    let out_dir = workspace_root.join("target/acpi");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("ACPI check: failed to create {}: {e}", out_dir.display()))?;
    let aml_path = out_dir.join(format!("{board_name}-dsdt.aml"));
    let dsl_prefix = out_dir.join(format!("{board_name}-dsdt"));
    let dsl_path = out_dir.join(format!("{board_name}-dsdt.dsl"));
    fs::write(&aml_path, &dsdt)
        .map_err(|e| format!("ACPI check: failed to write {}: {e}", aml_path.display()))?;

    eprintln!(
        "[fstart] ACPI check: wrote DSDT {} ({} bytes)",
        aml_path.display(),
        dsdt.len()
    );
    run_iasl_check(&aml_path, &dsl_prefix, &dsl_path)?;
    Ok(())
}

fn extract_dsdt(table_set: &[u8]) -> Result<Vec<u8>, String> {
    let dsdt_off = table_set
        .windows(4)
        .position(|w| w == b"DSDT")
        .ok_or_else(|| "ACPI check: DSDT signature not found".to_string())?;
    if dsdt_off + 8 > table_set.len() {
        return Err("ACPI check: truncated DSDT header".to_string());
    }
    let len = u32::from_le_bytes(
        table_set[dsdt_off + 4..dsdt_off + 8]
            .try_into()
            .map_err(|_| "ACPI check: invalid DSDT length field".to_string())?,
    ) as usize;
    if dsdt_off + len > table_set.len() {
        return Err(format!(
            "ACPI check: DSDT length {len} extends past table set size {}",
            table_set.len()
        ));
    }
    let dsdt = table_set[dsdt_off..dsdt_off + len].to_vec();
    let sum = dsdt.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
    if sum != 0 {
        return Err(format!("ACPI check: DSDT checksum failed ({sum:#04x})"));
    }
    Ok(dsdt)
}

fn run_iasl_check(aml_path: &Path, dsl_prefix: &Path, dsl_path: &Path) -> Result<(), String> {
    let disassemble = format!(
        "iasl -d -p '{}' '{}'",
        dsl_prefix.display(),
        aml_path.display()
    );
    run_acpica_command(&disassemble, "disassemble")?;

    let compile = format!("iasl -tc '{}'", dsl_path.display());
    run_acpica_command(&compile, "compile")?;

    eprintln!("[fstart] ACPI check: iasl disassemble/compile passed");
    Ok(())
}

fn run_acpica_command(command: &str, phase: &str) -> Result<(), String> {
    let shell = if crate::which_in_path("iasl").is_some() {
        command.to_string()
    } else if crate::which_in_path("nix-shell").is_some() {
        format!("nix-shell -p acpica-tools --run {:?}", command)
    } else {
        return Err(
            "ACPI check requires `iasl` in PATH or `nix-shell -p acpica-tools`".to_string(),
        );
    };

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&shell)
        .output()
        .map_err(|e| format!("ACPI check: failed to run iasl {phase}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ACPI check: iasl {phase} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
