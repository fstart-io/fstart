// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso host compiler policy. No fictitious FFS/factory-image projection.
extern crate std;
use crate::{
    layout,
    picasso::{self, BoardFacts},
};
use fstart_driver_amd::picasso::cpu::microcode::{Bundle, MAX_PATCHES, PATCH_BYTES};
use fstart_image_build::{
    build_plan::{
        Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, FileBinding,
        UnitOutput,
    },
    elf::{Architecture, Expectations},
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    format,
    io::Read,
    string::{String, ToString},
    vec,
};

pub const MICROCODE_INPUT: &str = "FSTART_AMD_MICROCODE_BUNDLE";
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: BoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(selection).expect("invalid Picasso compiler plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize Picasso plan")
        );
    }
}
/// Compiler support and optional PSP partition packaging. The checkpoint still
/// halts without MP/patch loading. Build owner excludes filesystem mutation from
/// resolution through compilation AND image packaging; no trust is inferred.
pub fn resolve<B: BoardFacts>(selection: BuildSelection) -> Result<BuildPlan, String> {
    selection.validate_compiler_files()?;
    if B::CONFIG.console_uart() >= 4 {
        return Err("Picasso console UART index exceeds hardware table".into());
    }
    if selection
        .payload
        .as_deref()
        .is_some_and(|payload| payload != "halt")
        || selection.has_x86_linux_overrides()
    {
        return Err(
            "Picasso checkpoint is halt-only; native payload flow is not implemented".into(),
        );
    }
    if selection.compiler_files.len() != 1
        || selection.compiler_files[0].environment != MICROCODE_INPUT
    {
        return Err(format!(
            "Picasso requires exactly --compiler-input {MICROCODE_INPUT}=PATH"
        ));
    }
    let input = &selection.compiler_files[0];
    let capacity = (PATCH_BYTES * MAX_PATCHES) as u64;
    let mut bytes = std::vec::Vec::new();
    let metadata = std::fs::metadata(&input.path).map_err(|e| format!("{}: {e}", input.path))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > capacity {
        return Err("microcode input is not a nonempty bounded regular file".into());
    }
    std::fs::File::open(&input.path)
        .map_err(|e| e.to_string())?
        .take(capacity + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Bundle::parse(&bytes).map_err(|e| format!("invalid flat microcode bundle: {e:?}"))?;
    let image = Span {
        base: picasso::RESET_IMAGE_BASE,
        size: u64::from(picasso::RESET_IMAGE_SIZE),
    };
    let expectations = Expectations {
        architecture: Architecture::X86_64,
        elf64: true,
        little_endian: true,
        stored: vec![image],
        runtime: vec![
            image,
            Span {
                base: 0x02019000,
                size: 0x17000,
            },
            Span {
                base: layout::MP_START,
                size: layout::MP_END - layout::MP_START,
            },
            Span {
                base: layout::MICROCODE_START,
                size: layout::MICROCODE_END - layout::MICROCODE_START,
            },
        ],
        identity_mapping: true,
        entry: Some(0x0204fff0),
        copy: None,
        descriptor: None,
        stored_flags: Some(5),
        symbols: BTreeMap::from([
            ("_start".into(), 0x0204fff0),
            ("_fstart_early_reserved_start".into(), layout::EARLY_START),
            ("_fstart_early_reserved_end".into(), layout::MICROCODE_END),
            ("_fstart_mp_start".into(), layout::MP_START),
            ("_fstart_mp_end".into(), layout::MP_END),
            ("_fstart_microcode_start".into(), layout::MICROCODE_START),
            ("_fstart_microcode_end".into(), layout::MICROCODE_END),
            ("_fstart_stack_bottom".into(), 0x02026000),
            ("_fstart_stack_top".into(), image.base),
        ]),
    };
    expectations.validate()?;
    let assembly = if let Some(path) = &selection.image_config {
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 65536 {
            return Err("image configuration is not a bounded nonempty regular file".into());
        }
        let mut bytes = std::vec::Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("image configuration exceeds budget".into());
        }
        let files = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid Picasso image configuration: {e}"))?;
        let operation = fstart_image_build::amd::assembly::PicassoAssembly {
            reset_unit: "reset".into(),
            configuration: FileBinding {
                environment: "IMAGE_CONFIG".into(),
                path: path.clone(),
                capacity: 65536,
            },
            files,
            reset_destination: image.base,
            reset_size: picasso::RESET_IMAGE_SIZE,
            apob_destination: picasso::APOB_BASE,
            shared_memory: Some((picasso::SHARED_BASE, picasso::SHARED_SIZE)),
        };
        operation.validate()?;
        Assembly::PicassoPsp(std::boxed::Box::new(operation))
    } else {
        Assembly::Unassembled {
            platform: fstart_core::Platform::X86_64,
        }
    };
    let plan = BuildPlan {
        payload: "halt".into(),
        stages: vec!["reset".into()],
        inputs: vec![],
        assembly,
        units: vec![CompilationUnit {
            name: "reset".into(),
            cargo_target: CargoTarget::BoardBinary,
            target: "x86_64-unknown-none".into(),
            entry: "picasso-reset".into(),
            environment: "psp-dram".into(),
            payload: "halt".into(),
            cfg_schema: CompilerCfgSchema {
                entries: vec!["picasso-reset".into()],
                environments: vec!["psp-dram".into()],
                payloads: vec!["halt".into()],
            },
            features: vec!["bundle-checkpoint".into()],
            build_std: None,
            rustflags: vec![
                "-Crelocation-model=static".into(),
                "-Cno-redzone=yes".into(),
            ],
            release_only: true,
            linker_script: Some(picasso::LINKER_SCRIPT.into()),
            environment_values: BTreeMap::new(),
            bindings: vec![],
            file_bindings: vec![FileBinding {
                environment: input.environment.clone(),
                path: input.path.clone(),
                capacity,
            }],
            output: UnitOutput::Executable {
                expectations,
                load_address: image.base,
                flat_capacity: image.size,
            },
        }],
    };
    plan.validate_compiler_selections(&selection)?;
    plan.validate_image_selection(&selection)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_image_build::plan::CompilerFileSelection;
    struct Board;
    impl BoardFacts for Board {
        const CONFIG: &'static picasso::Config = &picasso::Config::new().build();
    }
    struct Source(std::path::PathBuf);
    impl Drop for Source {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn source(bytes: &[u8]) -> Source {
        static SERIAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "picasso-plan-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("source = exact.bin"), bytes).unwrap();
        Source(dir)
    }
    fn selection(source: &Source) -> BuildSelection {
        BuildSelection {
            compiler_files: vec![
                CompilerFileSelection::parse(&format!(
                    "{MICROCODE_INPUT}={}",
                    source.0.join("source = exact.bin").display()
                ))
                .unwrap(),
            ],
            ..Default::default()
        }
    }
    #[test]
    fn checkpoint_has_one_explicit_bounded_consumer_and_no_image_projection() {
        // Format-only synthetic record, never installed or represented as trusted.
        let mut bytes = vec![0; PATCH_BYTES];
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..26].copy_from_slice(&0x8180u16.to_le_bytes());
        let source = source(&bytes);
        let selected = selection(&source);
        let plan = resolve::<Board>(selected.clone()).unwrap();
        plan.validate_compiler_selections(&selected).unwrap();
        assert!(plan.assembly.ffs().is_err());
        assert_eq!(plan.selection("reset").unwrap().len(), 1);
        let unit = &plan.units[0];
        assert!(unit.release_only);
        assert_eq!(unit.features, ["bundle-checkpoint"]);
        assert_eq!(unit.file_bindings[0].path, selected.compiler_files[0].path);
        assert_eq!(unit.file_bindings[0].capacity, 9600);
        let UnitOutput::Executable { expectations, .. } = &unit.output else {
            unreachable!()
        };
        assert!(expectations.descriptor.is_none());
        assert_eq!(expectations.stored_flags, Some(5));
        let config_path = source.0.join("image = exact.json");
        std::fs::write(&config_path, r#"{"flash_size":16777216,"efs_offset":14811136,"partition_end":16777216,"spi_read_mode":4,"spi_speed":0,"micron":false,"soft_fuse":805306368,"psp":[],"bios":[],"apob_nv":null,"psp_nv":null}"#).unwrap();
        let mut image_selection = selected.clone();
        image_selection.image_config = Some(config_path.to_str().unwrap().into());
        let image_plan = resolve::<Board>(image_selection.clone()).unwrap();
        assert!(image_plan.assembly.config("test").is_err());
        let Assembly::PicassoPsp(operation) = &image_plan.assembly else {
            unreachable!()
        };
        assert_eq!(
            operation.configuration.path,
            image_selection.image_config.clone().unwrap()
        );
        assert_eq!(operation.reset_destination, picasso::RESET_IMAGE_BASE);
        image_plan
            .validate_image_selection(&image_selection)
            .unwrap();
        assert!(plan.validate_image_selection(&image_selection).is_err());
        let mut wrong = image_plan.clone();
        let Assembly::PicassoPsp(operation) = &mut wrong.assembly else {
            unreachable!()
        };
        operation.reset_destination += 0x10000;
        assert!(wrong.order().is_err());
        image_selection.image_config = Some("substituted.json".into());
        assert!(
            image_plan
                .validate_image_selection(&image_selection)
                .is_err()
        );
        for change in [
            |s: &mut BuildSelection| s.payload = Some("uefi".into()),
            |s: &mut BuildSelection| s.compiler_files[0].environment = "UNSUPPORTED".into(),
            |s: &mut BuildSelection| s.compiler_files.clear(),
            |s: &mut BuildSelection| s.compiler_files.push(s.compiler_files[0].clone()),
        ] {
            let mut invalid = selected.clone();
            change(&mut invalid);
            assert!(resolve::<Board>(invalid).is_err());
        }
        // Rejected before a firmware compiler: trailing bytes and bad header.
        bytes.push(0);
        std::fs::write(source.0.join("source = exact.bin"), &bytes).unwrap();
        assert!(resolve::<Board>(selected.clone()).is_err());
        bytes = vec![0; PATCH_BYTES];
        std::fs::write(source.0.join("source = exact.bin"), bytes).unwrap();
        assert!(resolve::<Board>(selected).is_err());
    }
}
