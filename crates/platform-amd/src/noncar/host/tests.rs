// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{picasso, zeppelin};
use fstart_driver_amd::{
    fch::sandstone::preinit::{ReadMode, Speed, SpiConfig},
    soc::{picasso::Picasso, zeppelin::Zeppelin},
};
use fstart_image_build::plan::CompilerFileSelection;
use sha2::{Digest, Sha256};

struct Vilboz;
impl Board for Vilboz {
    type Soc = Picasso;
    const CONFIG: &'static picasso::Config = &picasso::Config {
        fch: picasso::FchConfig {
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
        pcie: &[],
        power: None,
        usb: fstart_driver_amd::fch::sandstone::usb::Config {
            phy2: None,
            phy3_override: None,
            overcurrent: [fstart_driver_amd::fch::sandstone::usb::Overcurrent::None; 6],
            force_gen1: 0,
        },
        emmc: None,
        i2c: &[],
        graphics: fstart_driver_amd::nbio::picasso::gfx::Config {
            displays: &[],
            hdmi2_disable: 0,
        },
    };
    fn console() -> fstart_driver_uart::ns16550::Ns16550Config {
        picasso::console(0)
    }
}

struct Mj11;
impl Board for Mj11 {
    type Soc = Zeppelin;
    const CONFIG: &'static zeppelin::Config = &zeppelin::Config {
        subsystem_id: 0,
        ccx: zeppelin::CcxConfig::DEFAULT,
        fch: zeppelin::FchConfig::DEFAULT,
        dxio: zeppelin::DxioConfig {
            descriptors: [&[]; fstart_driver_amd::soc::zeppelin::MAX_DIES],
        },
    };
    fn console() -> fstart_driver_uart::ns16550::Ns16550Config {
        picasso::console(0)
    }
}

fn units_hash(plan: &BuildPlan) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&plan.units).unwrap())
    )
}

#[test]
fn picasso_payload_plan_includes_smm_and_verified_ffs() {
    let selected = BuildSelection {
        payload: Some("uefi".into()),
        ..Default::default()
    };
    let plan = resolve::<Vilboz>(selected.clone()).unwrap();
    plan.validate_compiler_selections(&selected).unwrap();
    assert_eq!(plan.units.len(), 2);
    assert_eq!(plan.units[0].name, "smm");
    let unit = &plan.units[1];
    assert_eq!(unit.name, "reset");
    assert!(unit.release_only);
    assert_eq!(unit.features, ["bundle-picasso", "payload-uefi"]);
    assert_eq!(unit.bindings[0].producer, "smm");
    assert_eq!(unit.payload, "crabefi");
    let UnitOutput::Executable { expectations, .. } = &unit.output else {
        unreachable!()
    };
    assert_eq!(expectations.entry, Some(picasso::LAYOUT.reset_vector()));
    assert!(plan.assembly.ffs().is_ok());
    let mut invalid = selected;
    invalid
        .compiler_files
        .push(CompilerFileSelection::parse("MICROCODE=ignored.bin").unwrap());
    assert!(resolve::<Vilboz>(invalid).is_err());
}

#[test]
fn picasso_ui_reserves_the_larger_reset_image_and_entry() {
    let plan = resolve::<Vilboz>(BuildSelection {
        payload: Some("uefi-ui".into()),
        ..Default::default()
    })
    .unwrap();
    let unit = plan.units.iter().find(|unit| unit.name == "reset").unwrap();
    assert_eq!(unit.features, ["bundle-picasso", "payload-uefi-ui"]);
    let UnitOutput::Executable {
        expectations,
        load_address,
        flat_capacity,
    } = &unit.output
    else {
        unreachable!()
    };
    assert_eq!(*load_address, 0x0300_0000);
    assert_eq!(*flat_capacity, 0x40_0000);
    assert_eq!(expectations.entry, Some(0x033f_fff0));
    assert_eq!(picasso::LAYOUT.init16(), 0x033f_e000);
    assert_eq!(picasso::LAYOUT.early.end(), 0x0340_0000);
    assert_eq!(picasso::LAYOUT.image.end(), picasso::LAYOUT.early.end());
    let script = unit.linker_script.as_ref().unwrap();
    assert!(script.contains("_fstart_early_reserved_end = 0x3400000;"));
    assert!(script.contains(".init16 0x33fe000"));
    assert!(script.contains(".reset 0x33ffff0"));
}

#[test]
fn picasso_packs_the_blob_tables_into_a_native_image() {
    let blobs = std::env::temp_dir();
    let mut selection = BuildSelection {
        payload: Some("uefi".into()),
        blobs: Some(blobs.to_str().unwrap().into()),
        ..Default::default()
    };
    selection.manifests.board = serde_json::json!({
        "board": "test-board",
        "amd": { "soc": "picasso", "psp-app": true, "apcb": { "instances": 1 } },
    });
    selection.manifests.platform = serde_json::json!({ "amd": { "picasso": {
        "bios-directory": "Family17hModel10",
        "efs-offset": 0xe2_0000, "partition-end": 0x100_0000,
        "spi-read-mode": 4, "spi-speed": 0, "soft-fuse": 0x3000_0000,
        "psp": [{ "type": 1, "file": "psp/boot.bin" }],
        "bios": [],
        "microcode": [{ "equivalence-id": 0x8201, "file": "ucode.bin" }],
        "vbios": [{ "device": 0x15dd, "file": "vbios.bin" }],
    }}});
    let plan = resolve::<Vilboz>(selection.clone()).unwrap();
    plan.validate_image_selection(&selection).unwrap();
    let Assembly::Amd(image) = &plan.assembly else {
        unreachable!()
    };
    let Firmware::Packed {
        files,
        reset_offset,
    } = &image.firmware
    else {
        unreachable!()
    };
    assert_eq!(*reset_offset, Some(0x78_0000));
    assert_eq!(image.reset_size, 0x40_0000);
    assert_eq!(image.reset_destination, 0x0300_0000);
    assert!(files.psp_app && image.psp.compress_reset);
    let window = image.ffs.as_ref().unwrap();
    assert_eq!(window.offset, 0xb8_0000);
    assert_eq!(reset_offset.unwrap() + image.reset_size, window.offset);
    assert_eq!(window.size, 0x29_f000);
    assert_eq!(
        files.bios[0].path,
        blobs.join("test-board/apcb/0.bin").to_str().unwrap()
    );
    let ffs = &image.ffs.as_ref().unwrap().assembly;
    assert_eq!(ffs.file_assets[0].name.as_str(), "pci1002,15dd.rom");
    assert!(matches!(&ffs.microcode, Some(MicrocodeConfig::Amd(m)) if m.files.len() == 1));
    // The tables are for another SoC.
    selection.manifests.board["amd"]["soc"] = "zeppelin".into();
    assert!(resolve::<Vilboz>(selection).is_err());
}

/// The compilation units, hence the stage binaries, stay as they are.
#[test]
fn zeppelin_units_are_pinned() {
    let units = |payload: &str| {
        units_hash(
            &resolve::<Mj11>(BuildSelection {
                payload: Some(payload.into()),
                ..Default::default()
            })
            .unwrap(),
        )
    };
    assert_eq!(
        ["halt", "linux", "uefi"].map(units),
        [
            "32644ab7d328030fe61ef8f493e8c62dc93a2243aafdad590de8d9e3f40d7617",
            "94e3b95781d2b05163253b7c19880c33207bb37a1f4bf7c1b14a4d511f6105ae",
            "b748553b2cdbb3dd7a19720a0c9814e21f4e6de703145cc6c00670640dea081a",
        ]
    );
}

#[test]
fn zeppelin_packs_a_two_level_native_image() {
    let blobs = std::env::temp_dir();
    let mut selection = BuildSelection {
        payload: Some("linux".into()),
        blobs: Some(blobs.to_str().unwrap().into()),
        ..Default::default()
    };
    selection.manifests.board = serde_json::json!({
        "board": "test-board",
        "amd": { "soc": "zeppelin", "apcb": { "entries": [
            { "type": 0x60, "level": 2, "file": "board/apcb.bin" },
        ]}},
    });
    selection.manifests.platform = serde_json::json!({ "amd": { "zeppelin": {
        "bios-directory": "Family17hModel00", "levels": 2,
        "efs-offset": 0x2_0000, "partition-end": 0x29_4000,
        "spi-read-mode": 7, "spi-speed": 1,
        "psp": [
            { "type": 0, "level": 1, "file": "psp/key.bin" },
            { "type": 0x0b, "level": 2, "value": 1 },
            { "type": 0x22, "level": 2, "erased": 0x1000 },
        ],
        "bios": [{ "type": 0x63, "level": 2, "erased": 0x2_0000 }],
        "microcode": [{ "equivalence-id": 0x8012, "file": "ucode.bin" }],
    }}});
    let plan = resolve::<Mj11>(selection.clone()).unwrap();
    plan.validate_image_selection(&selection).unwrap();
    let Assembly::Amd(image) = &plan.assembly else {
        unreachable!()
    };
    let Firmware::Packed {
        files,
        reset_offset,
    } = &image.firmware
    else {
        unreachable!()
    };
    assert_eq!(*reset_offset, Some(0xdb_0000));
    assert!(files.two_level && image.psp.compress_reset);
    assert_eq!((files.spi_read_mode, files.spi_speed), (Some(7), Some(1)));
    assert_eq!(files.fixed.len(), 3);
    assert_eq!(
        files.bios[0].level,
        fstart_image_build::amd::levels::Level::Two
    );
    let ffs = image.ffs.as_ref().unwrap();
    assert_eq!(
        (ffs.offset, ffs.size, image.reset_size),
        (0x29_4000, 0xdb_0000 - 0x29_4000, 0x25_0000)
    );
    assert!(matches!(&ffs.assembly.microcode, Some(MicrocodeConfig::Amd(m)) if m.files.len() == 1));
    // A vendor image instead of the blobs.
    let mut vendor = selection;
    vendor.blobs = None;
    vendor.image_config = Some("vendor.rom".into());
    assert!(resolve::<Mj11>(vendor).is_err());
}
