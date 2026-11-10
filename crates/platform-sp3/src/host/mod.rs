//! Host build plan and the SP3 flash image (FIRMWARE_IMAGE §7).
//!
//! fbuild's plan runner calls [`Plan::emit`] with the board's `Board` type
//! (which implements [`Sp3BoardFacts`]) and the JSON [`BuildSelection`];
//! the platform answers with the concrete [`BuildPlan`]: one firmware unit,
//! `reset`, built with every generation the board supports, linked at the
//! common reset image address ([`layout::RESET`]) through the shared AMD
//! reset entry ([`reset::Layout`]), and an
//! [`Assembly::Sp3Flash`] that `fbuild assemble` composes with
//! [`fstart_image_build::sp3_flash`]: one PSP partition per generation the
//! blobs have, packed from them (`--blobs`, or `--donor`, whose blobs
//! fbuild extracts), a reset image copy per half, one FFS in the lower
//! half. Without blobs the plan builds the units and assembles no image.
//!
//! The FFS holds the payload and a copy of the stage unit: the FFS builder
//! finalizes the unit's trust policy only in FFS-resident files, and the
//! composer copies it, the FFS locator and the flash anchor (FFS flash
//! offset and size) into the PSP-copied `0x62` image.
//!
//! A second unit, `smm`, builds the board crate's library with the FCH SMI
//! handler (`smm::handle`, bound by the board's `smm_bin!`) into a
//! position-independent SMM image with one entry stub per CPU the board
//! allows; the stage unit embeds it through `FSTART_SMM_IMAGE` and its build
//! script checks that it fits the TSEG ([`crate::smm::TSEG_SIZE`]), as the
//! MIT Intel and QEMU q35 plans do.

use std::boxed::Box;
use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::{format, vec};

use fstart_core::board::{AmdMicrocodeConfig, MicrocodeConfig};
use fstart_core::layout::{Region, RegionKind};
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, SocImageFormat, StageBuildConfig, StageLayout, hstr,
};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_image_build::amd::{blobs, reset};
use fstart_image_build::build_plan::{
    ArtifactBinding, Assembly, BuildPlan, CargoTarget, CompilationUnit, FfsAssembly, InputFile,
    UnitOutput,
};
use fstart_image_build::elf::Expectations;
use fstart_image_build::layout::EncodedLayout;
use fstart_image_build::plan::{BuildSelection, Span as PlanSpan};
use fstart_image_build::sp3_flash::{
    FlashPlan, NvramSlot, PackedHalf, ResetSlot, Sp3FlashAssembly,
};

use crate::layout::{
    self, DATA, EARLY_REGION, FFS_WINDOW, HEAP, PAGE_TABLES, RESET, ROM3, STACK, STORE,
    Sp3BoardFacts, Span,
};

/// The firmware unit (and FFS file) name of the stage, which the PSP
/// copies to DRAM as the BIOS reset image.
pub const STAGE_UNIT: &str = "stage";
/// Platform feature that bundles the stage runtime.
pub const STAGE_FEATURE: &str = "bundle-stage";
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

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);

impl<B: Sp3BoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(&selection).expect("invalid SP3 board plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize SP3 plan")
        );
    }
}

/// The platform feature of a generation.
pub const fn feature(generation: Generation) -> &'static str {
    match generation {
        Generation::Naples => "naples",
        Generation::Rome => "rome",
        Generation::Milan => "milan",
    }
}

fn plan_span(span: Span) -> PlanSpan {
    PlanSpan {
        base: span.base,
        size: span.size,
    }
}

fn region(kind: RegionKind, span: Span) -> Region {
    Region {
        kind,
        base: span.base,
        size: span.size,
    }
}

/// The reset image's link contract: the common reset image, the early
/// region below it, the page tables, BSS, heap and stack.
fn reset_layout() -> reset::Layout {
    reset::Layout {
        image: plan_span(RESET.dram()),
        early: plan_span(EARLY_REGION),
        pages: plan_span(PAGE_TABLES),
        data: plan_span(DATA),
        heap: plan_span(HEAP),
        stack: plan_span(STACK),
    }
}

/// Runtime layout descriptor of the reset image (read by
/// `fstart_stage::layout::current()`): the shared entry's regions plus the
/// flash (ROM3), the FFS window within it and the store.
pub fn descriptor() -> Result<EncodedLayout, String> {
    reset_layout().descriptor(&[
        region(RegionKind::Flash, ROM3),
        region(RegionKind::Firmware, FFS_WINDOW),
        region(RegionKind::FirmwareStore, STORE),
    ])
}

/// The reset image linker script (one for every generation).
pub fn linker_script() -> Result<String, String> {
    reset_layout().linker_script(&descriptor()?)
}

/// What the linked reset image must look like.
pub fn expectations() -> Result<Expectations, String> {
    reset_layout().expectations(&descriptor()?)
}

/// `FSTART_MP_MAX_CPUS` for both units: the MP code's bound and the SMM
/// image's entry count agree.
fn mp_capacity<B: Sp3BoardFacts>() -> BTreeMap<String, String> {
    BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), B::MAX_CPUS.to_string())])
}

/// The SMM image unit: the board library with the platform's FCH handler,
/// built for every generation the board lists, linked position independent
/// (the installer copies it to the top of TSEG) with one entry stub per
/// CPU the board allows.
fn smm_unit<B: Sp3BoardFacts>() -> CompilationUnit {
    CompilationUnit {
        name: SMM_UNIT.into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: "x86_64-unknown-none".into(),
        features: SMM_FEATURES
            .into_iter()
            .chain(B::GENERATIONS.iter().map(|&g| feature(g)))
            .map(String::from)
            .collect(),
        build_std: None,
        release_only: true,
        linker_script: None,
        // The Intel/q35 SMM flags, but PIE instead of static: the handler
        // takes the address of its SMRAM statics (P2C/C2P pages) and the
        // drivers use jump tables, which static code addresses absolutely
        // (`R_X86_64_32S`); PIE code reaches them RIP-relative, and PLT
        // calls (`-Zplt=yes`, also for `memcpy`) link to direct ones. The
        // image builder still rejects any load-base-dependent reference.
        rustflags: [
            "-Cpanic=abort",
            "-Copt-level=s",
            "-Crelocation-model=pie",
            "-Zplt=yes",
            "-Cno-redzone=yes",
            "-Clinker-plugin-lto=no",
            "-Cembed-bitcode=no",
            "-Zfunction-sections=yes",
        ]
        .map(String::from)
        .to_vec(),
        // No console in SMM: the log macros compile out (and with them
        // their formatting panics, which the image builder rejects).
        environment_values: mp_capacity::<B>()
            .into_iter()
            .chain([("FSTART_LOG_LEVEL".into(), "off".into())])
            .collect(),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec!["smm".into()],
        output: UnitOutput::SmmImage {
            entry_count: B::MAX_CPUS,
            stack_size: SMM_STACK_SIZE,
            coreboot_module_args: false,
            coreboot_header: false,
        },
    }
}

/// The payload a build selects (`--payload`, default halt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    Halt,
    /// Direct x86 Linux (bzImage from `--kernel`, stored in the FFS).
    Linux,
    /// CrabEFI linked into the stage unit.
    Uefi,
}

impl Payload {
    pub fn select(selection: &BuildSelection) -> Result<Self, String> {
        let payload = match selection.payload.as_deref() {
            None | Some("halt") => Self::Halt,
            Some("linux") => Self::Linux,
            Some("uefi") => Self::Uefi,
            Some(other) => {
                return Err(format!(
                    "SP3 supports the halt, linux and uefi payloads, not {other}"
                ));
            }
        };
        if selection.has_x86_linux_overrides() && payload != Self::Linux {
            return Err("x86 Linux options require '--payload linux'".into());
        }
        Ok(payload)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Linux => "linux",
            Self::Uefi => "uefi",
        }
    }

    /// Platform features the stage unit needs for this payload.
    pub const fn features(self) -> &'static [&'static str] {
        match self {
            Self::Halt => &[],
            Self::Linux => &["payload-linux"],
            Self::Uefi => &["payload-uefi"],
        }
    }

    fn config(self, selection: &BuildSelection) -> Result<Option<PayloadConfig>, String> {
        use fstart_core::payload_manifest::{
            X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR, X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
        };
        Ok(match self {
            Self::Halt => None,
            Self::Uefi => Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD)),
            Self::Linux => Some(fstart_core::x86_linux_payload(
                selection
                    .x86_linux_kernel_load_addr
                    .unwrap_or(X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR),
                X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                selection.x86_linux_bootargs.as_deref().unwrap_or_default(),
                selection.x86_linux_print_mtrrs,
                crate::compression::PAYLOAD,
            )?),
        })
    }
}

/// How the build's halves are laid out: every `0x62` entry of a packed
/// generation names the reset image at the top of the lower half (above
/// the early region), the FFS starts at [`layout::FFS_OFFSET`].
pub fn flash_plan<B: Sp3BoardFacts>() -> FlashPlan {
    FlashPlan {
        generations: B::GENERATIONS.iter().map(|g| g.profile().bios).collect(),
        reset: ResetSlot {
            flash_offset: RESET.flash_offset,
            size: RESET.size,
            destination: RESET.destination,
        },
        ffs_offset: layout::FFS_OFFSET,
        dram_floor: EARLY_REGION.end(),
        nvram: Some(NvramSlot {
            flash_offset: layout::NVRAM.offset,
            size: layout::NVRAM.size,
        }),
    }
}

/// The assembler's view of the stage unit and its FFS: the FFS lives in
/// the ROM3 firmware window ([`FFS_WINDOW`], an upper bound; the composer
/// enforces the vendor ROM's real one), the unit is monolithic and stored
/// in the FFS (see the module docs) besides its `0x62` copy.
fn ffs_assembly(
    payload: Payload,
    config: Option<PayloadConfig>,
    microcode: Option<MicrocodeConfig>,
) -> FfsAssembly {
    let (reset, ffs) = (RESET, FFS_WINDOW);
    let region = |name: &str, span: Span, kind| MemoryRegion {
        name: hstr(name),
        base: span.base,
        size: span.size,
        kind,
    };
    FfsAssembly {
        platform: fstart_core::Platform::X86_64,
        memory: vec![
            region("flash", ROM3, fstart_core::memory::RegionKind::Rom),
            region("early", EARLY_REGION, fstart_core::memory::RegionKind::Ram),
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
            load_addr: reset.destination,
            stack_size: STACK.size as u32,
            heap_size: Some(HEAP.size as u32),
            data_addr: Some(DATA.base),
            external: false,
        }),
        security: fstart_core::default_security_config(),
        payload: config,
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

/// Resolve the board's plan: one build for every generation it supports.
pub fn resolve<B: Sp3BoardFacts>(selection: &BuildSelection) -> Result<BuildPlan, String> {
    selection.reject_debug()?;
    if B::GENERATIONS.is_empty() {
        return Err("the board lists no SP3 generation".into());
    }
    // The board crate enables the generation features it lists; the host
    // runner builds the platform with them, so this checks the board.
    if let Some(g) = B::GENERATIONS.iter().find(|&&g| !layout::compiled(g)) {
        return Err(format!(
            "board lists {} but does not enable fstart-platform-sp3/{}",
            g.name(),
            feature(*g)
        ));
    }
    let payload = Payload::select(selection)?;
    if !selection.compiler_files.is_empty() {
        return Err("SP3 takes no compiler file inputs".into());
    }
    if B::MAX_CPUS == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let reset = RESET;
    let unit = CompilationUnit {
        name: STAGE_UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        features: core::iter::once(STAGE_FEATURE)
            .chain(B::GENERATIONS.iter().map(|&g| feature(g)))
            .chain(payload.features().iter().copied())
            .map(String::from)
            .collect(),
        build_std: Some("core,alloc".into()),
        release_only: false,
        // DRAM below 4 GiB: small code model, static relocations.
        rustflags: [
            "-Zub-checks=no",
            "-Crelocation-model=static",
            "-Ccode-model=small",
            "-Clink-arg=-no-pie",
        ]
        .map(String::from)
        .to_vec(),
        linker_script: Some(linker_script()?),
        environment_values: mp_capacity::<B>()
            .into_iter()
            .chain([(TSEG_SIZE_ENV.into(), crate::smm::TSEG_SIZE.to_string())])
            .collect(),
        bindings: vec![ArtifactBinding {
            producer: SMM_UNIT.into(),
            artifact: "image".into(),
            environment: SMM_IMAGE_ENV.into(),
        }],
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations: expectations()?,
            load_address: reset.destination,
            flat_capacity: u64::from(reset.size),
        },
    };
    // The kernel is the one file a caller supplies (`--kernel`); the
    // assembler enforces what actually fits next to the stage unit copy.
    let inputs = match payload {
        Payload::Linux => vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: FFS_WINDOW.size,
        }],
        Payload::Halt | Payload::Uefi => vec![],
    };
    // The PSP firmware is packed from the blob directory: `--blobs`, the
    // blobs fbuild extracts from a `--donor` image, or for a bundle those
    // its assembly extracts. Without one the plan builds the units and
    // assembles no image.
    let config = payload.config(selection)?;
    let deferred = selection
        .defer_firmware
        .then_some(fstart_image_build::bundle::BLOBS_DIR.to_string());
    let (assembly, inputs) = match selection.blobs.as_ref().or(deferred.as_ref()) {
        Some(dir) => {
            let (packed, microcode) = packed::<B>(selection, dir)?;
            let assembly = Assembly::Sp3Flash(Box::new(Sp3FlashAssembly {
                ffs: ffs_assembly(payload, config, Some(microcode)),
                packed,
                stage_unit: STAGE_UNIT.into(),
                compose: flash_plan::<B>(),
            }));
            (assembly, inputs)
        }
        None => (
            Assembly::Unassembled {
                platform: fstart_core::Platform::X86_64,
            },
            vec![],
        ),
    };
    let plan = BuildPlan {
        publication_payloads: ["halt", "linux", "uefi"].map(String::from).to_vec(),
        payload: payload.name().into(),
        units: vec![smm_unit::<B>(), unit],
        stages: vec![STAGE_UNIT.into()],
        assembly,
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// The halves packed from the blob directory `dir`: one per generation the
/// board lists, in boot ROM order (Naples below Rome below Milan), from the
/// platform's `[package.metadata.fstart.amd.<generation>]` table and the
/// board's; and the microcode files of all of them for the FFS. The
/// assembly keeps those the blobs have
/// ([`Sp3FlashAssembly::keep_present`]).
fn packed<B: Sp3BoardFacts>(
    selection: &BuildSelection,
    dir: &str,
) -> Result<(Vec<PackedHalf>, MicrocodeConfig), String> {
    let dir = if selection.defer_firmware {
        std::path::PathBuf::from(dir)
    } else {
        std::fs::canonicalize(dir).map_err(|e| format!("blob directory {dir}: {e}"))?
    };
    let tables = &selection.manifests;
    let boards = blobs::board_tables(&tables.board["amd"])?;
    let name = tables.board["board"]
        .as_str()
        .ok_or("the board table has no board name")?;
    let mut generations = B::GENERATIONS.to_vec();
    generations.sort();
    let mut halves = vec![];
    let mut files = heapless::Vec::new();
    for generation in generations {
        let soc_name = feature(generation);
        let board = boards
            .iter()
            .find(|board| board.soc == soc_name)
            .ok_or_else(|| format!("the board has no {soc_name} firmware table"))?;
        let soc: blobs::SocBlobs = serde_json::from_value(tables.platform["amd"][soc_name].clone())
            .map_err(|e| format!("platform [package.metadata.fstart.amd.{soc_name}]: {e}"))?;
        if soc.bios_directory != generation.profile().bios {
            return Err(format!("the {soc_name} table names another BIOS directory"));
        }
        let resolved = blobs::resolve(
            &soc,
            board,
            name,
            &dir,
            fstart_amdfw_format::HALF_SIZE as u32,
        )?;
        for file in &resolved.microcode {
            files
                .push(
                    file.as_str()
                        .try_into()
                        .map_err(|_| format!("microcode path {file} is too long"))?,
                )
                .map_err(|_| "too many microcode files")?;
        }
        halves.push(PackedHalf {
            generation: generation.profile().bios,
            files: resolved.files,
            apob_destination: board
                .apob_destination
                .or(soc.apob_destination)
                .ok_or_else(|| format!("the {soc_name} table has no APOB destination"))?,
            microcode: resolved.microcode,
        });
    }
    if halves.len() > 2 {
        return Err("an SP3 flash has two halves, not one per generation".into());
    }
    Ok((halves, MicrocodeConfig::Amd(AmdMicrocodeConfig { files })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_image_build::sp3_flash;

    struct S8030;
    impl Sp3BoardFacts for S8030 {
        const GENERATIONS: &'static [Generation] = &[Generation::Rome, Generation::Milan];
        const MAX_CPUS: u16 = 256;
    }

    fn sp3(plan: &BuildPlan) -> &Sp3FlashAssembly {
        let Assembly::Sp3Flash(image) = &plan.assembly else {
            panic!("not an SP3 flash assembly")
        };
        image
    }

    #[test]
    fn plan_has_one_reset_unit_for_every_board_generation() {
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        assert_eq!(plan.stages, [STAGE_UNIT]);
        let unit = plan.unit(STAGE_UNIT).unwrap();
        assert_eq!(unit.features, [STAGE_FEATURE, "rome", "milan"]);
        assert_eq!(unit.environment_values["FSTART_MP_MAX_CPUS"], "256");
        let UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        } = &unit.output
        else {
            panic!("not executable")
        };
        assert_eq!((*load_address, *flat_capacity), (0x75de_0000, 0x22_0000));
        assert_eq!(expectations.entry, Some(0x75ff_fff0));
        let script = unit.linker_script.as_deref().unwrap();
        for needle in [
            "ORIGIN = 0x75de0000, LENGTH = 0x220000",
            ".sp3.entry16 (0x76000000 - 0x1000)",
            "ORIGIN = 0x75000000, LENGTH = 0x101000",
            "_fstart_layout_start = .;",
            "QUAD(0x400000)",
            "the common 0x220000 image",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        struct Nothing;
        impl Sp3BoardFacts for Nothing {
            const GENERATIONS: &'static [Generation] = &[];
            const MAX_CPUS: u16 = 1;
        }
        assert!(resolve::<Nothing>(&BuildSelection::default()).is_err());
    }

    /// The selection packing the S8030's tables from a blob directory
    /// with the PSP firmware of the generations in `present`.
    fn packed_selection(name: &str, present: &[&str]) -> BuildSelection {
        let dir =
            std::env::temp_dir().join(std::format!("fstart-sp3-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for soc in present {
            std::fs::create_dir_all(dir.join(soc)).unwrap();
            std::fs::write(dir.join(soc).join("boot.bin"), [0u8; 16]).unwrap();
        }
        let mut selection = BuildSelection {
            blobs: Some(dir.to_str().unwrap().into()),
            ..Default::default()
        };
        let apcb = |soc: &str| {
            serde_json::json!({ "apcb": { "entries": [
                { "type": 0x68, "file": format!("b/{soc}/apcb.bin") },
            ]}})
        };
        selection.manifests.board = serde_json::json!({
            "board": "b",
            "amd": { "milan": apcb("milan"), "rome": apcb("rome") },
        });
        let soc = |name: &str, slot: &str| {
            serde_json::json!({
                "bios-directory": slot, "levels": 2,
                "efs-offset": 0x2_0000, "partition-end": 0x60_0000,
                "apob-destination": 0x3000_0000,
                "psp": [{ "type": 1, "file": format!("{name}/boot.bin") }],
                "bios": [],
                "microcode": [{ "equivalence-id": 0x8310, "file": format!("{name}/ucode.bin") }],
            })
        };
        selection.manifests.platform = serde_json::json!({ "amd": {
            "rome": soc("rome", "Family17hModel30"),
            "milan": soc("milan", "Family19h"),
        }});
        selection
    }

    fn generations(plan: &BuildPlan) -> std::vec::Vec<fstart_image_build::sp3_flash::Generation> {
        sp3(plan).packed.iter().map(|h| h.generation).collect()
    }

    #[test]
    fn packs_one_half_per_generation_from_the_blob_tables() {
        let selection = packed_selection("both", &["rome", "milan"]);
        let plan = resolve::<S8030>(&selection).unwrap();
        let image = sp3(&plan);
        assert_eq!(generations(&plan), [sp3_flash::ROME, sp3_flash::MILAN]);
        assert_eq!(
            image.packed[1].files.bios[0].path.rsplit('/').next(),
            Some("apcb.bin")
        );
        let Some(MicrocodeConfig::Amd(microcode)) = &image.ffs.microcode else {
            panic!("no microcode")
        };
        assert_eq!(microcode.files.len(), 2);
        // A donor with only one generation's firmware packs that one,
        // without the other's microcode.
        let rome = packed_selection("rome", &["rome"]);
        let mut plan = resolve::<S8030>(&rome).unwrap();
        let Assembly::Sp3Flash(image) = &mut plan.assembly else {
            panic!("not an SP3 flash")
        };
        image
            .keep_present(&|path| std::path::Path::new(path).exists())
            .unwrap();
        assert_eq!(generations(&plan), [sp3_flash::ROME]);
        let Some(MicrocodeConfig::Amd(microcode)) = &sp3(&plan).ffs.microcode else {
            panic!("no microcode")
        };
        assert_eq!(microcode.files.len(), 1);
        assert!(microcode.files[0].ends_with("rome/ucode.bin"));
        // None of them, or a board without a generation's table, fails.
        let mut plan = resolve::<S8030>(&packed_selection("none", &[])).unwrap();
        let Assembly::Sp3Flash(image) = &mut plan.assembly else {
            panic!("not an SP3 flash")
        };
        assert!(
            image
                .keep_present(&|path| std::path::Path::new(path).exists())
                .is_err()
        );
        let mut rome_only = selection;
        rome_only.manifests.board["amd"]
            .as_object_mut()
            .unwrap()
            .remove("milan");
        assert!(resolve::<S8030>(&rome_only).is_err());
        // Without blobs the plan builds the units and assembles no image.
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        assert!(matches!(plan.assembly, Assembly::Unassembled { .. }));
    }

    #[test]
    fn smm_unit_feeds_its_image_to_the_reset_unit() {
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        let order: std::vec::Vec<_> = plan
            .order()
            .unwrap()
            .iter()
            .map(|u| u.name.as_str())
            .collect();
        assert_eq!(order, [SMM_UNIT, STAGE_UNIT]);
        let smm = plan.unit(SMM_UNIT).unwrap();
        assert!(matches!(smm.cargo_target, CargoTarget::BoardLibrary));
        assert_eq!(smm.features, ["smm", "bundle-smm", "rome", "milan"]);
        assert!(smm.release_only && smm.rustflags.iter().any(|f| f == "-Cpanic=abort"));
        assert_eq!(smm.environment_values["FSTART_MP_MAX_CPUS"], "256");
        let UnitOutput::SmmImage {
            entry_count,
            stack_size,
            coreboot_module_args: false,
            coreboot_header: false,
        } = smm.output
        else {
            panic!("not a native SMM image")
        };
        // One entry stub per thread the MP code may bring up.
        assert_eq!((entry_count, stack_size), (256, SMM_STACK_SIZE));

        let reset = plan.unit(STAGE_UNIT).unwrap();
        let [binding] = reset.bindings.as_slice() else {
            panic!("stage unit takes exactly the SMM image")
        };
        assert_eq!(
            (
                binding.producer.as_str(),
                binding.artifact.as_str(),
                binding.environment.as_str()
            ),
            (SMM_UNIT, "image", "FSTART_SMM_IMAGE")
        );
        assert_eq!(reset.environment_values[TSEG_SIZE_ENV], "8388608");
        // Only the stage unit is a stage.
        assert_eq!(plan.stages, [STAGE_UNIT]);
    }

    #[test]
    fn plan_composes_every_supported_half_with_the_ffs_in_the_common_window() {
        let packed = packed_selection("plan", &["rome", "milan"]);
        let plan = resolve::<S8030>(&packed).unwrap();
        let image = sp3(&plan);
        assert_eq!(image.stage_unit, STAGE_UNIT);
        assert_eq!(
            image.compose,
            FlashPlan {
                generations: vec![sp3_flash::ROME, sp3_flash::MILAN],
                reset: ResetSlot {
                    flash_offset: 0xde_0000,
                    size: 0x22_0000,
                    destination: 0x75de_0000
                },
                ffs_offset: 0x60_0000,
                dram_floor: 0x75c0_0000,
                nvram: Some(NvramSlot {
                    flash_offset: 0xda_0000,
                    size: 0x4_0000
                }),
            }
        );
        let config = plan.assembly.config("tyan-s8030").unwrap();
        assert_eq!(
            config.build.firmware_image,
            FirmwareImagePolicy::memory_mapped(0xfd_0060_0000, 0x7a_0000)
        );
        let StageLayout::Monolithic(stage) = &config.stages else {
            panic!("not monolithic")
        };
        assert!(!stage.external && stage.load_addr == 0x75de_0000);
        assert!(config.payload.is_none() && plan.inputs.is_empty());

        // Payloads and what is refused.
        let select = |payload: &str| BuildSelection {
            payload: Some(payload.into()),
            ..packed.clone()
        };
        let linux = resolve::<S8030>(&BuildSelection {
            x86_linux_bootargs: Some("console=ttyS0,115200".into()),
            ..select("linux")
        })
        .unwrap();
        let unit = linux.unit(STAGE_UNIT).unwrap();
        assert_eq!(unit.features.last().unwrap(), "payload-linux");
        assert_eq!(linux.inputs[0].name, "kernel");
        let payload = sp3(&linux).ffs.payload.as_ref().unwrap();
        assert_eq!(payload.bootargs.as_deref(), Some("console=ttyS0,115200"));
        let uefi = resolve::<S8030>(&select("uefi")).unwrap();
        assert_eq!(
            uefi.unit(STAGE_UNIT).unwrap().features.last().unwrap(),
            "payload-uefi"
        );
        assert!(resolve::<S8030>(&select("coreboot")).is_err());
        assert!(
            resolve::<S8030>(&BuildSelection {
                x86_linux_print_mtrrs: true,
                ..Default::default()
            })
            .is_err()
        );
        // The plan round-trips through fbuild's JSON transport.
        let json = serde_json::to_string(&plan).unwrap();
        let back: BuildPlan = serde_json::from_str(&json).unwrap();
        back.order().unwrap();
        assert_eq!(sp3(&back).compose, sp3(&plan).compose);
    }

    #[test]
    fn descriptor_decodes_with_the_layout_regions() {
        let bytes = descriptor().unwrap();
        let layout = fstart_core::layout::Layout::parse(bytes.as_bytes()).unwrap();
        let find = |kind| layout.regions().find(|r| r.kind == kind).unwrap();
        assert_eq!(find(RegionKind::Image).base, 0x75de_0000);
        assert_eq!(find(RegionKind::Firmware).base, 0xfd_0060_0000);
        assert_eq!(find(RegionKind::Firmware).end(), Some(0xfd_00da_0000));
        assert_eq!(find(RegionKind::Flash).base, 0xfd_0000_0000);
        assert_eq!(find(RegionKind::Stack).end(), Some(STACK.end()));
    }
}
