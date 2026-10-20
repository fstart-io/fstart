// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin host compiler and image policy.
//!
//! One DRAM stage, the BIOS reset image the PSP copies, runs from reset vector
//! to payload. The payload and its manifest go into an FFS between the vendor
//! PSP prefix and the reset image. The vendor flash image is the explicit
//! `--image-config` input: its BIOS directory must describe the reset image and
//! APOB destination this layout was linked for.
extern crate std;
use super::layout::{LAYOUT as L, RESET_FLASH_OFFSET, VENDOR_PREFIX};
use crate::noncar::{Board, host as noncar};
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, Platform, SocImageFormat, StageBuildConfig, StageLayout, hstr,
};
use fstart_driver_amd::soc::zeppelin::Zeppelin;
use fstart_image_build::{
    amd::vendor_flash::{VendorFlashAssembly, VendorFlashLayout, read_bios_directory},
    build_plan::{
        ArtifactBinding, Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema,
        FfsAssembly, FileBinding, InputFile, UnitOutput,
    },
    plan::BuildSelection,
};
use std::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

pub const ENTRY: &str = "amd-psp-reset";
const UNIT: &str = "stage";
const SMM_UNIT: &str = "smm";
/// Up to 32 threads (two dies of eight SMT cores).
const MAX_CPUS: u16 = 32;

fn cfg_schema() -> CompilerCfgSchema {
    CompilerCfgSchema {
        entries: vec![ENTRY.into()],
        environments: vec!["monolithic".into(), "smm".into()],
        payloads: vec!["halt".into(), "linux".into(), "crabefi".into()],
    }
}

/// The board's SMM handler as a standalone image, one entry per CPU, which
/// the stage embeds (`FSTART_SMM_IMAGE`).
fn smm_unit() -> CompilationUnit {
    CompilationUnit {
        name: SMM_UNIT.into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: "x86_64-unknown-none".into(),
        entry: ENTRY.into(),
        cfg_schema: cfg_schema(),
        environment: "smm".into(),
        payload: "halt".into(),
        features: vec!["smm".into(), "bundle-smm".into()],
        build_std: None,
        release_only: true,
        linker_script: None,
        rustflags: [
            "-Cpanic=abort",
            "-Copt-level=s",
            // Direct relative calls; absolute cross-section references are audited.
            "-Crelocation-model=static",
            "-Cno-redzone=yes",
            "-Clinker-plugin-lto=no",
            "-Cembed-bitcode=no",
            "-Zfunction-sections=yes",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        environment_values: BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), MAX_CPUS.to_string())]),
        bindings: vec![],
        file_bindings: vec![],
        output: UnitOutput::SmmImage {
            entry_count: MAX_CPUS,
            stack_size: 0x800,
            coreboot_module_args: false,
            coreboot_header: false,
        },
    }
}

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board<Soc = Zeppelin>> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(selection).expect("invalid Zeppelin build plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize Zeppelin plan")
        );
    }
}

/// The FFS between the vendor prefix and the reset image.
const FFS: crate::noncar::layout::Region = match L.ffs {
    Some(ffs) => ffs,
    None => panic!("Zeppelin images have an FFS"),
};
const FLASH_SIZE: u32 = L.flash.size as u32;

/// Check the explicit vendor image against the layout this stage is linked for.
fn vendor_layout(path: &str) -> Result<(FileBinding, VendorFlashLayout), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("vendor image {path}: {e}"))?;
    if bytes.len() != FLASH_SIZE as usize {
        return Err(format!(
            "vendor image {path} is {:#x} bytes, the flash {:#x}",
            bytes.len(),
            FLASH_SIZE
        ));
    }
    let facts = read_bios_directory(&bytes, VENDOR_PREFIX)?;
    let layout = VendorFlashLayout {
        flash_size: FLASH_SIZE,
        prefix_size: VENDOR_PREFIX,
        ffs_offset: VENDOR_PREFIX,
        ffs_size: FFS.size as u32,
        reset_offset: RESET_FLASH_OFFSET,
        reset_size: L.image.size as u32,
        reset_destination: L.image.base,
        apob_destination: fstart_driver_amd::soc::zeppelin::APOB_BASE,
    };
    layout.validate()?;
    if (
        facts.reset_offset,
        facts.reset_size,
        facts.reset_destination,
        facts.apob_destination,
    ) != (
        layout.reset_offset,
        layout.reset_size,
        layout.reset_destination,
        layout.apob_destination,
    ) {
        return Err(format!(
            "vendor BIOS directory {facts:?} doesn't match the Zeppelin layout {layout:?}"
        ));
    }
    Ok((
        FileBinding {
            environment: "VENDOR_FLASH".into(),
            path: path.into(),
            capacity: u64::from(FLASH_SIZE),
        },
        layout,
    ))
}

pub fn resolve<B: Board<Soc = Zeppelin>>(selection: BuildSelection) -> Result<BuildPlan, String> {
    selection.validate_compiler_files()?;
    if !selection.compiler_files.is_empty() {
        return Err("Zeppelin takes no compiler inputs".into());
    }
    let requested = selection.payload.clone().unwrap_or_else(|| "halt".into());
    if selection.has_x86_linux_overrides() && requested != "linux" {
        return Err("x86 Linux options require '--payload linux'".into());
    }
    let (payload, feature, payload_config): (&str, Option<&str>, Option<PayloadConfig>) =
        match requested.as_str() {
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
            "uefi-basic" => (
                "crabefi",
                Some("payload-uefi-basic"),
                Some(fstart_core::x86_uefi_payload()),
            ),
            other => {
                return Err(format!(
                    "Zeppelin supports halt, linux and uefi, not {other}"
                ));
            }
        };

    let descriptor = noncar::descriptor(&L)?;
    let expectations = noncar::expectations(&L, &descriptor);
    expectations.validate()?;
    let image = noncar::image(&L);

    let ffs = FfsAssembly {
        platform: Platform::X86_64,
        memory: vec![
            MemoryRegion {
                name: hstr("flash"),
                base: L.flash.base,
                size: L.flash.size,
                kind: fstart_core::RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("dram"),
                base: L.early.base,
                size: L.early.size,
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
                payload: payload != "halt",
                pci: true,
                ..Default::default()
            },
            load_addr: L.image.base,
            data_addr: Some(L.data.base),
            stack_size: L.stack.size as u32,
            heap_size: Some(L.heap.size as u32),
            page_table_addr: None,
            page_size: Default::default(),
            external: true,
        }),
        security: fstart_core::dev_security_config("keys/dev-signing.pub"),
        payload: payload_config,
        microcode: None,
        data_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(FFS.base, FFS.size),
            ..Default::default()
        },
    };
    let inputs: Vec<InputFile> = if payload == "linux" {
        vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: FFS.size,
        }]
    } else {
        vec![]
    };
    let assembly = match &selection.image_config {
        Some(path) => {
            let (vendor, layout) = vendor_layout(path)?;
            Assembly::VendorFlash(std::boxed::Box::new(VendorFlashAssembly {
                ffs,
                vendor,
                layout,
                reset_unit: UNIT.into(),
            }))
        }
        None => Assembly::Ffs(std::boxed::Box::new(ffs)),
    };
    let plan = BuildPlan {
        payload: payload.into(),
        stages: vec![UNIT.into()],
        inputs,
        assembly,
        units: vec![
            smm_unit(),
            CompilationUnit {
                name: UNIT.into(),
                cargo_target: CargoTarget::BoardBinary,
                target: "x86_64-unknown-none".into(),
                entry: ENTRY.into(),
                environment: "monolithic".into(),
                payload: payload.into(),
                cfg_schema: cfg_schema(),
                features: core::iter::once("bundle-zeppelin")
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
                release_only: false,
                linker_script: Some(noncar::linker_script("Zeppelin", &L, &descriptor)),
                environment_values: BTreeMap::from([(
                    "FSTART_MP_MAX_CPUS".into(),
                    MAX_CPUS.to_string(),
                )]),
                bindings: vec![ArtifactBinding {
                    producer: SMM_UNIT.into(),
                    artifact: "image".into(),
                    environment: "FSTART_SMM_IMAGE".into(),
                }],
                file_bindings: vec![],
                output: UnitOutput::Executable {
                    expectations,
                    load_address: image.base,
                    flat_capacity: image.size,
                },
            },
        ],
    };
    plan.order()?;
    plan.validate_compiler_selections(&selection)?;
    plan.validate_image_selection(&selection)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zeppelin::{Config, DxioConfig, FchConfig};
    use sha2::{Digest, Sha256};
    struct Board;
    impl crate::noncar::Board for Board {
        type Soc = Zeppelin;
        const CONFIG: &'static Config = &Config {
            subsystem_id: 0,
            fch: FchConfig::DEFAULT,
            dxio: DxioConfig {
                descriptors: [&[]; fstart_driver_amd::soc::zeppelin::MAX_DIES],
            },
        };
        fn console() -> fstart_driver_uart::ns16550::Ns16550Config {
            crate::picasso::console(0)
        }
    }

    /// The compilation units, hence the stage binaries, stay as they are.
    #[test]
    fn units_are_pinned() {
        let units = |payload: &str| {
            let plan = resolve::<Board>(BuildSelection {
                payload: Some(payload.into()),
                ..Default::default()
            })
            .unwrap();
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&plan.units).unwrap())
            )
        };
        assert_eq!(
            ["halt", "linux", "uefi"].map(units),
            [
                "58ec8cbda4f8ffae319be0218a4828bf1985b7866ba7c71b16f97388f116a0b2",
                "7b95efa1400a6bae9aaf5d2009a73119a012e5d368a19d2082a0348f2d25ea1d",
                "fce4a81e8fedd6c40500f5f1dbcf7a663358c078c406b43d1420d5c4067f505c",
            ]
        );
    }
}
