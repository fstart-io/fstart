//! Build planning for board firmware images.
//!
//! This module turns a parsed board description into stage build plans: target
//! triple, cargo features, build-std selection, and stage image post-processing
//! requirements. Driver features come from board-owned metadata rather than a
//! central device registry.

use std::collections::BTreeSet;

use fstart_core::acpi::AcpiExtraDevice;
use fstart_core::stage::PageSize;
use fstart_core::{
    effective_stage_load_addr, BoardConfig, Capability, Platform, RegionKind, SecurityConfig,
    SocImageFormat, StageLayout,
};

use crate::toolchain::TargetSpec;

/// Board config plus host-only side tables loaded from the selected board crate.
#[derive(Debug, Clone)]
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// ACPI-only descriptors collected separately from runtime devices.
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
}

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

    /// Iterate over feature names in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.features.iter().map(String::as_str)
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

/// Produce a complete build plan for a parsed board using Cargo metadata.
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
    needs_flat_binary: bool,
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
) -> StageBuildPlan {
    let mut features = plan_context.base_features.clone();
    features.extend(capability_features(
        stage.capabilities,
        &config.security,
        config,
    ));

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
    let needs_alloc = stage_uses_ffs(stage.capabilities)
        || stage_uses_fdt(stage.capabilities)
        || stage_uses_acpi(stage.capabilities)
        || stage_has_crabefi
        || stage.heap_size.is_some()
        || stage_uses_pci(stage.capabilities);
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

/// Compute backend feature flags for a single stage.
fn capability_features(
    capabilities: &[Capability],
    security: &SecurityConfig,
    config: &BoardConfig,
) -> Vec<&'static str> {
    let mut features = Vec::new();

    let uses_ffs = stage_uses_ffs(capabilities);
    if uses_ffs {
        features.push("ffs");
        if capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::PayloadLoad))
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

    features
}

fn stage_uses_fdt(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare))
}

fn stage_uses_ffs(capabilities: &[Capability]) -> bool {
    capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    })
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
        .is_some_and(|p| p.kind == fstart_core::PayloadKind::UefiPayload)
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
    use std::path::PathBuf;

    use fstart_core::{
        hstr, BoardBuildPolicy, Capability, DigestAlgorithm, MemoryMap, MemoryRegion,
        MonolithicConfig, Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat,
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

        let mut capabilities = heapless::Vec::new();
        capabilities
            .push(Capability::ConsoleInit)
            .expect("capability capacity");

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
            StageLayout::Monolithic(stage) => stage
                .capabilities
                .push(Capability::SigVerify)
                .expect("capability capacity"),
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
