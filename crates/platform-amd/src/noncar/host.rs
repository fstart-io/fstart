// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host side of a noncar reset image: the build plan of every noncar SoC,
//! from its [`Layout`] and its [`Policy`]. One stage, the BIOS reset image
//! the PSP copies, runs from the reset vector to the payload. A payload and
//! its manifest go into an FFS in the flash if the layout has one.
//!
//! With `--image-config` the plan also composes the flash image with the PSP
//! firmware ([`ImageConfig`]).
extern crate std;
use super::{Board, Soc, layout::Layout};
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, Platform, SocImageFormat, StageBuildConfig, StageLayout,
    board::{AmdMicrocodeConfig, AmdVendorMicrocode, MicrocodeConfig},
    layout::RegionKind,
};
use fstart_image_build::{
    amd::{
        assembly::{AmdAssembly, FfsWindow, Firmware, PspOptions},
        blobs,
    },
    build_plan::{
        ArtifactBinding, Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema,
        FfsAssembly, FileBinding, InputFile, UnitOutput,
    },
    elf::{Architecture, Descriptor, Expectations},
    layout::EncodedLayout,
    plan::{BuildSelection, Span},
};
use std::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
};

pub const ENTRY: &str = "amd-psp-reset";
const UNIT: &str = "reset";
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
pub trait HostSoc: Soc {
    const POLICY: Policy;
    /// Check a board's configuration before building.
    fn check(_config: &Self::Config) -> Result<(), &'static str> {
        Ok(())
    }
}

/// The board's plan generator: `fstart_platform_amd::Plan::<Board>::emit`.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board> Plan<B>
where
    B::Soc: HostSoc,
{
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(selection).expect("invalid noncar build plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize the plan")
        );
    }
}

fn span(base: u64, size: u64) -> Span {
    Span { base, size }
}

/// The PSP-loaded image.
pub fn image(layout: &Layout) -> Span {
    span(layout.image.base, layout.image.size)
}

/// The runtime layout descriptor the stage reads: its image, the flash and
/// FFS, data through stack, stack and heap.
pub fn descriptor(layout: &Layout) -> Result<EncodedLayout, String> {
    let writable = span(layout.data.base, layout.stack.end() - layout.data.base);
    let regions: std::vec::Vec<_> = [
        Some(image(layout).region(RegionKind::Image)),
        Some(span(layout.flash.base, layout.flash.size).region(RegionKind::Flash)),
        layout
            .ffs
            .map(|ffs| span(ffs.base, ffs.size).region(RegionKind::Firmware)),
        Some(writable.region(RegionKind::Writable)),
        Some(span(layout.stack.base, layout.stack.size).region(RegionKind::Stack)),
        Some(span(layout.heap.base, layout.heap.size).region(RegionKind::Heap)),
    ]
    .into_iter()
    .flatten()
    .collect();
    EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())
}

/// What the linked ELF must look like.
pub fn expectations(layout: &Layout, descriptor: &EncodedLayout) -> Expectations {
    let image = image(layout);
    let writable = span(layout.data.base, layout.stack.end() - layout.data.base);
    Expectations {
        architecture: Architecture::X86_64,
        elf64: true,
        little_endian: true,
        stored: vec![image],
        runtime: vec![image, writable],
        identity_mapping: false,
        entry: Some(layout.reset_vector()),
        copy: None,
        descriptor: Some(Descriptor {
            section: ".fstart.layout".into(),
            start_symbol: "_fstart_layout_start".into(),
            end_symbol: "_fstart_layout_end".into(),
            bytes: descriptor.as_bytes().to_vec(),
            reservation: image,
        }),
        stored_flags: None,
        symbols: BTreeMap::from([
            ("_start".into(), layout.reset_vector()),
            ("_fstart_page_tables".into(), layout.page_tables),
            ("_FSTART_HEAP".into(), layout.heap.base),
            ("_stack_bottom".into(), layout.stack.base),
            ("_stack_top".into(), layout.stack.end()),
        ]),
    }
}

/// The linker script of the reset image: code, read-only data and the
/// initial `.data` in the image, `.data` and BSS run from the data region.
/// `psp_reset.S` copies `.data` and clears BSS.
pub fn linker_script(name: &str, layout: &Layout, descriptor: &EncodedLayout) -> String {
    format!(
        r#"/* Generated by fstart-platform-amd: {name} PSP DRAM reset image. */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
MEMORY {{
    image (rwx) : ORIGIN = {image:#x}, LENGTH = {image_size:#x}
    data (rw) : ORIGIN = {data:#x}, LENGTH = {data_size:#x}
    heap (rw) : ORIGIN = {heap:#x}, LENGTH = {heap_size:#x}
    stack (rw) : ORIGIN = {stack:#x}, LENGTH = {stack_size:#x}
}}
_fstart_page_tables = {page_tables:#x};
_fstart_early_reserved_start = {early:#x};
_fstart_early_reserved_end = {early_end:#x};
SECTIONS {{
    .text {image:#x} : {{
        _text_start = .;
        KEEP(*(.text.entry))
        *(.text .text.* .ltext .ltext.*)
    }} > image
{layout}    .rodata : ALIGN(16) {{
        *(.rodata .rodata.* .lrodata .lrodata.*)
        KEEP(*(.fstart.keep))
    }} > image    .fstart.anchor : ALIGN(16) {{
        _fstart_anchor_early = .;
        KEEP(*(.fstart.anchor))
        _fstart_early_microcode_enabled = .;
        LONG(0)
        . = ALIGN(8);
        _FSTART_HEAP_SIZE = .;
        QUAD({heap_size:#x})
    }} > image
    .got : ALIGN(8) {{ *(.got) *(.got.plt) }} > image    .data : ALIGN(16) {{
        _data_start = .;
        *(.data .data.* .ldata .ldata.*)
        . = ALIGN(16);
        _data_end = .;
    }} > data AT > image
    _data_load = LOADADDR(.data);
    _fstart_text_end = LOADADDR(.data) + SIZEOF(.data);
    _binary_end = _fstart_text_end;
    .bss (NOLOAD) : ALIGN(16) {{
        _bss_start = .;
        _fstart_bss_start = .;
        *(.bss .bss.* .lbss .lbss.*)
        *(COMMON)
        . = ALIGN(16);
        _bss_end = .;
        _fstart_bss_end = .;
    }} > data
    .fstart.heap (NOLOAD) : ALIGN(16) {{
        _FSTART_HEAP = .;
        . = . + {heap_size:#x};
    }} > heap
    .stack (NOLOAD) : ALIGN(16) {{
        _stack_bottom = .;
        _fstart_stack_bottom = .;
        . = . + {stack_size:#x};
        _stack_top = .;
        _fstart_stack_top = .;
        _writable_end = .;
    }} > stack
    .init16 {init16:#x} : {{ KEEP(*(.init16)) }} > image
    .reset {reset:#x} : {{ KEEP(*(.reset)) }} > image
    .last_byte {last:#x} : {{ BYTE(0xff); }} > image
    .unsupported_dynamic : {{ *(.dynamic) }} > image
    .unsupported_rel : {{ *(.rel.*) *(.rela.*) *(.relr.*) }} > image
    .unsupported_loader : {{ *(.interp .plt .plt.* .iplt) }} > image
    /DISCARD/ : {{ *(.eh_frame*) *(.comment*) *(.note*) }}
}}
_fstart_gdt_offset = _fstart_gdt_pointer & 0xffff;
/* The architectural CAR entry is linked but never runs: no Cache-as-RAM. */
_has_car = 0;
_car_base = 0;
_car_size = 0;
_ecar_stack = _stack_top;
_rom_mtrr_base = 0;
_rom_mtrr_mask = 0;
ASSERT(SIZEOF(.unsupported_dynamic) + SIZEOF(.unsupported_rel) + SIZEOF(.unsupported_loader) == 0, "the reset image can't require runtime linking")
ASSERT(_fstart_text_end <= ADDR(.init16), "code and data exceed the PSP reset image")
ASSERT(ADDR(.init16) + SIZEOF(.init16) <= ADDR(.reset), "16-bit entry overflows the reset vector")
ASSERT(SIZEOF(.reset) <= 15, "reset vector too large")
ASSERT(_bss_end <= ORIGIN(heap), "data and BSS exceed their reservation")
"#,
        image = layout.image.base,
        image_size = layout.image.size,
        data = layout.data.base,
        data_size = layout.data.size,
        heap = layout.heap.base,
        heap_size = layout.heap.size,
        stack = layout.stack.base,
        stack_size = layout.stack.size,
        page_tables = layout.page_tables,
        early = layout.early.base,
        early_end = layout.early.end(),
        init16 = layout.init16(),
        reset = layout.reset_vector(),
        last = layout.image.end() - 1,
        layout = fstart_image_build::linker::layout_section(descriptor, "image"),
    )
}

fn cfg_schema(policy: &Policy) -> CompilerCfgSchema {
    CompilerCfgSchema {
        entries: vec![ENTRY.into()],
        environments: core::iter::once("monolithic")
            .chain(policy.smm.then_some("smm"))
            .map(Into::into)
            .collect(),
        payloads: core::iter::once("halt")
            .chain(
                policy
                    .payloads
                    .then_some(["linux", "crabefi"])
                    .into_iter()
                    .flatten(),
            )
            .map(Into::into)
            .collect(),
    }
}

/// The board's SMM handler as a standalone image, one entry per CPU, which
/// the stage embeds (`FSTART_SMM_IMAGE`).
fn smm_unit<S: HostSoc>() -> CompilationUnit {
    CompilationUnit {
        name: SMM_UNIT.into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: "x86_64-unknown-none".into(),
        entry: ENTRY.into(),
        cfg_schema: cfg_schema(&S::POLICY),
        environment: "smm".into(),
        payload: "halt".into(),
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
    if selection.has_x86_linux_overrides() && requested != "linux" {
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
                    selection.x86_linux_kernel_load_addr.unwrap_or(
                        fstart_core::payload_manifest::X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR,
                    ),
                    fstart_core::payload_manifest::X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                    selection.x86_linux_bootargs.as_deref().unwrap_or(""),
                    selection.x86_linux_print_mtrrs,
                )
                .map_err(ToString::to_string)?,
            ),
        ),
        "uefi" => (
            "crabefi",
            Some("payload-uefi"),
            Some(fstart_core::x86_uefi_payload()),
        ),
        "uefi-ui" => (
            "crabefi",
            Some("payload-uefi-ui"),
            Some(fstart_core::x86_uefi_payload()),
        ),
        "uefi-basic" => (
            "crabefi",
            Some("payload-uefi-basic"),
            Some(fstart_core::x86_uefi_payload()),
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
            page_table_addr: None,
            page_size: Default::default(),
            external: true,
        }),
        security: fstart_core::default_security_config(),
        payload,
        microcode,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::None,
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
    let config = match (&S::POLICY.image_config, &selection.image_config, packed) {
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
    let (ImageConfig::Packed { .. }, Some(dir)) = (&S::POLICY.image_config, &selection.blobs)
    else {
        return Ok(None);
    };
    if selection.image_config.is_some() {
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

fn assembly<S: HostSoc>(
    selection: &BuildSelection,
    packed: Option<blobs::Resolved>,
    ffs: Option<FfsAssembly>,
) -> Result<Assembly, String> {
    let layout = &S::LAYOUT;
    let flash_size = layout.flash.size as u32;
    let (firmware, psp) = match (S::POLICY.image_config, &selection.image_config, packed) {
        (ImageConfig::Vendor { prefix, psp }, Some(path), _) => (
            Firmware::Vendor {
                image: FileBinding {
                    environment: "VENDOR_FLASH".into(),
                    path: path.clone(),
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
        _ => {
            return Ok(match ffs {
                Some(ffs) => Assembly::Ffs(Box::new(ffs)),
                None => Assembly::Unassembled {
                    platform: Platform::X86_64,
                },
            });
        }
    };
    let assembly = AmdAssembly {
        firmware,
        ffs: ffs.zip(layout.ffs).map(|(assembly, window)| FfsWindow {
            assembly,
            offset: (window.base - layout.flash.base) as u32,
            size: window.size as u32,
        }),
        reset_unit: UNIT.into(),
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
    Ok(Assembly::Amd(Box::new(assembly)))
}

/// The build plan of board `B`.
pub fn resolve<B: Board>(selection: BuildSelection) -> Result<BuildPlan, String>
where
    B::Soc: HostSoc,
{
    let policy = &<B::Soc as HostSoc>::POLICY;
    let layout = &<B::Soc as Soc>::LAYOUT;
    selection.validate_compiler_files()?;
    if !selection.compiler_files.is_empty() {
        return Err("noncar SoCs take no compiler inputs".into());
    }
    <B::Soc as HostSoc>::check(B::CONFIG)?;
    let (payload, feature, payload_config) = payload(policy, &selection)?;
    let descriptor = descriptor(layout)?;
    let expectations = expectations(layout, &descriptor);
    expectations.validate()?;
    let image = image(layout);
    let ffs_window = layout.ffs.map(|ffs| span(ffs.base, ffs.size));
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
    let assembly = assembly::<B::Soc>(&selection, packed, ffs)?;
    let stage = CompilationUnit {
        name: UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        entry: ENTRY.into(),
        environment: "monolithic".into(),
        payload: payload.into(),
        cfg_schema: cfg_schema(policy),
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
        linker_script: Some(linker_script(<B::Soc as Soc>::NAME, layout, &descriptor)),
        environment_values: BTreeMap::from([(
            "FSTART_MP_MAX_CPUS".into(),
            <B::Soc as Soc>::MAX_CPUS.to_string(),
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
        stages: vec![UNIT.into()],
        inputs,
        assembly,
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
