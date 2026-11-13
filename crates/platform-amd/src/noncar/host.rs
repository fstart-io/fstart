// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host side of a noncar reset image: the build plan of every noncar SoC,
//! from its [`Layout`] and its [`Policy`]. One stage, the BIOS reset image
//! the PSP copies, runs from the reset vector to the payload. A payload and
//! its manifest go into an FFS in the flash if the layout has one.
//!
//! With `--image-config` the plan also composes the flash image with the PSP
//! firmware ([`ImageConfig`]).
extern crate std;
use super::{
    Board, DieSoc,
    layout::{Layout, Region},
};
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, Platform, StageBuildConfig, StageLayout,
    board::{AmdMicrocodeConfig, AmdVendorMicrocode, MicrocodeConfig},
    layout::RegionKind,
};
use fstart_image_build::{
    amd::{
        assembly::{AmdAssembly, FfsWindow, Firmware, PspOptions},
        blobs, reset,
    },
    build_plan::{
        ArtifactBinding, BootImageFormat, BuildPlan, CargoTarget, CompilationUnit, FfsAssembly,
        FileBinding, ImageOperation, ImageStep, InputFile, UnitOutput,
    },
    layout::EncodedLayout,
    plan::{BuildSelection, HostPlan, ResolvedPlan, Span},
};
use std::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
};

const UNIT: &str = "stage";
const SMM_UNIT: &str = "smm";

/// What a SoC's host build differs in.
pub struct Policy {
    /// The board crate feature of the SoC's stage.
    pub feature: &'static str,
    /// The flow reaches a payload; else it halts at its checkpoint.
    pub payloads: bool,
    /// The stage embeds the board's SMM handler, the board library's `smm`
    /// unit.
    pub smm: bool,
    /// Debug builds don't fit the image.
    pub release_only: bool,
    /// Where the PSP firmware of a full image comes from.
    pub image_config: ImageConfig,
}

/// What the `--image-config` of a full image is.
pub enum ImageConfig {
    /// A vendor flash image: its first `prefix` bytes (EFS, directories,
    /// signed blobs) stay apart from the `psp` fields, the stage replaces
    /// the reset image.
    Vendor { prefix: u32, psp: PspOptions },
    /// PSP firmware packed from the files of the SoC's and the board's blob
    /// tables in the `--blobs` directory ([`blobs`]). With
    /// `reset_offset`, a whole native image: the PSP partition, the reset
    /// image at that flash offset and the layout's FFS.
    Packed {
        reset_offset: Option<u32>,
        psp: PspOptions,
    },
}

/// A noncar SoC's host build.
pub trait HostSoc: DieSoc {
    const POLICY: Policy;
    /// Check a board's configuration before building.
    fn check(_config: &Self::Config) -> Result<(), &'static str> {
        Ok(())
    }
}

/// The board's plan generator: `fstart_platform_amd::Plan::<Board>::emit`.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board> HostPlan<B> for Plan<B>
where
    B::Soc: HostSoc,
{
    fn resolve(selection: BuildSelection) -> Result<ResolvedPlan, String> {
        resolve::<B>(selection)
    }
}

fn span(region: Region) -> Span {
    Span {
        base: region.base,
        size: region.size,
    }
}

/// The PSP-loaded image.
pub fn image(layout: &Layout) -> Span {
    span(layout.image)
}

/// The reset image's link contract through the shared AMD reset entry: the
/// image, the firmware RAM below it and what lives there.
pub fn reset_layout(layout: &Layout) -> reset::Layout {
    reset::Layout {
        image: span(layout.image),
        early: span(Region::new(
            layout.early.base,
            layout.image.base - layout.early.base,
        )),
        pages: span(layout.pages()),
        data: span(layout.data),
        heap: span(layout.heap),
        stack: span(layout.stack),
        cache: span(layout.cache_window()),
    }
}

/// The runtime layout descriptor the stage reads: the shared entry's
/// regions, the flash and the FFS.
pub fn descriptor(layout: &Layout) -> Result<EncodedLayout, String> {
    let media: std::vec::Vec<_> = core::iter::once(span(layout.flash).region(RegionKind::Flash))
        .chain(layout.ffs.map(|ffs| span(ffs).region(RegionKind::Firmware)))
        .collect();
    reset_layout(layout).descriptor(&media)
}

/// The board's SMM handler as a standalone image, one entry per CPU, which
/// the stage embeds (`FSTART_SMM_IMAGE`).
fn smm_unit<S: HostSoc>() -> CompilationUnit {
    CompilationUnit {
        name: SMM_UNIT.into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: "x86_64-unknown-none".into(),
        features: vec!["smm".into(), "bundle-smm".into()],
        build_std: None,
        release_only: true,
        linker_script: None,
        rustflags: [
            "-Cpanic=abort",
            "-Copt-level=s",
            // Position independent, as SP3's: lookup tables and the
            // addresses of constants are RIP-relative, calls direct
            // (`-Zplt=yes`); the image builder still rejects any
            // load-base-dependent reference.
            "-Crelocation-model=pie",
            "-Zplt=yes",
            "-Cno-redzone=yes",
            "-Clinker-plugin-lto=no",
            "-Cembed-bitcode=no",
            "-Zfunction-sections=yes",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        environment_values: BTreeMap::from([(
            "FSTART_MP_MAX_CPUS".into(),
            S::MAX_CPUS.to_string(),
        )]),
        bindings: vec![],
        file_bindings: vec![],
        // The board's SMM handler module, outside the stage `main`.
        board_features: vec!["smm".into()],
        output: UnitOutput::SmmImage {
            entry_count: S::MAX_CPUS,
            stack_size: 0x800,
            coreboot_module_args: false,
            coreboot_header: false,
        },
    }
}

/// The payload: its name, the stage feature and its configuration.
type Payload = (&'static str, Option<&'static str>, Option<PayloadConfig>);

fn payload(policy: &Policy, selection: &BuildSelection) -> Result<Payload, String> {
    let requested = selection.payload.as_deref().unwrap_or("halt");
    let x86_linux = selection.x86_linux()?;
    if x86_linux.any() && requested != "linux" {
        return Err("x86 Linux options require '--payload linux'".into());
    }
    if !policy.payloads && requested != "halt" {
        return Err("the SoC's flow halts at its checkpoint; it has no payload".into());
    }
    Ok(match requested {
        "halt" => ("halt", None, None),
        "linux" => (
            "linux",
            Some("payload-linux"),
            Some(
                fstart_core::x86_linux_payload(
                    x86_linux.kernel_load_addr.unwrap_or(
                        fstart_core::payload_manifest::X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR,
                    ),
                    fstart_core::payload_manifest::X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                    x86_linux.bootargs.as_deref().unwrap_or(""),
                    x86_linux.print_mtrrs,
                    crate::compression::PAYLOAD,
                )
                .map_err(ToString::to_string)?,
            ),
        ),
        "uefi" => (
            "crabefi",
            Some("payload-uefi"),
            Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD)),
        ),
        "uefi-ui" => (
            "crabefi",
            Some("payload-uefi-ui"),
            Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD)),
        ),
        "uefi-basic" => (
            "crabefi",
            Some("payload-uefi-basic"),
            Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD)),
        ),
        other => {
            return Err(format!(
                "noncar payloads are halt, linux, uefi, uefi-ui and uefi-basic, not {other}"
            ));
        }
    })
}

/// The FFS of the payload, in the flash window `ffs`.
fn ffs(
    layout: &Layout,
    ffs: Span,
    payload: Option<PayloadConfig>,
    microcode: Option<MicrocodeConfig>,
) -> FfsAssembly {
    FfsAssembly {
        platform: Platform::X86_64,
        memory: vec![
            MemoryRegion {
                name: fstart_core::hstr("flash"),
                base: layout.flash.base,
                size: layout.flash.size,
                kind: fstart_core::RegionKind::Rom,
            },
            MemoryRegion {
                name: fstart_core::hstr("dram"),
                base: layout.early.base,
                size: layout.early.size,
                kind: fstart_core::RegionKind::Ram,
            },
        ],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                verify_firmware: true,
                payload: payload.is_some(),
                pci: true,
                ..Default::default()
            },
            load_addr: layout.image.base,
            data_addr: Some(layout.data.base),
            stack_size: layout.stack.size as u32,
            heap_size: Some(layout.heap.size as u32),
            external: true,
        }),
        security: fstart_core::default_security_config(),
        payload,
        microcode,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        boot_image_format: BootImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(ffs.base, ffs.size),
            ..Default::default()
        },
    }
}

fn bounded<const N: usize>(path: &str) -> Result<heapless::String<N>, String> {
    path.try_into()
        .map_err(|_| format!("microcode path {path} is too long"))
}

/// The microcode patches the FFS carries for the CPU driver: the BIOS
/// directory microcode of a vendor image, or the microcode files of the
/// SoC's blob table. None without an image configuration or blobs.
fn microcode<S: HostSoc>(
    selection: &BuildSelection,
    packed: Option<&blobs::Resolved>,
) -> Result<Option<MicrocodeConfig>, String> {
    let config = match (&S::POLICY.image_config, selection.image_config(), packed) {
        (ImageConfig::Vendor { .. }, Some(path), _) => {
            let image =
                std::fs::canonicalize(path).map_err(|e| format!("vendor image {path}: {e}"))?;
            AmdMicrocodeConfig {
                files: heapless::Vec::new(),
                vendor_images: heapless::Vec::from_slice(&[AmdVendorMicrocode {
                    image: bounded(&image.to_string_lossy())?,
                    bios_directory: S::BIOS_DIRECTORY,
                    origin: 0,
                }])
                .map_err(|_| "too many vendor images")?,
            }
        }
        (ImageConfig::Packed { .. }, _, Some(packed)) if !packed.microcode.is_empty() => {
            let mut files = heapless::Vec::new();
            for file in &packed.microcode {
                files
                    .push(bounded(file)?)
                    .map_err(|_| "too many microcode files")?;
            }
            AmdMicrocodeConfig {
                files,
                vendor_images: heapless::Vec::new(),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(MicrocodeConfig::Amd(config)))
}

/// The files of a packed image: the SoC's blob table
/// (`[package.metadata.fstart.amd.<soc>]` of this crate) and the board's
/// (`[package.metadata.fstart.amd]`) in the selected blob directory. None
/// without a blob directory.
fn packed<S: HostSoc>(selection: &BuildSelection) -> Result<Option<blobs::Resolved>, String> {
    let (ImageConfig::Packed { .. }, Some(dir)) = (&S::POLICY.image_config, selection.blobs())
    else {
        return Ok(None);
    };
    if selection.image_config().is_some() {
        return Err(format!(
            "{} images are packed from --blobs, not --image-config",
            S::NAME
        ));
    }
    let tables = &selection.manifests;
    let board: blobs::BoardBlobs = serde_json::from_value(tables.board["amd"].clone())
        .map_err(|e| format!("board [package.metadata.fstart.amd]: {e}"))?;
    if board.apob_destination.is_some() {
        return Err(format!("{} places the APOB itself", S::NAME));
    }
    if board.soc != S::NAME {
        return Err(format!(
            "the board's AMD firmware is for {}, not {}",
            board.soc,
            S::NAME
        ));
    }
    let soc: blobs::SocBlobs = serde_json::from_value(tables.platform["amd"][S::NAME].clone())
        .map_err(|e| format!("platform [package.metadata.fstart.amd.{}]: {e}", S::NAME))?;
    if soc.bios_directory != S::BIOS_DIRECTORY {
        return Err("the SoC's blob table names another BIOS directory".into());
    }
    let name = tables.board["board"]
        .as_str()
        .ok_or("the board table has no board name")?;
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("blob directory {dir}: {e}"))?;
    blobs::resolve(&soc, &board, name, &dir, S::LAYOUT.flash.size as u32).map(Some)
}

fn images<S: HostSoc>(
    selection: &BuildSelection,
    packed: Option<blobs::Resolved>,
    ffs: Option<FfsAssembly>,
) -> Result<Vec<ImageStep>, String> {
    let layout = &S::LAYOUT;
    let flash_size = layout.flash.size as u32;
    let (firmware, psp) = match (S::POLICY.image_config, selection.image_config(), packed) {
        (ImageConfig::Vendor { prefix, psp }, Some(path), _) => (
            Firmware::Vendor {
                image: FileBinding {
                    environment: "VENDOR_FLASH".into(),
                    path: path.to_string(),
                    capacity: u64::from(flash_size),
                },
                prefix_size: prefix,
            },
            psp,
        ),
        (ImageConfig::Packed { reset_offset, psp }, _, Some(packed)) => (
            Firmware::Packed {
                files: packed.files,
                reset_offset,
            },
            psp,
        ),
        _ => return Ok(ffs.map(ImageStep::from).into_iter().collect()),
    };
    let assembly = AmdAssembly {
        firmware,
        ffs: ffs.as_ref().zip(layout.ffs).map(|(_, window)| FfsWindow {
            offset: (window.base - layout.flash.base) as u32,
            size: window.size as u32,
        }),
        stage_unit: UNIT.into(),
        flash_size,
        bios_directory: S::BIOS_DIRECTORY,
        reset_size: layout.image.size as u32,
        reset_destination: layout.image.base,
        apob_destination: S::APOB.base,
        // Entry 0x6B is only for a PSP verstage.
        shared_memory: None,
        psp,
    };
    assembly.validate()?;
    // Check the vendor image against the layout the stage is linked for
    // before building.
    if let Firmware::Vendor { image, .. } = &assembly.firmware {
        let path = &image.path;
        let vendor = std::fs::read(path).map_err(|e| format!("vendor image {path}: {e}"))?;
        assembly
            .check_vendor(&vendor)
            .map_err(|e| format!("vendor image {path}: {e}"))?;
    }
    let mut steps: Vec<ImageStep> = ffs.map(ImageStep::from).into_iter().collect();
    steps.push(ImageStep::new(
        "flash",
        ImageOperation::AmdEfs(Box::new(assembly)),
    ));
    Ok(steps)
}

/// The build plan of board `B`.
pub fn resolve<B: Board>(selection: BuildSelection) -> Result<BuildPlan, String>
where
    B::Soc: HostSoc,
{
    let policy = &<B::Soc as HostSoc>::POLICY;
    let layout = &<B::Soc as DieSoc>::LAYOUT;
    selection.reject_debug()?;
    selection.reject_unknown_options(&[
        fstart_image_build::plan::options::IMAGE_CONFIG,
        fstart_image_build::plan::options::BLOBS,
        fstart_image_build::plan::options::DONOR,
        fstart_image_build::plan::options::X86_LINUX_KERNEL_LOAD_ADDR,
        fstart_image_build::plan::options::X86_LINUX_BOOTARGS,
        fstart_image_build::plan::options::X86_LINUX_PRINT_MTRRS,
    ])?;
    selection.validate_compiler_files()?;
    if !selection.compiler_files.is_empty() {
        return Err("noncar SoCs take no compiler inputs".into());
    }
    <B::Soc as HostSoc>::check(B::CONFIG)?;
    let (payload, feature, payload_config) = payload(policy, &selection)?;
    let descriptor = descriptor(layout)?;
    let expectations = reset_layout(layout).expectations(&descriptor)?;
    expectations.validate()?;
    let image = image(layout);
    let ffs_window = layout.ffs.map(span);
    let inputs = match ffs_window {
        Some(window) if payload == "linux" => vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: window.size,
        }],
        _ => vec![],
    };
    let packed = packed::<B::Soc>(&selection)?;
    let microcode = microcode::<B::Soc>(&selection, packed.as_ref())?;
    let mut ffs = ffs_window.map(|window| ffs(layout, window, payload_config, microcode));
    // The video BIOSes of the SoC's blob table go into the FFS.
    if let (Some(ffs), Some(packed)) = (&mut ffs, &packed) {
        ffs.file_assets = packed.files.assets.clone();
    }
    let images = images::<B::Soc>(&selection, packed, ffs)?;
    let stage = CompilationUnit {
        name: UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        features: core::iter::once(policy.feature)
            .chain(feature)
            .map(String::from)
            .collect(),
        build_std: None,
        rustflags: vec![
            "-Zub-checks=no".into(),
            "-Crelocation-model=static".into(),
            "-Cno-redzone=yes".into(),
            "--cfg".into(),
            "curve25519_dalek_backend=\"serial\"".into(),
        ],
        release_only: policy.release_only,
        linker_script: Some(reset_layout(layout).linker_script(&descriptor)?),
        environment_values: BTreeMap::from([(
            "FSTART_MP_MAX_CPUS".into(),
            <B::Soc as DieSoc>::MAX_CPUS.to_string(),
        )]),
        bindings: policy
            .smm
            .then(|| ArtifactBinding {
                producer: SMM_UNIT.into(),
                artifact: "image".into(),
                environment: "FSTART_SMM_IMAGE".into(),
            })
            .into_iter()
            .collect(),
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations,
            load_address: image.base,
            flat_capacity: image.size,
        },
    };
    let plan = BuildPlan {
        payload: payload.into(),
        platform: Platform::X86_64,
        stages: vec![UNIT.into()],
        inputs,
        run: None,
        images,
        units: policy
            .smm
            .then(smm_unit::<B::Soc>)
            .into_iter()
            .chain([stage])
            .collect(),
    };
    plan.order()?;
    plan.validate_compiler_selections(&selection)?;
    plan.validate_image_selection(&selection)?;
    Ok(plan)
}

#[cfg(test)]
mod tests;
