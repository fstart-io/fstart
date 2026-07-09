use std::collections::BTreeSet;

use fstart_core::acpi::AcpiExtraDevice;
use fstart_core::stage::PageSize;
use fstart_core::{
    effective_stage_load_addr, BoardConfig, Platform, RegionKind, SecurityConfig, SocImageFormat,
    StageBuildConfig, StageLayout,
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
                stage_plan(
                    config,
                    &StageContext {
                        build: &stage.build,
                        heap_size: stage.heap_size,
                        page_size: stage.page_size,
                        page_table_addr: stage.page_table_addr,
                        stage_name: Some(stage.name.to_string()),
                        stage_env: if idx == 0 { "car" } else { "ram" },
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

    if let Some(manifest_target) = &manifest.target {
        if manifest_target != target.triple {
            return Err(format!(
                "manifest target '{}' does not match platform-derived target '{}'",
                manifest_target, target.triple
            ));
        }
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

    if config.soc_image_format == SocImageFormat::AllwinnerEgon {
        features.insert("sunxi");
    }

    if needs_aarch64_el2_relocate_entry(config) {
        features.insert("aarch64-el2-relocate-entry");
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
    let needs_alloc = stage_uses_ffs(stage.build)
        || stage.build.fdt
        || stage.build.acpi
        || stage_has_crabefi
        || stage.heap_size.is_some()
        || stage.build.pci;
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
        features.push("lz4");
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

    if build.fdt {
        features.push("fdt");
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
    build.firmware_image.is_some()
        || build.verify_firmware
        || build.load_next_stage.is_some()
        || build.payload
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
        hstr, BoardBuildPolicy, DigestAlgorithm, MemoryMap, MemoryRegion, MonolithicConfig,
        Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageBuildConfig,
        StageLayout,
    };

    use super::{plan, ParsedBoard};

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
            platform: Some("riscv64".to_string()),
            target: Some(Platform::Riscv64.target_triple().to_string()),
            features: vec!["custom-driver".to_string()],
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
    fn plan_rejects_manifest_target_mismatch() {
        let parsed = parsed(minimal_config());
        let mut manifest = manifest();
        manifest.target = Some("wrong-target".to_string());

        let err = plan(&parsed, &manifest).expect_err("target mismatch should fail");
        assert!(err.contains("wrong-target"));
        assert!(err.contains(Platform::Riscv64.target_triple()));
    }
}
