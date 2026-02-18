//! FFS image assembly — `cargo xtask assemble`.
//!
//! Reads a board config, collects built binaries, and assembles them into
//! a signed FFS firmware image using the fstart-ffs builder.

use fstart_ffs::builder::{build_image, FfsImageConfig, InputFile, InputRegion, InputSegment};
use fstart_types::ffs::{
    Compression, FileType, SegmentFlags, SegmentKind, Signature, VerificationKey,
};
use fstart_types::StageLayout;
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
    // For multi-stage builds, convert ELFs to flat binaries so the FFS image
    // is directly bootable (CPU executes from offset 0 of the first file).
    let mut ro_files = Vec::new();

    match &config.stages {
        StageLayout::Monolithic(mono) => {
            let stage = &build_result.stages[0];
            // Convert ELF to flat binary so the FFS image is directly
            // bootable as raw bytes at the flash base address. The CPU
            // begins executing at offset 0 of the FFS image, which must
            // be the _start code — not ELF headers.
            let flat_binary = elf_to_flat_binary(&stage.path)?;
            eprintln!(
                "[fstart] stage flat binary: {} bytes (from {})",
                flat_binary.len(),
                stage.path.display(),
            );

            ro_files.push(InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: flat_binary,
                    load_addr: mono.load_addr,
                    compression: Compression::None,
                    flags: SegmentFlags::CODE,
                }],
            });
        }
        StageLayout::MultiStage(_stages) => {
            for (i, stage_bin) in build_result.stages.iter().enumerate() {
                // Convert ELF to flat binary so the FFS image is directly
                // loadable as raw bytes at the flash base address.
                let flat_binary = elf_to_flat_binary(&stage_bin.path)?;

                // The first file (bootblock) must be uncompressed — it
                // executes directly from flash/RAM and contains the
                // FSTART_ANCHOR placeholder that gets patched in-place.
                // Subsequent stages are LZ4-compressed and decompressed
                // in-place at their load address during StageLoad.
                let compression = if i == 0 {
                    Compression::None
                } else {
                    Compression::Lz4
                };

                let label = match compression {
                    Compression::None => "uncompressed",
                    Compression::Lz4 => "lz4",
                };
                eprintln!(
                    "[fstart] {} flat binary: {} bytes, {} (from {})",
                    stage_bin.name,
                    flat_binary.len(),
                    label,
                    stage_bin.path.display(),
                );

                ro_files.push(InputFile {
                    name: stage_bin.name.clone(),
                    file_type: FileType::StageCode,
                    segments: vec![InputSegment {
                        name: ".text".to_string(),
                        kind: SegmentKind::Code,
                        data: flat_binary,
                        load_addr: stage_bin.load_addr,
                        compression,
                        flags: SegmentFlags::CODE,
                    }],
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

/// Convert an ELF file to a flat binary using `llvm-objcopy -O binary`.
///
/// This strips ELF headers and produces raw bytes starting at the lowest
/// load address. Needed for FFS images that are loaded as raw data.
fn elf_to_flat_binary(elf_path: &Path) -> Result<Vec<u8>, String> {
    use std::process::Command;

    let bin_path = elf_path.with_extension("ffs.bin");
    let status = Command::new("llvm-objcopy")
        .arg("-O")
        .arg("binary")
        .arg(elf_path)
        .arg(&bin_path)
        .status()
        .map_err(|e| format!("failed to run llvm-objcopy: {e}"))?;

    if !status.success() {
        return Err(format!("llvm-objcopy failed for {}", elf_path.display()));
    }

    let data = fs::read(&bin_path)
        .map_err(|e| format!("failed to read flat binary {}: {e}", bin_path.display()))?;

    Ok(data)
}

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
