//! The host build of an SP3 board: its [`crate::host::Host`] data, from
//! the board's facts ([`crate::board::Board`]), the SP3 layout and the SP3 flash
//! ([`crate::host::dual`]): one reset image for every generation the board
//! supports, linked at the common reset image address ([`layout::RESET`]),
//! which every composed half's `0x62` entry names.

use std::vec::Vec;

use crate::board::Board;
use crate::host::{Host, dual};
use crate::sp3::layout::{self, EARLY_REGION, LAYOUT, RESET};
use crate::sp3::silicon::Generation;
use fstart_image_build::amd::halved_flash::{FlashPlan, NvramSlot, ResetSlot};

pub use crate::host::{SMM_STACK_SIZE, SMM_UNIT, STAGE_FEATURE, STAGE_UNIT, TSEG_SIZE_ENV};

/// The SP3 generations of the SoCs board `B` lists, in boot ROM order
/// (Naples below Rome below Milan).
fn generations<B: Board>() -> Vec<Generation> {
    use crate::board::SocId;
    let mut generations: Vec<_> = (B::SOCS.iter())
        .filter_map(|soc| match soc {
            SocId::Naples => Some(Generation::Naples),
            SocId::Rome => Some(Generation::Rome),
            SocId::Milan => Some(Generation::Milan),
            SocId::Picasso | SocId::Zeppelin | SocId::Genoa | SocId::Siena => None,
        })
        .collect();
    generations.sort();
    generations
}

/// The name of a generation's firmware tables (`[package.metadata.fstart.amd.<name>]`).
pub const fn soc_name(generation: Generation) -> &'static str {
    match generation {
        Generation::Naples => "naples",
        Generation::Rome => "rome",
        Generation::Milan => "milan",
    }
}

/// The host data of board `B`.
pub fn host<B: Board>() -> Host {
    Host {
        name: "sp3",
        layout: LAYOUT,
        max_cpus: B::MAX_CPUS,
        tseg_size: crate::sp3::smm::TSEG_SIZE,
        release_only: false,
        i2c_input: false,
        image: crate::host::Image::Dual(dual::Image {
            halves: (generations::<B>().into_iter())
                .map(|generation| dual::Half {
                    name: soc_name(generation),
                    bios_directory: generation.profile().bios,
                })
                .collect(),
            plan: flash_plan::<B>(),
        }),
        // The settings store below the reset image of the lower half.
        option_store: Some(fstart_image_build::options::OptionStore::Varstore {
            offset: layout::NVRAM.offset,
            size: layout::NVRAM.size,
        }),
    }
}

/// How the build's halves are laid out: every `0x62` entry of a packed
/// generation names the reset image at the top of the lower half (above
/// the early region), the FFS starts at [`layout::FFS_OFFSET`].
pub fn flash_plan<B: Board>() -> FlashPlan {
    FlashPlan {
        generations: generations::<B>()
            .iter()
            .map(|g| g.profile().bios)
            .collect(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::board::MicrocodeConfig;
    use fstart_core::layout::RegionKind;
    use fstart_core::{FirmwareImagePolicy, StageLayout};
    use fstart_image_build::amd::halved_flash;
    use fstart_image_build::amd::halved_flash::AmdHalvedFlashAssembly;
    use fstart_image_build::build_plan::{CargoTarget, ImageOperation, UnitOutput};

    use crate::board::{Board, SocId};
    use fstart_image_build::build_plan::BuildPlan;
    use fstart_image_build::plan::BuildSelection;

    struct S8030;
    impl Board for S8030 {
        const NAME: &'static str = "s8030";
        const SOCS: &'static [SocId] = &[SocId::Rome, SocId::Milan];
        const MAX_CPUS: u16 = 256;
        fn acpi_board() -> crate::acpi::Board {
            crate::acpi::Board::default()
        }
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
            options: [("blobs".into(), dir.to_str().unwrap().into())].into(),
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

    fn generations(plan: &BuildPlan) -> std::vec::Vec<halved_flash::Generation> {
        sp3(plan).packed.iter().map(|h| h.generation).collect()
    }

    /// The plan of `B` with the selection's options parsed as the plan
    /// runner parses them.
    fn resolve<B: Board>(selection: &BuildSelection) -> Result<BuildPlan, std::string::String> {
        let options = selection.options()?;
        crate::host::resolve_board::<B>(selection, &options).map_err(|e| e.to_string())
    }

    fn sp3(plan: &BuildPlan) -> &AmdHalvedFlashAssembly {
        plan.images
            .iter()
            .find_map(|step| match &step.operation {
                ImageOperation::AmdHalvedFlash(image) => Some(image.as_ref()),
                _ => None,
            })
            .expect("not an SP3 flash assembly")
    }

    #[test]
    fn plan_has_one_reset_unit_for_every_board_generation() {
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        assert_eq!(plan.stages, [STAGE_UNIT]);
        let unit = plan.unit(STAGE_UNIT).unwrap();
        assert_eq!(unit.features, [STAGE_FEATURE]);
        assert_eq!(unit.environment_values["FSTART_MP_MAX_CPUS"], "256");
        let UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        } = &unit.output
        else {
            panic!("not executable")
        };
        assert_eq!((*load_address, *flat_capacity), (0x75d0_0000, 0x30_0000));
        assert_eq!(expectations.entry, Some(0x75ff_fff0));
        let script = unit.linker_script.as_deref().unwrap();
        for needle in [
            "ORIGIN = 0x75d00000, LENGTH = 0x300000",
            ".amd.entry16 (0x76000000 - 0x1000)",
            "ORIGIN = 0x75000000, LENGTH = 0x101000",
            "_fstart_layout_start = .;",
            "QUAD(0x400000)",
            "the common 0x300000 image",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        struct Nothing;
        impl Board for Nothing {
            const NAME: &'static str = "nothing";
            const SOCS: &'static [SocId] = &[];
            const MAX_CPUS: u16 = 1;
            fn acpi_board() -> crate::acpi::Board {
                crate::acpi::Board::default()
            }
        }
        assert!(resolve::<Nothing>(&BuildSelection::default()).is_err());
    }

    #[test]
    fn packs_one_half_per_generation_the_blobs_have() {
        let selection = packed_selection("both", &["rome", "milan"]);
        let plan = resolve::<S8030>(&selection).unwrap();
        let image = sp3(&plan);
        assert_eq!(
            generations(&plan),
            [halved_flash::ROME, halved_flash::MILAN]
        );
        assert_eq!(
            image.packed[1].files.bios[0].path.rsplit('/').next(),
            Some("apcb.bin")
        );
        let Some(MicrocodeConfig::Amd(microcode)) = &plan.ffs_assembly().unwrap().microcode else {
            panic!("no microcode")
        };
        assert_eq!(microcode.files.len(), 2);
        // A Rome-only donor makes a Rome-only image, with Rome's microcode.
        let rome = packed_selection("rome", &["rome"]);
        let mut plan = resolve::<S8030>(&rome).unwrap();
        assert_eq!(
            generations(&plan),
            [halved_flash::ROME, halved_flash::MILAN]
        );
        // The image steps find the plan's blob directory at the host's.
        let in_dir = |selection: &BuildSelection| {
            let blobs = Some(selection.options["blobs"].as_str().into());
            move |path: &str| {
                use fstart_image_build::files::{Files, HostFiles};
                HostFiles {
                    blobs: blobs.clone(),
                }
                .named_exists(std::path::Path::new("/"), std::path::Path::new(path))
            }
        };
        halved_flash::keep_present(&mut plan, &in_dir(&rome)).unwrap();
        assert_eq!(generations(&plan), [halved_flash::ROME]);
        let Some(MicrocodeConfig::Amd(microcode)) = &plan.ffs_assembly().unwrap().microcode else {
            panic!("no microcode")
        };
        assert!(microcode.files.iter().all(|file| file.contains("rome")));
        let none = packed_selection("none", &[]);
        let mut plan = resolve::<S8030>(&none).unwrap();
        assert!(halved_flash::keep_present(&mut plan, &in_dir(&none)).is_err());
        // Every listed generation needs a board table.
        let mut no_milan_table = selection;
        no_milan_table.manifests.board["amd"]
            .as_object_mut()
            .unwrap()
            .remove("milan");
        assert!(resolve::<S8030>(&no_milan_table).is_err());
    }

    #[test]
    fn the_options_store_is_the_flash_plans_settings_store() {
        use fstart_image_build::options::OptionStore;
        let plan = resolve::<S8030>(&packed_selection("store", &["rome"])).unwrap();
        let slot = sp3(&plan).compose.nvram.unwrap();
        assert_eq!(
            plan.options.store,
            Some(OptionStore::Varstore {
                offset: slot.flash_offset,
                size: slot.size,
            })
        );
        // It ends where the reset image starts.
        assert_eq!(slot.end(), sp3(&plan).compose.reset.flash_offset);
        assert_eq!(plan.options.options[0].key, "debug_level");
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
        assert_eq!(smm.features, ["smm", "bundle-smm"]);
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
        let selection = packed_selection("window", &["rome", "milan"]);
        let plan = resolve::<S8030>(&selection).unwrap();
        let image = sp3(&plan);
        assert_eq!(image.stage_unit, STAGE_UNIT);
        assert_eq!(
            image.compose,
            FlashPlan {
                generations: vec![halved_flash::ROME, halved_flash::MILAN],
                reset: ResetSlot {
                    flash_offset: 0xd0_0000,
                    size: 0x30_0000,
                    destination: 0x75d0_0000
                },
                ffs_offset: 0x60_0000,
                dram_floor: 0x75c0_0000,
                nvram: Some(NvramSlot {
                    flash_offset: 0xcc_0000,
                    size: 0x4_0000
                }),
            }
        );
        let config = plan.config("tyan-s8030").unwrap();
        assert_eq!(
            config.build.firmware_image,
            FirmwareImagePolicy::memory_mapped(0xfd_0060_0000, 0x6c_0000)
        );
        let StageLayout::Monolithic(stage) = &config.stages else {
            panic!("not monolithic")
        };
        assert!(!stage.external && stage.load_addr == 0x75d0_0000);
        assert!(config.payload.is_none() && plan.inputs.is_empty());
        // Without blobs, the units and the FFS but no flash image.
        let unpacked = resolve::<S8030>(&BuildSelection::default()).unwrap();
        assert!(unpacked.images.iter().all(|step| step.name == "ffs"));

        // Payloads and what is refused.
        let select = |payload: &str| BuildSelection {
            payload: Some(payload.into()),
            ..Default::default()
        };
        let linux = resolve::<S8030>(&BuildSelection {
            options: [("x86-linux-bootargs".into(), "console=ttyS0,115200".into())].into(),
            ..select("linux")
        })
        .unwrap();
        let unit = linux.unit(STAGE_UNIT).unwrap();
        assert_eq!(unit.features.last().unwrap(), "payload-linux");
        assert_eq!(linux.inputs[0].name, "kernel");
        let payload = linux.ffs_assembly().unwrap().payload.as_ref().unwrap();
        assert_eq!(payload.bootargs.as_deref(), Some("console=ttyS0,115200"));
        let uefi = resolve::<S8030>(&select("uefi")).unwrap();
        assert_eq!(
            uefi.unit(STAGE_UNIT).unwrap().features.last().unwrap(),
            "payload-uefi"
        );
        let ui = resolve::<S8030>(&select("uefi-ui")).unwrap();
        assert_eq!(ui.payload, "uefi-ui");
        assert_eq!(
            ui.unit(STAGE_UNIT).unwrap().features.last().unwrap(),
            "payload-uefi-ui-core"
        );
        assert!(ui.ffs_assembly().unwrap().payload.is_some());
        assert!(ui.inputs.is_empty());
        // CrabEFI's full profile keeps Secure Boot in the settings store.
        assert!(uefi.options.secure_boot && ui.options.secure_boot);
        assert!(
            !resolve::<S8030>(&select("uefi-basic"))
                .unwrap()
                .options
                .secure_boot
        );
        assert!(!linux.options.secure_boot);
        assert!(resolve::<S8030>(&select("coreboot")).is_err());
        assert!(
            resolve::<S8030>(&BuildSelection {
                options: [("x86-linux-print-mtrrs".into(), "true".into())].into(),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            resolve::<S8030>(&BuildSelection {
                options: [("image-config".into(), "/tmp/dump.rom".into())].into(),
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
        let bytes = LAYOUT.descriptor().unwrap();
        let layout = fstart_core::layout::Layout::parse(bytes.as_bytes()).unwrap();
        let find = |kind| layout.regions().find(|r| r.kind == kind).unwrap();
        assert_eq!(find(RegionKind::Image).base, 0x75d0_0000);
        assert_eq!(find(RegionKind::Firmware).base, 0xfd_0060_0000);
        assert_eq!(find(RegionKind::Firmware).end(), Some(0xfd_00cc_0000));
        assert_eq!(find(RegionKind::Flash).base, 0xfd_0000_0000);
        assert_eq!(find(RegionKind::Stack).end(), Some(LAYOUT.stack.end()));
    }
}
