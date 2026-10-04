// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-thread CCX work after coordinated pre-Rust cache restoration.
use super::{Error as CpuError, Msr, SYSCFG, TOP_MEM, TOP_MEM2};

const ROUTING_MSRS: [u32; 5] = [SYSCFG, TOP_MEM, TOP_MEM2, 0x277, 0x2ff];
const TOM2_WB: u64 = (1 << 21) | (1 << 22);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Cpu(CpuError),
    PspBase,
    Routing {
        index: u32,
        expected: u64,
        actual: u64,
    },
    CachePolicy,
}

/// Cached BSP policy, not discovery of physical PSP/cache ownership.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    routing: [u64; 5],
    psp_base: u64,
}
impl Policy {
    /// BSP before AP release. The platform must have established PAT support,
    /// exclusive access and the locked PSP BAR. Capture does not change MSRs.
    pub fn capture(msr: &mut impl Msr, psp_base: u64) -> Result<Self, Error> {
        if psp_base != u64::from(crate::picasso::nbio::PSP_MMIO_BASE) {
            return Err(Error::PspBase);
        }
        super::memory_limits(msr).map_err(Error::Cpu)?;
        let routing = ROUTING_MSRS.map(|index| msr.read(index));
        // Early setup already installs TOM2En/TOM2WB. Never copy the native C
        // tail's late disable-cache/SYSCFG update while other CPUs are running.
        if routing[0] & TOM2_WB != TOM2_WB
            || routing[0] & (1 << 23) != 0
            || routing[4] & 0xcff != 0xc00
        {
            return Err(Error::CachePolicy);
        }
        let result = Self { routing, psp_base };
        result.validate(msr)?;
        Ok(result)
    }

    /// Read-only check of the intended routing after AP restoration. This does
    /// not authenticate the complete MTRR image or physical cache semantics.
    /// Return the validated MCA capability snapshot for the later write phase.
    pub fn validate(&self, msr: &mut impl Msr) -> Result<u64, Error> {
        for (index, expected) in ROUTING_MSRS.into_iter().zip(self.routing) {
            let actual = msr.read(index);
            if actual != expected {
                return Err(Error::Routing {
                    index,
                    expected,
                    actual,
                });
            }
        }
        let capability = msr.read(0x179);
        let banks = (capability & 0xff) as u8;
        if !(17..=32).contains(&banks) {
            return Err(Error::Cpu(CpuError::McaBanks(banks)));
        }
        Ok(capability)
    }

    /// All logical CPUs, serialized by the platform (including SMT siblings).
    /// Cache/routing restoration and microcode update must already be complete.
    /// BSP early reset/MCA setup precedes MP; APs still need reset policy here.
    /// Asynchronous exclusion and applicability of shared non-cache MSRs are
    /// caller obligations. Errors are pre-write; a hardware fault is terminal.
    pub fn initialize(&self, msr: &mut impl Msr, bsp: bool, primary: bool) -> Result<(), Error> {
        let capability = self.validate(msr)?;
        if !bsp {
            super::reset_registers(msr);
        }
        super::enable_ecam(msr);
        msr.write(super::PSP_ADDRESS, self.psp_base);
        // ACPI_IO_BASE=0x400, CSTATE_CONTROL=+0x10; not CSTATE_CONFIG.
        msr.write(0xc0010073, 0x410);
        super::mca_init_with_cap(msr, capability).map_err(Error::Cpu)?;
        super::per_cpu_tail(msr, primary);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{collections::BTreeMap, vec::Vec};

    struct Cpu {
        values: BTreeMap<u32, u64>,
        writes: Vec<(u32, u64)>,
    }
    impl Cpu {
        fn new() -> Self {
            Self {
                values: [
                    (SYSCFG, 0x1f << 18),
                    (TOP_MEM, 0x80000000),
                    (TOP_MEM2, 0x140000000),
                    (0x277, 0x0007040600070406),
                    (0x2ff, 0xc00),
                    (0x179, 23),
                ]
                .into_iter()
                .collect(),
                writes: Vec::new(),
            }
        }
    }
    impl Msr for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            if index == 0x179 {
                assert!(
                    self.writes.is_empty(),
                    "MCA capability must precede policy writes"
                );
            }
            *self.values.get(&index).unwrap_or(&0)
        }
        fn write(&mut self, index: u32, value: u64) {
            self.values.insert(index, value);
            self.writes.push((index, value));
        }
    }
    #[test]
    fn routing_and_mca_rejections_precede_all_writes() {
        let mut cpu = Cpu::new();
        let policy = Policy::capture(&mut cpu, 0xfd100000).unwrap();
        assert!(cpu.writes.is_empty());
        for index in ROUTING_MSRS {
            let old = cpu.values[&index];
            cpu.values.insert(index, old ^ 1);
            assert!(matches!(
                policy.initialize(&mut cpu, false, true),
                Err(Error::Routing { .. })
            ));
            assert!(cpu.writes.is_empty());
            cpu.values.insert(index, old);
        }
        cpu.values.insert(0x179, 16);
        assert_eq!(
            policy.initialize(&mut cpu, false, true),
            Err(Error::Cpu(CpuError::McaBanks(16)))
        );
        assert!(cpu.writes.is_empty());
    }
    #[test]
    fn ap_tail_uses_cached_psp_ecam_and_cstate_without_cache_policy_writes() {
        for primary in [false, true] {
            let mut cpu = Cpu::new();
            let policy = Policy::capture(&mut cpu, 0xfd100000).unwrap();
            policy.initialize(&mut cpu, false, primary).unwrap();
            assert!(cpu.writes.contains(&(super::super::MMCONFIG, 0xf8000019)));
            assert!(
                cpu.writes
                    .contains(&(super::super::PSP_ADDRESS, 0xfd100000))
            );
            assert!(cpu.writes.contains(&(0xc0010073, 0x410)));
            assert!(
                !cpu.writes
                    .iter()
                    .any(|(index, _)| ROUTING_MSRS.contains(index))
            );
            assert_eq!(
                cpu.writes
                    .iter()
                    .filter(|(index, _)| *index == 0xc0011076)
                    .count(),
                if primary { 21 } else { 0 }
            );
        }
        let mut cpu = Cpu::new();
        let policy = Policy::capture(&mut cpu, 0xfd100000).unwrap();
        policy.initialize(&mut cpu, true, true).unwrap();
        assert!(!cpu.writes.iter().any(|(index, _)| *index == 0xc0010074)); // BSP reset was early
    }
}
