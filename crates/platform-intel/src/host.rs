//! Intel family policy, evaluated on the host from the selected board's Rust facts.
extern crate std;
use crate::facts::{BoardFacts, Chipset, IntelBoardFacts};
use fstart_image_build::{
    intel_plan::{IntelReservations, IntelStage, StageReservation},
    plan::{BuildSelection, IntelPlan, IntelStagePlan, ResolvedPlan, Span},
};
use std::{format, string::ToString, vec, vec::Vec};

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: IntelBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::FACTS, selection).expect("invalid Intel board plan");
        std::println!(
            "{}",
            serde_json::to_string(&ResolvedPlan::Intel(plan)).expect("serialize Intel plan")
        );
    }
}

pub fn reservations(facts: BoardFacts) -> Result<IntelReservations, std::string::String> {
    let flash = facts.flash;
    flash.check().map_err(str::to_string)?;
    if flash.size() != facts.flash_size {
        return Err("IFD map differs from physical flash capacity".into());
    }
    let bios = flash.bios_region().ok_or("missing BIOS region")?;
    let span = |base, size| Span { base, size };
    let car = match facts.chipset {
        Chipset::Gm965Ich8 => span(0xfef00000, 0x80000),
        Chipset::I945Ich7 => span(crate::i945::I945_CAR_BASE, crate::i945::I945_CAR_SIZE),
    };
    let reservations = IntelReservations {
        flash: span(
            0x1_0000_0000 - u64::from(facts.flash_size),
            u64::from(facts.flash_size),
        ),
        firmware: span(
            flash.bios_base().ok_or("missing BIOS")?,
            u64::from(bios.size),
        ),
        bootstrap_ram: span(0x100000, 0x3ff00000),
        bootblock: StageReservation {
            image: span(flash.end() - 0x40000, 0x40000),
            writable: car,
            stack: 0x2000,
            heap: 0,
        },
        postcar: StageReservation {
            image: span(0x1000000, 0x10000),
            writable: span(0x1010000, 0x10000),
            stack: 0x2000,
            heap: 0,
        },
        ramstage: StageReservation {
            image: span(0x4000000, 0x400000),
            writable: span(0x4400000, 0xc00000),
            stack: 0x400000,
            heap: 0x200000,
        },
        low_memory: span(0, 0x100000),
        scratch: span(0x2000000, 0x1000000),
    };
    reservations.validate()?;
    Ok(reservations)
}

pub fn resolve(
    facts: BoardFacts,
    selection: BuildSelection,
) -> Result<IntelPlan, std::string::String> {
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    if !matches!(payload.as_str(), "halt" | "uefi") {
        return Err("Intel supports halt and UEFI, not direct Linux/DTB loading".into());
    }
    let stages = [
        (IntelStage::Bootblock, "bundle-bootblock"),
        (IntelStage::Postcar, "bundle-postcar"),
        (IntelStage::Ramstage, "bundle-ramstage"),
    ]
    .map(|(role, bundle)| {
        let selected_payload = if role == IntelStage::Ramstage {
            payload.as_str()
        } else {
            "halt"
        };
        let mut features = vec![bundle.into()];
        if selected_payload == "uefi" {
            features.push("payload-uefi".into());
        }
        IntelStagePlan {
            role,
            features,
            payload: selected_payload.into(),
        }
    });
    let reservations = reservations(facts)?;
    let signatures: &[&str] =
        match facts.chipset {
            Chipset::Gm965Ich8 => &[
                "06-0f-02", "06-0f-06", "06-0f-07", "06-0f-0a", "06-0f-0b", "06-0f-0d", "06-16-01",
            ],
            Chipset::I945Ich7 => return Err(
                "only GM965 microcode inputs have migrated; reservation reuse is tested separately"
                    .into(),
            ),
        };
    let plan = IntelPlan {
        reservations,
        target: "x86_64-unknown-none".into(),
        payload,
        stages,
        smm_features: vec!["bundle-smm".into()],
        ifd: facts.flash.into(),
        max_cpus: facts.max_cpus,
        microcode: signatures
            .iter()
            .map(|name| format!("intel-microcode/intel-ucode/{name}"))
            .collect::<Vec<_>>(),
        security: fstart_core::dev_security_config("keys/dev-signing.pub"),
        smm: fstart_core::SmmConfig {
            entry_points: Some(facts.max_cpus),
            stack_size: 0x400,
            ..Default::default()
        },
    };
    plan.validate()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::{
        ConstVec, IntelIfdFlashLayout, IntelIfdRegion as Kind, IntelIfdRegionConfig as Region,
    };
    const DESCRIPTOR: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    const FLASH: IntelIfdFlashLayout =
        IntelIfdFlashLayout::new(ConstVec::new(DESCRIPTOR).push(DESCRIPTOR).push(Region {
            kind: Kind::Bios,
            offset: 0x280000,
            size: 0x180000,
        }));
    const FACTS: BoardFacts = BoardFacts::new(FLASH, 0x400000, 2, Chipset::Gm965Ich8);
    #[test]
    fn platform_selects_target_payload_stage_bundles_and_smm() {
        let default = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(default.target, "x86_64-unknown-none");
        assert_eq!(default.payload, "halt");
        assert!(default.stages.iter().all(|stage| stage.payload == "halt"));
        let uefi = resolve(
            FACTS,
            BuildSelection {
                payload: Some("uefi".into()),
            },
        )
        .unwrap();
        assert_eq!(
            uefi.stages
                .iter()
                .map(|stage| stage.payload.as_str())
                .collect::<Vec<_>>(),
            ["halt", "halt", "uefi"]
        );
        assert_eq!(uefi.stages[0].features, ["bundle-bootblock"]);
        assert_eq!(uefi.stages[1].features, ["bundle-postcar"]);
        assert_eq!(uefi.stages[2].features, ["bundle-ramstage", "payload-uefi"]);
        assert_eq!(uefi.smm_features, ["bundle-smm"]);
        assert!(
            resolve(
                FACTS,
                BuildSelection {
                    payload: Some("linux".into())
                }
            )
            .is_err()
        );
    }

    #[test]
    fn bios_identity_and_capacity_follow_typed_ifd() {
        let plan = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(plan.reservations.firmware.base, 0xffe80000);
        assert_eq!(plan.reservations.firmware.size, 0x180000);
        assert_eq!(plan.reservations.filesystem_capacity().unwrap(), 0x140000);
        const LARGER_BIOS: IntelIfdFlashLayout =
            IntelIfdFlashLayout::new(ConstVec::new(DESCRIPTOR).push(DESCRIPTOR).push(Region {
                kind: Kind::Bios,
                offset: 0x200000,
                size: 0x200000,
            }));
        let changed = resolve(
            BoardFacts::new(LARGER_BIOS, 0x400000, 2, Chipset::Gm965Ich8),
            BuildSelection::default(),
        )
        .unwrap();
        assert_eq!(changed.reservations.firmware.base, 0xffe00000);
        assert_eq!(
            changed.reservations.filesystem_capacity().unwrap(),
            0x1c0000
        );
        assert_eq!(
            changed.reservations.bootblock.image.base,
            plan.reservations.bootblock.image.base
        );
    }
    #[test]
    fn i945_pairing_reuses_family_reservations_with_real_car_delta() {
        let gm = reservations(FACTS).unwrap();
        let i945 = reservations(BoardFacts {
            chipset: Chipset::I945Ich7,
            ..FACTS
        })
        .unwrap();
        assert_eq!(i945.bootblock.writable.base, crate::i945::I945_CAR_BASE);
        assert_eq!(i945.bootblock.writable.size, crate::i945::I945_CAR_SIZE);
        for role in [
            fstart_image_build::intel_plan::IntelStage::Postcar,
            fstart_image_build::intel_plan::IntelStage::Ramstage,
        ] {
            assert_eq!(
                gm.descriptor(role).unwrap().as_bytes(),
                i945.descriptor(role).unwrap().as_bytes()
            );
        }
        assert_eq!(
            gm.filesystem_capacity().unwrap(),
            i945.filesystem_capacity().unwrap()
        );
    }
}
