//! Build planning for board firmware images.
//!
//! This module turns a parsed board description into stage build plans: target
//! triple, cargo features, build-std selection, and stage image post-processing
//! requirements.  It intentionally keeps driver classification in the typed
//! device registry instead of duplicating string lists in xtask.

use std::collections::BTreeSet;

use fstart_codegen::ron_loader::ParsedBoard;
use fstart_device_registry::Service;
use fstart_types::stage::PageSize;
use fstart_types::{
    effective_stage_load_addr, BoardConfig, Capability, Platform, RegionKind, SecurityConfig,
    SocImageFormat, StageLayout,
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
    #[cfg(test)]
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

/// Produce a complete build plan for a parsed board.
pub fn plan(parsed: &ParsedBoard) -> BuildPlan {
    let config = &parsed.config;
    let target = TargetSpec::for_platform(config.platform);
    let base_features = base_features(parsed, target);
    let is_multi_stage = matches!(&config.stages, StageLayout::MultiStage(_));
    let pci_root_backend = pci_root_backend(parsed);
    let has_pci_driver = pci_root_backend.is_some();

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
                &base_features,
                target.needs_flat_binary,
                config.soc_image_format,
                false,
                pci_root_backend,
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
                    &base_features,
                    target.needs_flat_binary,
                    soc_format,
                    has_pci_driver,
                    pci_root_backend,
                )
            })
            .collect(),
    };

    debug_assert_eq!(is_multi_stage, stages.len() > 1);
    BuildPlan { target, stages }
}

fn base_features(parsed: &ParsedBoard, target: TargetSpec) -> FeatureSet {
    let config = &parsed.config;
    let mut features = FeatureSet::default();
    features.insert(target.platform_feature);

    for inst in &parsed.driver_instances {
        if let Some(feature) = inst.driver_feature() {
            features.insert(feature);
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PciRootBackend {
    GenericEcam,
    Q35HostBridge,
}

fn pci_root_backend(parsed: &ParsedBoard) -> Option<PciRootBackend> {
    let (idx, _) = parsed
        .device_services
        .iter()
        .enumerate()
        .find(|(_, services)| services.contains(Service::PciRootBus))?;

    if parsed.driver_instances[idx].driver_name() == "q35-hostbridge" {
        Some(PciRootBackend::Q35HostBridge)
    } else {
        Some(PciRootBackend::GenericEcam)
    }
}

fn stage_plan(
    config: &BoardConfig,
    stage: &StageContext<'_>,
    base_features: &FeatureSet,
    needs_flat_binary: bool,
    soc_format: SocImageFormat,
    include_global_pci_alloc: bool,
    pci_root_backend: Option<PciRootBackend>,
) -> StageBuildPlan {
    let mut features = base_features.clone();
    features.extend(capability_features(
        stage.capabilities,
        &config.security,
        config,
        pci_root_backend,
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
        needs_flat_binary,
        build_std,
        soc_format,
        load_addr: stage.load_addr,
    }
}

/// Compute the capability-driven feature flags for a single stage.
fn capability_features(
    capabilities: &[Capability],
    security: &SecurityConfig,
    config: &BoardConfig,
    pci_root_backend: Option<PciRootBackend>,
) -> Vec<&'static str> {
    let mut features = Vec::new();

    for cap in capabilities {
        match cap {
            Capability::ClockInit { .. } => features.push("stage-flow-clock-init"),
            Capability::ConsoleInit { .. } => features.push("stage-flow-console-init"),
            Capability::MemoryInit => features.push("stage-flow-memory-init"),
            Capability::DramInit { .. } => features.push("stage-flow-dram-init"),
            Capability::DriverInit => features.push("stage-flow-driver-init"),
            Capability::PreConsoleInit { .. }
            | Capability::EarlyInit { .. }
            | Capability::StageLocalInit { .. }
            | Capability::PostDramInit { .. }
            | Capability::FinalizeInit { .. } => features.push("stage-flow-phases"),
            Capability::PciInit { .. } => features.push("stage-flow-pci"),
            Capability::MemoryDetect { .. } => features.push("stage-flow-memory-detect"),
            _ => {}
        }
    }

    let uses_ffs = capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    });

    if uses_ffs {
        features.push("stage-flow-ffs");
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
            features.push("stage-flow-payload-fit");
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
        features.push("stage-flow-fdt");
        features.push("fdt");
        if config
            .payload
            .as_ref()
            .is_some_and(|payload| matches!(payload.fdt, fstart_types::FdtSource::Override(_)))
        {
            features.push("stage-flow-fdt-ffs");
        }
    }

    if stage_uses_pci(capabilities) {
        match pci_root_backend {
            Some(PciRootBackend::Q35HostBridge) => features.push("q35-hostbridge"),
            Some(PciRootBackend::GenericEcam) | None => features.push("pci-ecam"),
        }
    }

    let has_payload_load = capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if has_payload_load && stage_uses_crabefi(config) {
        features.push("crabefi");
    }

    if stage_uses_acpi(capabilities) {
        features.push("stage-flow-acpi");
        features.push("acpi");
    }

    if stage_uses_smbios(capabilities) {
        features.push("stage-flow-smbios");
        features.push("smbios");
    }

    for cap in capabilities {
        if let Capability::MpInit { cpu_model, .. } = cap {
            features.push("stage-flow-mp");
            features.push("mp");
            if cpu_model.as_str().contains("pineview")
                || cpu_model.as_str().contains("106cx")
                || cpu_model.as_str().contains("core2")
                || cpu_model.as_str().contains("6fx")
            {
                features.push("intel-cpu");
            }
        }
    }

    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiLoad { .. }))
    {
        features.push("acpi-load");
    }

    if capabilities
        .iter()
        .any(|cap| matches!(cap, Capability::BootMedia(_)))
    {
        features.push("stage-flow-boot-media");
    }

    if capabilities.iter().any(|cap| {
        matches!(
            cap,
            Capability::LoadNextStage { .. } | Capability::ReturnToFel
        )
    }) {
        features.push("stage-flow-fel");
    }

    if capabilities
        .iter()
        .any(|c| matches!(c, Capability::MemoryDetect { .. }))
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
        .any(|c| matches!(c, Capability::PciInit { .. }))
}

fn stage_uses_acpi(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare | Capability::AcpiLoad { .. }))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fstart_codegen::ron_loader;

    fn load_plan(board: &'static str) -> super::BuildPlan {
        // The all-drivers registry has large enum/config values; parse on a
        // larger stack so x86 boards with nested chipset configs are reliable
        // under `cargo test`'s default test-thread stack.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let board_ron = manifest
                    .join("..")
                    .join("boards")
                    .join(board)
                    .join("board.ron");
                let parsed = ron_loader::load_parsed_board(&board_ron)
                    .unwrap_or_else(|e| panic!("failed to load {}: {e}", board_ron.display()));
                super::plan(&parsed)
            })
            .expect("spawn build-plan test loader")
            .join()
            .expect("build-plan test loader panicked")
    }

    #[test]
    fn acpi_only_devices_do_not_become_cargo_features() {
        let plan = load_plan("qemu-sbsa");
        let features = &plan.stages[0].features;

        assert!(!features.contains("ahci"));
        assert!(!features.contains("xhci"));
        assert!(!features.contains("pcie-root"));
        assert!(features.contains("pci-ecam"));
        assert!(features.contains("acpi"));
        assert!(features.contains("aarch64-el2-relocate-entry"));
    }

    #[test]
    fn aarch64_el2_relocate_entry_is_derived_from_memory_layout() {
        let source = include_str!("build_plan.rs");
        assert!(!source.contains(concat!("name.as_str()", ".contains")));

        let qemu_aarch64 = load_plan("qemu-aarch64");
        assert!(!qemu_aarch64.stages[0]
            .features
            .contains("aarch64-el2-relocate-entry"));

        let orangepi_pc2 = load_plan("orangepi-pc2");
        for stage in &orangepi_pc2.stages {
            assert!(!stage.features.contains("aarch64-el2-relocate-entry"));
        }
    }

    #[test]
    fn structural_nodes_do_not_become_cargo_features() {
        let plan = load_plan("foxconn-d41s");
        for stage in &plan.stages {
            assert!(!stage.features.contains(concat!("_", "structural")));
            assert!(stage.features.contains("intel-pineview"));
            assert!(stage.features.contains("intel-ich7"));
        }
    }

    #[test]
    fn q35_multi_stage_keeps_x86_boot_policy() {
        let plan = load_plan("qemu-q35-uefi");
        assert_eq!(plan.stages.len(), 2);

        let bootblock = &plan.stages[0];
        assert!(bootblock.features.contains("x86-boot"));
        assert!(bootblock.features.contains("x86-writable-page-tables"));
        assert_eq!(bootblock.build_std, "core,alloc");

        let main = &plan.stages[1];
        assert!(main.features.contains("x86-boot"));
        assert!(!main.features.contains("x86-static-page-tables"));
        assert!(main.features.contains("crabefi"));
    }

    #[test]
    fn allwinner_boards_enable_sunxi_feature() {
        let plan = load_plan("bananapi-m1");
        for stage in &plan.stages {
            assert!(stage.features.contains("sunxi"));
        }
    }

    #[test]
    fn stage_plan_flow_features_follow_capabilities() {
        let plan = load_plan("bananapi-m1");
        let bootblock = &plan.stages[0];
        assert!(bootblock.features.contains("stage-flow-clock-init"));
        assert!(bootblock.features.contains("stage-flow-console-init"));
        assert!(bootblock.features.contains("stage-flow-dram-init"));
        assert!(bootblock.features.contains("stage-flow-driver-init"));
        assert!(bootblock.features.contains("stage-flow-fel"));
        assert!(!bootblock.features.contains("stage-flow-ffs"));
        assert!(!bootblock.features.contains("stage-flow-fdt"));
    }
}
