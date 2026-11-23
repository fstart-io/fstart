// SPDX-License-Identifier: MIT OR Apache-2.0
//! SYNTHETIC SPD/map/ECAM/FIFO inputs only. Private DID/table tests do not
//! demonstrate successful public training. No CPUID/RDTSC/RDRAND or hardware I/O.
extern crate std;
use super::*;
use crate::{
    skylake::{
        cold_training::tests::input,
        early_sa::{EcamConfig, GDXC_BASE},
        registers::{
            MCHBAR_BASE,
            testing::{Image, image},
        },
        sa_registers::*,
        spd::{
            Dimm, Dimms,
            testing::{seal, sodimm},
        },
        uncore_prmrr::tests::request,
    },
    sunrise_point::{pcr::testing::ecam, pmc::MemoryBootMode},
};
use fstart_pci::PciAddress;
use std::vec::Vec;

fn uma(mib: u8) -> UmaRequest {
    UmaRequest::from_shadow(0x10000 | u32::from(mib))
        .unwrap()
        .unwrap()
}

// This exercises the real map programmer on ordinary RAM-backed registers,
// not physical config space and not trained RAM. Never certifies usability.
fn map(sa: &EcamDevice, i: &Image, dimms: &Dimms<'_>, me: u8, prm: u16) -> ProgrammedMap {
    let ecam = EcamConfig {
        base: 0xe0000000,
        buses: 256,
    };
    sa.write32(0, 0x191f8086);
    sa.write32(8, 0x06000001);
    sa.write32(0x60, ecam.encoded_pciexbar().unwrap());
    sa.write32(0x48, MCHBAR_BASE as u32 | 1);
    sa.write32(0xe8, 8);
    sa.write16(GGC, 0x2c2); // real geometry input: 64MiB stolen, 8MiB GTT, IVD
    sa.write8(SMRAMC, 2);
    for offset in [TOM, REMAP_BASE, MESEG_BASE] {
        let address = 0x7ffffu64 << 20;
        sa.write32(offset, address as u32);
        sa.write32(offset + 4, (address >> 32) as u32);
    }
    sa.write32(BGSM, 1 << 20);
    sa.write32(TOLUD, 1 << 20);
    i.registers().gdxc_bar.set(GDXC_BASE | 1);
    let g = image();
    let gdxc = unsafe { crate::skylake::registers::GdxcRegisters::at(g.0.as_ptr() as usize) };
    unsafe {
        memory_map::program_cold(
            sa,
            i.registers(),
            gdxc,
            ecam,
            MemoryBootMode::Cold,
            dimms,
            input(0x906e9).capabilities,
            uma(me),
            memory_map::Policy {
                tseg_mib: 8,
                mmio_mib: 2048,
            },
            request(prm),
        )
    }
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
        let raw = sodimm();
        let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
        let (bus, _backing) = ecam();
        let sa = bus.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        let i = image();
        let m = map(&sa, &i, &dimms, 8, prm);
        let p = m.plan();
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
        publish_synthetic(&mut table, p, 8, &reservations);
        let low = low_top(p, uma(8)).unwrap();
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
    let mut raw = sodimm();
    raw[4] = 0x83;
    raw[5] = 0x11;
    seal(&mut raw);
    let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
    let (bus, _backing) = ecam();
    let sa = bus.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    let i = image();
    let m = map(&sa, &i, &dimms, 0, 0);
    // Graphics reduce usable decode below TOM, so this fixture still reclaims.
    let mut p = *m.plan();
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
    let raw = sodimm();
    let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
    let (bus, _backing) = ecam();
    let sa = bus.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    let i = image();
    let m = map(&sa, &i, &dimms, 8, 0);
    let mut table = E820State::new();
    for r in [
        Reservation {
            base: u64::MAX,
            size: 2,
            kind: E820Kind::Reserved,
        },
        Reservation {
            base: m.plan().touud,
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
            base: m.plan().tseg,
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
            stage_table(&mut table, m.plan(), uma(8), &[r]),
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
        stage_table(&mut table, m.plan(), uma(8), &[r, r]),
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
        stage_table(&mut table, m.plan(), uma(8), &many),
        Err(ValidationError::Table(ServiceError::HardwareError))
    ));
    assert_eq!((table.count(), table.total_ram()), (0, 0));
}
