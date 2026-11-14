//! Concrete host execution contract. Platforms choose policy before emitting it.
//! This is transport, not a board-authored workflow or hardware lifecycle DSL.
use crate::plan::Span;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    pub payload: String,
    /// The ISA every stage in this plan targets, stated once by the
    /// platform resolver (board manifests do not restate it for the plan).
    pub platform: fstart_core::Platform,
    pub units: Vec<CompilationUnit>,
    /// Units contributing stage binaries, in image-directory order.
    pub stages: Vec<String>,
    /// Image composition steps, executed in order after the compilation
    /// units: each step consumes stage binaries and the artefacts of the
    /// steps before it. Empty means compiler/check/editor support only:
    /// no image, flash or run contract. Never infer compiler selections
    /// or artifact dependencies from the image steps.
    pub images: Vec<ImageStep>,
    pub inputs: Vec<InputFile>,
    /// How to run the built image under QEMU, if the platform offers an
    /// emulator equivalent. Host-only data: it never reaches firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunRecipe>,
}

/// A QEMU launch description owned by the platform/board: the machine,
/// CPU and memory defaults, how the firmware artefact attaches, and the
/// devices a normal boot carries. The runner renders argv from this
/// data alone — it has no per-machine branches. Machine, CPU and device
/// strings are QEMU's own namespace, passed through verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecipe {
    /// QEMU binary, e.g. `qemu-system-aarch64`.
    pub qemu: String,
    /// `-machine` value, properties included.
    pub machine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Prefer KVM when the host offers it (`FSTART_QEMU_ACCEL` still
    /// overrides); a missing `cpu` then means `host` under KVM, `max`
    /// under TCG.
    #[serde(default)]
    pub accel_kvm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// vCPU count default (`-smp`); `FSTART_QEMU_SMP` overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smp: Option<String>,
    #[serde(default)]
    pub nographic: bool,
    pub media: Vec<RunMedia>,
    /// Verbatim `-device` values.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Verbatim extra argv (console/display setup, `-no-reboot`, …).
    #[serde(default)]
    pub args: Vec<String>,
    /// How a caller-supplied `--disk` attaches, if at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<RunDisk>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RunDisk {
    Ide,
    Nvme,
}

/// Which built artefact a medium carries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RunArtifact {
    /// The FFS step's artefact.
    Ffs,
    /// The final image step's artefact.
    Image,
    /// The first stage's flat binary (plans without image steps).
    Stage,
}

/// One firmware medium of a run. The transforms are QEMU-attachment
/// mechanics (padding to a flash bank, splitting banks, placing the
/// image on a card at the boot ROM's offset, composing an x86
/// top-mapped flash from the FFS and the linked stage), performed by
/// the runner from these parameters — the sizes and offsets are the
/// platform's data, not runner constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunMedia {
    Bios {
        artifact: RunArtifact,
    },
    Pflash {
        artifact: RunArtifact,
        unit: u32,
        size: u64,
        #[serde(default)]
        readonly: bool,
    },
    /// A caller-supplied complete pflash image, bound by input `name`
    /// (e.g. TF-A for a secure bank), checked to be exactly `size`.
    ExternalPflash {
        name: String,
        unit: u32,
        size: u64,
    },
    /// Two pflash banks of `bank_size`: `-bios` when the artefact fits
    /// the first bank, otherwise split across units 0 and 1.
    SplitPflash {
        artifact: RunArtifact,
        bank_size: u64,
    },
    /// A zero-filled SD card of `size` with the artefact at `offset`
    /// (the Allwinner BROM's eGON offset).
    SdCard {
        artifact: RunArtifact,
        offset: u64,
        size: u64,
    },
    Mtd {
        artifact: RunArtifact,
    },
    /// An x86 flash of `size` mapped below 4 GiB: the FFS artefact at
    /// `ffs_offset`, overlaid with `stage_unit`'s ELF flash segments,
    /// its anchor and trust patched from the FFS; attached as `-bios`.
    X86Flash {
        stage_unit: String,
        size: u64,
        ffs_offset: u64,
    },
}

impl RunRecipe {
    /// QEMU's q35 machine with the x86 top-mapped flash composed from
    /// the FFS and `stage_unit`'s linked ELF: the launch shared by the
    /// Intel boards and the q35 machine-model boards. A protected-mode
    /// build runs on a CPU without long mode, like the Core Duo it
    /// exists for.
    pub fn q35(stage_unit: &str, flash_size: u64, ffs_offset: u64, protected_mode: bool) -> Self {
        let strings = |items: &[&str]| items.iter().map(ToString::to_string).collect();
        Self {
            qemu: "qemu-system-x86_64".into(),
            machine: "q35".into(),
            cpu: protected_mode.then(|| "coreduo".into()),
            accel_kvm: true,
            memory: Some("1G".into()),
            smp: Some("1".into()),
            nographic: false,
            media: vec![RunMedia::X86Flash {
                stage_unit: stage_unit.into(),
                size: flash_size,
                ffs_offset,
            }],
            devices: strings(&["bochs-display"]),
            args: strings(&[
                "-no-reboot",
                "-display",
                "none",
                "-chardev",
                "stdio,id=char0,mux=on,signal=off",
                "-serial",
                "chardev:char0",
                "-mon",
                "chardev=char0,mode=readline",
                "-vga",
                "none",
            ]),
            disk: Some(RunDisk::Ide),
        }
    }
}

/// One image-format operation in a plan. The format's config type and its
/// execution live with the format in this crate (`crate::image_steps`
/// dispatches); tools executing a plan never match on formats themselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStep {
    pub name: String,
    pub operation: ImageOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageOperation {
    /// The signed fstart firmware filesystem over the stage binaries.
    Ffs(Box<FfsAssembly>),
    /// An AMD EFS flash image: PSP firmware, the stage the PSP loads and
    /// the artefact of a preceding FFS step in its window, if any.
    AmdEfs(Box<crate::amd::assembly::AmdAssembly>),
    /// An AMD halved flash: the artefact of a preceding FFS step and the
    /// PSP-copied stage unit composed per repeated 16 MiB boot half.
    AmdHalvedFlash(Box<crate::amd::halved_flash::AmdHalvedFlashAssembly>),
}

impl ImageStep {
    pub fn new(name: impl Into<String>, operation: ImageOperation) -> Self {
        Self {
            name: name.into(),
            operation,
        }
    }

    pub fn ffs(assembly: FfsAssembly) -> Self {
        Self::new("ffs", ImageOperation::Ffs(Box::new(assembly)))
    }
}

impl From<FfsAssembly> for ImageStep {
    fn from(image: FfsAssembly) -> Self {
        Self::ffs(image)
    }
}

impl BuildPlan {
    /// The plan's FFS step configuration, if it has one.
    pub fn ffs_assembly(&self) -> Result<&FfsAssembly, String> {
        self.images
            .iter()
            .find_map(|step| match &step.operation {
                ImageOperation::Ffs(image) => Some(image.as_ref()),
                _ => None,
            })
            .ok_or_else(|| "plan has no image assembly/run/flash contract".into())
    }

    pub fn bootstrap(&self) -> &[(String, BootstrapRole)] {
        match self.ffs_assembly() {
            Ok(image) => &image.bootstrap,
            Err(_) => &[],
        }
    }

    pub fn resolve_workspace_inputs(
        &mut self,
        root: &std::path::Path,
        board: &std::path::Path,
    ) -> Result<(), String> {
        for step in &mut self.images {
            if let ImageOperation::Ffs(image) = &mut step.operation {
                image.resolve_workspace_inputs(root, board)?;
            }
        }
        Ok(())
    }

    pub fn config(&self, name: &str) -> Result<fstart_core::BoardConfig, String> {
        self.ffs_assembly()?.config(name)
    }
}

/// Boot-ROM image format the first stage must carry, if the SoC's boot
/// ROM requires one. Host-side only: the linker emits the header
/// sections and the FFS assembler patches their length/checksum fields;
/// firmware never reads this value, which is why it lives in the image
/// builder and not in `fstart-core`'s runtime board config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BootImageFormat {
    /// No boot-ROM header — the binary starts with `.text.entry`.
    #[default]
    None,
    /// Allwinner eGON.BT0: branch over the header at 0x00, `"eGON.BT0"`
    /// magic at 0x04, word-add checksum at 0x0C and total length
    /// (512-byte aligned) at 0x10, patched post-build like U-Boot's
    /// `mksunxiboot`. Required by every Allwinner BROM (sun4i..sun20i).
    AllwinnerEgon,
    /// ASPEED AST2400/AST2500 SPI boot: execute in place from FMC CE0
    /// address 0; the image starts with the ARM vector table and, at
    /// 0x40, the strap block loaded before the CPU is released
    /// (both in `.head.aspeed`).
    AspeedSpi,
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
    /// Files from outside the board directory, packaged as verified data
    /// assets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_assets: Vec<fstart_core::board::FileAsset>,
    pub full_flash_image: bool,
    pub boot_image_format: BootImageFormat,
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
        let mut file_assets = heapless::Vec::new();
        for asset in &self.file_assets {
            file_assets
                .push(asset.clone())
                .map_err(|_| "too many file assets")?;
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
            file_assets,
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
    /// Features of the selected platform dependency, qualified from Cargo's
    /// actual direct dependency alias by the consumer. They include the
    /// payload launcher `--payload` selected, if the unit boots one.
    pub features: Vec<String>,
    pub build_std: Option<String>,
    pub rustflags: Vec<String>,
    pub release_only: bool,
    pub linker_script: Option<String>,
    pub environment_values: BTreeMap<String, String>,
    pub bindings: Vec<ArtifactBinding>,
    pub file_bindings: Vec<FileBinding>,
    /// Features of the board crate itself, unqualified. A board owns one
    /// binary per stage; these only select library code a unit links that
    /// is not a stage `main`, such as the SMM handler.
    #[serde(default)]
    pub board_features: Vec<String>,
    pub output: UnitOutput,
}

impl CompilationUnit {
    /// The board-library SMM handler unit every x86 platform builds:
    /// release-only, no linker script (the SMM image producer links
    /// the handler itself), the board's `smm` library feature, and the
    /// flag set the retained-relocation audit expects. `pie` selects
    /// the relocation model: handlers that take the address of their
    /// SMRAM statics need PIE with PLT calls resolved direct; the
    /// others link static. The 32-bit JSON target has no prebuilt
    /// core, so it builds `core,alloc`.
    #[allow(clippy::too_many_arguments)]
    pub fn smm(
        name: &str,
        target: String,
        features: Vec<String>,
        pie: bool,
        environment_values: BTreeMap<String, String>,
        entry_count: u16,
        stack_size: u32,
        coreboot_module_args: bool,
        coreboot_header: bool,
    ) -> Self {
        // PLT-style call relocations resolve locally at link time in
        // either model, avoiding GOT-indirect calls the audited image
        // cannot carry.
        let relocation: &[&str] = if pie {
            &["-Crelocation-model=pie", "-Zplt=yes"]
        } else {
            &["-Crelocation-model=static", "-Zplt=yes"]
        };
        Self {
            name: name.into(),
            cargo_target: CargoTarget::BoardLibrary,
            build_std: (target == "i686-unknown-none").then(|| "core,alloc".into()),
            target,
            features,
            release_only: true,
            linker_script: None,
            rustflags: ["-Cpanic=abort", "-Copt-level=s"]
                .into_iter()
                .chain(relocation.iter().copied())
                .chain([
                    "-Cno-redzone=yes",
                    "-Clinker-plugin-lto=no",
                    "-Cembed-bitcode=no",
                    "-Zfunction-sections=yes",
                ])
                .map(String::from)
                .collect(),
            environment_values,
            bindings: vec![],
            file_bindings: vec![],
            // The board's SMM handler module, outside every stage `main`.
            board_features: vec!["smm".into()],
            output: UnitOutput::SmmImage {
                entry_count,
                stack_size,
                coreboot_module_args,
                coreboot_header,
            },
        }
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
// A plan has a handful of units: the size of the variants does not matter.
#[allow(clippy::large_enum_variant)]
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

    pub fn validate_image_selection(
        &self,
        selection: &crate::plan::BuildSelection,
    ) -> Result<(), String> {
        // An explicit `--image-config` must be bound unchanged by the one
        // image step that consumes a vendor image; a packed image and
        // plans without such a step bind no image configuration.
        let requested = selection.image_config();
        for step in &self.images {
            let bound = match &step.operation {
                ImageOperation::AmdEfs(image) => match &image.firmware {
                    crate::amd::assembly::Firmware::Vendor { image, .. } => {
                        Some(Some(image.path.as_str()))
                    }
                    crate::amd::assembly::Firmware::Packed { .. } => Some(None),
                },
                // A halved flash may fall back to the board's own vendor
                // ROM, so only an explicit request must match it.
                ImageOperation::AmdHalvedFlash(image) => {
                    return match requested {
                        None => Ok(()),
                        Some(path) if path == image.vendor_image => Ok(()),
                        Some(_) => Err(
                            "image configuration was not bound unchanged by the platform".into(),
                        ),
                    };
                }
                ImageOperation::Ffs(_) => None,
            };
            if let Some(bound) = bound {
                return if requested == bound {
                    Ok(())
                } else {
                    Err("image configuration was not bound unchanged by the platform".into())
                };
            }
        }
        if requested.is_none() {
            Ok(())
        } else {
            Err("image configuration was not bound unchanged by the platform".into())
        }
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
            if self
                .images
                .iter()
                .any(|step| matches!(step.operation, ImageOperation::Ffs(_)))
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
        // Image steps: bounded, uniquely named, and a step that consumes
        // an FFS artefact comes after the FFS step that produces it.
        if self.images.len() > 8 {
            return Err("plan has too many image steps".into());
        }
        let mut image_names = BTreeSet::new();
        let mut seen_ffs = false;
        for step in &self.images {
            if !identifier(&step.name) || !image_names.insert(step.name.as_str()) {
                return Err(format!("invalid or duplicate image step {}", step.name));
            }
            match &step.operation {
                ImageOperation::Ffs(image) if image.platform != self.platform => {
                    return Err(format!(
                        "FFS step {} is for {}, the plan for {}",
                        step.name, image.platform, self.platform
                    ));
                }
                ImageOperation::Ffs(_) => seen_ffs = true,
                ImageOperation::AmdEfs(image) if image.ffs.is_some() && !seen_ffs => {
                    return Err("AMD EFS step consumes an FFS no earlier step produces".into());
                }
                ImageOperation::AmdHalvedFlash(_) if !seen_ffs => {
                    return Err(
                        "AMD halved-flash step consumes an FFS no earlier step produces".into(),
                    );
                }
                _ => {}
            }
        }
        let mut bootstrap = BTreeSet::new();
        // The stage unit's stored bytes are the image the PSP copies; its
        // `.data` may run from a copy elsewhere, as in every noncar image.
        for step in &self.images {
            let ImageOperation::AmdEfs(image) = &step.operation else {
                continue;
            };
            image.validate()?;
            let unit = self.unit(&image.stage_unit)?;
            let UnitOutput::Executable {
                expectations,
                load_address,
                flat_capacity,
            } = &unit.output
            else {
                return Err("PSP reset producer is not executable".into());
            };
            if !self.stages.contains(&image.stage_unit)
                || *load_address != image.reset_destination
                || *flat_capacity != u64::from(image.reset_size)
                || !matches!(expectations.architecture, crate::elf::Architecture::X86_64)
                || !expectations.elf64
                || !expectations.little_endian
                || expectations.copy.is_some()
                || expectations.stored.len() != 1
                || expectations.stored[0].base != image.reset_destination
                || expectations.stored[0].size != u64::from(image.reset_size)
                || expectations.entry
                    != image
                        .reset_destination
                        .checked_add(u64::from(image.reset_size))
                        .and_then(|end| end.checked_sub(16))
            {
                return Err("PSP reset producer differs from image operation".into());
            }
        }
        for step in &self.images {
            if let ImageOperation::AmdHalvedFlash(image) = &step.operation {
                image.validate(self)?;
            }
        }
        for (name, _) in self.bootstrap() {
            if !self.stages.iter().skip(1).any(|s| s == name) || !bootstrap.insert(name) {
                return Err(format!("unknown or duplicate bootstrap binding {name}"));
            }
        }
        self.validate_input_bindings()?;
        Ok(ordered)
    }

    /// External files a caller may bind by name (`--input NAME=PATH`);
    /// the names are the platform's, not a fixed slot list.
    pub fn validate_input_bindings(&self) -> Result<(), String> {
        let mut inputs = BTreeSet::new();
        for input in &self.inputs {
            if !identifier(&input.name) || !inputs.insert(input.name.as_str()) {
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
            features: vec![],
            build_std: None,
            rustflags: vec![],
            release_only: false,
            linker_script: None,
            environment_values: BTreeMap::new(),
            file_bindings: vec![],
            board_features: vec![],
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
            platform: Platform::Aarch64,
            units: vec![
                launch,
                unit("render-cache", &["seed"]),
                unit("seed", &[]),
                unit("spare", &[]),
            ],
            stages: vec!["launch".into()],
            inputs: vec![],
            run: None,
            images: vec![
                FfsAssembly {
                    platform: Platform::Aarch64,
                    memory: vec![],
                    flash: None,
                    bootstrap: vec![],
                    stages: StageLayout::MultiStage(hvec([])),
                    security: default_security_config(),
                    payload: None,
                    microcode: None,
                    data_assets: vec![],
                    file_assets: vec![],
                    full_flash_image: false,
                    boot_image_format: BootImageFormat::None,
                    boot_hart_id: 0,
                    build: BoardBuildPolicy::default(),
                }
                .into(),
            ],
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
        plan.images = vec![];
        plan.order().unwrap();
        assert!(plan.config("test").is_err());
        assert!(plan.bootstrap().is_empty());
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
            security: default_security_config(),
            payload: None,
            microcode: None,
            data_assets: vec![],
            file_assets: vec![],
            full_flash_image: false,
            boot_image_format: BootImageFormat::None,
            boot_hart_id: 0,
            build: BoardBuildPolicy::default(),
        };
        let plan = |units| BuildPlan {
            payload: "halt".into(),
            platform: Platform::X86_64,
            units,
            stages: vec![],
            images: vec![assembly.clone().into()],
            run: None,
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
