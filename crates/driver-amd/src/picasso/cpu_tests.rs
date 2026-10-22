// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::{
    cache::{self, CacheControl, Plan},
    cpu::{self, Msr, Silicon},
};
use std::{collections::BTreeMap, vec::Vec};

#[derive(Default)]
struct Cpu {
    registers: BTreeMap<u32, u64>,
    writes: Vec<(u32, u64)>,
    cache: Vec<&'static str>,
}
impl Msr for Cpu {
    fn read(&mut self, index: u32) -> u64 {
        *self.registers.get(&index).unwrap_or(&0)
    }
    fn write(&mut self, index: u32, value: u64) {
        self.registers.insert(index, value);
        self.writes.push((index, value));
    }
}
impl CacheControl for Cpu {
    fn disable(&mut self) {
        self.cache.push("disable");
    }
    fn flush(&mut self) {
        self.cache.push("flush");
    }
    fn enable(&mut self) {
        self.cache.push("enable");
    }
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
    let mut cpu = Cpu::default();
    cpu.registers.insert(0x179, 16);
    assert_eq!(cpu::mca_init(&mut cpu), Err(cpu::Error::McaBanks(16)));
    assert!(cpu.writes.is_empty());
    cpu.registers.insert(0x179, 23);
    cpu.registers.insert(0xc0002004, 6 | (1 << 33));
    cpu::mca_init(&mut cpu).unwrap();
    assert_eq!(cpu.registers[&0xc0002004], 6 | (1 << 32) | (1 << 34));
    assert_eq!(cpu.registers[&0xc0010400] & 0x600, 0x600);
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
    let mut cpu = Cpu::default();
    cpu.registers.insert(0xfe, 8); // fixed MTRRs absent
    assert_eq!(
        cache::install(&mut cpu, &plan),
        Err(cache::Error::FixedUnsupported)
    );
    assert!(cpu.cache.is_empty() && cpu.writes.is_empty());
    cpu.registers.insert(0xfe, 0x108);
    cache::install(&mut cpu, &plan).unwrap();
    assert_eq!(cpu.cache, ["disable", "flush", "flush", "enable"]);
    assert_eq!(cpu.registers[&0x259], 0);
    assert_eq!(cpu.registers[&0x268], 0x1e1e1e1e1e1e1e1e);
    assert_eq!(cpu.registers[&cpu::SYSCFG] & (0x1f << 18), 0x1f << 18);
    assert_eq!(cpu.registers[&0x2ff] & 0xfff, 0xc00);
    assert_eq!(cpu.registers[&0x20f], 0); // unused pair 7 disabled
}
