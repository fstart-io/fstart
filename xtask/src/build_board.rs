//! Board build orchestration.
//!
//! 1. Parse board.ron
//! 2. Determine target triple, cargo features, and environment
//! 3. Invoke cargo build on fstart-stage (once for monolithic, per-stage for multi-stage)
//! 4. Return the path(s) to the built binary(ies)

use fstart_codegen::ron_loader;
use fstart_types::{Capability, Platform, SecurityConfig, SocImageFormat, StageLayout};
use std::path::PathBuf;
use std::process::Command;

/// Result of building a board — one or more stage binaries.
pub struct BuildResult {
    /// Built stage binaries, in order. For monolithic boards this has one entry
    /// with name "stage". For multi-stage boards it has one entry per stage.
    pub stages: Vec<StageBinary>,
}

/// A built stage binary.
pub struct StageBinary {
    /// Stage name (e.g., "bootblock", "main", or "stage" for monolithic).
    pub name: String,
    /// Path to the ELF binary on disk (used by assembler for objcopy).
    pub path: PathBuf,
    /// Path to run in QEMU (flat binary for AArch64, same as `path` otherwise).
    pub run_path: PathBuf,
    /// Load address from the board config.
    pub load_addr: u64,
}

impl BuildResult {
    /// Get the first (or only) binary — used for QEMU boot.
    pub fn primary_binary(&self) -> &StageBinary {
        &self.stages[0]
    }
}

/// Build firmware for the given board. Returns all stage binaries.
pub fn build(board_name: &str, release: bool) -> Result<BuildResult, String> {
    let workspace_root = workspace_root()?;
    let board_dir = workspace_root.join("boards").join(board_name);
    let board_ron = board_dir.join("board.ron");

    if !board_ron.exists() {
        return Err(format!("board config not found: {}", board_ron.display()));
    }

    eprintln!("[fstart] loading board config: {}", board_ron.display());
    let config = ron_loader::load_board_config(&board_ron)?;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] mode: {:?}", config.mode);

    // Determine target triple from the Platform enum.
    let target = config.platform.target_triple();

    // Base features: platform + all driver features (every stage constructs
    // all devices, so driver features are always needed globally).
    let mut base_features = Vec::new();
    base_features.push(config.platform.as_str().to_string());
    for device in &config.devices {
        base_features.push(device.driver.to_string());
    }

    // Multi-stage boards need the handoff feature for inter-stage data passing.
    let is_multi_stage = matches!(&config.stages, StageLayout::MultiStage(_));
    if is_multi_stage {
        base_features.push("handoff".to_string());
    }

    // Check if the board uses a FIT image with runtime parsing (board-level feature)
    let uses_fit_runtime = config.payload.as_ref().is_some_and(|p| {
        p.kind == fstart_types::PayloadKind::FitImage
            && p.fit_parse.unwrap_or(fstart_types::FitParseMode::Buildtime)
                == fstart_types::FitParseMode::Runtime
    });
    if uses_fit_runtime {
        base_features.push("fit".to_string());
    }

    // Allwinner sunxi SoCs use the eGON boot format and need
    // fstart-soc-sunxi for the eGON header, boot media detection,
    // and FEL support.  This is driven by `soc_image_format`, not by
    // the platform — sunxi boards exist on both ARMv7 and AArch64.
    if config.soc_image_format == SocImageFormat::AllwinnerEgon {
        base_features.push("sunxi".to_string());
    }

    // SBSA boards use a special entry point that copies firmware from
    // flash to DRAM before executing (the flash→DRAM gap exceeds the
    // AArch64 ADRP relocation range).
    if config.name.as_str().contains("sbsa") {
        base_features.push("sbsa".to_string());
    }

    eprintln!("[fstart] target: {target}");

    // All bare-metal platforms need flat binaries: AArch64 uses -bios
    // which expects raw binary, RISC-V uses pflash, and ARMv7 Allwinner
    // boot ROM loads raw binary from SD/SPI/eMMC.
    let needs_flat_binary = matches!(
        config.platform,
        Platform::Aarch64 | Platform::Riscv64 | Platform::Armv7 | Platform::X86_64
    );

    let soc_format = config.soc_image_format;

    match &config.stages {
        StageLayout::Monolithic(mono) => {
            // Single build — compute features from this stage's capabilities.
            let mut features = base_features.clone();
            let cap_features = capability_features(&mono.capabilities, &config.security, &config);
            features.extend(cap_features);
            if mono.page_size == fstart_types::stage::PageSize::Size1GiB {
                features.push("x86-1g-pages".to_string());
            }
            let features_str = features.join(",");
            let needs_alloc = stage_uses_fdt(&mono.capabilities)
                || stage_uses_acpi(&mono.capabilities)
                || stage_uses_crabefi(&config)
                || mono.heap_size.is_some();
            let build_std = if needs_alloc { "core,alloc" } else { "core" };

            eprintln!("[fstart] features: {features_str}");

            let (elf_path, run_path) = build_one_stage(
                &workspace_root,
                &board_ron,
                None,
                target,
                &features_str,
                release,
                needs_flat_binary,
                build_std,
                soc_format,
            )?;
            Ok(BuildResult {
                stages: vec![StageBinary {
                    name: "stage".to_string(),
                    path: elf_path,
                    run_path,
                    load_addr: mono.load_addr,
                }],
            })
        }
        StageLayout::MultiStage(stages) => {
            let mut result = Vec::new();
            for (i, stage) in stages.iter().enumerate() {
                let stage_name = stage.name.to_string();
                eprintln!("[fstart] building stage: {stage_name}");

                // Compute per-stage features: base (platform + drivers) +
                // capability-driven features (FFS, crypto, FDT) for THIS
                // stage only. The bootblock doesn't need FFS/crypto/FDT
                // even if the main stage does.
                let mut features = base_features.clone();
                let cap_features =
                    capability_features(&stage.capabilities, &config.security, &config);
                features.extend(cap_features);
                if stage.page_size == fstart_types::stage::PageSize::Size1GiB {
                    features.push("x86-1g-pages".to_string());
                }
                let features_str = features.join(",");

                let stage_has_crabefi = stage
                    .capabilities
                    .iter()
                    .any(|c| matches!(c, Capability::PayloadLoad))
                    && stage_uses_crabefi(&config);
                // PCI and Q35 driver features pull in fstart-alloc, which
                // needs alloc in build-std even for stages that don't
                // directly use PCI. Driver features are global.
                let has_pci_driver = config
                    .devices
                    .iter()
                    .any(|d| d.services.iter().any(|s| s.as_str() == "PciRootBus"));
                let needs_alloc = stage_uses_fdt(&stage.capabilities)
                    || stage_uses_acpi(&stage.capabilities)
                    || stage_has_crabefi
                    || stage.heap_size.is_some()
                    || has_pci_driver;
                let build_std = if needs_alloc { "core,alloc" } else { "core" };

                eprintln!("[fstart] features: {features_str}");

                // Allwinner eGON only applies to the first stage (the one
                // the BROM loads).  Later stages are loaded by fstart.
                let stage_format = if i == 0 {
                    soc_format
                } else {
                    SocImageFormat::None
                };
                let (elf_path, run_path) = build_one_stage(
                    &workspace_root,
                    &board_ron,
                    Some(&stage_name),
                    target,
                    &features_str,
                    release,
                    needs_flat_binary,
                    build_std,
                    stage_format,
                )?;
                result.push(StageBinary {
                    name: stage_name,
                    path: elf_path,
                    run_path,
                    load_addr: stage.load_addr,
                });
            }
            Ok(BuildResult { stages: result })
        }
    }
}

/// Build a single fstart-stage binary.
///
/// `stage_name` is `None` for monolithic, `Some("bootblock")` etc. for multi-stage.
#[allow(clippy::too_many_arguments)]
/// Returns (elf_path, run_path). For AArch64 and RISC-V these differ
/// (ELF vs flat binary for QEMU); for other platforms they are the same.
fn build_one_stage(
    workspace_root: &std::path::Path,
    board_ron: &std::path::Path,
    stage_name: Option<&str>,
    target: &str,
    features: &str,
    release: bool,
    needs_flat_binary: bool,
    build_std: &str,
    soc_format: SocImageFormat,
) -> Result<(PathBuf, PathBuf), String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--package")
        .arg("fstart-stage")
        .arg("--target")
        .arg(target)
        .arg("--features")
        .arg(features)
        .arg("-Z")
        .arg(format!("build-std={build_std}"));

    if release {
        cmd.arg("--release");
    }

    // Disable UB precondition checks on volatile ops (nightly core library
    // adds alignment/null checks on read_volatile/write_volatile that are
    // incompatible with MMIO register access in firmware debug builds).
    //
    // Force strict-align on ARM/RISC-V: the armv7a-none-eabi target spec
    // already sets +strict-align, but we re-assert it here to ensure LLVM
    // never emits unaligned loads/stores to MMIO addresses (device memory
    // faults on unaligned access even when the core supports it for normal
    // memory).  Not applicable to x86_64 (unaligned access is always allowed).
    let rustflags = if target.starts_with("x86_64") {
        // x86_64 firmware: static relocation, large code model.
        // Large model is needed because ROM at 0xFF800000 and RAM/BSS at
        // 0x10000 are ~4 GiB apart, exceeding small/medium/kernel model
        // ±2 GiB limits. LTO (enabled in release profile) mitigates the
        // indirect-call overhead by inlining across crate boundaries.
        // Future: move BSS/stack to addresses within 2 GiB of ROM to
        // allow kernel code model with fast PC-relative calls.
        //
        // Force curve25519-dalek to use the scalar ("serial") backend.
        // This must be in RUSTFLAGS (not Cargo.toml) because:
        // 1. Cargo features can't set --cfg on transitive dependencies
        // 2. .cargo/config.toml would affect host builds too
        // 3. This only applies to x86_64-unknown-none (firmware target)
        //
        // The auto-detected "simd" backend (AVX2) causes LLVM crashes
        // when compiling for x86_64-unknown-none: adding +avx2 globally
        // breaks compiler_builtins (f16/f128 getCopyFromParts mismatch),
        // and without it LLVM can't lower AVX2 intrinsics. The scalar
        // backend is correct and sufficient for firmware signature verify.
        "-Zub-checks=no -Crelocation-model=static -Ccode-model=large \
         --cfg curve25519_dalek_backend=\"serial\""
            .to_string()
    } else {
        "-Zub-checks=no -Ctarget-feature=+strict-align".to_string()
    };
    cmd.env("RUSTFLAGS", &rustflags);

    // Pass board RON path to build.rs
    cmd.env("FSTART_BOARD_RON", board_ron.to_str().unwrap());
    if let Some(name) = stage_name {
        cmd.env("FSTART_STAGE_NAME", name);
    }

    eprintln!("[fstart] building fstart-stage...");
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "build failed{}",
            stage_name
                .map(|n| format!(" for stage '{n}'"))
                .unwrap_or_default()
        ));
    }

    // Determine output binary path
    let profile = if release { "release" } else { "debug" };
    let elf_path = workspace_root
        .join("target")
        .join(target)
        .join(profile)
        .join("fstart-stage");

    // For multi-stage: copy the binary to a stage-specific name so subsequent
    // builds don't overwrite it (cargo always outputs to "fstart-stage").
    let final_elf = if let Some(name) = stage_name {
        let dest = elf_path.with_file_name(format!("fstart-{name}"));
        std::fs::copy(&elf_path, &dest).map_err(|e| format!("failed to copy stage binary: {e}"))?;
        dest
    } else {
        elf_path.clone()
    };

    // Produce a flat binary for QEMU. AArch64 uses -bios which needs a raw
    // binary; RISC-V uses pflash which also needs raw binary data.
    //
    // Both platforms use XIP (code in ROM, data in RAM). The .data
    // section's LMA is in ROM (via `AT > ROM` in the linker script) so it
    // is contiguous with .text/.rodata and must NOT be removed — the _start
    // assembly copies those initializers to RAM. Only .bss is removed: it
    // is NOLOAD and its VMA is in RAM, which would cause objcopy to span
    // the ROM→RAM gap (producing a multi-GiB file of mostly zeros). The
    // entry code clears BSS at runtime.
    let run_path = if needs_flat_binary {
        let bin_path = final_elf.with_extension("bin");
        eprintln!(
            "[fstart] objcopy: {} -> {}",
            final_elf.display(),
            bin_path.display()
        );
        let objcopy_status = Command::new("llvm-objcopy")
            .arg("-O")
            .arg("binary")
            .arg("--remove-section=.bss")
            .arg(&final_elf)
            .arg(&bin_path)
            .status()
            .map_err(|e| format!("failed to run llvm-objcopy: {e}"))?;
        if !objcopy_status.success() {
            return Err("llvm-objcopy failed".to_string());
        }

        // Allwinner eGON: compute the actual binary size, pad to
        // 512-byte alignment, and patch both length and checksum.
        if let SocImageFormat::AllwinnerEgon = soc_format {
            patch_allwinner_egon(&bin_path)?;
        }

        bin_path
    } else {
        final_elf.clone()
    };

    eprintln!("[fstart] built: {}", run_path.display());
    Ok((final_elf, run_path))
}

/// Public wrapper for workspace root (used by other xtask modules).
pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

/// Patch an Allwinner eGON binary: compute size, pad, write length + checksum.
///
/// Like U-Boot's `mksunxiboot` / `sunxi_egon.c`, this computes the
/// image size from the actual binary content (rounded up to 8K block
/// alignment, matching U-Boot's `PAD_SIZE = 8192`), then:
/// 1. Verifies the eGON magic and checksum sentinel
/// 2. Pads the binary to the 8K-aligned size
/// 3. Writes the length at offset 0x10
/// 4. Computes the word-add checksum and writes it at offset 0x0C
/// 5. Self-verifies using U-Boot's `egon_verify_header` algorithm
fn patch_allwinner_egon(bin_path: &std::path::Path) -> Result<(), String> {
    let mut data = std::fs::read(bin_path).map_err(|e| format!("failed to read binary: {e}"))?;

    if data.len() < 96 {
        return Err("binary too small for Allwinner eGON header (< 96 bytes)".to_string());
    }
    if &data[4..12] != b"eGON.BT0" {
        return Err("eGON.BT0 magic not found at offset 0x04".to_string());
    }

    let stamp = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);
    if stamp != 0x5F0A6C39 {
        return Err(format!(
            "eGON checksum sentinel not found (got {stamp:#010x}, expected 0x5F0A6C39)"
        ));
    }

    // Compute the image size: round up to 8K (0x2000) block alignment.
    // U-Boot's sunxi_egon.c uses PAD_SIZE = 8192; the BROM reads this
    // many bytes from the SD card.  512-byte alignment is the minimum
    // the hardware accepts, but U-Boot always pads to 8K blocks.
    let raw_size = data.len();
    let image_size = ((raw_size + 0x1FFF) & !0x1FFF) as u32;

    // Pad to the aligned size.
    data.resize(image_size as usize, 0);

    // Write the length field at offset 0x10.
    data[0x10..0x14].copy_from_slice(&image_size.to_le_bytes());

    // Write the SPL signature at offset 0x14 — "SPL\x02".
    // U-Boot's sunxi SPL includes this so that sunxi-fel and other tools
    // recognise the binary as a version-2 SPL header.
    data[0x14..0x18].copy_from_slice(b"SPL\x02");

    // Compute checksum per U-Boot's gen_check_sum() / egon_set_header():
    //   1. Stamp value (0x5F0A6C39) is already in the checksum field
    //   2. Sum all u32 words (stamp participates in the sum)
    //   3. Write the sum as the checksum
    let mut checksum: u32 = 0;
    for chunk in data.chunks_exact(4) {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        checksum = checksum.wrapping_add(word);
    }

    // Write the checksum at offset 0x0C.
    data[0x0C..0x10].copy_from_slice(&checksum.to_le_bytes());

    // Self-verify using U-Boot's egon_verify_header() algorithm:
    //   1. Save the checksum from the header
    //   2. Put the stamp value back into the checksum field
    //   3. Sum all words up to length/4
    //   4. The sum must equal the saved checksum
    allwinner_egon_verify(&data)?;

    std::fs::write(bin_path, &data).map_err(|e| format!("failed to write patched binary: {e}"))?;

    eprintln!(
        "[fstart] Allwinner eGON patched: raw_size={raw_size:#x}, \
         image_size={image_size:#x}, checksum={checksum:#010x}"
    );
    Ok(())
}

/// Verify an Allwinner eGON binary using U-Boot's verification algorithm.
///
/// Mirrors `egon_verify_header()` from `tools/sunxi_egon.c`:
///   1. Check branch instruction: ARM (`0xEA` at byte 3) or RISC-V JAL (`0x6F` at byte 0)
///   2. Check "eGON.BT0" magic at offset 0x04
///   3. Check length is 512-byte aligned and within buffer
///   4. Save checksum, put stamp back, re-sum, compare
pub(crate) fn allwinner_egon_verify(data: &[u8]) -> Result<(), String> {
    if data.len() < 96 {
        return Err("verify: binary too small".to_string());
    }

    // Branch instruction check:
    // - ARM: byte 3 == 0xEA (ARM `b` opcode)
    // - RISC-V: byte 0 == 0x6F (RISC-V JAL opcode, `j _start`)
    let is_arm_branch = data[3] == 0xEA;
    let is_riscv_jal = (data[0] & 0x7F) == 0x6F; // JAL opcode = 0x6F (bits [6:0])
    if !is_arm_branch && !is_riscv_jal {
        return Err(format!(
            "verify: unrecognized branch instruction (bytes: {:#04x} {:#04x} {:#04x} {:#04x})",
            data[0], data[1], data[2], data[3]
        ));
    }

    // Magic check.
    if &data[4..12] != b"eGON.BT0" {
        return Err("verify: eGON.BT0 magic mismatch".to_string());
    }

    // Read length and checksum from header.
    let length = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]);
    let saved_checksum = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);

    if length == 0 || (length & 0x1FF) != 0 {
        return Err(format!(
            "verify: length {length:#x} is not a positive multiple of 512"
        ));
    }
    if length as usize > data.len() {
        return Err(format!(
            "verify: length {length:#x} exceeds buffer size {:#x}",
            data.len()
        ));
    }

    // Re-compute: put stamp back in checksum field, sum length/4 words.
    let num_words = length as usize / 4;
    let mut verify_sum: u32 = 0;
    for i in 0..num_words {
        let off = i * 4;
        let word = if i == 3 {
            // Checksum field (offset 0x0C) — use stamp value, not stored checksum.
            0x5F0A6C39_u32
        } else {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        verify_sum = verify_sum.wrapping_add(word);
    }

    if verify_sum != saved_checksum {
        return Err(format!(
            "verify: checksum mismatch — stored {saved_checksum:#010x}, \
             computed {verify_sum:#010x}"
        ));
    }

    Ok(())
}

/// Patch the Allwinner eGON header at the start of an FFS image.
///
/// The FFS assembler reads stage ELFs directly, so the eGON header fields
/// (length, SPL signature, checksum) are left at their unpatched defaults.
/// This function patches them in-place using the bootblock size from the
/// standalone build.
///
/// `bootblock_size` is the eGON image size (8K-aligned) from the patched
/// standalone `.bin`. The BROM loads exactly this many bytes into SRAM;
/// the rest of the FFS stays on SD card for later loading.
pub(crate) fn patch_allwinner_egon_ffs(
    ffs_image: &mut [u8],
    bootblock_size: u32,
) -> Result<(), String> {
    if (ffs_image.len() as u32) < bootblock_size {
        return Err(format!(
            "FFS image ({:#x} bytes) is smaller than bootblock size ({:#x})",
            ffs_image.len(),
            bootblock_size
        ));
    }
    if ffs_image.len() < 96 {
        return Err("FFS image too small for eGON header".to_string());
    }
    if &ffs_image[4..12] != b"eGON.BT0" {
        return Err("eGON.BT0 magic not found at start of FFS image".to_string());
    }

    // Write the bootblock length at offset 0x10.
    ffs_image[0x10..0x14].copy_from_slice(&bootblock_size.to_le_bytes());

    // Write the SPL signature at offset 0x14.
    ffs_image[0x14..0x18].copy_from_slice(b"SPL\x02");

    // Put the stamp value back in the checksum field before computing.
    ffs_image[0x0C..0x10].copy_from_slice(&0x5F0A6C39_u32.to_le_bytes());

    // Compute checksum over the bootblock area only (what the BROM loads).
    let bb = &ffs_image[..bootblock_size as usize];
    let mut checksum: u32 = 0;
    for chunk in bb.chunks_exact(4) {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        checksum = checksum.wrapping_add(word);
    }

    // Write the checksum at offset 0x0C.
    ffs_image[0x0C..0x10].copy_from_slice(&checksum.to_le_bytes());

    // Self-verify.
    allwinner_egon_verify(&ffs_image[..bootblock_size as usize])?;

    eprintln!(
        "[fstart] eGON patched in FFS: bootblock_size={bootblock_size:#x}, \
         checksum={checksum:#010x}"
    );
    Ok(())
}

/// Compute the capability-driven feature flags for a single stage.
///
/// Examines the stage's capabilities to determine which FFS, crypto,
/// and FDT features are needed. Driver features are NOT included here
/// (they are always global since every stage constructs all devices).
fn capability_features(
    capabilities: &[Capability],
    security: &SecurityConfig,
    config: &fstart_types::BoardConfig,
) -> Vec<String> {
    let mut features = Vec::new();

    let uses_ffs = capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    });

    if uses_ffs {
        features.push("ffs".to_string());
        // LZ4 decompression support — always enabled when FFS is active
        // so the runtime can handle compressed stage/payload segments.
        features.push("lz4".to_string());
        // Enable crypto features based on board security config
        match security.signing_algorithm {
            fstart_types::SignatureAlgorithm::Ed25519 => features.push("ed25519".to_string()),
            fstart_types::SignatureAlgorithm::EcdsaP256 => {} // future: add ecdsa feature
        }
        for digest in &security.required_digests {
            match digest {
                fstart_types::DigestAlgorithm::Sha256 => {
                    features.push("sha2-digest".to_string());
                }
                fstart_types::DigestAlgorithm::Sha3_256 => {
                    features.push("sha3-digest".to_string());
                }
            }
        }
    }

    if stage_uses_fdt(capabilities) {
        features.push("fdt".to_string());
    }

    // PCI features are determined by the actual driver, not the platform.
    // The Q35 host bridge handles CF8/CFC bootstrap internally.
    if stage_uses_pci(capabilities) {
        // Find the PCI root bus device to determine which driver is used.
        let pci_driver = config
            .devices
            .iter()
            .find(|d| d.services.iter().any(|s| s.as_str() == "PciRootBus"));
        match pci_driver.map(|d| d.driver.as_str()) {
            Some("q35-hostbridge") => features.push("q35-hostbridge".to_string()),
            _ => features.push("pci-ecam".to_string()),
        }
    }

    // CrabEFI is only needed by stages that actually have PayloadLoad.
    // For multi-stage boards, the bootblock doesn't need CrabEFI.
    let has_payload_load = capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if has_payload_load && stage_uses_crabefi(config) {
        features.push("crabefi".to_string());
    }

    if stage_uses_acpi(capabilities) {
        features.push("acpi".to_string());
    }

    if stage_uses_smbios(capabilities) {
        features.push("smbios".to_string());
    }

    // AcpiLoad needs the fw_cfg driver and x86-boot support
    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiLoad { .. }))
    {
        features.push("acpi-load".to_string());
    }

    // MemoryDetect needs the memory-detect feature
    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::MemoryDetect { .. }))
    {
        features.push("memory-detect".to_string());
    }

    // NS16550 PIO mode needs the pio feature propagated
    if config.platform == Platform::X86_64 {
        features.push("ns16550-pio".to_string());
        features.push("x86-boot".to_string());
    }

    features
}

/// Check if a stage's capabilities require the FDT feature.
fn stage_uses_fdt(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare))
}

/// Check if a stage's capabilities require the PCI ECAM feature.
fn stage_uses_pci(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::PciInit { .. }))
}

/// Check if a stage's capabilities require the ACPI feature.
fn stage_uses_acpi(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare | Capability::AcpiLoad { .. }))
}

/// Check if the board uses CrabEFI as a UEFI payload.
fn stage_uses_crabefi(config: &fstart_types::BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == fstart_types::PayloadKind::UefiPayload)
}

/// Check if a stage's capabilities require the SMBIOS feature.
fn stage_uses_smbios(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::SmBiosPrepare))
}

fn workspace_root() -> Result<PathBuf, String> {
    // Walk up from current dir looking for the workspace Cargo.toml
    let mut dir = std::env::current_dir().map_err(|e| format!("no cwd: {e}"))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root".to_string());
        }
    }
}
