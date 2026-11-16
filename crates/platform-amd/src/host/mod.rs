// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host build plan of every AMD board: fbuild's plan runner hands a
//! board's [`Host`] data and the JSON [`BuildSelection`] to [`resolve`],
//! which answers with the [`BuildPlan`].
//!
//! One stage, the BIOS reset image the PSP copies, runs from the reset
//! vector to the payload; it is linked for the SoC's [`Layout`] through the
//! shared AMD reset entry. A second unit, `smm`, builds the board crate's
//! library with its SMM handler (bound by the board's `smm_bin!`) into a
//! position-independent SMM image with one entry stub per CPU the board
//! allows; the stage embeds it through `FSTART_SMM_IMAGE` and its build
//! script checks that it fits TSEG. The payload and its manifest go into
//! the FFS in the layout's window.
//!
//! What the flash image is made of is the SoC's image kind ([`Image`]):
//! one 16 MiB image with the PSP firmware of one SoC ([`single`]), or the
//! SP3 flash with a vendor half per generation ([`dual`]).
extern crate std;

pub mod dual;
pub mod single;

use crate::HostOptions;
use crate::board::Board;
use crate::layout::{Layout, Region};
use fstart_core::board::MicrocodeConfig;
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, Platform, StageBuildConfig, StageLayout, hstr,
};
use fstart_image_build::build_plan::{
    ArtifactBinding, BootImageFormat, BuildPlan, CargoTarget, CompilationUnit, FfsAssembly,
    InputFile, SmmRelocation, SmmUnit, UnitOutput,
};
use fstart_image_build::plan::{BuildSelection, HostPlan, ResolvedPlan, X86LinuxOptions};
use fstart_image_build::smm_image::ImageOptions;
use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::{format, vec};

/// The firmware unit (and FFS file) name of the stage, which the PSP
/// copies to DRAM as the BIOS reset image.
pub const STAGE_UNIT: &str = "stage";
/// Platform feature that bundles the stage runtime.
pub const STAGE_FEATURE: &str = "bundle";
/// The SMM image unit; the stage unit is its only consumer.
pub const SMM_UNIT: &str = "smm";
/// Platform features of the SMM unit: the handler and its binding macro
/// with the SMM panic handler.
pub const SMM_FEATURES: [&str; 2] = ["smm", "bundle-smm"];
/// Stage-unit environment carrying the SMM image path (`build.rs`).
pub const SMM_IMAGE_ENV: &str = "FSTART_SMM_IMAGE";
/// Stage-unit environment carrying the TSEG size the image must fit.
pub const TSEG_SIZE_ENV: &str = "FSTART_SMM_TSEG_SIZE";
/// Per-thread SMM stack: the linked handler's deepest static call path
/// (it has no indirect calls) needs about 1 KiB; 128 threads take 1 MiB of
/// the 8 MiB TSEG.
pub const SMM_STACK_SIZE: u32 = 0x2000;

/// What a board's host build is made of.
pub struct Host {
    /// Shown in errors.
    pub name: &'static str,
    pub layout: Layout,
    /// Logical CPU ceiling of the board (all sockets): sizes the AP stacks
    /// and the SMM image.
    pub max_cpus: u16,
    pub tseg_size: u64,
    /// Debug builds don't fit the image.
    pub release_only: bool,
    /// The graphical UEFI payload drives the board's I2C input devices
    /// (touchpads, keyboards of a laptop).
    pub i2c_input: bool,
    pub image: Image,
}

/// What the flash image is made of.
pub enum Image {
    /// One 16 MiB image with the PSP firmware of one SoC.
    Single(single::Image),
    /// The SP3 flash: a 16 MiB half per generation, composed into a vendor
    /// ROM or packed from blobs.
    Dual(dual::Image),
}

/// The payload a build selects (`--payload`, default halt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    Halt,
    /// Direct x86 Linux (bzImage from `--kernel`, stored in the FFS).
    Linux,
    /// CrabEFI linked into the stage unit.
    Uefi,
    /// CrabEFI's graphical boot manager.
    UefiUi,
    /// CrabEFI without the graphical boot manager.
    UefiBasic,
}

impl Payload {
    pub fn select(selection: &BuildSelection, linux: &X86LinuxOptions) -> Result<Self, String> {
        let payload = match selection.payload.as_deref() {
            None | Some("halt") => Self::Halt,
            Some("linux") => Self::Linux,
            Some("uefi") => Self::Uefi,
            Some("uefi-ui") => Self::UefiUi,
            Some("uefi-basic") => Self::UefiBasic,
            Some(other) => {
                return Err(format!(
                    "AMD payloads are halt, linux, uefi, uefi-ui and uefi-basic, not {other}"
                ));
            }
        };
        if linux.any() && payload != Self::Linux {
            return Err("x86 Linux options require '--payload linux'".into());
        }
        Ok(payload)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Linux => "linux",
            Self::Uefi => "uefi",
            Self::UefiUi => "uefi-ui",
            Self::UefiBasic => "uefi-basic",
        }
    }

    /// The platform feature the stage unit needs for this payload.
    pub const fn feature(self, i2c_input: bool) -> Option<&'static str> {
        match self {
            Self::Halt => None,
            Self::Linux => Some("payload-linux"),
            Self::Uefi => Some("payload-uefi"),
            Self::UefiUi if i2c_input => Some("payload-uefi-ui"),
            Self::UefiUi => Some("payload-uefi-ui-core"),
            Self::UefiBasic => Some("payload-uefi-basic"),
        }
    }

    fn config(self, linux: &X86LinuxOptions) -> Result<Option<PayloadConfig>, String> {
        use fstart_core::payload_manifest::{
            X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR, X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
        };
        Ok(match self {
            Self::Halt => None,
            Self::Uefi | Self::UefiUi | Self::UefiBasic => {
                Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD))
            }
            Self::Linux => Some(
                fstart_core::x86_linux_payload(
                    linux
                        .kernel_load_addr
                        .unwrap_or(X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR),
                    X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                    linux.bootargs.as_deref().unwrap_or_default(),
                    linux.print_mtrrs,
                    crate::compression::PAYLOAD,
                )
                .map_err(ToString::to_string)?,
            ),
        })
    }
}

/// `FSTART_MP_MAX_CPUS` for both units: the MP code's bound and the SMM
/// image's entry count agree.
fn mp_capacity(host: &Host) -> BTreeMap<String, String> {
    BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), host.max_cpus.to_string())])
}

/// The SMM image unit: the board library with its handler, linked position
/// independent (the installer copies it to the top of TSEG) with one entry
/// stub per CPU the board allows.
fn smm_unit(host: &Host) -> CompilationUnit {
    // PIE, not static: the handler takes the address of its SMRAM statics
    // (P2C/C2P pages) and the drivers use jump tables, which static code
    // addresses absolutely (`R_X86_64_32S`); PIE code reaches them
    // RIP-relative, and PLT calls (`-Zplt=yes`, also for `memcpy`) link to
    // direct ones. The image builder still rejects any load-base-dependent
    // reference.
    SmmUnit {
        name: SMM_UNIT.into(),
        platform: Platform::X86_64,
        features: SMM_FEATURES.map(String::from).to_vec(),
        relocation: SmmRelocation::Pie,
        // No console in SMM: the log macros compile out (and with them
        // their formatting panics, which the image builder rejects).
        environment_values: mp_capacity(host)
            .into_iter()
            .chain([("FSTART_LOG_LEVEL".into(), "off".into())])
            .collect(),
        image: ImageOptions {
            entry_count: host.max_cpus,
            stack_size: SMM_STACK_SIZE,
            coreboot_module_args: false,
            coreboot_header: false,
        },
    }
    .into()
}

/// The reset image: linked at the layout's image in DRAM below 4 GiB
/// (small code model, static relocations).
fn stage_unit(host: &Host, payload: Payload) -> Result<CompilationUnit, String> {
    let layout = &host.layout;
    Ok(CompilationUnit {
        name: STAGE_UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        features: core::iter::once(STAGE_FEATURE)
            .chain(payload.feature(host.i2c_input))
            .map(String::from)
            .collect(),
        build_std: Some("core,alloc".into()),
        release_only: host.release_only,
        rustflags: [
            "-Zub-checks=no",
            "-Crelocation-model=static",
            "-Ccode-model=small",
            "-Clink-arg=-no-pie",
            "-Cno-redzone=yes",
            "--cfg",
            "curve25519_dalek_backend=\"serial\"",
        ]
        .map(String::from)
        .to_vec(),
        linker_script: Some(layout.linker_script()?),
        environment_values: mp_capacity(host)
            .into_iter()
            .chain([(TSEG_SIZE_ENV.into(), host.tseg_size.to_string())])
            .collect(),
        bindings: vec![ArtifactBinding {
            producer: SMM_UNIT.into(),
            artifact: "image".into(),
            environment: SMM_IMAGE_ENV.into(),
        }],
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations: layout.expectations()?,
            load_address: layout.image.base,
            flat_capacity: layout.image.size,
        },
    })
}

/// The FFS of the payload in the layout's window. An `external` stage is
/// only in the PSP's reset image; otherwise the FFS holds a copy of it as
/// well (the dual-half composer copies the unit's trust policy from it).
fn ffs(
    layout: &Layout,
    window: Region,
    payload: Payload,
    config: Option<PayloadConfig>,
    microcode: Option<MicrocodeConfig>,
    external: bool,
) -> FfsAssembly {
    let region = |name: &str, span: Region, kind| MemoryRegion {
        name: hstr(name),
        base: span.base,
        size: span.size,
        kind,
    };
    FfsAssembly {
        platform: Platform::X86_64,
        memory: vec![
            region("flash", layout.flash, fstart_core::RegionKind::Rom),
            region("early", layout.early, fstart_core::RegionKind::Ram),
        ],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig::default()),
                verify_firmware: true,
                payload: payload != Payload::Halt,
                pci: true,
                acpi: true,
                ..Default::default()
            },
            load_addr: layout.image.base,
            data_addr: Some(layout.data.base),
            stack_size: layout.stack.size as u32,
            heap_size: Some(layout.heap.size as u32),
            external,
        }),
        security: fstart_core::default_security_config(),
        payload: config,
        microcode,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        boot_image_format: BootImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(window.base, window.size),
            ..Default::default()
        },
    }
}

/// The build plan of the board `host` describes, with the build's
/// `options`.
pub fn resolve(
    host: &Host,
    selection: &BuildSelection,
    options: &HostOptions,
) -> Result<BuildPlan, String> {
    selection.reject_debug()?;
    selection.validate_compiler_files()?;
    if !selection.compiler_files.is_empty() {
        return Err(format!("{} takes no compiler inputs", host.name));
    }
    if host.max_cpus == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let payload = Payload::select(selection, &options.x86_linux)?;
    let config = payload.config(&options.x86_linux)?;
    let window = host.layout.ffs;
    // The kernel is the one file a caller supplies (`--kernel`); the
    // assembler enforces what actually fits next to the other files.
    let inputs = match window {
        Some(window) if payload == Payload::Linux => vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: window.size,
        }],
        _ => vec![],
    };
    let images = {
        let ffs = |microcode, external| {
            window.map(|window| ffs(&host.layout, window, payload, config, microcode, external))
        };
        match &host.image {
            Image::Single(image) => image.images(host, selection, options, ffs)?,
            Image::Dual(image) => image.images(host, selection, options, ffs)?,
        }
    };
    let plan = BuildPlan {
        payload: payload.name().into(),
        platform: Platform::X86_64,
        stages: vec![STAGE_UNIT.into()],
        inputs,
        images,
        run: None,
        units: vec![smm_unit(host), stage_unit(host, payload)?],
    };
    plan.order()?;
    plan.validate_compiler_selections(selection)?;
    plan.validate_image_selection(selection)?;
    Ok(plan)
}

/// The host data of board `B`: its SoC's image, or the SP3 flash with a
/// half per SoC it lists.
pub fn host<B: Board>() -> Result<Host, String> {
    use crate::board::SocId;
    Ok(match B::SOCS {
        [SocId::Picasso] => crate::noncar::host::host::<crate::picasso::Picasso>(B::MAX_CPUS),
        [SocId::Zeppelin] => crate::noncar::host::host::<crate::zeppelin::Zeppelin>(B::MAX_CPUS),
        socs if !socs.is_empty() && socs.iter().all(|soc| soc.sp3()) => {
            crate::sp3::host::host::<B>()
        }
        _ => {
            return Err(format!(
                "{} carries SoCs one image cannot: one of Picasso and Zeppelin, or the SP3 SoCs",
                B::NAME
            ));
        }
    })
}

/// The build plan of board `B` with the build's `options`.
pub fn resolve_board<B: Board>(
    selection: &BuildSelection,
    options: &HostOptions,
) -> Result<BuildPlan, String> {
    resolve(&host::<B>()?, selection, options)
}

/// Every board's plan generator, which fbuild's plan runner calls: the
/// board's SoCs pick the image kind ([`host`]).
pub struct Plan<B>(core::marker::PhantomData<B>);

impl<B: Board> HostPlan<B> for Plan<B> {
    type Options = HostOptions;

    fn resolve(selection: BuildSelection, options: Self::Options) -> Result<ResolvedPlan, String> {
        resolve_board::<B>(&selection, &options)
    }
}

/// `path` as a microcode path of the FFS's microcode table.
fn bounded<const N: usize>(path: &str) -> Result<heapless::String<N>, String> {
    path.try_into()
        .map_err(|_| format!("microcode path {path} is too long"))
}
