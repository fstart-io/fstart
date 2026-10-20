// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-thread CCX work after coordinated pre-Rust cache restoration.
use super::Error as CpuError;
use crate::fam17h::msr::{CSTATE_BASE_ADDRESS, PSP_ADDRESS};
use fstart_arch::x86::cpu::{
    amd::msr::{SYSCFG, TOP_MEM, TOP_MEM2},
    msr::{MCG_CAP, MTRR_DEF_TYPE, MsrAccess, PAT},
};
use tock_registers::interfaces::{Readable, Writeable};

const ROUTING_MSRS: [u32; 5] = [
    SYSCFG.index(),
    TOP_MEM.index(),
    TOP_MEM2.index(),
    PAT.index(),
    MTRR_DEF_TYPE.index(),
];
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
    pub fn capture(cpu: &impl MsrAccess, psp_base: u64) -> Result<Self, Error> {
        if psp_base != u64::from(crate::picasso::nbio::PSP_MMIO_BASE) {
            return Err(Error::PspBase);
        }
        super::memory_limits(cpu).map_err(Error::Cpu)?;
        let routing = ROUTING_MSRS.map(|index| cpu.read(index));
        // Early setup already installs TOM2En/TOM2WB. Never copy the native C
        // tail's late disable-cache/SYSCFG update while other CPUs are running.
        if routing[0] & TOM2_WB != TOM2_WB
            || routing[0] & (1 << 23) != 0
            || routing[4] & 0xcff != 0xc00
        {
            return Err(Error::CachePolicy);
        }
        let result = Self { routing, psp_base };
        result.validate(cpu)?;
        Ok(result)
    }

    /// Read-only check of the intended routing after AP restoration. This does
    /// not authenticate the complete MTRR image or physical cache semantics.
    /// Return the validated MCA capability snapshot for the later write phase.
    pub fn validate(&self, cpu: &impl MsrAccess) -> Result<u64, Error> {
        for (index, expected) in ROUTING_MSRS.into_iter().zip(self.routing) {
            let actual = cpu.read(index);
            if actual != expected {
                return Err(Error::Routing {
                    index,
                    expected,
                    actual,
                });
            }
        }
        let capability = cpu.reg(MCG_CAP).get();
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
    pub fn initialize(&self, cpu: &impl MsrAccess, bsp: bool, primary: bool) -> Result<(), Error> {
        let capability = self.validate(cpu)?;
        if !bsp {
            super::reset_registers(cpu);
        }
        super::enable_ecam(cpu);
        cpu.reg(PSP_ADDRESS).set(self.psp_base);
        // ACPI_IO_BASE=0x400, CSTATE_CONTROL=+0x10; not CSTATE_CONFIG.
        cpu.reg(CSTATE_BASE_ADDRESS).set(0x410);
        super::mca_init_with_cap(cpu, capability).map_err(Error::Cpu)?;
        super::per_cpu_tail(cpu, primary);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::{MsrModel, MsrSpace};

    /// Plain registers that check the MCA capability is read before any write.
    struct Cpu(MsrSpace);
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            if index == MCG_CAP.index() {
                assert!(
                    self.0.writes.is_empty(),
                    "MCA capability must precede policy writes"
                );
            }
            self.0.read(index)
        }
        fn write(&mut self, index: u32, value: u64) {
            self.0.write(index, value)
        }
    }
    fn cpu() -> RefCell<Cpu> {
        RefCell::new(Cpu(MsrSpace::new([
            (SYSCFG.index(), 0x1f << 18),
            (TOP_MEM.index(), 0x80000000),
            (TOP_MEM2.index(), 0x140000000),
            (PAT.index(), 0x0007040600070406),
            (MTRR_DEF_TYPE.index(), 0xc00),
            (MCG_CAP.index(), 23),
        ])
        .into_inner()))
    }
    fn writes(cpu: &RefCell<Cpu>) -> std::vec::Vec<(u32, u64)> {
        cpu.borrow().0.writes.clone()
    }
    fn set(cpu: &RefCell<Cpu>, index: u32, value: u64) {
        cpu.borrow_mut().0.values.insert(index, value);
    }
    #[test]
    fn routing_and_mca_rejections_precede_all_writes() {
        let cpu = cpu();
        let policy = Policy::capture(&cpu, 0xfd100000).unwrap();
        assert!(writes(&cpu).is_empty());
        for index in ROUTING_MSRS {
            let old = cpu.borrow().0.get(index);
            set(&cpu, index, old ^ 1);
            assert!(matches!(
                policy.initialize(&cpu, false, true),
                Err(Error::Routing { .. })
            ));
            assert!(writes(&cpu).is_empty());
            set(&cpu, index, old);
        }
        set(&cpu, MCG_CAP.index(), 16);
        assert_eq!(
            policy.initialize(&cpu, false, true),
            Err(Error::Cpu(CpuError::McaBanks(16)))
        );
        assert!(writes(&cpu).is_empty());
    }
    #[test]
    fn ap_tail_uses_cached_psp_ecam_and_cstate_without_cache_policy_writes() {
        use crate::fam17h::msr::DPM_WAC_INDEX;
        use fstart_arch::x86::cpu::amd::msr::MMIO_CFG_BASE;
        for primary in [false, true] {
            let cpu = cpu();
            let policy = Policy::capture(&cpu, 0xfd100000).unwrap();
            policy.initialize(&cpu, false, primary).unwrap();
            let writes = writes(&cpu);
            assert!(writes.contains(&(MMIO_CFG_BASE.index(), 0xf8000019)));
            assert!(writes.contains(&(PSP_ADDRESS.index(), 0xfd100000)));
            assert!(writes.contains(&(CSTATE_BASE_ADDRESS.index(), 0x410)));
            assert!(!writes.iter().any(|(index, _)| ROUTING_MSRS.contains(index)));
            assert_eq!(
                writes
                    .iter()
                    .filter(|(index, _)| *index == DPM_WAC_INDEX.index())
                    .count(),
                if primary { 21 } else { 0 }
            );
        }
        let cpu = cpu();
        let policy = Policy::capture(&cpu, 0xfd100000).unwrap();
        policy.initialize(&cpu, true, true).unwrap();
        assert!(!writes(&cpu).iter().any(|(index, _)| *index == 0xc0010074)); // BSP reset was early
    }
}
