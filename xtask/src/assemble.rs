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
use fstart_types::StageLayout;
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
            let segments = parse_elf_segments(&stage.path, Compression::None)?;

            log_stage_segments("stage", &stage.path, &segments);

            // Monolithic stages use the board config load_addr as a fallback,
            // but ELF-parsed segments already have correct p_paddr values.
            // Sanity-check that the entry point matches expectations.
            let elf_data = fs::read(&stage.path)
                .map_err(|e| format!("failed to read {}: {e}", stage.path.display()))?;
            let elf = Elf::parse(&elf_data)
                .map_err(|e| format!("failed to parse {}: {e}", stage.path.display()))?;
            if elf.entry != mono.load_addr {
                eprintln!(
                    "[fstart] note: ELF entry {:#x} differs from board load_addr {:#x}",
                    elf.entry, mono.load_addr,
                );
            }

            ro_files.push(InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments,
            });
        }
        StageLayout::MultiStage(_stages) => {
            for (i, stage_bin) in build_result.stages.iter().enumerate() {
                // The first file (bootblock) must be uncompressed — it
                // executes directly from flash (XIP) and contains the
                // FSTART_ANCHOR placeholder that gets patched in-place.
                // Subsequent stages can be LZ4-compressed.
                let compression = if i == 0 {
                    Compression::None
                } else {
                    Compression::Lz4
                };

                let segments = parse_elf_segments(&stage_bin.path, compression)?;

                log_stage_segments(&stage_bin.name, &stage_bin.path, &segments);

                ro_files.push(InputFile {
                    name: stage_bin.name.clone(),
                    file_type: FileType::StageCode,
                    segments,
                });
            }
        }
    }

    // Add firmware and kernel blobs if this board has a LinuxBoot payload.
    //
    // Resolution order for paths:
    //   1. CLI flags (--kernel, --firmware)
    //   2. Board RON payload config (payload.firmware.file, payload.kernel_file)
    //      resolved relative to the board directory
    //   3. Skip — no external blob added
    if let Some(ref payload) = config.payload {
        // Resolve firmware blob path
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
                        compression: Compression::None,
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

    // Write the FFS image
    let output_dir = workspace_root.join("target").join("ffs");
    fs::create_dir_all(&output_dir).map_err(|e| format!("failed to create output dir: {e}"))?;

    let image_path = output_dir.join(format!("{}.ffs", config.name));
    fs::write(&image_path, &ffs_image.image)
        .map_err(|e| format!("failed to write FFS image: {e}"))?;

    eprintln!(
        "[fstart] FFS image: {} ({} bytes)",
        image_path.display(),
        ffs_image.image.len()
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
