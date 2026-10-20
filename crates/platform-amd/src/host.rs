// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso host compiler policy. No fictitious FFS/factory-image projection.
extern crate std;
use crate::{
    layout,
    picasso::{self, BoardFacts},
};
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
    // The CPUs load microcode from the BIOS directory (type 0x66), where the
    // image packing puts it.
    if !selection.compiler_files.is_empty() {
        return Err("Picasso takes no compiler inputs".into());
    }
    let image = Span {
        base: layout::IMAGE_BASE,
        size: u64::from(layout::IMAGE_SIZE),
    };
    let expectations = Expectations {
        architecture: Architecture::X86_64,
        elf64: true,
        little_endian: true,
        stored: vec![image],
        runtime: vec![
            image,
            Span {
                base: layout::BSS_START,
                size: layout::STACK_TOP - layout::BSS_START,
            },
            Span {
                base: layout::MICROCODE_START,
                size: layout::MICROCODE_END - layout::MICROCODE_START,
            },
            Span {
                base: layout::PSP_START,
                size: layout::PSP_END - layout::PSP_START,
            },
        ],
        identity_mapping: true,
        entry: Some(layout::RESET_VECTOR),
        copy: None,
        descriptor: None,
        stored_flags: Some(5),
        symbols: BTreeMap::from([
            ("_start".into(), layout::RESET_VECTOR),
            ("_fstart_early_reserved_start".into(), layout::EARLY_START),
            ("_fstart_early_reserved_end".into(), layout::EARLY_END),
            ("_fstart_page_tables".into(), layout::PAGE_TABLES),
            ("_fstart_microcode_start".into(), layout::MICROCODE_START),
            ("_fstart_microcode_end".into(), layout::MICROCODE_END),
            ("_fstart_psp_start".into(), layout::PSP_START),
            ("_fstart_psp_end".into(), layout::PSP_END),
            ("_fstart_stack_bottom".into(), layout::STACK_BOTTOM),
            ("_fstart_stack_top".into(), layout::STACK_TOP),
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
            reset_size: layout::IMAGE_SIZE,
            apob_destination: layout::APOB_BASE,
            // Entry 0x6B is only for a PSP verstage.
            shared_memory: None,
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
            file_bindings: vec![],
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
        const SPI: fstart_driver_amd::fch::sandstone::preinit::SpiConfig = {
            use fstart_driver_amd::fch::sandstone::preinit::{ReadMode, Speed, SpiConfig};
            SpiConfig {
                mode: ReadMode::Normal33,
                normal: Speed::Mhz33,
                fast: Speed::Mhz66,
                alternate: Speed::Mhz66,
                tpm: Speed::Mhz66,
                burst_4dw: false,
            }
        };
    }
    struct Source(std::path::PathBuf);
    impl Drop for Source {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn source() -> Source {
        static SERIAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "picasso-plan-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Source(dir)
    }
    #[test]
    fn checkpoint_has_no_compiler_inputs_and_no_image_projection() {
        let source = source();
        let selected = BuildSelection::default();
        let plan = resolve::<Board>(selected.clone()).unwrap();
        plan.validate_compiler_selections(&selected).unwrap();
        assert!(plan.assembly.ffs().is_err());
        assert_eq!(plan.selection("reset").unwrap().len(), 1);
        let unit = &plan.units[0];
        assert!(unit.release_only);
        assert_eq!(unit.features, ["bundle-checkpoint"]);
        assert!(unit.file_bindings.is_empty());
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
        assert_eq!(operation.reset_destination, layout::IMAGE_BASE);
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
        let microcode = source.0.join("microcode.bin");
        std::fs::write(&microcode, [0; 16]).unwrap();
        for change in [
            |s: &mut BuildSelection, _: &str| s.payload = Some("uefi".into()),
            |s: &mut BuildSelection, path: &str| {
                s.compiler_files
                    .push(CompilerFileSelection::parse(&format!("MICROCODE={path}")).unwrap())
            },
        ] {
            let mut invalid = selected.clone();
            change(&mut invalid, microcode.to_str().unwrap());
            assert!(resolve::<Board>(invalid).is_err());
        }
    }
}
