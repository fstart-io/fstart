// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{picasso, zeppelin};
use fstart_amdfw_format::{
    directory::{BIOS_COOKIE, BiosEntry, PSP_COOKIE, PspEntry, bios, write_table},
    efs::Efs,
};
use fstart_driver_amd::{
    fch::sandstone::preinit::{ReadMode, Speed, SpiConfig},
    soc::{picasso::Picasso, zeppelin::Zeppelin},
};
use fstart_image_build::plan::CompilerFileSelection;
use sha2::{Digest, Sha256};
use zerocopy::IntoBytes;

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
        fch: zeppelin::FchConfig::DEFAULT,
        dxio: zeppelin::DxioConfig {
            descriptors: [&[]; fstart_driver_amd::soc::zeppelin::MAX_DIES],
        },
    };
    fn console() -> fstart_driver_uart::ns16550::Ns16550Config {
        picasso::console(0)
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
        "noncar-plan-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    Source(dir)
}

fn units_hash(plan: &BuildPlan) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&plan.units).unwrap())
    )
}

#[test]
fn checkpoint_has_no_compiler_inputs_and_no_image_projection() {
    let source = source();
    let selected = BuildSelection::default();
    let plan = resolve::<Vilboz>(selected.clone()).unwrap();
    plan.validate_compiler_selections(&selected).unwrap();
    // The compilation unit, hence the reset image, stays as it is.
    assert_eq!(
        units_hash(&plan),
        "778d1bc0b03b7985a8517798b60b1ec1bd8c007e3516379ee09baf461e132175"
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
    let image_plan = resolve::<Vilboz>(image_selection.clone()).unwrap();
    assert!(image_plan.assembly.config("test").is_err());
    let Assembly::Amd(operation) = &image_plan.assembly else {
        unreachable!()
    };
    assert!(operation.ffs.is_none());
    assert_eq!(
        operation.firmware.configuration().path,
        image_selection.image_config.clone().unwrap()
    );
    assert_eq!(operation.reset_destination, picasso::LAYOUT.image.base);
    image_plan
        .validate_image_selection(&image_selection)
        .unwrap();
    assert!(plan.validate_image_selection(&image_selection).is_err());
    let mut wrong = image_plan.clone();
    let Assembly::Amd(operation) = &mut wrong.assembly else {
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
        assert!(resolve::<Vilboz>(invalid).is_err());
    }
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
            "2f3f70f9f8bf7bbb95ccee6769e6d810b30c1af9c164da60d9a23c8ce20e4367",
            "642f9dd5c1ab2022604e5fc460b9608498193fe0d9e366f809dbea5c829ac84d",
            "c300ac57c6ecb8f90480edd0c61daee8f2cf7d39577dfdb5e44b15102c9da500",
        ]
    );
}

/// A vendor flash with the PSP and BIOS directories of the MJ11 layout.
fn vendor_flash(reset_destination: u64) -> Vec<u8> {
    let mut flash = vec![0xffu8; 0x100_0000];
    let mut efs = Efs::erased();
    efs.new_psp_directory.set(0xff05_7000);
    efs.bios_directory_mut(Zeppelin::BIOS_DIRECTORY)
        .set(0xff0f_7000);
    flash[0x2_0000..0x2_0060].copy_from_slice(efs.as_bytes());
    write_table(
        &mut flash[0x5_7000..],
        PSP_COOKIE,
        0,
        &[PspEntry::new(0, 0, 0, 0x240, 0xff05_7400)],
    )
    .unwrap();
    write_table(
        &mut flash[0xf_7000..],
        BIOS_COOKIE,
        0,
        &[
            BiosEntry::new(bios::APOB, 0, 0, 0, 0, 0, 0x3000_0000),
            BiosEntry::new(
                bios::RESET_IMAGE,
                0,
                0,
                BiosEntry::RESET | BiosEntry::COPY,
                0x25_0000,
                0xffdb_0000,
                reset_destination,
            ),
        ],
    )
    .unwrap();
    flash
}

#[test]
fn zeppelin_composes_the_vendor_image() {
    let source = source();
    let path = source.0.join("vendor.rom");
    let selection = |path: &std::path::Path| BuildSelection {
        payload: Some("linux".into()),
        image_config: Some(path.to_str().unwrap().into()),
        ..Default::default()
    };
    std::fs::write(&path, vendor_flash(0x76db_0000)).unwrap();
    let plan = resolve::<Mj11>(selection(&path)).unwrap();
    let Assembly::Amd(image) = &plan.assembly else {
        unreachable!()
    };
    assert_eq!(image.firmware.configuration().path, path.to_str().unwrap());
    let ffs = image.ffs.as_ref().unwrap();
    assert_eq!(
        (ffs.offset, ffs.size, image.reset_size),
        (0x29_4000, 0xdb_0000 - 0x29_4000, 0x25_0000)
    );
    assert_eq!(plan.inputs[0].capacity, 0xdb_0000 - 0x29_4000);
    // The vendor's PSP copies the reset image elsewhere.
    std::fs::write(&path, vendor_flash(0x7600_0000)).unwrap();
    assert!(resolve::<Mj11>(selection(&path)).is_err());
}
