// SPDX-License-Identifier: MIT OR Apache-2.0
//! The reset image's build plan. With a blob directory (`--blobs`, or the
//! files `--donor` extracts) it is a whole flash image fstart packs: the
//! SoC's blob table (`[package.metadata.fstart.amd.<soc>]` of this crate)
//! and the board's APCBs in a two-level PSP partition, and the reset image
//! after it, at the end of the 16 MiB the boot ROM maps. Without, it is
//! compile-only.
use crate::{Board, Region, Soc};
use fstart_image_build::amd::{
    assembly::{AmdAssembly, Firmware},
    blobs,
    reset::Layout,
};
use fstart_image_build::build_plan::{Assembly, BuildPlan};
use fstart_image_build::plan::{BuildSelection, CompilerFileSelection, Span};
use std::{borrow::Cow, boxed::Box, format, string::String, vec};

/// The flash the boot ROM maps; the reset image ends it.
const WINDOW: u32 = 0x100_0000;

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(&selection).expect("invalid Rs reset plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize Rs plan")
        );
    }
}

const fn span(r: Region) -> Span {
    Span {
        base: r.base,
        size: r.size,
    }
}

/// The link and ELF contract of `S`'s reset image.
pub const fn layout<S: Soc>() -> Layout {
    let l = S::LAYOUT;
    Layout {
        image: span(l.reset),
        early: span(l.early),
        data: span(l.data),
        heap: span(l.heap),
        stack: span(l.stack),
        pages: span(l.pages),
    }
}

/// The descriptor regions besides the entry's own: the PSP's APOB.
fn regions<S: Soc>() -> [Region; 1] {
    [Region {
        kind: fstart_core::layout::RegionKind::Reserved,
        ..S::LAYOUT.apob
    }]
}

/// The files a packed image is made of: the SoC's and the board's blob
/// tables in the selected blob directory. None without one.
fn packed<B: Board>(
    selection: &BuildSelection,
) -> Result<Option<(blobs::SocBlobs, blobs::Resolved)>, String> {
    let soc = B::Soc::NAME;
    let Some(dir) = &selection.blobs else {
        return Ok(None);
    };
    let tables = &selection.manifests;
    let board: blobs::BoardBlobs = serde_json::from_value(tables.board["amd"].clone())
        .map_err(|e| format!("board [package.metadata.fstart.amd]: {e}"))?;
    if board.soc != soc {
        return Err(format!(
            "the board's AMD firmware is for {}, not {soc}",
            board.soc
        ));
    }
    if board.apob_destination.is_some() || board.psp_app {
        return Err(format!(
            "{soc} places the APOB itself and runs no PSP application"
        ));
    }
    let table: blobs::SocBlobs = serde_json::from_value(tables.platform["amd"][soc].clone())
        .map_err(|e| format!("platform [package.metadata.fstart.amd.{soc}]: {e}"))?;
    if table.levels != 2 {
        return Err(format!("{soc}'s PSP partition has two levels"));
    }
    let name = tables.board["board"]
        .as_str()
        .ok_or("the board table has no board name")?;
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("blob directory {dir}: {e}"))?;
    let resolved = blobs::resolve(&table, &board, name, &dir, B::FLASH_SIZE)?;
    Ok(Some((table, resolved)))
}

/// The selection with the SoC table's microcode patch, unless it names one.
fn with_microcode<'a>(
    selection: &'a BuildSelection,
    packed: Option<&blobs::Resolved>,
) -> Result<Cow<'a, BuildSelection>, String> {
    let microcode = fstart_platform_amd::rs_host::MICROCODE;
    let named = selection
        .compiler_files
        .iter()
        .any(|input| input.environment == microcode);
    match packed.map(|p| p.microcode.as_slice()) {
        Some([patch]) if !named => {
            let mut selection = selection.clone();
            selection.compiler_files.push(CompilerFileSelection {
                environment: microcode.into(),
                path: patch.clone(),
            });
            Ok(Cow::Owned(selection))
        }
        Some([_, _, ..]) => Err(format!(
            "{microcode} is one patch; the SoC table names more"
        )),
        _ => Ok(Cow::Borrowed(selection)),
    }
}

/// The packed image: the partition up to the reset image, which ends the
/// boot ROM's window.
fn assembly<B: Board>(
    table: &blobs::SocBlobs,
    resolved: blobs::Resolved,
) -> Result<AmdAssembly, String> {
    let layout = &B::Soc::LAYOUT;
    let reset_size = u32::try_from(layout.reset.size).map_err(|_| "reset image too large")?;
    let reset_offset = WINDOW
        .checked_sub(reset_size)
        .ok_or("the reset image exceeds the boot ROM's window")?;
    if resolved.files.partition_end > reset_offset {
        return Err(format!(
            "{}'s PSP partition ends at {:#x}, after the reset image at {reset_offset:#x}",
            B::Soc::NAME,
            resolved.files.partition_end
        ));
    }
    let assembly = AmdAssembly {
        firmware: Firmware::Packed {
            files: resolved.files,
            reset_offset: Some(reset_offset),
        },
        // The payload is inside the reset image.
        ffs: None,
        stage_unit: fstart_platform_amd::rs_host::STAGE.into(),
        flash_size: B::FLASH_SIZE,
        bios_directory: table.bios_directory,
        reset_size,
        reset_destination: layout.reset.base,
        apob_destination: layout.apob.base,
        shared_memory: None,
        psp: Default::default(),
    };
    assembly.validate()?;
    Ok(assembly)
}

pub fn resolve<B: Board>(selection: &BuildSelection) -> Result<BuildPlan, String> {
    let soc = B::Soc::NAME;
    selection.validate_compiler_files()?;
    if selection.image_config.is_some() {
        return Err(format!(
            "{soc} images are packed from --blobs or --donor, not composed into --image-config"
        ));
    }
    let packed = packed::<B>(selection)?;
    let mut unit = fstart_platform_amd::rs_host::stage_unit(
        &layout::<B::Soc>(),
        &regions::<B::Soc>(),
        crate::MAX_CPUS,
    )?;
    fstart_platform_amd::rs_host::bind(
        &mut unit,
        &*with_microcode(selection, packed.as_ref().map(|(_, r)| r))?,
        &fstart_platform_amd::rs_host::Inputs {
            soc,
            equivalence: B::Soc::EQUIVALENCE,
            linux_requires_microcode: B::Soc::LINUX_REQUIRES_MICROCODE,
            linux_capacity: B::Soc::LINUX_CAPACITY,
            default_bootargs: B::LINUX_BOOTARGS,
        },
    )?;
    // CI links and audits the ELF without AMD firmware.
    let assembly = match packed {
        Some((table, resolved)) => Assembly::Amd(Box::new(assembly::<B>(&table, resolved)?)),
        None => Assembly::Unassembled {
            platform: fstart_core::Platform::X86_64,
        },
    };
    let smm = fstart_platform_amd::rs_host::bind_smm(&mut unit, crate::MAX_CPUS as u16);
    let plan = BuildPlan {
        payload: selection
            .payload
            .clone()
            .unwrap_or_else(|| fstart_platform_amd::rs_host::DEFAULT_PAYLOAD.into()),
        units: vec![smm, unit],
        stages: vec![fstart_platform_amd::rs_host::STAGE.into()],
        assembly,
        inputs: vec![],
    };
    plan.order()?;
    plan.validate_compiler_selections(selection)?;
    plan.validate_image_selection(selection)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Console;
    use std::vec::Vec;

    macro_rules! test_board {
        ($name:ident, $soc:ty) => {
            struct $name;
            impl Board for $name {
                type Soc = $soc;
                const NAME: &'static str = "test";
                const SMBIOS_MAX_DIMM_MIB: u32 = 1;
                const MEMORY_CHANNELS: u16 = 1;
                const FLASH_SIZE: u32 = 0x400_0000;
                const CONSOLE: Console = Console::SuperIoCom1 { irq: 4 };
                const LINUX_BOOTARGS: &'static str = "console=ttyS0,115200";
                unsafe fn early_init() {
                    panic!("host plan must not access hardware")
                }
            }
        };
    }
    #[cfg(feature = "genoa")]
    test_board!(GenoaBoard, crate::Genoa);
    #[cfg(feature = "siena")]
    test_board!(SienaBoard, crate::Siena);

    fn compile_only<B: Board>() {
        let selection = BuildSelection::default();
        let plan = resolve::<B>(&selection).unwrap();
        plan.validate_image_selection(&selection).unwrap();
        assert!(matches!(plan.assembly, Assembly::Unassembled { .. }));
        assert!(plan.assembly.ffs().is_err());
        // CrabEFI by default; a halt only on request.
        assert_eq!(plan.payload, "uefi");
        let halt = BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        };
        assert_eq!(resolve::<B>(&halt).unwrap().payload, "halt");
        // Linux needs its explicit kernel input; other payloads don't exist.
        for payload in ["linux", "crabefi", "coreboot"] {
            let selection = BuildSelection {
                payload: Some(payload.into()),
                ..Default::default()
            };
            assert!(resolve::<B>(&selection).is_err());
        }
        // A vendor image only donates blobs.
        let composed = BuildSelection {
            image_config: Some("/nonexistent/backup.bin".into()),
            ..Default::default()
        };
        assert!(resolve::<B>(&composed).is_err());
    }

    /// A blob directory with a file for every file the board's and the
    /// SoC's tables name, and the selection that packs from it.
    fn blob_directory(soc: &str, board: serde_json::Value) -> (std::path::PathBuf, BuildSelection) {
        let text =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        let manifest: toml::Value = toml::from_str(&text).unwrap();
        let platform = serde_json::to_value(&manifest["package"]["metadata"]["fstart"]).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fstart-rs-blobs-{}-{nonce}", std::process::id()));
        let table = &platform["amd"][soc];
        let files = ["psp", "bios"]
            .iter()
            .flat_map(|t| table[t].as_array().unwrap())
            .chain(board["amd"]["apcb"]["entries"].as_array().unwrap())
            .filter_map(|e| e["file"].as_str());
        for (i, file) in files.enumerate() {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![i as u8; 0x800 + i]).unwrap();
        }
        // One flat patch of the SoC table's equivalence ID.
        let microcode = &table["microcode"][0];
        let id = microcode["equivalence-id"].as_u64().unwrap() as u16;
        let mut patch = vec![0; 0x15c0];
        patch[4..8].copy_from_slice(&(u32::from(id & 0xff00) << 12 | 0x219).to_le_bytes());
        patch[24..26].copy_from_slice(&id.to_le_bytes());
        std::fs::write(dir.join(microcode["file"].as_str().unwrap()), patch).unwrap();
        let selection = BuildSelection {
            blobs: Some(dir.to_str().unwrap().into()),
            manifests: fstart_image_build::plan::PackageTables { board, platform },
            ..Default::default()
        };
        (dir, selection)
    }

    #[test]
    #[cfg(feature = "genoa")]
    fn genoa_packs_from_amds_blob_names_with_the_reset_image_ending_the_window() {
        let board = serde_json::json!({
            "board": "test",
            "amd": { "soc": "genoa", "apcb": { "entries": [
                { "type": 0x60, "file": "test/apcb-60-0.bin" },
                { "type": 0x68, "file": "test/apcb-68-0.bin" },
            ] } },
        });
        let (dir, selection) = blob_directory("genoa", board);
        let plan = resolve::<GenoaBoard>(&selection).unwrap();
        let Assembly::Amd(image) = &plan.assembly else {
            panic!("not a packed image")
        };
        let Firmware::Packed {
            files,
            reset_offset,
        } = &image.firmware
        else {
            panic!("not packed")
        };
        assert_eq!(*reset_offset, Some(0x80_0000));
        assert_eq!(files.generation_mask, Some(1));
        let sources: Vec<Vec<u8>> = image
            .bindings()
            .unwrap()
            .iter()
            .map(|b| std::fs::read(&b.path).unwrap())
            .collect();
        let sources: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
        let reset = vec![0xa5; image.reset_size as usize];
        let packed = image.assemble(&sources, &reset, None).unwrap();
        assert_eq!(packed.bytes.len(), 0x400_0000);
        assert_eq!(
            packed.bytes[0x2_0024..0x2_0028],
            0xffff_fffeu32.to_le_bytes()
        );
        assert_eq!(packed.facts.reset_destination, 0x0a00_0000);
        assert_eq!(packed.facts.apob_destination, 0x0700_1000);
        assert_eq!(packed.bytes[0x80_0000..0x100_0000], reset[..]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(feature = "siena")]
    fn siena_packs_its_partition_and_the_reset_image_in_the_boot_window() {
        let board = serde_json::json!({
            "board": "test",
            "amd": { "soc": "siena", "apcb": { "entries": [
                { "type": 0x60, "level": 2, "file": "test/apcb-60-0.bin" },
                { "type": 0x68, "file": "test/apcb-68-0.bin" },
            ] } },
        });
        let (dir, selection) = blob_directory("siena", board);
        let plan = resolve::<SienaBoard>(&selection).unwrap();
        let Assembly::Amd(image) = &plan.assembly else {
            panic!("not a packed image")
        };
        let Firmware::Packed {
            files,
            reset_offset,
        } = &image.firmware
        else {
            panic!("not packed")
        };
        assert_eq!(*reset_offset, Some(0xa0_0000));
        assert_eq!(files.generation_mask, Some(1));
        // The table's patch, without a --compiler-input.
        let stage = plan.unit(fstart_platform_amd::rs_host::STAGE).unwrap();
        assert!(stage.file_bindings.iter().any(|b| {
            b.environment == fstart_platform_amd::rs_host::MICROCODE
                && b.path.ends_with("siena/PSP/Typex66_0_2_0_UCodePatch.bin")
        }));
        let sources: Vec<Vec<u8>> = image
            .bindings()
            .unwrap()
            .iter()
            .map(|b| std::fs::read(&b.path).unwrap())
            .collect();
        let sources: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
        let reset = vec![0xa5; image.reset_size as usize];
        let packed = image.assemble(&sources, &reset, None).unwrap();
        assert_eq!(packed.bytes.len(), 0x400_0000);
        assert_eq!(
            packed.bytes[0x2_0024..0x2_0028],
            0xffff_fffeu32.to_le_bytes()
        );
        assert_eq!(packed.facts.reset_destination, 0x75a0_0000);
        assert_eq!(packed.facts.apob_destination, 0x7590_0000);
        assert_eq!(packed.bytes[0xa0_0000..0x100_0000], reset[..]);
        // An explicit patch wins over the table's.
        let mut explicit = selection.clone();
        let path = dir.join("other.bin");
        std::fs::copy(dir.join("siena/PSP/Typex66_0_2_0_UCodePatch.bin"), &path).unwrap();
        explicit.compiler_files.push(CompilerFileSelection {
            environment: fstart_platform_amd::rs_host::MICROCODE.into(),
            path: path.to_str().unwrap().into(),
        });
        let plan = resolve::<SienaBoard>(&explicit).unwrap();
        let stage = plan.unit(fstart_platform_amd::rs_host::STAGE).unwrap();
        let patches: Vec<_> = stage
            .file_bindings
            .iter()
            .filter(|b| b.environment == fstart_platform_amd::rs_host::MICROCODE)
            .collect();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, path.to_str().unwrap());
        // Another SoC's board table is refused.
        let mut genoa = selection;
        genoa.manifests.board["amd"]["soc"] = "genoa".into();
        assert!(resolve::<SienaBoard>(&genoa).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(feature = "genoa")]
    fn genoa_is_compile_only_without_a_backup() {
        compile_only::<GenoaBoard>();
    }

    #[test]
    #[cfg(feature = "siena")]
    fn siena_is_compile_only_without_a_backup() {
        compile_only::<SienaBoard>();
    }

    #[test]
    #[cfg(feature = "genoa")]
    fn explicit_microcode_is_bound_unchanged_and_rejects_wrong_format() {
        use fstart_image_build::plan::CompilerFileSelection;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("fstart-genoa-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("patch.bin");
        let mut bytes = vec![0; 0x15c0];
        bytes[4..8].copy_from_slice(&0x0a10_1020u32.to_le_bytes());
        bytes[24..26].copy_from_slice(&0xa110u16.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let mut selection = BuildSelection {
            compiler_files: vec![CompilerFileSelection {
                environment: "FSTART_RS_MICROCODE".into(),
                path: path.to_str().unwrap().into(),
            }],
            ..Default::default()
        };
        let plan = resolve::<GenoaBoard>(&selection).unwrap();
        let binding = &plan
            .unit(fstart_platform_amd::rs_host::STAGE)
            .unwrap()
            .file_bindings[0];
        assert_eq!(binding.path, selection.compiler_files[0].path);
        assert_eq!(binding.environment, "FSTART_RS_MICROCODE");
        assert_eq!(binding.capacity, 0x15c0);
        selection.compiler_files[0].environment = "FSTART_OTHER".into();
        assert!(resolve::<GenoaBoard>(&selection).is_err());
        selection.compiler_files[0].environment = "FSTART_RS_MICROCODE".into();
        for bad in [
            vec![],
            bytes[..bytes.len() - 1].to_vec(),
            {
                let mut milan = bytes.clone();
                milan[24..26].copy_from_slice(&0xa011u16.to_le_bytes());
                milan
            },
            {
                let mut chipset = bytes.clone();
                chipset[16] = 1;
                chipset
            },
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(resolve::<GenoaBoard>(&selection).is_err());
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(feature = "genoa")]
    fn measured_onyx_reset_and_apob_are_not_public_opensil_or_siena_defaults() {
        let layout = layout::<crate::Genoa>();
        let descriptor = layout.descriptor(&regions::<crate::Genoa>()).unwrap();
        let script = layout.linker_script(&descriptor).unwrap();
        assert!(script.contains("ORIGIN = 0xa000000, LENGTH = 0x800000"));
        let expected = layout.expectations(&descriptor).unwrap();
        assert_eq!(expected.entry, Some(0x0a7f_fff0));
        assert_eq!(
            expected.runtime[1].base + expected.runtime[1].size,
            0x0a00_0000
        );
        assert_eq!(<crate::Genoa as Soc>::LAYOUT.apob.base, 0x0700_1000);
    }

    #[test]
    #[cfg(feature = "siena")]
    fn siena_linker_and_elf_contract_keep_writable_storage_below_apob() {
        let layout = layout::<crate::Siena>();
        let descriptor = layout.descriptor(&regions::<crate::Siena>()).unwrap();
        let script = layout.linker_script(&descriptor).unwrap();
        assert!(!script.contains('@'));
        assert!(script.contains("ORIGIN = 0x75a00000, LENGTH = 0x600000"));
        assert!(script.contains("ORIGIN = 0x75700000, LENGTH = 0x100000"));
        let expected = layout.expectations(&descriptor).unwrap();
        assert_eq!(expected.entry, Some(0x75ff_fff0));
        assert_eq!(
            expected.runtime[1].base + expected.runtime[1].size,
            0x7580_0000
        );
    }
}
