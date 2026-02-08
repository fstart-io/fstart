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
    assemble_impl(board_name, false)
}

/// Assemble with explicit release flag.
pub fn assemble_release(board_name: &str, release: bool) -> Result<PathBuf, String> {
    assemble_impl(board_name, release)
}

fn assemble_impl(board_name: &str, release: bool) -> Result<PathBuf, String> {
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
            let stage_data =
                fs::read(&stage.path).map_err(|e| format!("failed to read stage binary: {e}"))?;
            eprintln!(
                "[fstart] stage binary: {} ({} bytes)",
                stage.path.display(),
                stage_data.len()
            );

            ro_files.push(InputFile {
                name: "stage".to_string(),
                file_type: FileType::StageCode,
                segments: vec![InputSegment {
                    name: ".text".to_string(),
                    kind: SegmentKind::Code,
                    data: stage_data,
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
