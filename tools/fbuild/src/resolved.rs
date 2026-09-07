//! Metadata-owned, fixed-capacity monolithic XIP builds.
//!
//! The first supported entry is RISC-V virt. This module resolves geometry and
//! compiler selections, not hardware sequencing. BoardConfig is only a derived
//! adapter into the existing image assembler, never an authored input here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use fstart_core::layout::{Region, RegionKind};
use fstart_core::*;
use fstart_image_build::layout::EncodedLayout;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
#[path = "resolved_tests.rs"]
pub(crate) mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRef {
    pub dependency: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Overrides {
    pub image_capacity: Option<u64>,
    pub writable_capacity: Option<u64>,
    pub stack: Option<u64>,
    pub heap: Option<u64>,
    pub firmware_offset: Option<u64>,
    pub firmware_capacity: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub base: u64,
    pub size: u64,
}

impl Span {
    fn end(self) -> Result<u64, String> {
        if self.size == 0 {
            return Err("empty layout range".into());
        }
        self.base
            .checked_add(self.size)
            .ok_or_else(|| "layout range overflows".into())
    }
    pub fn contains(self, base: u64, size: u64) -> bool {
        base >= self.base
            && base
                .checked_add(size)
                .zip(self.base.checked_add(self.size))
                .is_some_and(|(end, limit)| end <= limit)
    }
    fn overlaps(self, other: Self) -> bool {
        self.base < other.base + other.size && other.base < self.base + self.size
    }
    fn region(self, kind: RegionKind) -> Region {
        Region {
            kind,
            base: self.base,
            size: self.size,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Payload {
    features: Vec<String>,
    kernel: Option<Span>,
    kernel_file: Option<String>,
    firmware: Option<Span>,
    firmware_file: Option<String>,
    device_tree: Option<Span>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Profile {
    target: String,
    entry: String,
    env: String,
    features: Vec<String>,
    flash: Span,
    image_capacity: u64,
    firmware_offset: u64,
    firmware_capacity: u64,
    writable: Span,
    stack: u64,
    heap: u64,
    default_payload: String,
    payloads: BTreeMap<String, Payload>,
    security: SecurityConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformMetadata {
    schema: u32,
    layouts: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedBuild {
    pub(crate) target: String,
    pub(crate) entry: String,
    pub(crate) env: String,
    pub(crate) payload: String,
    pub(crate) features: Vec<String>,
    pub(crate) flash: Span,
    pub(crate) image: Span,
    pub(crate) firmware: Span,
    pub(crate) writable: Span,
    pub(crate) stack: Span,
    pub(crate) heap: Span,
    inputs: Payload,
    security: SecurityConfig,
    origins: BTreeMap<String, String>,
}

impl ResolvedBuild {
    pub fn load(
        root: &Path,
        board: &crate::board_manifest::BoardManifest,
        choice: Option<crate::payload::PayloadChoice>,
    ) -> Result<Self, String> {
        let reference = board
            .build_profile
            .as_ref()
            .ok_or("missing build-profile")?;
        let workspace = crate::build_board::prepare_selected_board_workspace(root, board)?;
        let output = Command::new("cargo")
            .current_dir(root)
            .args([
                "metadata",
                "--format-version=1",
                "--no-default-features",
                "--features",
                "stage",
            ])
            .arg("--manifest-path")
            .arg(workspace.join("Cargo.toml"))
            .output()
            .map_err(|e| format!("cargo metadata: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        let packages = metadata["packages"]
            .as_array()
            .ok_or("Cargo metadata has no packages")?;
        let board_path = board
            .dir
            .join("Cargo.toml")
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let package = packages
            .iter()
            .find(|p| {
                p["manifest_path"]
                    .as_str()
                    .and_then(|s| Path::new(s).canonicalize().ok())
                    .as_ref()
                    == Some(&board_path)
            })
            .ok_or("selected board missing from Cargo metadata")?;
        let dependencies = package["dependencies"]
            .as_array()
            .ok_or("missing board dependencies")?;
        if !dependencies
            .iter()
            .any(|d| d["rename"].as_str().or(d["name"].as_str()) == Some(&reference.dependency))
        {
            return Err(format!(
                "build-profile dependency '{}' is not direct",
                reference.dependency
            ));
        }
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("missing Cargo resolve graph")?;
        let node = nodes
            .iter()
            .find(|node| node["id"] == package["id"])
            .ok_or("missing board resolve node")?;
        let key = reference.dependency.replace('-', "_");
        let dep = node["deps"]
            .as_array()
            .ok_or("missing resolved dependencies")?
            .iter()
            .find(|dep| dep["name"].as_str() == Some(&key))
            .ok_or("profile dependency is not enabled")?;
        let platform = packages
            .iter()
            .find(|p| p["id"] == dep["pkg"])
            .ok_or("missing platform package")?;
        let mut declared: PlatformMetadata =
            serde_json::from_value(platform["metadata"]["fstart"].clone())
                .map_err(|e| format!("platform fstart metadata: {e}"))?;
        if declared.schema != 1 {
            return Err("unsupported platform metadata schema".into());
        }
        let profile = declared
            .layouts
            .remove(&reference.name)
            .ok_or_else(|| format!("unknown layout '{}'", reference.name))?;
        let source = format!(
            "{}:package.metadata.fstart.layouts.{}",
            platform["manifest_path"].as_str().unwrap_or("platform"),
            reference.name
        );
        let mut resolved =
            Self::resolve(profile, &board.layout, choice.map(|c| c.as_str()), &source)?;
        if board
            .platform
            .as_deref()
            .is_some_and(|platform| platform != "riscv64")
        {
            return Err("board platform conflicts with resolved entry".into());
        }
        if board
            .target
            .as_deref()
            .is_some_and(|target| target != resolved.target)
        {
            return Err("board target conflicts with build profile".into());
        }
        if !board.features.is_empty() {
            return Err(
                "metadata builds use profile dependency features, not board feature forwarding"
                    .into(),
            );
        }
        resolved
            .features
            .extend(board.variant_features.iter().cloned());
        resolved.features.sort();
        resolved.features.dedup();
        validate_features(&resolved.features, package, &metadata)?;
        Ok(resolved)
    }

    fn resolve(
        mut p: Profile,
        overrides: &Overrides,
        choice: Option<&str>,
        source: &str,
    ) -> Result<Self, String> {
        if p.target != "riscv64gc-unknown-none-elf" || p.entry != "riscv64" || p.env != "monolithic"
        {
            return Err("unsupported target/entry/environment for resolved XIP layout".into());
        }
        let mut origins = BTreeMap::new();
        for key in [
            "image-capacity",
            "writable-capacity",
            "stack",
            "heap",
            "firmware-offset",
            "firmware-capacity",
        ] {
            origins.insert(key.into(), source.into());
        }
        for (key, value, field) in [
            (
                "image-capacity",
                overrides.image_capacity,
                &mut p.image_capacity,
            ),
            (
                "writable-capacity",
                overrides.writable_capacity,
                &mut p.writable.size,
            ),
            ("stack", overrides.stack, &mut p.stack),
            ("heap", overrides.heap, &mut p.heap),
            (
                "firmware-offset",
                overrides.firmware_offset,
                &mut p.firmware_offset,
            ),
            (
                "firmware-capacity",
                overrides.firmware_capacity,
                &mut p.firmware_capacity,
            ),
        ] {
            if let Some(value) = value {
                *field = value;
                origins.insert(key.into(), "board:package.metadata.fstart.layout".into());
            }
        }
        let payload = choice.unwrap_or(&p.default_payload).to_string();
        if !matches!(payload.as_str(), "halt" | "linux" | "uefi") {
            return Err(format!("unsupported payload '{payload}'"));
        }
        let inputs = p
            .payloads
            .remove(&payload)
            .ok_or_else(|| format!("profile does not support payload '{payload}'"))?;
        let image = Span {
            base: p.flash.base,
            size: p.image_capacity,
        };
        let firmware = Span {
            base: p
                .flash
                .base
                .checked_add(p.firmware_offset)
                .ok_or("firmware offset overflow")?,
            size: p.firmware_capacity,
        };
        for (name, range) in [
            ("flash", p.flash),
            ("image", image),
            ("firmware", firmware),
            ("writable", p.writable),
        ] {
            range.end().map_err(|e| format!("{name}: {e}"))?;
            if range.base % 16 != 0 || range.size % 16 != 0 {
                return Err(format!("{name}: base/size must be 16-byte aligned"));
            }
        }
        if !p.flash.contains(image.base, image.size)
            || !p.flash.contains(firmware.base, firmware.size)
            || image.overlaps(firmware)
        {
            return Err("image/firmware partitions overlap or exceed flash".into());
        }
        if p.flash.overlaps(p.writable) {
            return Err("writable reservation overlaps flash".into());
        }
        if p.stack == 0 || p.heap == 0 || !p.stack.is_multiple_of(16) || !p.heap.is_multiple_of(16)
        {
            return Err("stack/heap budgets must be nonzero and 16-byte aligned".into());
        }
        let rest = p
            .writable
            .size
            .checked_sub(p.stack)
            .and_then(|n| n.checked_sub(p.heap))
            .ok_or("stack/heap exceed writable capacity")?;
        if rest == 0 {
            return Err("writable capacity leaves no data/BSS space".into());
        }
        let heap = Span {
            base: p.writable.base + rest,
            size: p.heap,
        };
        let stack = Span {
            base: heap.base + heap.size,
            size: p.stack,
        };
        if inputs
            .device_tree
            .is_some_and(|range| range.size != 0x10000)
        {
            return Err("the current FDT workspace requires a 64 KiB reservation".into());
        }
        let mut ranges = vec![p.flash, p.writable];
        for range in [inputs.kernel, inputs.firmware, inputs.device_tree]
            .into_iter()
            .flatten()
        {
            range.end()?;
            if range.base % 16 != 0
                || range.size % 16 != 0
                || ranges.iter().any(|r| r.overlaps(range))
            {
                return Err("payload ranges are unaligned or overlap another reservation".into());
            }
            ranges.push(range);
        }
        if (payload == "linux" && (inputs.kernel.is_none() || inputs.kernel_file.is_none()))
            || (payload != "halt"
                && (inputs.firmware.is_none()
                    || inputs.firmware_file.is_none()
                    || inputs.device_tree.is_none()))
            || (payload == "halt"
                && (inputs.kernel.is_some()
                    || inputs.firmware.is_some()
                    || inputs.device_tree.is_some()))
        {
            return Err("payload profile has missing or incompatible ranges/inputs".into());
        }
        if payload == "uefi" && (inputs.kernel.is_some() || inputs.kernel_file.is_some()) {
            return Err("UEFI profile cannot include a direct kernel input".into());
        }
        let mut features = p.features;
        features.extend(inputs.features.iter().cloned());
        Ok(Self {
            target: p.target,
            entry: p.entry,
            env: p.env,
            payload,
            features,
            flash: p.flash,
            image,
            firmware,
            writable: p.writable,
            stack,
            heap,
            inputs,
            security: p.security,
            origins,
        })
    }

    pub fn descriptor(&self) -> Result<EncodedLayout, String> {
        let mut regions = vec![
            self.image.region(RegionKind::Image),
            self.writable.region(RegionKind::Writable),
            self.stack.region(RegionKind::Stack),
            self.heap.region(RegionKind::Heap),
            self.flash.region(RegionKind::Flash),
            self.firmware.region(RegionKind::Firmware),
        ];
        for (kind, range) in [
            (RegionKind::Payload, self.inputs.kernel),
            (RegionKind::PayloadFirmware, self.inputs.firmware),
            (RegionKind::DeviceTree, self.inputs.device_tree),
        ] {
            if let Some(range) = range {
                regions.push(range.region(kind));
            }
        }
        EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())
    }

    /// Type-check exactly the same target, features and cfgs as a firmware build.
    pub fn check(
        &self,
        root: &Path,
        board: &crate::board_manifest::BoardManifest,
        release: bool,
    ) -> Result<(), String> {
        crate::resolved_build::check(root, board, self, release)
    }

    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        // Cargo does not track -T file contents. Include the projection as
        // well as its input so linker-generator-only edits also force relink.
        let mut hasher = Sha256::new();
        hasher.update(self.json()?.as_bytes());
        hasher.update([0]);
        hasher.update(crate::linker::resolved_xip(self)?.as_bytes());
        hasher.update([0]);
        hasher.update(crate::toolchain::rustflags_for_triple(&self.target).as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        Ok(root
            .join("target/fstart-build")
            .join(board)
            .join(if release { "release" } else { "debug" })
            .join("stage")
            .join(digest))
    }

    /// Project resolved values into the existing assembler API. No linker or
    /// compiler decision is inferred back from this temporary value.
    pub fn assembler_config(&self, board: &str) -> Result<BoardConfig, String> {
        let payload = if self.payload == "halt" {
            None
        } else {
            Some(PayloadConfig {
                kind: if self.payload == "linux" {
                    PayloadKind::LinuxBoot
                } else {
                    PayloadKind::UefiPayload
                },
                kernel_file: self
                    .inputs
                    .kernel_file
                    .as_deref()
                    .map(hstring)
                    .transpose()?,
                kernel_load_addr: self.inputs.kernel.map(|r| r.base),
                fdt: FdtSource::Platform,
                dtb_addr: self.inputs.device_tree.map(|r| r.base),
                src_dtb_addr: None,
                bootargs: None,
                print_x86_mtrrs: false,
                compression: Compression::Lz4,
                firmware: self
                    .inputs
                    .firmware
                    .map(|range| {
                        Ok::<_, String>(FirmwareConfig {
                            kind: FirmwareKind::OpenSbi,
                            file: hstring(
                                self.inputs
                                    .firmware_file
                                    .as_deref()
                                    .ok_or("missing firmware file")?,
                            )?,
                            load_addr: range.base,
                        })
                    })
                    .transpose()?,
                fit_file: None,
                fit_config: None,
                fit_parse: None,
            })
        };
        Ok(BoardConfig {
            name: hstring(board)?,
            platform: Platform::Riscv64,
            memory: MemoryMap {
                regions: hvec([
                    MemoryRegion {
                        name: hstr("flash"),
                        base: self.flash.base,
                        size: self.flash.size,
                        kind: fstart_core::RegionKind::Rom,
                    },
                    MemoryRegion {
                        name: hstr("stage-reservation"),
                        base: self.writable.base,
                        size: self.writable.size,
                        kind: fstart_core::RegionKind::Ram,
                    },
                ]),
                flash_layout: None,
                car: None,
            },
            stages: StageLayout::Monolithic(MonolithicConfig {
                build: StageBuildConfig {
                    firmware_image: Some(FirmwareImageConfig {
                        temp_ram_buffer: None,
                    }),
                    verify_firmware: true,
                    payload: true,
                    fdt: true,
                    pci: true,
                    ..Default::default()
                },
                load_addr: self.image.base,
                data_addr: Some(self.writable.base),
                stack_size: self
                    .stack
                    .size
                    .try_into()
                    .map_err(|_| "stack exceeds assembler u32 budget")?,
                heap_size: Some(
                    self.heap
                        .size
                        .try_into()
                        .map_err(|_| "heap exceeds assembler u32 budget")?,
                ),
                page_table_addr: None,
                page_size: Default::default(),
            }),
            security: self.security.clone(),
            payload,
            microcode: None,
            soc_image_format: SocImageFormat::None,
            full_flash_image: true,
            build: BoardBuildPolicy {
                firmware_image: FirmwareImagePolicy::memory_mapped(
                    self.firmware.base,
                    self.firmware.size,
                ),
                flash_image: Some(FirmwareImagePolicy::memory_mapped(
                    self.flash.base,
                    self.flash.size,
                )),
                ..Default::default()
            },
            acpi: None,
            smbios: None,
            smm: None,
            boot_hart_id: 0,
        })
    }

    pub fn validate_inputs(
        &self,
        board_dir: &Path,
        kernel: Option<&str>,
        firmware: Option<&str>,
        fit: Option<&str>,
    ) -> Result<(), String> {
        if fit.is_some() {
            return Err("FIT input unsupported by this profile".into());
        }
        for (label, input, default, range) in [
            (
                "kernel",
                kernel,
                self.inputs.kernel_file.as_deref(),
                self.inputs.kernel,
            ),
            (
                "firmware",
                firmware,
                self.inputs.firmware_file.as_deref(),
                self.inputs.firmware,
            ),
        ] {
            let path = input
                .map(PathBuf::from)
                .or_else(|| default.map(|p| board_dir.join(p)));
            match (path, range) {
                (Some(path), Some(range)) => {
                    let size = std::fs::metadata(&path)
                        .map_err(|e| format!("{label} {}: {e}", path.display()))?
                        .len();
                    if size == 0 || size > range.size {
                        return Err(format!(
                            "{label} input exceeds its resolved capacity or is empty"
                        ));
                    }
                }
                (None, None) => (),
                _ => return Err(format!("{label} input does not match selected payload")),
            }
        }
        Ok(())
    }
}

fn hstring<const N: usize>(value: &str) -> Result<heapless::String<N>, String> {
    heapless::String::try_from(value).map_err(|_| format!("metadata string too long: {value}"))
}

fn validate_features(
    features: &[String],
    board: &serde_json::Value,
    metadata: &serde_json::Value,
) -> Result<(), String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("missing resolve graph")?;
    let node = nodes
        .iter()
        .find(|n| n["id"] == board["id"])
        .ok_or("missing board node")?;
    for feature in features {
        let Some((dependency, name)) = feature.split_once('/') else {
            if board["features"].get(feature).is_none() {
                return Err(format!("unknown board feature '{feature}'"));
            }
            continue;
        };
        let key = dependency.replace('-', "_");
        let dep = node["deps"]
            .as_array()
            .and_then(|deps| deps.iter().find(|d| d["name"].as_str() == Some(&key)))
            .ok_or_else(|| {
                format!("feature '{feature}' does not name an enabled direct dependency")
            })?;
        let package = packages
            .iter()
            .find(|p| p["id"] == dep["pkg"])
            .ok_or("missing dependency package")?;
        if package["features"].get(name).is_none() {
            return Err(format!("unknown dependency feature '{feature}'"));
        }
    }
    Ok(())
}
