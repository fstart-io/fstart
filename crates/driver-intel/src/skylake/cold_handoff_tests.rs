// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure E820 handoff table and reservation validation tests.
extern crate std;
use super::*;
use crate::skylake::uncore_prmrr;
use std::vec::Vec;

fn uma(mib: u8) -> UmaRequest {
    UmaRequest::from_shadow(0x10000 | u32::from(mib))
        .unwrap()
        .unwrap()
}

fn plan(dram_mib: u32, me: u8, prm: u32) -> Plan {
    memory_map::compute(
        dram_mib,
        u32::from(me),
        64,
        8,
        memory_map::Policy {
            tseg_mib: 8,
            mmio_mib: 2048,
        },
        uncore_prmrr::Request::for_test(prm),
    )
    .unwrap()
}

fn ranges(table: &E820State) -> Vec<(u64, u64, u32)> {
    table
        .entries()
        .iter()
        .map(|e| (e.addr, e.size, e.kind))
        .collect()
}
fn kind_at(table: &E820State, address: u64) -> Option<u32> {
    table
        .entries()
        .iter()
        .find(|e| e.addr <= address && address < e.addr + e.size)
        .map(|e| e.kind)
}
fn publish_synthetic(table: &mut E820State, p: &Plan, me: u8, reservations: &[Reservation]) {
    let (count, total) = stage_table(table, p, uma(me), reservations).unwrap();
    assert_eq!((table.count(), table.total_ram()), (0, 0));
    // TEST-ONLY inspection of staged geometry, not a public completion path.
    table.set_detected(count, total).unwrap();
}

#[test]
fn cold_handoff_table_protects_legacy_prmrr_alignment_graphics_and_mmio_not_physical_me_twice() {
    for prm in [0, 1, 32, 64, 128] {
        let p = plan(8192, 8, prm);
        let reservations = [
            Reservation {
                base: 0x9e000,
                size: 0x1000,
                kind: E820Kind::Reserved,
            }, // actual larger EBDA input
            Reservation {
                base: 0x200000,
                size: 0x10000,
                kind: E820Kind::Reserved,
            }, // live boot allocation
            Reservation {
                base: 0x300000,
                size: 0x2000,
                kind: E820Kind::Acpi,
            },
            Reservation {
                base: FOUR_GIB + 0x1000,
                size: 0x1000,
                kind: E820Kind::Nvs,
            },
        ];
        let mut table = E820State::new();
        publish_synthetic(&mut table, &p, 8, &reservations);
        let low = low_top(&p, uma(8)).unwrap();
        for address in [
            0,
            0x9e000,
            0x9f000,
            0xa0000,
            0xf0000,
            0x200000,
            low,
            p.tseg,
            p.bgsm,
            p.bdsm,
            p.tolud - 1,
        ] {
            assert_eq!(
                kind_at(&table, address),
                Some(E820Kind::Reserved as u32),
                "address {address:x}, PRMRR {prm}"
            );
        }
        // The PCI hole is the host bridge window, not a reservation.
        for address in [p.tolud, FOUR_GIB - 1] {
            assert_eq!(kind_at(&table, address), None, "address {address:x}");
        }
        assert_eq!(kind_at(&table, 0x300000), Some(E820Kind::Acpi as u32));
        assert_eq!(
            kind_at(&table, FOUR_GIB + 0x1000),
            Some(E820Kind::Nvs as u32)
        );
        assert_eq!(
            kind_at(&table, p.me_physical_base.unwrap()),
            Some(E820Kind::Ram as u32)
        );
        assert_eq!(
            table.total_ram(),
            low - MIB + 0x9e000 - 0x1000 + p.touud - FOUR_GIB - 0x10000 - 0x2000 - 0x1000
        );
        assert_eq!(
            ranges(&table).last().unwrap().0 + ranges(&table).last().unwrap().1,
            p.touud
        );
    }
}

#[test]
fn cold_handoff_table_zero_uma_no_reclaim_and_bad_geometry() {
    let mut p = plan(2048, 0, 0);
    p.tom = p.tolud;
    p.remap_base = 0x7ffff << 20;
    p.remap_limit = 0;
    p.touud = FOUR_GIB;
    let mut table = E820State::new();
    publish_synthetic(&mut table, &p, 0, &[]);
    assert!(!table.entries().iter().any(|e| e.addr >= FOUR_GIB));
    let good = p;
    for case in 0..6 {
        p = good;
        match case {
            0 => p.tseg = 0,
            1 => p.touud = FOUR_GIB - 1,
            2 => p.tolud = FOUR_GIB,
            3 => p.bdsm = p.bgsm - 1,
            4 => p.remap_limit = p.remap_base,
            _ => p.tom = 0,
        }
        assert!(stage_table(&mut table, &p, uma(0), &[]).is_err());
        assert_eq!((table.count(), table.total_ram()), (0, 0));
    }
    assert_eq!(
        stage_table(&mut table, &good, uma(8), &[]),
        Err(ValidationError::Uma)
    );
}

#[test]
fn cold_handoff_reservation_validation_and_capacity_failure_never_publish_partial_tables() {
    let p = plan(8192, 8, 0);
    let mut table = E820State::new();
    for r in [
        Reservation {
            base: u64::MAX,
            size: 2,
            kind: E820Kind::Reserved,
        },
        Reservation {
            base: p.touud,
            size: 1,
            kind: E820Kind::Reserved,
        },
        Reservation {
            base: 0,
            size: 0,
            kind: E820Kind::Reserved,
        },
        Reservation {
            base: 0,
            size: 4096,
            kind: E820Kind::Ram,
        },
        Reservation {
            base: 0x9e000,
            size: 0x2000,
            kind: E820Kind::Nvs,
        },
        Reservation {
            base: p.tseg,
            size: 4096,
            kind: E820Kind::Acpi,
        },
        Reservation {
            base: FOUR_GIB - 4096,
            size: 8192,
            kind: E820Kind::Unusable,
        },
    ] {
        assert_eq!(
            stage_table(&mut table, &p, uma(8), &[r]),
            Err(ValidationError::Reservation(0))
        );
        assert!(table.entries().is_empty());
        assert_eq!(table.total_ram(), 0);
    }
    let r = Reservation {
        base: MIB,
        size: 4096,
        kind: E820Kind::Reserved,
    };
    assert_eq!(
        stage_table(&mut table, &p, uma(8), &[r, r]),
        Err(ValidationError::Reservation(1))
    );
    let many: Vec<_> = (0..128)
        .map(|n| Reservation {
            base: MIB + n * 8192,
            size: 4096,
            kind: E820Kind::Reserved,
        })
        .collect();
    assert!(matches!(
        stage_table(&mut table, &p, uma(8), &many),
        Err(ValidationError::Table(ServiceError::HardwareError))
    ));
    assert_eq!((table.count(), table.total_ram()), (0, 0));
}
