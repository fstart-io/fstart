// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::{
    cache::{self, Plan},
    cpu::{self, Silicon},
};
use crate::fam17h::{
    cache::testing::{CACHE, DISABLE, ENABLE, FLUSH},
    ccx::Thread,
};
use core::cell::RefCell;
use fstart_arch::x86::cpu::{
    amd::msr::SYSCFG,
    msr::{
        MCG_CAP, MTRR_CAP, MTRR_DEF_TYPE,
        testing::{MsrSpace, fingerprint},
    },
};
use std::vec::Vec;

type Cpu = RefCell<MsrSpace>;
fn set(cpu: &Cpu, index: u32, value: u64) {
    cpu.borrow_mut().values.insert(index, value);
}
fn get(cpu: &Cpu, index: u32) -> u64 {
    cpu.borrow().get(index)
}
type Log = Vec<(u32, u64)>;
const BSP: Thread = Thread::from_cpuid(0, 1 << 8);
/// The MSR writes and the cache operations.
fn writes(cpu: &Cpu) -> (Log, Log) {
    cpu.borrow()
        .writes
        .iter()
        .partition(|(index, _)| *index != CACHE)
}
#[test]
fn cpuid_silicon_and_reduced_io_are_not_conflated() {
    let pco = cpu::Identity::from_cpuid(0x00810f80, 0, 0).unwrap();
    assert_eq!(pco.silicon, Silicon::Picasso);
    assert!(!pco.reduced_io && !pco.raven2_cpuid);
    let dali = cpu::Identity::from_cpuid(0x00810f80, 0, 1 << 31).unwrap();
    assert!(dali.reduced_io && !dali.raven2_cpuid);
    let mystery_rv2 = cpu::Identity::from_cpuid(0x00810f80, 0, 1 << 30).unwrap();
    assert_eq!(mystery_rv2.silicon, Silicon::Raven2);
    assert!(!mystery_rv2.raven2_cpuid);
    let rv2 = cpu::Identity::from_cpuid(0x00820f01, 3 << 28, 0).unwrap();
    assert_eq!(rv2.silicon, Silicon::Raven2);
    assert!(rv2.reduced_io && rv2.raven2_cpuid);
    assert!(cpu::Identity::from_cpuid(0x000906a0, 0, 0).is_err());
}
#[test]
fn mca_invalid_geometry_fails_before_any_msr_write() {
    let cpu = Cpu::default();
    set(&cpu, MCG_CAP.index(), 16);
    assert_eq!(cpu::mca_init(&cpu, BSP), Err(cpu::Error::McaBanks(16)));
    assert!(cpu.borrow().writes.is_empty());
    set(&cpu, MCG_CAP.index(), 23);
    set(&cpu, 0xc0002004, 6 | (1 << 33));
    cpu::mca_init(&cpu, BSP).unwrap();
    assert_eq!(get(&cpu, 0xc0002004), 6 | (1 << 32) | (1 << 34));
    assert_eq!(get(&cpu, 0xc0010400) & 0x600, 0x600);
}
#[test]
fn cache_plan_covers_only_dram_and_flash_and_fails_before_disabling_cache() {
    let plan = Plan::new(0xd0000000, 40, 8).unwrap();
    assert_eq!(plan.ranges().len(), 4);
    assert_eq!(plan.ranges()[0].size, 0x80000000);
    assert_eq!(plan.ranges()[1].base, 0x80000000);
    assert_eq!(plan.ranges()[2].base + plan.ranges()[2].size, 0xd0000000);
    assert_eq!(plan.ranges()[3].kind, 5);
    for range in plan.ranges() {
        assert!(range.size.is_power_of_two());
        assert_eq!(range.base & (range.size - 1), 0);
    }
    assert_eq!(plan.encoded(3), Some((0xff000005, 0xffff000800)));
    assert!(matches!(
        Plan::new(0xd0000000, 40, 3),
        Err(cache::Error::NoMtrrs)
    ));
    let cpu = Cpu::default();
    set(&cpu, MTRR_CAP.index(), 8); // fixed MTRRs absent
    assert_eq!(
        cache::install(&cpu, &plan),
        Err(cache::Error::FixedUnsupported)
    );
    assert!(cpu.borrow().writes.is_empty());
    set(&cpu, MTRR_CAP.index(), 0x108);
    cache::install(&cpu, &plan).unwrap();
    assert_eq!(writes(&cpu).1, [DISABLE, FLUSH, FLUSH, ENABLE]);
    assert_eq!(get(&cpu, 0x259), 0);
    assert_eq!(get(&cpu, 0x268), 0x1e1e1e1e1e1e1e1e);
    assert_eq!(get(&cpu, SYSCFG.index()) & (0x1f << 18), 0x1f << 18);
    assert_eq!(get(&cpu, 0x2ff) & 0xfff, 0xc00);
    assert_eq!(get(&cpu, 0x20f), 0); // unused pair 7 disabled
}
#[test]
fn bsp_mca_and_cache_write_sequences_are_unchanged() {
    // Pinned on the code before the typed MSR rework.
    let cpu = Cpu::default();
    set(&cpu, MCG_CAP.index(), 23);
    set(&cpu, SYSCFG.index(), (1 << 23) | (1 << 21));
    for bank in 0..23u32 {
        set(
            &cpu,
            0xc0002004 + bank * 16,
            u64::from(bank % 8) | (1 << 33),
        );
        set(&cpu, 0xc0002003 + bank * 16, 0xff00_0001);
    }
    cpu::bsp_init(&cpu);
    cpu::mca_init(&cpu, BSP).unwrap();
    let (msrs, _) = writes(&cpu);
    assert_eq!(msrs.len(), 79);
    assert_eq!(fingerprint(&msrs), 0x5636fcd0cfab9d07);

    let cpu = Cpu::default();
    set(&cpu, MTRR_CAP.index(), 0x108);
    set(&cpu, SYSCFG.index(), (1 << 21) | (1 << 18));
    set(&cpu, MTRR_DEF_TYPE.index(), 0x1234_0000_0000_0000);
    cache::install(&cpu, &Plan::new(0xd0000000, 40, 8).unwrap()).unwrap();
    let (msrs, _) = writes(&cpu);
    assert_eq!(msrs.len(), 39);
    assert_eq!(fingerprint(&msrs), 0x2489c2a1ba9209e);
}
