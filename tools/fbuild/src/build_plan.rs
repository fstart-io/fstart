use std::collections::BTreeSet;

use fstart_core::acpi::AcpiExtraDevice;
use fstart_core::ffs::Compression;
use fstart_core::stage::{POSTCAR_STAGE_NAME, PageSize};
use fstart_core::{
    BoardConfig, Platform, RegionKind, SecurityConfig, SocImageFormat, StageBuildConfig,
    StageLayout, effective_stage_load_addr,
};

use crate::toolchain::TargetSpec;

#[derive(Debug, Clone)]
pub struct ParsedBoard {
    pub config: BoardConfig,
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureSet {
    features: BTreeSet<String>,
}

impl FeatureSet {
    pub fn insert(&mut self, feature: impl Into<String>) {
        self.features.insert(feature.into());
    }

    pub fn extend<I, S>(&mut self, features: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for feature in features {
            self.insert(feature);
        }
    }

    pub fn contains(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.features.iter().map(String::as_str)
    }

    pub fn to_cargo_arg(&self) -> String {
        self.features.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

#[derive(Debug, Clone)]
pub struct BuildPlan {
    pub target: TargetSpec,
    pub stages: Vec<StageBuildPlan>,
}

#[derive(Debug, Clone)]
pub struct StageBuildPlan {
    pub stage_name: Option<String>,
    pub stage_env: &'static str,
    pub display_name: String,
    pub features: FeatureSet,
    pub needs_flat_binary: bool,
    pub build_std: &'static str,
    pub soc_format: SocImageFormat,
    pub load_addr: u64,
}

impl StageBuildPlan {
    pub fn features_arg(&self) -> String {
        self.features.to_cargo_arg()
    }
}

pub fn plan(
    parsed: &ParsedBoard,
    manifest: &crate::board_manifest::BoardManifest,
) -> Result<BuildPlan, String> {
    let config = &parsed.config;
    let target = TargetSpec::for_platform(config.platform);
    validate_manifest(config, manifest, target)?;
    let base_features = base_features(config, manifest);
    let is_multi_stage = matches!(&config.stages, StageLayout::MultiStage(_));
    let plan_context = PlanContext {
        base_features: &base_features,
        needs_flat_binary: target.needs_flat_binary,
    };

    let stages = match &config.stages {
        StageLayout::Monolithic(stage) => {
            vec![stage_plan(
                config,
                &StageContext {
                    build: &stage.build,
                    heap_size: stage.heap_size,
                    page_size: stage.page_size,
                    page_table_addr: stage.page_table_addr,
                    stage_name: None,
                    stage_env: "monolithic",
                    display_name: "stage".to_string(),
                    stage_idx: 0,
                    load_addr: stage.load_addr,
                },
                &plan_context,
                config.soc_image_format,
            )]
        }
        StageLayout::MultiStage(stages) => stages
            .iter()
            .enumerate()
            .map(|(idx, stage)| {
                let soc_format = if idx == 0 {
                    config.soc_image_format
                } else {
                    SocImageFormat::None
                };
                // idx 0 runs before DRAM (CAR/SRAM); the stage named
                // "postcar" is the Cut-B CAR-teardown loader (its own
                // stage_env so `--cfg` fingerprints stay sound across the
                // per-stage rebuilds); everything else runs from DRAM.
                let stage_env = if idx == 0 {
                    "car"
                } else if stage.name.as_str() == POSTCAR_STAGE_NAME {
                    "postcar"
                } else {
                    "ram"
                };
                stage_plan(
                    config,
                    &StageContext {
                        build: &stage.build,
                        heap_size: stage.heap_size,
                        page_size: stage.page_size,
                        page_table_addr: stage.page_table_addr,
                        stage_name: Some(stage.name.to_string()),
                        stage_env,
                        display_name: stage.name.to_string(),
                        stage_idx: idx,
                        load_addr: effective_stage_load_addr(config, idx, stage),
                    },
                    &plan_context,
                    soc_format,
                )
            })
            .collect(),
    };

    debug_assert_eq!(is_multi_stage, stages.len() > 1);
    Ok(BuildPlan { target, stages })
}

fn validate_manifest(
    config: &BoardConfig,
    manifest: &crate::board_manifest::BoardManifest,
    target: TargetSpec,
) -> Result<(), String> {
    if manifest.board != config.name.as_str() {
        return Err(format!(
            "manifest board '{}' does not match board config name '{}'",
            manifest.board, config.name
        ));
    }

    if let Some(manifest_target) = &manifest.target
        && manifest_target != target.triple
    {
        return Err(format!(
            "manifest target '{}' does not match platform-derived target '{}'",
            manifest_target, target.triple
        ));
    }

    Ok(())
}

fn base_features(
    config: &BoardConfig,
    manifest: &crate::board_manifest::BoardManifest,
) -> FeatureSet {
    let mut features = FeatureSet::default();
    features.insert(config.platform.as_str());
    features.extend(manifest.features.iter().map(String::as_str));

    if matches!(&config.stages, StageLayout::MultiStage(_)) {
        features.insert("handoff");
    }

    let uses_fit_runtime = config.payload.as_ref().is_some_and(|p| {
        p.kind == fstart_core::PayloadKind::FitImage
            && p.fit_parse.unwrap_or(fstart_core::FitParseMode::Buildtime)
                == fstart_core::FitParseMode::Runtime
    });
    if uses_fit_runtime {
        features.insert("fit");
    }

    if needs_aarch64_el2_relocate_entry(config) {
        features.insert("aarch64-el2-relocate-entry");
    }
    // 64-bit Allwinner SoCs boot in AArch32 from the BROM; the eGON image
    // needs the RMR AArch32->AArch64 warm-reset entry.
    if config.platform == Platform::Aarch64
        && config.soc_image_format == SocImageFormat::AllwinnerEgon
    {
        features.insert("aarch64-sunxi-rmr-entry");
    }
    // The D1's T-Head C906 needs its vendor cache CSRs configured at entry.
    if config.platform == Platform::Riscv64
        && config.soc_image_format == SocImageFormat::AllwinnerEgon
    {
        features.insert("riscv64-sunxi-entry");
    }

    features
}

fn needs_aarch64_el2_relocate_entry(config: &BoardConfig) -> bool {
    if config.platform != Platform::Aarch64
        || config.soc_image_format == SocImageFormat::AllwinnerEgon
    {
        return false;
    }

    let first_stage_load_addr = match &config.stages {
        StageLayout::Monolithic(stage) => stage.load_addr,
        StageLayout::MultiStage(stages) => stages
            .first()
            .map(|stage| effective_stage_load_addr(config, 0, stage))
            .unwrap_or(0),
    };

    let has_rom_region = config
        .memory
        .regions
        .iter()
        .any(|region| region.kind == RegionKind::Rom);
    let loads_into_ram = config.memory.regions.iter().any(|region| {
        region.kind == RegionKind::Ram
            && first_stage_load_addr >= region.base
            && first_stage_load_addr < region.base.saturating_add(region.size)
    });

    has_rom_region && loads_into_ram
}

#[derive(Debug)]
struct PlanContext<'a> {
    base_features: &'a FeatureSet,
    needs_flat_binary: bool,
}

#[derive(Debug)]
struct StageContext<'a> {
    build: &'a StageBuildConfig,
    heap_size: Option<u32>,
    page_size: PageSize,
    page_table_addr: Option<(u64, u64)>,
    stage_name: Option<String>,
    stage_env: &'static str,
    display_name: String,
    stage_idx: usize,
    load_addr: u64,
}

fn stage_plan(
    config: &BoardConfig,
    stage: &StageContext<'_>,
    plan_context: &PlanContext<'_>,
    soc_format: SocImageFormat,
) -> StageBuildPlan {
    let mut features = plan_context.base_features.clone();
    features.extend(stage_features(stage.build, &config.security, config));

    if stage.page_size == PageSize::Size1GiB {
        features.insert("x86-1g-pages");
    }
    if config.platform == Platform::X86_64 {
        if stage.page_table_addr.is_some() {
            features.insert("x86-writable-page-tables");
        } else if stage.stage_name.is_none() || stage.stage_idx == 0 {
            features.insert("x86-static-page-tables");
        }
    }

    let stage_has_crabefi = stage.build.payload && stage_uses_crabefi(config);
    // Note: the lz4 raw-loader case is included: fstart-stage's allocator
    // module rides on the ffs feature, which lz4 implies, so a stage with
    // the decoder linked needs build-std alloc even without other alloc uses.
    let needs_alloc = stage_uses_ffs(stage.build)
        || stage.build.fdt
        || stage.build.acpi
        || stage_has_crabefi
        || stage.heap_size.is_some()
        || stage.build.pci
        || stage_loads_compressed_next_stage(stage.build, config);
    let build_std = if needs_alloc { "core,alloc" } else { "core" };

    StageBuildPlan {
        stage_name: stage.stage_name.clone(),
        stage_env: stage.stage_env,
        display_name: stage.display_name.clone(),
        features,
        needs_flat_binary: plan_context.needs_flat_binary,
        build_std,
        soc_format,
        load_addr: stage.load_addr,
    }
}

fn stage_features(
    build: &StageBuildConfig,
    security: &SecurityConfig,
    config: &BoardConfig,
) -> Vec<&'static str> {
    let mut features = Vec::new();

    // Stages that read FFS (firmware-image stages, verification, payloads)
    // pull in manifest parsing plus the signature/digest stacks.
    // Raw next-stage loading (e.g. sunxi SRAM bootblocks reading eGON images
    // from MMC with read_stage_to_addr) needs none of that; dragging it into
    // BROM-loaded SRAM windows tens of KiB in size overflows .text.
    // Cut-B postcar is the same kind of raw loader (stash extent + LZ4),
    // with its own feature rules below.
    if stage_uses_ffs(build) {
        features.push("ffs");
        if build.payload
            && config.payload.as_ref().is_some_and(|payload| {
                payload.kind == fstart_core::PayloadKind::FitImage
                    && payload
                        .fit_parse
                        .unwrap_or(fstart_core::FitParseMode::Buildtime)
                        == fstart_core::FitParseMode::Runtime
            })
        {
            features.push("fit");
        }
        // Signature/digest stacks ride only where verification happens:
        // manifest-signature verification (bootblock, ramstage) or payload
        // digest checks. Cut-B postcar verifies nothing — the bootblock
        // authenticated the manifest before postcar ran (ROM-immutable), and
        // the ramstage re-verifies its own bytes — so postcar skips ed25519
        // (~8 KB with curve25519), sha512 (~15 KB, via dalek), the manifest
        // verifier (~15 KB), and the anchor reader (~8 KB): ~46 KB total.
        if stage_verifies(build) {
            match security.signing_algorithm {
                fstart_core::SignatureAlgorithm::Ed25519 => features.push("ed25519"),
                fstart_core::SignatureAlgorithm::EcdsaP256 => {}
            }
            for digest in &security.required_digests {
                match digest {
                    fstart_core::DigestAlgorithm::Sha256 => features.push("sha2-digest"),
                    fstart_core::DigestAlgorithm::Sha3_256 => features.push("sha3-digest"),
                }
            }
        }
    }

    // LZ4 rides only where decompression happens, independent of the FFS
    // gate above: payload stages keep it (conservative — kernels arrive
    // uncompressed today), and a stage whose named next stage is packaged
    // compressed (postcar loading an Lz4 ramstage). The CAR bootblock
    // raw-copies the uncompressed postcar, so it drops the decoder and
    // shrinks out of CAR pressure. "ffs" rides along: the decoder lives
    // in fstart-ffs, and the board maps both strings into fstart-stage.
    if build.payload || stage_loads_compressed_next_stage(build, config) {
        features.push("ffs");
        features.push("lz4");
    }

    if build.fdt {
        features.push("fdt");
    }
    if build.payload
        && config
            .payload
            .as_ref()
            .is_some_and(|payload| payload.kind == fstart_core::PayloadKind::LinuxBoot)
    {
        features.push("linux");
    }
    if build.payload && stage_uses_crabefi(config) {
        features.push("crabefi");
    }
    if build.acpi {
        features.push("acpi");
    }
    if build.smbios {
        features.push("smbios");
    }
    if build.mp.is_some() {
        features.push("mp");
    }

    features
}

fn stage_uses_ffs(build: &StageBuildConfig) -> bool {
    build.firmware_image.is_some() || build.verify_firmware || build.payload
}

/// Whether this stage links the signature/digest stacks.
///
/// True for stages that verify the manifest signature or file digests
/// (firmware-image readers, explicit verifiers, payload loaders). False for
/// raw loaders that move bytes without authenticating them — Cut-B postcar
/// (trusts the bootblock-verified manifest via the stash) and raw eGON/MMC
/// loaders (no manifest at all).
fn stage_verifies(build: &StageBuildConfig) -> bool {
    build.firmware_image.is_some() || build.verify_firmware || build.payload
}

/// Whether this stage must decompress a named next stage packaged with
/// compression (Cut-B postcar loading an Lz4 ramstage). Looks the
/// `load_next_stage` name up in the board's stage layout.
fn stage_loads_compressed_next_stage(build: &StageBuildConfig, config: &BoardConfig) -> bool {
    let Some(next) = build.load_next_stage.as_ref() else {
        return false;
    };
    let StageLayout::MultiStage(stages) = &config.stages else {
        return false;
    };
    stages
        .iter()
        .find(|stage| stage.name.as_str() == next.as_str())
        .is_some_and(|stage| stage.compression != Compression::None)
}

fn stage_uses_crabefi(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == fstart_core::PayloadKind::UefiPayload)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fstart_core::{
        BoardBuildPolicy, Compression, DigestAlgorithm, FirmwareImageConfig, MemoryMap,
        MemoryRegion, MonolithicConfig, Platform, RegionKind, RunsFrom, SecurityConfig,
        SignatureAlgorithm, SocImageFormat, StageBuildConfig, StageConfig, StageLayout, hstr, hvec,
    };

    use super::{ParsedBoard, plan};

    fn minimal_config() -> fstart_core::BoardConfig {
        let mut regions = heapless::Vec::new();
        regions
            .push(MemoryRegion {
                name: hstr("rom"),
                base: 0x1000,
                size: 0x1000,
                kind: RegionKind::Rom,
            })
            .expect("memory region capacity");

        let mut digests = heapless::Vec::new();
        digests
            .push(DigestAlgorithm::Sha256)
            .expect("digest capacity");

        fstart_core::BoardConfig {
            name: hstr("test-board"),
            platform: Platform::Riscv64,
            memory: MemoryMap {
                regions,
                flash_layout: None,
                car: None,
            },
            stages: StageLayout::Monolithic(MonolithicConfig {
                build: StageBuildConfig::default(),
                load_addr: 0x1000,
                stack_size: 0x4000,
                heap_size: None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            }),
            security: SecurityConfig {
                signing_algorithm: SignatureAlgorithm::Ed25519,
                pubkey_file: hstr("keys/dev-signing.pub"),
                required_digests: digests,
            },
            payload: None,
            microcode: None,
            soc_image_format: SocImageFormat::None,
            full_flash_image: false,
            build: BoardBuildPolicy::default(),
            acpi: None,
            smbios: None,
            smm: None,
            boot_hart_id: 0,
        }
    }

    fn parsed(config: fstart_core::BoardConfig) -> ParsedBoard {
        ParsedBoard {
            config,
            acpi_only_devices: Vec::new(),
        }
    }

    fn manifest() -> crate::board_manifest::BoardManifest {
        crate::board_manifest::BoardManifest {
            board: "test-board".to_string(),
            package: "fstart-board-test".to_string(),
            dir: PathBuf::new(),
            rel_dir: PathBuf::new(),
            platform: Some("riscv64".to_string()),
            target: Some(Platform::Riscv64.target_triple().to_string()),
            features: vec!["custom-driver".to_string()],
            variant_features: Vec::new(),
            variants: Vec::new(),
            acpi_only_devices: false,
            stage_bin: Some("fstart-stage".to_string()),
        }
    }

    #[test]
    fn plan_uses_manifest_features() {
        let parsed = parsed(minimal_config());
        let manifest = manifest();

        let plan = plan(&parsed, &manifest).expect("manifest should plan");
        let features = &plan.stages[0].features;

        assert!(features.contains("riscv64"));
        assert!(features.contains("custom-driver"));
    }

    #[test]
    fn plan_selects_security_flow_for_sigverify() {
        let mut config = minimal_config();
        match &mut config.stages {
            StageLayout::Monolithic(stage) => stage.build.verify_firmware = true,
            StageLayout::MultiStage(_) => unreachable!("minimal config is monolithic"),
        }
        let parsed = parsed(config);
        let manifest = manifest();

        let plan = plan(&parsed, &manifest).expect("sigverify stage should plan");
        let features = &plan.stages[0].features;

        assert!(features.contains("ffs"));
        assert!(features.contains("ed25519"));
        assert!(features.contains("sha2-digest"));
    }

    #[test]
    fn plan_armv7_egon_uses_board_features_without_stale_sunxi_feature() {
        let mut config = minimal_config();
        config.platform = Platform::Armv7;
        config.memory.regions[0] = MemoryRegion {
            name: hstr("sram"),
            base: 0,
            size: 0x8000,
            kind: RegionKind::Ram,
        };
        config.stages = StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    load_next_stage: Some(hstr("main")),
                    ..StageBuildConfig::default()
                },
                load_addr: 0,
                stack_size: 0x1000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("main"),
                build: StageBuildConfig::default(),
                load_addr: 0x4100_0000,
                stack_size: 0x10000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ]));
        config.soc_image_format = SocImageFormat::AllwinnerEgon;
        let mut manifest = manifest();
        manifest.platform = Some("armv7".to_string());
        manifest.target = Some(Platform::Armv7.target_triple().to_string());
        manifest.features = vec!["allwinner-a20".to_string()];

        let plan = plan(&parsed(config), &manifest).expect("eGON board should plan");
        assert_eq!(plan.target.triple, Platform::Armv7.target_triple());
        assert_eq!(plan.stages.len(), 2);
        assert!(plan.stages[0].features.contains("armv7"));
        assert!(plan.stages[0].features.contains("allwinner-a20"));
        assert!(plan.stages[0].features.contains("handoff"));
        // A raw next-stage loader reads eGON images straight from MMC; it
        // must not drag the FFS/manifest/crypto stack into the tiny BROM
        // loaded SRAM window.
        assert!(!plan.stages[0].features.contains("ffs"));
        assert!(!plan.stages[0].features.contains("ed25519"));
        assert!(!plan.stages[0].features.contains("lz4"));
        // The DRAM mainstage in real boards verifies firmware and thus keeps
        // the full set; covered by plan_firmware_image_stage_keeps_ffs_features.
        assert!(!plan.stages[0].features.contains("sunxi"));
        assert_eq!(plan.stages[0].soc_format, SocImageFormat::AllwinnerEgon);
        assert_eq!(plan.stages[1].soc_format, SocImageFormat::None);
    }

    #[test]
    fn plan_firmware_image_stage_keeps_ffs_features() {
        let mut config = minimal_config();
        config.stages = StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    firmware_image: Some(FirmwareImageConfig {
                        temp_ram_buffer: None,
                    }),
                    load_next_stage: Some(hstr("main")),
                    ..StageBuildConfig::default()
                },
                load_addr: 0xfff0_0000,
                stack_size: 0x2000,
                heap_size: None,
                runs_from: RunsFrom::Rom,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("main"),
                build: StageBuildConfig {
                    verify_firmware: true,
                    ..StageBuildConfig::default()
                },
                load_addr: 0x0010_0000,
                stack_size: 0x10000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ]));

        let plan = plan(&parsed(config), &manifest()).expect("firmware stage should plan");
        assert!(plan.stages[0].features.contains("ffs"));
        assert!(plan.stages[0].features.contains("ed25519"));
        // The bootblock raw-copies the uncompressed next stage, so the LZ4
        // decoder stays out of the CAR footprint even with FFS enabled.
        assert!(!plan.stages[0].features.contains("lz4"));
        assert!(plan.stages[0].features.contains("sha2-digest"));
        assert!(plan.stages[1].features.contains("ffs"));
    }

    #[test]
    fn plan_stage_loading_compressed_next_stage_keeps_lz4() {
        // Cut-B postcar shape: middle stage loads an Lz4-packaged ramstage.
        let mut config = minimal_config();
        config.stages = StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    firmware_image: Some(FirmwareImageConfig {
                        temp_ram_buffer: None,
                    }),
                    load_next_stage: Some(hstr("postcar")),
                    ..StageBuildConfig::default()
                },
                load_addr: 0xfff0_0000,
                stack_size: 0x2000,
                heap_size: None,
                runs_from: RunsFrom::Rom,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("postcar"),
                build: StageBuildConfig {
                    // No firmware_image / verify_firmware: postcar raw-loads
                    // from the stash extent, no FFS parser or crypto.
                    load_next_stage: Some(hstr("ramstage")),
                    ..StageBuildConfig::default()
                },
                load_addr: 0x0100_0000,
                stack_size: 0x2000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("ramstage"),
                build: StageBuildConfig {
                    verify_firmware: true,
                    ..StageBuildConfig::default()
                },
                load_addr: 0x0400_0000,
                stack_size: 0x10000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::Lz4,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ]));

        let plan = plan(&parsed(config), &manifest()).expect("postcar stages should plan");
        assert_eq!(plan.stages.len(), 3);
        // Bootblock: FFS yes, LZ4 no (raw-copies uncompressed postcar).
        assert!(plan.stages[0].features.contains("ffs"));
        assert!(!plan.stages[0].features.contains("lz4"));
        // Postcar: decompresses the Lz4 ramstage, so it keeps the decoder
        // (ffs rides along via lz4), but carries no signature/digest stack:
        // the bootblock authenticated the manifest and the ramstage
        // re-verifies its own bytes.
        assert!(plan.stages[1].features.contains("ffs"));
        assert!(plan.stages[1].features.contains("lz4"));
        assert!(!plan.stages[1].features.contains("ed25519"));
        assert!(!plan.stages[1].features.contains("sha2-digest"));
    }

    #[test]
    fn plan_rejects_manifest_target_mismatch() {
        let parsed = parsed(minimal_config());
        let mut manifest = manifest();
        manifest.target = Some("wrong-target".to_string());

        let err = plan(&parsed, &manifest).expect_err("target mismatch should fail");
        assert!(err.contains("wrong-target"));
        assert!(err.contains(Platform::Riscv64.target_triple()));
    }
}
