// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso host compiler policy. No fictitious FFS/factory-image projection.
extern crate std;
use super::LAYOUT as L;
use crate::noncar::{Board, Soc, host as noncar};
use fstart_driver_amd::soc::picasso::Picasso;
use fstart_image_build::{
    build_plan::{
        Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, FileBinding,
        UnitOutput,
    },
    plan::BuildSelection,
};
use std::{
    collections::BTreeMap,
    format,
    io::Read,
    string::{String, ToString},
    vec,
};

const ENTRY: &str = "amd-psp-reset";

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board<Soc = Picasso>> Plan<B> {
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
/// Compiler support and optional PSP partition packaging. The flow still
/// halts at its checkpoint. Build owner excludes filesystem mutation from
/// resolution through compilation AND image packaging; no trust is inferred.
pub fn resolve<B: Board<Soc = Picasso>>(selection: BuildSelection) -> Result<BuildPlan, String> {
    selection.validate_compiler_files()?;
    if B::CONFIG.fch.console_uart >= 4 {
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
    let descriptor = noncar::descriptor(&L)?;
    let expectations = noncar::expectations(&L, &descriptor);
    expectations.validate()?;
    let image = noncar::image(&L);
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
            reset_size: image.size as u32,
            apob_destination: Picasso::APOB.base,
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
            entry: ENTRY.into(),
            environment: "monolithic".into(),
            payload: "halt".into(),
            cfg_schema: CompilerCfgSchema {
                entries: vec![ENTRY.into()],
                environments: vec!["monolithic".into()],
                payloads: vec!["halt".into()],
            },
            features: vec!["bundle-picasso".into()],
            build_std: None,
            rustflags: vec![
                "-Zub-checks=no".into(),
                "-Crelocation-model=static".into(),
                "-Cno-redzone=yes".into(),
                "--cfg".into(),
                "curve25519_dalek_backend=\"serial\"".into(),
            ],
            release_only: true,
            linker_script: Some(noncar::linker_script("Picasso", &L, &descriptor)),
            environment_values: BTreeMap::from([(
                "FSTART_MP_MAX_CPUS".into(),
                Picasso::MAX_CPUS.to_string(),
            )]),
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
    use crate::picasso::{Config, FchConfig};
    use fstart_driver_amd::fch::sandstone::preinit::{ReadMode, Speed, SpiConfig};
    use fstart_image_build::plan::CompilerFileSelection;
    struct Board;
    impl crate::noncar::Board for Board {
        type Soc = Picasso;
        const CONFIG: &'static Config = &Config {
            fch: FchConfig {
                console_uart: 0,
                spi: SpiConfig {
                    mode: ReadMode::Normal33,
                    normal: Speed::Mhz33,
                    fast: Speed::Mhz66,
                    alternate: Speed::Mhz66,
                    tpm: Speed::Mhz66,
                    burst_4dw: false,
                },
                espi: None,
                early_gpios: &[],
                disable_rom_sharing: false,
            },
        };
        fn console() -> fstart_driver_uart::ns16550::Ns16550Config {
            crate::picasso::console(0)
        }
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
        // The compilation unit, hence the reset image, stays as it is.
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&plan.units).unwrap())
            ),
            "05a2647092cb10a57532a932199562d2e1d6054669e6cc0ab41841cd03aad19e"
        );
        assert!(plan.assembly.ffs().is_err());
        assert_eq!(plan.selection("reset").unwrap().len(), 1);
        let unit = &plan.units[0];
        assert!(unit.release_only);
        assert_eq!(unit.features, ["bundle-picasso"]);
        assert!(unit.file_bindings.is_empty());
        let UnitOutput::Executable { expectations, .. } = &unit.output else {
            unreachable!()
        };
        assert_eq!(expectations.entry, Some(0x022f_fff0));
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
        assert_eq!(operation.reset_destination, L.image.base);
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
