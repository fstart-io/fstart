//! Intel reservation projections shared with the platform host resolver.
#[cfg(test)]
use fstart_core::layout::RegionKind;
pub use fstart_image_build::intel_plan::*;
#[cfg(test)]
use fstart_image_build::plan::Span;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fstart_core::layout::Layout;

    // Candidate capacities for validation, not a second authored board profile.
    pub(crate) fn candidate() -> IntelReservations {
        let span = |base, size| Span { base, size };
        IntelReservations {
            flash: span(0xffc00000, 0x400000),
            firmware: span(0xffe80000, 0x180000),
            bootstrap_ram: span(0x100000, 0x3ff00000),
            bootblock: StageReservation {
                image: span(0xfffc0000, 0x40000),
                writable: span(0xfef00000, 0x80000),
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
        }
    }

    #[test]
    fn stages_keep_fixed_budgets_and_distinct_wire_identities() {
        let config = candidate();
        assert_eq!(config.filesystem_capacity().unwrap(), 0x140000);
        for (index, role) in [
            IntelStage::Bootblock,
            IntelStage::Postcar,
            IntelStage::Ramstage,
        ]
        .into_iter()
        .enumerate()
        {
            let encoded = config.descriptor(role).unwrap();
            let layout = Layout::parse(encoded.as_bytes()).unwrap();
            assert_eq!(layout.stage_index(), index as u16);
            let stack = layout.region(RegionKind::Stack).unwrap();
            let writable = layout.region(RegionKind::Writable).unwrap();
            assert_eq!(stack.base + stack.size, writable.base + writable.size);
            assert_eq!(layout.region(RegionKind::Heap).is_some(), index == 2);
            for (kind, stage) in [
                (RegionKind::BootstrapPostcar, config.postcar),
                (RegionKind::BootstrapMainstage, config.ramstage),
            ] {
                let window = layout.region(kind).unwrap();
                assert_eq!(window.base, stage.image.base);
                assert_eq!(window.end(), Some(stage.writable.end().unwrap()));
            }
            assert_eq!(
                layout.region(RegionKind::BootMediaScratch).unwrap().base,
                config.scratch.base
            );
        }
    }

    #[test]
    fn rejects_colliding_lifetimes_bad_reset_mapping_and_exhausted_budgets() {
        let mut config = candidate();
        config.scratch = config.postcar.writable;
        assert!(config.validate().unwrap_err().contains("overlaps"));
        let mut config = candidate();
        config.bootblock.image.base -= 4096;
        assert!(config.validate().unwrap_err().contains("reset"));
        let mut config = candidate();
        config.ramstage.stack = config.ramstage.writable.size;
        assert!(config.validate().unwrap_err().contains("capacity"));
        let mut config = candidate();
        config.postcar.image.base = u64::MAX - 4095;
        assert!(config.validate().is_err());
        let mut config = candidate();
        config.bootstrap_ram.size = 0x4700000;
        assert!(config.ramstage.image.end().unwrap() <= config.bootstrap_ram.end().unwrap());
        assert!(config.validate().unwrap_err().contains("complete stage"));
    }
}
