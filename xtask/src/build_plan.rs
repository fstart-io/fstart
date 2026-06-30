//! Build planning for board firmware images.
//!
//! This module turns a parsed board description into stage build plans: target
//! triple, cargo features, build-std selection, and stage image post-processing
//! requirements. Driver features come from board-owned metadata rather than a
//! central device registry.

use std::collections::BTreeSet;

use fstart_codegen::board_loader::ParsedBoard;
use fstart_services::ServiceKind as Service;
use fstart_types::stage::PageSize;
use fstart_types::{
    effective_stage_load_addr, BoardConfig, BoardDataMode, BuildInfo, Capability, FlowProfile,
    Platform, RegionKind, SecurityConfig, SocImageFormat, StageLayout,
};

use crate::toolchain::TargetSpec;

/// Deterministic cargo feature collection.
#[derive(Debug, Clone, Default)]
pub struct FeatureSet {
    features: BTreeSet<String>,
}

impl FeatureSet {
    /// Insert a feature name.
    pub fn insert(&mut self, feature: impl Into<String>) {
        self.features.insert(feature.into());
    }

    /// Insert all features from an iterator.
    pub fn extend<I, S>(&mut self, features: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for feature in features {
            self.insert(feature);
        }
    }

    /// Whether the set contains a feature.
    pub fn contains(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    /// Return a comma-separated feature list for Cargo.
    pub fn to_cargo_arg(&self) -> String {
        self.features.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

/// Complete build plan for a board.
#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// Target/toolchain policy.
    pub target: TargetSpec,
    /// Per-stage builds to execute.
    pub stages: Vec<StageBuildPlan>,
}

/// Build plan for one `fstart-stage` invocation.
#[derive(Debug, Clone)]
pub struct StageBuildPlan {
    /// Stage name passed to build.rs. `None` means monolithic.
    pub stage_name: Option<String>,
    /// Human-readable label for logging/artifact names.
    pub display_name: String,
    /// Cargo feature set.
    pub features: FeatureSet,
    /// Whether this stage needs a flat `.bin` extracted from ELF PT_LOAD data.
    pub needs_flat_binary: bool,
    /// `-Z build-std=...` value.
    pub build_std: &'static str,
    /// SoC image format to post-process for this stage.
    pub soc_format: SocImageFormat,
    /// Effective load address for packaging.
    pub load_addr: u64,
}

impl StageBuildPlan {
    /// Comma-separated Cargo feature argument.
    pub fn features_arg(&self) -> String {
        self.features.to_cargo_arg()
    }
}

/// Produce a complete build plan for a parsed board using board-owned build metadata.
pub fn plan(parsed: &ParsedBoard, build_info: &BuildInfo) -> Result<BuildPlan, String> {
    let config = &parsed.config;
    let target = TargetSpec::for_platform(config.platform);
    validate_build_info(config, build_info, target)?;
    let base_features = base_features(config, build_info);
    let is_multi_stage = matches!(&config.stages, StageLayout::MultiStage(_));
    let pci_root_feature = config.build.pci_root_feature.as_deref();
    let has_pci_driver = parsed
        .device_services
        .iter()
        .any(|services| services.contains(Service::PciRootBus));
    let plan_context = PlanContext {
        base_features: &base_features,
        flow_profile: build_info.flow_profile,
        needs_flat_binary: target.needs_flat_binary,
        pci_root_feature,
    };

    let stages = match &config.stages {
        StageLayout::Monolithic(stage) => {
            vec![stage_plan(
                config,
                &StageContext {
                    capabilities: &stage.capabilities,
                    heap_size: stage.heap_size,
                    page_size: stage.page_size,
                    page_table_addr: stage.page_table_addr,
                    stage_name: None,
                    display_name: "stage".to_string(),
                    stage_idx: 0,
                    load_addr: stage.load_addr,
                },
                &plan_context,
                config.soc_image_format,
                false,
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
                        capabilities: &stage.capabilities,
                        heap_size: stage.heap_size,
                        page_size: stage.page_size,
                        page_table_addr: stage.page_table_addr,
                        stage_name: Some(stage.name.to_string()),
                        display_name: stage.name.to_string(),
                        stage_idx: idx,
                        load_addr: effective_stage_load_addr(config, idx, stage),
                    },
                    &plan_context,
                    soc_format,
                    has_pci_driver,
                )
            })
            .collect(),
    };

    debug_assert_eq!(is_multi_stage, stages.len() > 1);
    Ok(BuildPlan { target, stages })
}

fn validate_build_info(
    config: &BoardConfig,
    build_info: &BuildInfo,
    target: TargetSpec,
) -> Result<(), String> {
    if build_info.board_data_mode != BoardDataMode::StaticTyped {
        return Err(format!(
            "board '{}' selected {:?}, but xtask static stage builds currently support only StaticTyped board data",
            build_info.name, build_info.board_data_mode
        ));
    }

    if build_info.name.as_str() != config.name.as_str() {
        return Err(format!(
            "build_info name '{}' does not match board config name '{}'",
            build_info.name, config.name
        ));
    }

    if build_info.target.as_str() != target.triple {
        return Err(format!(
            "build_info target '{}' does not match platform-derived target '{}'",
            build_info.target, target.triple
        ));
    }

    let expected_stages = expected_stage_builds(config);
    if build_info.stages.len() != expected_stages.len() {
        return Err(format!(
            "build_info declares {} stage(s), but board config declares {} stage(s)",
            build_info.stages.len(),
            expected_stages.len()
        ));
    }

    for (idx, (actual, expected)) in build_info
        .stages
        .iter()
        .zip(expected_stages.iter())
        .enumerate()
    {
        if actual.name.as_str() != expected.name.as_str() {
            return Err(format!(
                "build_info stage {idx} is named '{}', but board config expects '{}'",
                actual.name, expected.name
            ));
        }
        if actual.load_addr != expected.load_addr {
            return Err(format!(
                "build_info stage '{}' load address {:#x} does not match board config {:#x}",
                actual.name, actual.load_addr, expected.load_addr
            ));
        }
    }

    Ok(())
}

#[derive(Debug)]
struct ExpectedStageBuild {
    name: String,
    load_addr: u64,
}

fn expected_stage_builds(config: &BoardConfig) -> Vec<ExpectedStageBuild> {
    match &config.stages {
        StageLayout::Monolithic(stage) => vec![ExpectedStageBuild {
            name: "stage".to_string(),
            load_addr: stage.load_addr,
        }],
        StageLayout::MultiStage(stages) => stages
            .iter()
            .enumerate()
            .map(|(idx, stage)| ExpectedStageBuild {
                name: stage.name.to_string(),
                load_addr: effective_stage_load_addr(config, idx, stage),
            })
            .collect(),
    }
}

fn base_features(config: &BoardConfig, build_info: &BuildInfo) -> FeatureSet {
    let mut features = FeatureSet::default();
    features.extend(build_info.features.iter().map(|feature| feature.as_str()));

    if matches!(&config.stages, StageLayout::MultiStage(_)) {
        features.insert("handoff");
    }

    let uses_fit_runtime = config.payload.as_ref().is_some_and(|p| {
        p.kind == fstart_types::PayloadKind::FitImage
            && p.fit_parse.unwrap_or(fstart_types::FitParseMode::Buildtime)
                == fstart_types::FitParseMode::Runtime
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
    // Current AArch64 ROM-to-RAM boards use the EL2/TF-A entry protocol.
    // If a future board needs ROM-to-RAM relocation without that protocol,
    // add explicit boot-protocol schema instead of widening this predicate.
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
    flow_profile: FlowProfile,
    needs_flat_binary: bool,
    pci_root_feature: Option<&'a str>,
}

#[derive(Debug)]
struct StageContext<'a> {
    capabilities: &'a [Capability],
    heap_size: Option<u32>,
    page_size: PageSize,
    page_table_addr: Option<(u64, u64)>,
    stage_name: Option<String>,
    display_name: String,
    stage_idx: usize,
    load_addr: u64,
}

fn stage_plan(
    config: &BoardConfig,
    stage: &StageContext<'_>,
    plan_context: &PlanContext<'_>,
    soc_format: SocImageFormat,
    include_global_pci_alloc: bool,
) -> StageBuildPlan {
    let mut features = plan_context.base_features.clone();
    features.extend(flow_profile_features(plan_context.flow_profile));
    features.extend(capability_features(
        stage.capabilities,
        &config.security,
        config,
    ));
    if stage_uses_pci(stage.capabilities) {
        features.insert(plan_context.pci_root_feature.unwrap_or("pci-ecam"));
    }
    if stage_uses_mp(stage.capabilities) {
        if let Some(cpu_feature) = config.build.cpu_feature.as_deref() {
            features.insert(cpu_feature);
        } else if config.platform == Platform::X86_64 {
            features.insert("cpu-generic-x86");
        }
    }

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

    let stage_has_crabefi = stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad))
        && stage_uses_crabefi(config);
    let needs_alloc = stage_uses_fdt(stage.capabilities)
        || stage_uses_acpi(stage.capabilities)
        || stage_has_crabefi
        || stage.heap_size.is_some()
        || include_global_pci_alloc;
    let build_std = if needs_alloc { "core,alloc" } else { "core" };

    StageBuildPlan {
        stage_name: stage.stage_name.clone(),
        display_name: stage.display_name.clone(),
        features,
        needs_flat_binary: plan_context.needs_flat_binary,
        build_std,
        soc_format,
        load_addr: stage.load_addr,
    }
}

/// Compute the coarse fixed-flow feature families selected by board metadata.
fn flow_profile_features(profile: FlowProfile) -> Vec<&'static str> {
    match profile {
        FlowProfile::Minimal => vec!["flow-profile-minimal"],
        FlowProfile::LinuxBoot => vec!["flow-profile-linuxboot"],
        FlowProfile::Uefi => vec!["flow-profile-uefi"],
        FlowProfile::MultiStage => vec!["flow-profile-multistage"],
    }
}

/// Compute backend feature flags for a single stage.
fn capability_features(
    capabilities: &[Capability],
    security: &SecurityConfig,
    config: &BoardConfig,
) -> Vec<&'static str> {
    let mut features = Vec::new();

    let uses_ffs = capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    });

    if uses_ffs {
        features.push("ffs");
        if capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::PayloadLoad))
            && config.payload.as_ref().is_some_and(|payload| {
                payload.kind == fstart_types::PayloadKind::FitImage
                    && payload
                        .fit_parse
                        .unwrap_or(fstart_types::FitParseMode::Buildtime)
                        == fstart_types::FitParseMode::Runtime
            })
        {
            features.push("fit");
        }
        features.push("lz4");
        match security.signing_algorithm {
            fstart_types::SignatureAlgorithm::Ed25519 => features.push("ed25519"),
            fstart_types::SignatureAlgorithm::EcdsaP256 => {}
        }
        for digest in &security.required_digests {
            match digest {
                fstart_types::DigestAlgorithm::Sha256 => features.push("sha2-digest"),
                fstart_types::DigestAlgorithm::Sha3_256 => features.push("sha3-digest"),
            }
        }
    }

    if stage_uses_fdt(capabilities) {
        features.push("fdt");
    }

    let has_payload_load = capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if has_payload_load && stage_uses_crabefi(config) {
        features.push("crabefi");
    }

    if stage_uses_acpi(capabilities) {
        features.push("acpi");
    }

    if stage_uses_smbios(capabilities) {
        features.push("smbios");
    }

    if stage_uses_mp(capabilities) {
        features.push("mp");
    }

    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiLoad))
    {
        features.push("acpi-load");
    }

    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::MemoryDetect))
    {
        features.push("memory-detect");
    }

    if config.platform == Platform::X86_64 {
        features.push("ns16550-pio");
        features.push("x86-boot");
    }

    features
}

fn stage_uses_fdt(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare))
}

fn stage_uses_pci(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::PciInit))
}

fn stage_uses_acpi(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare | Capability::AcpiLoad))
}

fn stage_uses_crabefi(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == fstart_types::PayloadKind::UefiPayload)
}

fn stage_uses_smbios(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::SmBiosPrepare))
}

fn stage_uses_mp(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::MpInit { .. }))
}

#[cfg(test)]
mod tests {
    use fstart_codegen::board_loader::ParsedBoard;
    use fstart_types::{
        hstr, BoardBuildPolicy, BoardDataMode, Build, BuildInfo, BuildProfile, Capability,
        DigestAlgorithm, FlowProfile, MemoryMap, MemoryRegion, MonolithicConfig, Platform,
        RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageBuildInfo,
        StageLayout,
    };

    use super::plan;

    fn minimal_config() -> fstart_types::BoardConfig {
        let mut regions = heapless::Vec::new();
        regions
            .push(MemoryRegion {
                name: hstr("rom"),
                base: 0x1000,
                size: 0x1000,
                kind: RegionKind::Rom,
            })
            .expect("memory region capacity");

        let mut capabilities = heapless::Vec::new();
        capabilities
            .push(Capability::ConsoleInit)
            .expect("capability capacity");

        let mut digests = heapless::Vec::new();
        digests
            .push(DigestAlgorithm::Sha256)
            .expect("digest capacity");

        fstart_types::BoardConfig {
            name: hstr("test-board"),
            platform: Platform::Riscv64,
            memory: MemoryMap {
                regions,
                flash_layout: None,
                car: None,
            },
            devices: heapless::Vec::new(),
            stages: StageLayout::Monolithic(MonolithicConfig {
                capabilities,
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

    fn parsed(config: fstart_types::BoardConfig) -> ParsedBoard {
        ParsedBoard {
            config,
            device_tree: Vec::new(),
            device_services: Vec::new(),
            acpi_only_devices: Vec::new(),
        }
    }

    fn build_info(
        mode: BoardDataMode,
        flow_profile: FlowProfile,
        stage_load_addr: u64,
    ) -> BuildInfo {
        Build::new("test-board")
            .board_package("fstart-board-test")
            .target(Platform::Riscv64.target_triple())
            .profile(BuildProfile::Dev)
            .flow_profile(flow_profile)
            .board_data_mode(mode)
            .stage(StageBuildInfo::new("stage", stage_load_addr))
            .feature("riscv64")
            .feature("custom-driver")
            .build()
    }

    #[test]
    fn plan_uses_build_info_features_and_flow_profile() {
        let parsed = parsed(minimal_config());
        let info = build_info(BoardDataMode::StaticTyped, FlowProfile::Minimal, 0x1000);

        let plan = plan(&parsed, &info).expect("build info should plan");
        let features = &plan.stages[0].features;

        assert!(features.contains("riscv64"));
        assert!(features.contains("custom-driver"));
        assert!(features.contains("flow-profile-minimal"));
        assert!(!features.contains("flow-profile-linuxboot"));
    }

    #[test]
    fn plan_rejects_dynamic_blob_until_runtime_loader_exists() {
        let parsed = parsed(minimal_config());
        let info = build_info(BoardDataMode::DynamicBlob, FlowProfile::Minimal, 0x1000);

        let err = plan(&parsed, &info).expect_err("dynamic blob mode is not wired yet");
        assert!(err.contains("DynamicBlob"));
        assert!(err.contains("StaticTyped"));
    }

    #[test]
    fn plan_rejects_build_info_stage_mismatch() {
        let parsed = parsed(minimal_config());
        let info = build_info(BoardDataMode::StaticTyped, FlowProfile::Minimal, 0x2000);

        let err = plan(&parsed, &info).expect_err("stage load address mismatch should fail");
        assert!(err.contains("load address"));
        assert!(err.contains("0x2000"));
    }
}
