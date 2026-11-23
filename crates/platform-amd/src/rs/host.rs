// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host build of an RS board: one image with the PSP firmware of its
//! SoC packed from the blob tables (`--blobs` or `--donor`), the FFS
//! between the PSP partition and the reset image, which ends the 16 MiB the
//! boot ROM maps ([`crate::host::single`]).
extern crate std;
use super::Variant;
use crate::host::{Host, single};
use fstart_amdfw_format::efs::BiosDirectorySlot;

/// The host data of a board with the RS SoC `V` and up to `max_cpus` CPUs.
pub fn host<V: Variant>(max_cpus: u16) -> Host {
    let layout = V::LAYOUT;
    Host {
        name: V::NAME,
        layout,
        max_cpus,
        tseg_size: super::TSEG_SIZE.into(),
        release_only: true,
        i2c_input: false,
        image: crate::host::Image::Single(single::Image {
            soc: V::NAME,
            bios_directory: BiosDirectorySlot::Family19h,
            apob_destination: V::APOB.base,
            source: single::Source {
                reset_offset: Some((layout.flash.size - layout.image.size) as u32),
                psp: Default::default(),
            },
        }),
        option_store: V::NVRAM.map(
            |region| fstart_image_build::options::OptionStore::Varstore {
                offset: (region.base - layout.flash.base) as u32,
                size: region.size as u32,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Image;
    use fstart_image_build::options::OptionStore;

    #[test]
    fn the_reset_image_ends_the_boot_window_after_ffs_and_nvram() {
        for host in [
            host::<crate::genoa::Stones>(192),
            host::<crate::siena::Dense>(128),
        ] {
            let layout = host.layout;
            let ffs = layout.ffs.unwrap();
            let Image::Single(image) = host.image else {
                panic!("one image")
            };
            let Some(reset) = image.source.reset_offset else {
                panic!("a whole image")
            };
            assert_eq!(u64::from(reset) + layout.image.size, layout.flash.size);
            match host.option_store {
                Some(OptionStore::Varstore { offset, size }) => {
                    assert_eq!((offset, size), (0xcc_0000, 0x4_0000));
                    assert_eq!(ffs.end(), layout.flash.base + u64::from(offset));
                    assert_eq!(offset + size, reset);
                }
                None => assert_eq!(ffs.end(), layout.flash.base + u64::from(reset)),
                other => panic!("unexpected settings store: {other:?}"),
            }
        }
    }

    #[test]
    fn siena_ab_plan_emits_ro_rw_and_update_contract() {
        use crate::ab::{
            AB_NVRAM_OFFSET, AB_NVRAM_SIZE, AB_RO_DRAM_BASE, AB_RO_FLASH_OFFSET, AB_RO_SIZE,
            SIENA_AB_SLOT_SIZE, host::siena_slot_layout,
        };
        use crate::board::{AbUpdateConfig, Board, SocId};
        use fstart_image_build::build_plan::{BuildPlan, ImageOperation};
        use fstart_image_build::plan::BuildSelection;

        const TEST_GUID: [u8; 16] = [
            0xaa, 0xbb, 0xcc, 0xdd, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x00,
            0x12, 0x34,
        ];
        struct S8040Ab;
        impl Board for S8040Ab {
            const NAME: &'static str = "tyan-s8040-ab";
            const SOCS: &'static [SocId] = &[SocId::Siena];
            const MAX_CPUS: u16 = 128;
            const AB_UPDATE: Option<AbUpdateConfig> = Some(AbUpdateConfig {
                image_family: "tyan-s8040",
                firmware_guid: TEST_GUID,
                lowest_supported_version: 1,
                capsule_trust_certs: &[],
                require_tpm_floor: false,
                fail_trial_before_confirm: false,
            });
            const AB_DEFAULT: bool = true;
        }

        let mut selection = BuildSelection {
            payload: Some("uefi".into()),
            options: [
                ("blobs".into(), "/tmp/siena-blobs".into()),
                ("dev-keys".into(), "true".into()),
            ]
            .into(),
            ..Default::default()
        };
        selection.manifests.board = serde_json::json!({
            "board": "tyan-s8040",
            "amd": {
                "soc": "siena",
                "apcb": { "entries": [{ "type": 0x68, "file": "tyan-s8040/apcb.bin" }] },
            },
        });
        selection.manifests.platform = serde_json::json!({
            "amd": {
                "siena": {
                    "bios-directory": "Family19h",
                    "levels": 2,
                    "efs-offset": 0x2_0000,
                    "partition-end": 0x80_0000,
                    "psp": [{ "type": 1, "file": "siena/boot.bin" }],
                    "bios": [],
                    "microcode": [{ "equivalence-id": 0xaa02, "file": "siena/ucode.bin" }],
                }
            }
        });
        let options = selection.options().unwrap();
        let plan = crate::host::resolve_board::<S8040Ab>(&selection, &options).unwrap();
        assert_eq!(plan.stages, ["ro", "stage"]);
        assert_eq!(
            plan.ffs_assembly().unwrap().image_family.as_deref(),
            Some("tyan-s8040")
        );
        let efs = plan
            .images
            .iter()
            .find_map(|s| match &s.operation {
                ImageOperation::AmdEfs(a) => Some(a.as_ref()),
                _ => None,
            })
            .expect("AmdEfs step");
        assert_eq!(efs.stage_unit, "ro");
        assert_eq!(efs.ab, Some(siena_slot_layout()));
        assert_eq!(efs.reset_size, AB_RO_SIZE);
        assert_eq!(efs.reset_destination, AB_RO_DRAM_BASE);
        assert_eq!(
            efs.nvram.map(|n| (n.offset, n.size)),
            Some((AB_NVRAM_OFFSET, AB_NVRAM_SIZE))
        );
        let fstart_image_build::amd::assembly::Firmware::Packed { reset_offset, .. } =
            &efs.firmware;
        assert_eq!(*reset_offset, Some(AB_RO_FLASH_OFFSET));
        let update = plan.update.as_ref().expect("update contract");
        assert_eq!(update.firmware_guid, TEST_GUID);
        assert_eq!(update.slot_size, SIENA_AB_SLOT_SIZE);
        assert!(update.ab && update.capsule_trust_provisioned);

        let json = serde_json::to_string(&plan).unwrap();
        let back: BuildPlan = serde_json::from_str(&json).unwrap();
        back.order().unwrap();
        assert_eq!(back.update, plan.update);
    }
}
