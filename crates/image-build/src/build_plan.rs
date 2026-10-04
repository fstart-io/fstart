//! Concrete host execution contract. Platforms choose policy before emitting it.
//! This is transport, not a board-authored workflow or hardware lifecycle DSL.
use crate::plan::Span;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    pub payload: String,
    pub units: Vec<CompilationUnit>,
    /// Units contributing stage binaries, in image-directory order.
    pub stages: Vec<String>,
    /// Image operation or explicit lack of one. Never infer compiler selections
    /// or artifact dependencies from the image projection.
    pub assembly: Assembly,
    pub inputs: Vec<InputFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Assembly {
    Ffs(Box<FfsAssembly>),
    /// Compiler/check/editor support only. No image/flash/run contract supplied.
    Unassembled {
        platform: fstart_core::Platform,
    },
}
impl Assembly {
    pub fn platform(&self) -> fstart_core::Platform {
        match self {
            Self::Ffs(image) => image.platform,
            Self::Unassembled { platform } => *platform,
        }
    }
    pub fn bootstrap(&self) -> &[(String, BootstrapRole)] {
        match self {
            Self::Ffs(image) => &image.bootstrap,
            Self::Unassembled { .. } => &[],
        }
    }
    pub fn ffs(&self) -> Result<&FfsAssembly, String> {
        match self {
            Self::Ffs(image) => Ok(image),
            Self::Unassembled { .. } => Err("plan has no image assembly/run/flash contract".into()),
        }
    }
    pub fn resolve_workspace_inputs(
        &mut self,
        root: &std::path::Path,
        board: &std::path::Path,
    ) -> Result<(), String> {
        match self {
            Self::Ffs(image) => image.resolve_workspace_inputs(root, board),
            Self::Unassembled { .. } => Ok(()),
        }
    }
    pub fn config(&self, name: &str) -> Result<fstart_core::BoardConfig, String> {
        self.ffs()?.config(name)
    }
}
impl From<FfsAssembly> for Assembly {
    fn from(image: FfsAssembly) -> Self {
        Self::Ffs(Box::new(image))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfsAssembly {
    pub platform: fstart_core::Platform,
    pub memory: Vec<fstart_core::MemoryRegion>,
    pub flash: Option<crate::plan::FlashTransport>,
    /// Explicit authenticated-bootstrap ABI bindings, independent of unit names.
    pub bootstrap: Vec<(String, BootstrapRole)>,
    pub stages: fstart_core::StageLayout,
    pub security: fstart_core::SecurityConfig,
    pub payload: Option<fstart_core::PayloadConfig>,
    pub microcode: Option<fstart_core::board::MicrocodeConfig>,
    /// Board-directory files packaged as verified `FileType::Data` assets.
    /// Their names are also the runtime lookup names.
    #[serde(default)]
    pub data_assets: Vec<String>,
    pub full_flash_image: bool,
    pub soc_image_format: fstart_core::SocImageFormat,
    pub boot_hart_id: u32,
    pub build: fstart_core::BoardBuildPolicy,
}
/// Existing authenticated-image wire roles, not firmware-family identities.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BootstrapRole {
    Postcar,
    Mainstage,
}
impl BootstrapRole {
    pub fn wire(self) -> fstart_ffs::root::BootstrapRole {
        match self {
            Self::Postcar => fstart_ffs::root::BootstrapRole::Postcar,
            Self::Mainstage => fstart_ffs::root::BootstrapRole::Mainstage,
        }
    }
}

impl FfsAssembly {
    /// Blob paths emitted by platform Rust are workspace-relative. Convert only
    /// for the legacy assembler's board-relative file API, without inferring policy.
    pub fn resolve_workspace_inputs(
        &mut self,
        root: &std::path::Path,
        board: &std::path::Path,
    ) -> Result<(), String> {
        use std::path::{Component, Path};
        let relative = board
            .strip_prefix(root)
            .map_err(|_| "board outside workspace")?;
        let prefix = "../".repeat(relative.components().count());
        if let Some(fstart_core::board::MicrocodeConfig::Intel(config)) = &mut self.microcode {
            for path in &mut config.files {
                if !Path::new(path.as_str())
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                {
                    return Err("microcode must name a workspace-relative file".into());
                }
                *path = format!("{prefix}{path}")
                    .as_str()
                    .try_into()
                    .map_err(|_| "microcode assembler path too long")?;
            }
        }
        Ok(())
    }

    pub fn config(&self, name: &str) -> Result<fstart_core::BoardConfig, String> {
        use fstart_core::*;
        let mut memory = hvec([]);
        for region in &self.memory {
            memory
                .push(region.clone())
                .map_err(|_| "too many assembly memory regions")?;
        }
        let mut data_assets = heapless::Vec::new();
        for name in &self.data_assets {
            data_assets
                .push(
                    name.as_str()
                        .try_into()
                        .map_err(|_| "data asset name too long")?,
                )
                .map_err(|_| "too many data assets")?;
        }
        Ok(BoardConfig {
            name: name.try_into().map_err(|_| "board name too long")?,
            platform: self.platform,
            memory: MemoryMap {
                regions: memory,
                flash_layout: self
                    .flash
                    .as_ref()
                    .map(crate::plan::FlashTransport::decode)
                    .transpose()?,
                car: None,
            },
            stages: self.stages.clone(),
            security: self.security.clone(),
            payload: self.payload.clone(),
            microcode: self.microcode.clone(),
            data_assets,
            soc_image_format: self.soc_image_format,
            full_flash_image: self.full_flash_image,
            // Tables, live CAR and SMM runtime settings are not assembler inputs.
            build: self.build.clone(),
            acpi: None,
            smbios: None,
            smm: None,
            boot_hart_id: self.boot_hart_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputFile {
    pub name: String,
    pub default: Option<String>,
    pub capacity: u64,
}

/// An explicit compiler input, snapshotted and content-bound by the executor.
/// Paths are workspace-relative or explicit absolute paths, never discovered.
/// Size bounds/hashes record provenance; they do not authenticate vendor bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBinding {
    pub environment: String,
    pub path: String,
    pub capacity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationUnit {
    pub name: String,
    pub cargo_target: CargoTarget,
    pub target: String,
    pub entry: String,
    pub cfg_schema: CompilerCfgSchema,
    pub environment: String,
    pub payload: String,
    /// Features of the selected platform dependency, qualified from Cargo's
    /// actual direct dependency alias by the consumer.
    pub features: Vec<String>,
    pub build_std: Option<String>,
    pub rustflags: Vec<String>,
    pub release_only: bool,
    pub linker_script: Option<String>,
    pub environment_values: BTreeMap<String, String>,
    pub bindings: Vec<ArtifactBinding>,
    pub file_bindings: Vec<FileBinding>,
    pub output: UnitOutput,
}

/// Allowed runtime cfg vocabulary, supplied independently of the selected values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerCfgSchema {
    pub entries: Vec<String>,
    pub environments: Vec<String>,
    pub payloads: Vec<String>,
}
impl CompilationUnit {
    pub fn check_cfg_flags(&self) -> Result<Vec<String>, String> {
        [
            (
                "fstart_stage_env",
                &self.environment,
                &self.cfg_schema.environments,
            ),
            ("fstart_entry", &self.entry, &self.cfg_schema.entries),
            ("fstart_payload", &self.payload, &self.cfg_schema.payloads),
        ]
        .into_iter()
        .map(|(name, selected, allowed)| {
            let unique: BTreeSet<_> = allowed.iter().collect();
            if allowed.is_empty()
                || unique.len() != allowed.len()
                || !allowed.iter().all(|v| cfg_value(v))
                || !allowed.contains(selected)
            {
                return Err(format!(
                    "selected {name} is outside its platform cfg schema"
                ));
            }
            Ok(format!(
                "--check-cfg=cfg({name},values({}))",
                allowed
                    .iter()
                    .map(|v| format!("\"{v}\""))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        })
        .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CargoTarget {
    BoardBinary,
    BoardLibrary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactBinding {
    pub producer: String,
    pub artifact: String,
    pub environment: String,
}

/// Bounded format operations, not platform identities or arbitrary commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnitOutput {
    Executable {
        expectations: crate::elf::Expectations,
        load_address: u64,
        flat_capacity: u64,
    },
    SmmImage {
        entry_count: u16,
        stack_size: u32,
        coreboot_module_args: bool,
        coreboot_header: bool,
    },
}
impl UnitOutput {
    pub fn artifacts(&self) -> &'static [&'static str] {
        match self {
            Self::Executable { .. } => &["elf", "flat"],
            Self::SmmImage {
                coreboot_header: true,
                ..
            } => &["image", "header"],
            Self::SmmImage { .. } => &["image"],
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

/// Portable environment identifiers; unlike unit names, neither a leading digit
/// nor a dash is valid. Compiler flag overrides are separate recorded fields.
pub(crate) fn environment_name(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && !matches!(value, "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "PATH")
}

fn cfg_value(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\"' | '\\'))
}

impl BuildPlan {
    /// Fail closed if platform resolution drops or substitutes an explicit input.
    /// Multiple consuming units may bind the same request, but none may replace it.
    pub fn validate_compiler_selections(
        &self,
        selection: &crate::plan::BuildSelection,
    ) -> Result<(), String> {
        selection.validate_compiler_files()?;
        self.order()?;
        for input in &selection.compiler_files {
            let bindings: Vec<_> = self
                .units
                .iter()
                .flat_map(|unit| &unit.file_bindings)
                .filter(|binding| binding.environment == input.environment)
                .collect();
            if bindings.is_empty() || bindings.iter().any(|binding| binding.path != input.path) {
                return Err(format!(
                    "compiler input {} was not bound unchanged by the platform",
                    input.environment
                ));
            }
        }
        Ok(())
    }

    pub fn unit(&self, name: &str) -> Result<&CompilationUnit, String> {
        self.units
            .iter()
            .find(|u| u.name == name)
            .ok_or_else(|| format!("unknown unit {name}"))
    }

    /// Validate the complete bounded graph, including unreachable units, before
    /// any compiler invocation. Order is deterministic and producers precede users.
    pub fn order(&self) -> Result<Vec<&CompilationUnit>, String> {
        if !identifier(&self.payload) {
            return Err("invalid selected payload name".into());
        }
        if self.units.is_empty() || self.units.len() > 32 {
            return Err("plan needs 1..32 compilation units".into());
        }
        let mut names = BTreeSet::new();
        for unit in &self.units {
            if !identifier(&unit.name) || !names.insert(unit.name.as_str()) {
                return Err(format!("invalid or duplicate unit {}", unit.name));
            }
            if ![&unit.entry, &unit.environment, &unit.payload]
                .into_iter()
                .all(|v| cfg_value(v))
            {
                return Err(format!("invalid compiler cfg value for {}", unit.name));
            }
            if unit
                .environment_values
                .iter()
                .any(|(key, value)| !environment_name(key) || value.contains('\0'))
            {
                return Err(format!("invalid compiler environment for {}", unit.name));
            }
            match (&unit.cargo_target, &unit.output) {
                (CargoTarget::BoardBinary, UnitOutput::Executable { flat_capacity, .. })
                    if unit.linker_script.is_some() && *flat_capacity != 0 => {}
                (
                    CargoTarget::BoardLibrary,
                    UnitOutput::SmmImage {
                        entry_count,
                        stack_size,
                        ..
                    },
                ) if *entry_count != 0 && *stack_size != 0 => {}
                _ => {
                    return Err(format!(
                        "incompatible compiler target/output for {}",
                        unit.name
                    ));
                }
            }
            unit.check_cfg_flags()?;
            if matches!(&self.assembly, Assembly::Ffs(_))
                && matches!(&unit.output,
                UnitOutput::Executable { expectations, .. } if expectations.descriptor.is_none())
            {
                return Err("FFS executable needs its runtime layout descriptor".into());
            }
            let mut bindings = BTreeSet::new();
            for binding in &unit.bindings {
                if !environment_name(&binding.environment)
                    || !bindings.insert(binding.environment.as_str())
                    || unit.environment_values.contains_key(&binding.environment)
                {
                    return Err(format!(
                        "invalid or duplicate binding {}",
                        binding.environment
                    ));
                }
            }
            for binding in &unit.file_bindings {
                if !environment_name(&binding.environment)
                    || !bindings.insert(binding.environment.as_str())
                    || unit.environment_values.contains_key(&binding.environment)
                    || binding.path.is_empty()
                    || binding.path.contains('\0')
                    || binding.capacity == 0
                    || binding
                        .capacity
                        .checked_add(1)
                        .and_then(|size| usize::try_from(size).ok())
                        .is_none()
                {
                    return Err(format!(
                        "invalid or duplicate compiler file {}",
                        binding.environment
                    ));
                }
            }
        }
        for unit in &self.units {
            for binding in &unit.bindings {
                if !self
                    .unit(&binding.producer)?
                    .output
                    .artifacts()
                    .contains(&binding.artifact.as_str())
                {
                    return Err(format!(
                        "unknown artifact {}:{}",
                        binding.producer, binding.artifact
                    ));
                }
            }
        }
        let mut ordered = Vec::new();
        let mut done = BTreeSet::new();
        while ordered.len() < self.units.len() {
            let next = self
                .units
                .iter()
                .find(|unit| {
                    !done.contains(unit.name.as_str())
                        && unit
                            .bindings
                            .iter()
                            .all(|b| done.contains(b.producer.as_str()))
                })
                .ok_or("artifact dependency cycle")?;
            done.insert(next.name.as_str());
            ordered.push(next);
        }
        let mut stages = BTreeSet::new();
        if self.stages.is_empty() {
            return Err("plan has no image stages".into());
        }
        for stage in &self.stages {
            if !stages.insert(stage)
                || !matches!(self.unit(stage)?.output, UnitOutput::Executable { .. })
            {
                return Err(format!("duplicate or non-executable image stage {stage}"));
            }
        }
        let mut bootstrap = BTreeSet::new();
        for (name, _) in self.assembly.bootstrap() {
            if !self.stages.iter().skip(1).any(|s| s == name) || !bootstrap.insert(name) {
                return Err(format!("unknown or duplicate bootstrap binding {name}"));
            }
        }
        self.validate_input_bindings()?;
        Ok(ordered)
    }

    /// The current assembler/CLI only exposes these three payload slots.
    pub fn validate_input_bindings(&self) -> Result<(), String> {
        let mut inputs = BTreeSet::new();
        for input in &self.inputs {
            if !matches!(input.name.as_str(), "kernel" | "firmware" | "fit")
                || !inputs.insert(input.name.as_str())
            {
                return Err(format!(
                    "unknown or duplicate payload input binding {}",
                    input.name
                ));
            }
            Span {
                base: 0,
                size: input.capacity,
            }
            .end()?;
        }
        Ok(())
    }

    /// The same dependency closure is used for build, check and editor views.
    /// The selected unit itself is checked/analyzed; every prerequisite is built.
    pub fn selection(&self, name: &str) -> Result<Vec<&CompilationUnit>, String> {
        self.unit(name)?;
        let ordered = self.order()?;
        let mut needed = BTreeSet::from([name]);
        for unit in ordered.iter().rev() {
            if needed.contains(unit.name.as_str()) {
                needed.extend(unit.bindings.iter().map(|b| b.producer.as_str()));
            }
        }
        Ok(ordered
            .into_iter()
            .filter(|u| needed.contains(u.name.as_str()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::FfsAssembly as Assembly;
    use super::*;
    fn unit(name: &str, dependencies: &[&str]) -> CompilationUnit {
        CompilationUnit {
            name: name.into(),
            cargo_target: CargoTarget::BoardLibrary,
            target: "independent-target".into(),
            entry: "entry".into(),
            cfg_schema: CompilerCfgSchema {
                entries: vec!["entry".into(), "aarch64-relocate".into()],
                environments: vec!["env".into(), "monolithic".into()],
                payloads: vec!["halt".into()],
            },
            environment: "env".into(),
            payload: "halt".into(),
            features: vec![],
            build_std: None,
            rustflags: vec![],
            release_only: false,
            linker_script: None,
            environment_values: BTreeMap::new(),
            file_bindings: vec![],
            bindings: dependencies
                .iter()
                .map(|name| ArtifactBinding {
                    producer: (*name).into(),
                    artifact: "image".into(),
                    environment: format!("INPUT_{}", name.replace('-', "_")),
                })
                .collect(),
            output: UnitOutput::SmmImage {
                entry_count: 1,
                stack_size: 1024,
                coreboot_module_args: false,
                coreboot_header: false,
            },
        }
    }
    fn fixture() -> BuildPlan {
        use fstart_core::*;
        let span = Span {
            base: 0x1000,
            size: 0x1000,
        };
        let mut launch = unit("launch", &["render-cache"]);
        launch.cargo_target = CargoTarget::BoardBinary;
        launch.target = "aarch64-unknown-none".into();
        launch.entry = "aarch64-relocate".into();
        launch.environment = "monolithic".into();
        launch.build_std = Some("core,alloc".into());
        launch.linker_script = Some("ENTRY(_start)".into());
        launch.output = UnitOutput::Executable {
            expectations: crate::elf::Expectations {
                architecture: crate::elf::Architecture::Aarch64,
                elf64: true,
                little_endian: true,
                stored: vec![span],
                runtime: vec![span],
                identity_mapping: true,
                entry: Some(span.base),
                copy: None,
                descriptor: Some(crate::elf::Descriptor {
                    section: ".layout".into(),
                    start_symbol: "begin".into(),
                    end_symbol: "end".into(),
                    bytes: vec![],
                    reservation: span,
                }),
                stored_flags: None,
                symbols: BTreeMap::new(),
            },
            load_address: span.base,
            flat_capacity: span.size,
        };
        BuildPlan {
            payload: "halt".into(),
            units: vec![
                launch,
                unit("render-cache", &["seed"]),
                unit("seed", &[]),
                unit("spare", &[]),
            ],
            stages: vec!["launch".into()],
            inputs: vec![],
            assembly: FfsAssembly {
                platform: Platform::Aarch64,
                memory: vec![],
                flash: None,
                bootstrap: vec![],
                stages: StageLayout::MultiStage(hvec([])),
                security: dev_security_config("unused"),
                payload: None,
                microcode: None,
                data_assets: vec![],
                full_flash_image: false,
                soc_image_format: SocImageFormat::None,
                boot_hart_id: 0,
                build: BoardBuildPolicy::default(),
            }
            .into(),
        }
    }

    #[test]
    fn compiler_only_contract_cannot_launder_a_missing_ffs_descriptor() {
        let mut plan = fixture();
        let UnitOutput::Executable { expectations, .. } = &mut plan.units[0].output else {
            unreachable!()
        };
        expectations.descriptor = None;
        assert!(
            plan.order()
                .unwrap_err()
                .contains("runtime layout descriptor")
        );
        plan.assembly = super::Assembly::Unassembled {
            platform: fstart_core::Platform::Aarch64,
        };
        plan.order().unwrap();
        assert!(plan.assembly.config("test").is_err());
        assert!(plan.assembly.bootstrap().is_empty());
    }

    #[test]
    fn shuffled_non_family_names_select_only_their_transitive_producers() {
        let plan = fixture();
        let names = |units: Vec<&CompilationUnit>| {
            units
                .into_iter()
                .map(|u| u.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(plan.order().unwrap()),
            ["seed", "render-cache", "launch", "spare"]
        );
        assert_eq!(
            names(plan.selection("launch").unwrap()),
            ["seed", "render-cache", "launch"]
        );
        assert_eq!(names(plan.selection("seed").unwrap()), ["seed"]);
        assert_eq!(names(plan.selection("spare").unwrap()), ["spare"]);
        assert_ne!(
            plan.unit("seed").unwrap().target,
            plan.unit("launch").unwrap().target
        );
        assert_ne!(
            plan.unit("seed").unwrap().build_std,
            plan.unit("launch").unwrap().build_std
        );
        assert!(plan.selection("ramstage").is_err());
    }

    #[test]
    fn bindings_inputs_and_cfg_values_are_checked_before_commands() {
        let mut plan = fixture();
        let duplicate = plan.units[0].bindings[0].clone();
        plan.units[0].bindings.push(duplicate);
        assert!(plan.order().unwrap_err().contains("duplicate binding"));
        let mut plan = fixture();
        plan.units[0].bindings[0].artifact = "nonexistent".into();
        assert!(plan.order().unwrap_err().contains("unknown artifact"));
        for invalid in ["1INPUT", "INPUT-NAME", "RUSTFLAGS"] {
            let mut plan = fixture();
            plan.units[0].bindings[0].environment = invalid.into();
            assert!(plan.order().is_err());
        }
        let input = || InputFile {
            name: "kernel".into(),
            default: None,
            capacity: 4096,
        };
        let mut plan = fixture();
        plan.inputs = vec![input(), input()];
        assert!(
            plan.order()
                .unwrap_err()
                .contains("duplicate payload input")
        );
        plan.inputs = vec![InputFile {
            name: "../escape".into(),
            ..input()
        }];
        assert!(plan.order().is_err());
        let mut plan = fixture();
        plan.units[0].entry = "bad\"cfg".into();
        assert!(plan.order().unwrap_err().contains("cfg value"));
    }

    #[test]
    fn compiler_file_names_paths_and_bounds_share_the_binding_namespace() {
        let file = || FileBinding {
            environment: "FSTART_BYTES".into(),
            path: "input.bin".into(),
            capacity: 9600,
        };
        let mut plan = fixture();
        plan.units[0].file_bindings.push(file());
        plan.order().unwrap();
        plan.units[0].file_bindings.push(file());
        assert!(plan.order().is_err());
        let mut plan = fixture();
        let mut collision = file();
        collision.environment = plan.units[0].bindings[0].environment.clone();
        plan.units[0].file_bindings.push(collision);
        assert!(plan.order().is_err());
        let mut plan = fixture();
        plan.units[0]
            .environment_values
            .insert("FSTART_BYTES".into(), "scalar".into());
        plan.units[0].file_bindings.push(file());
        assert!(plan.order().is_err());
        for (environment, path, capacity) in [
            ("RUSTFLAGS", "input", 1),
            ("1FILE", "input", 1),
            ("FILE", "", 1),
            ("FILE", "bad\0path", 1),
            ("FILE", "input", 0),
            ("FILE", "input", u64::MAX),
        ] {
            let mut plan = fixture();
            plan.units[0].file_bindings.push(FileBinding {
                environment: environment.into(),
                path: path.into(),
                capacity,
            });
            assert!(plan.order().is_err());
        }
    }

    #[test]
    fn explicit_file_requests_must_be_bound_without_substitution() {
        use crate::plan::{BuildSelection, CompilerFileSelection};
        let selected = BuildSelection {
            compiler_files: vec![
                CompilerFileSelection::parse("FSTART_BYTES=exact source.bin").unwrap(),
            ],
            ..Default::default()
        };
        let mut plan = fixture();
        assert!(plan.validate_compiler_selections(&selected).is_err());
        let binding = || FileBinding {
            environment: "FSTART_BYTES".into(),
            path: "exact source.bin".into(),
            capacity: 9600,
        };
        plan.units[0].file_bindings.push(binding());
        plan.validate_compiler_selections(&selected).unwrap();
        plan.units[1].file_bindings.push(binding());
        plan.validate_compiler_selections(&selected).unwrap();
        plan.units[1].file_bindings[0].path = "alternate.bin".into();
        assert!(plan.validate_compiler_selections(&selected).is_err());
        let mut duplicate = selected.clone();
        duplicate.compiler_files.extend(selected.compiler_files);
        assert!(plan.validate_compiler_selections(&duplicate).is_err());
    }

    // Graph failures occur before image-stage validation; no firmware fixture is
    // needed to prove missing, duplicate and cyclic producer rejection.
    #[test]
    fn dependency_failures_are_rejected_before_execution() {
        use fstart_core::*;
        let assembly = Assembly {
            platform: Platform::X86_64,
            memory: vec![],
            flash: None,
            bootstrap: vec![],
            stages: StageLayout::MultiStage(hvec([])),
            security: dev_security_config("unused"),
            payload: None,
            microcode: None,
            data_assets: vec![],
            full_flash_image: false,
            soc_image_format: SocImageFormat::None,
            boot_hart_id: 0,
            build: BoardBuildPolicy::default(),
        };
        let plan = |units| BuildPlan {
            payload: "halt".into(),
            units,
            stages: vec![],
            assembly: assembly.clone().into(),
            inputs: vec![],
        };
        assert!(
            plan(vec![unit("a", &["missing"])])
                .order()
                .unwrap_err()
                .contains("unknown unit")
        );
        assert!(
            plan(vec![unit("a", &[]), unit("a", &[])])
                .order()
                .unwrap_err()
                .contains("duplicate unit")
        );
        assert!(
            plan(vec![unit("a", &["b"]), unit("b", &["a"])])
                .order()
                .unwrap_err()
                .contains("cycle")
        );
    }
}
