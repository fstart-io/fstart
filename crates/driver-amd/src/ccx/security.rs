//! Memory encryption controls: SYSCFG.SMEE and Milan's SEV-SNP reverse map
//! table (CCX_SP3 §3.9, §3.12). Both are policy and off by default.
//!
//! - SMEE: Naples' donor never sets it, Rome's always (on the BSP, then
//!   copied), Milan's per PCD. With SMEE set the OS may use the C-bit; off
//!   until SME is supported end to end.
//! - SNP: Milan's donor sizes and allocates the RMP and programs
//!   RMP_BASE/RMP_END on every thread (DXE `0x26f8`, decompiler-only); it
//!   never sets SYSCFG.SNPEn (erratum 1321's VM_HSAVE_PA rule is the OS's).

use fstart_arch::x86::cpu::amd::msr::{SYSCFG, Syscfg};
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::ccx::msr::{RMP_BASE, RMP_END};

/// The donor writes RMP_BASE/END keeping these bits.
const RMP_KEEP: u64 = 0xfff0_0000_0000_1fff;
const MIB: u64 = 1 << 20;

/// Set or clear SYSCFG.SMEE (BSP; the APs get the BSP's SYSCFG).
pub fn set_smee<M: MsrAccess + ?Sized>(cpu: &M, enable: bool) {
    let v = if enable {
        Syscfg::SMEE::SET
    } else {
        Syscfg::SMEE::CLEAR
    };
    cpu.reg(SYSCFG).modify(v);
}

/// Memory the RMP covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmpCoverage {
    /// All memory up to TOM2.
    Tom2,
    /// The first `mib` MiB (at least 16, at most TOM2).
    Limit { mib: u32 },
}

/// Milan SNP policy; `None` (the donor default) means no RMP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnpPolicy {
    pub coverage: RmpCoverage,
    /// 16-byte entries and SYSCFG.VMPLEn (else 8-byte entries).
    pub vmpl: bool,
}

/// The RMP to allocate: `size` bytes, 1 MiB aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RmpPlan {
    pub size: u64,
    pub vmpl: bool,
}

pub fn rmp_plan(policy: &SnpPolicy, tom2: u64) -> RmpPlan {
    let covered = match policy.coverage {
        RmpCoverage::Tom2 => tom2,
        RmpCoverage::Limit { mib } => (u64::from(mib) * MIB).max(16 * MIB).min(tom2),
    };
    let entry = if policy.vmpl { 16 } else { 8 };
    RmpPlan {
        size: (covered / 4096 * entry + 0x4000).next_multiple_of(MIB),
        vmpl: policy.vmpl,
    }
}

/// Program the RMP at `base` on the executing thread (BSP and every AP).
pub fn program_rmp<M: MsrAccess + ?Sized>(cpu: &M, base: u64, plan: &RmpPlan) {
    let put = |msr: Msr, v: u64| {
        let reg = cpu.reg(msr);
        reg.set((reg.get() & RMP_KEEP) | (v & !RMP_KEEP));
    };
    put(RMP_BASE, base);
    put(RMP_END, base + plan.size - 1);
    if plan.vmpl {
        cpu.reg(SYSCFG).modify(Syscfg::VMPL_EN::SET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn rmp_sizes() {
        let tom2 = 256 << 30;
        let full = rmp_plan(
            &SnpPolicy {
                coverage: RmpCoverage::Tom2,
                vmpl: false,
            },
            tom2,
        );
        assert_eq!(full.size, 513 * MIB, "512 MiB of entries + 16 KiB, rounded");
        let small = SnpPolicy {
            coverage: RmpCoverage::Limit { mib: 1 },
            vmpl: true,
        };
        assert_eq!(rmp_plan(&small, tom2).size, MIB, "at least 16 MiB covered");

        let cpu = MsrSpace::new([(RMP_END.index(), 0x1fff)]);
        program_rmp(&cpu, 0x1_0000_0000, &rmp_plan(&small, tom2));
        set_smee(&cpu, false);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(RMP_BASE.index()), 0x1_0000_0000);
        assert_eq!(cpu.get(RMP_END.index()), 0x1_000f_ffff);
        assert_eq!(cpu.get(0xc001_0010), 1 << 25);
    }
}
