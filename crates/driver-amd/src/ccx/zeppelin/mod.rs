// SPDX-License-Identifier: MIT OR Apache-2.0
//! CCX initialization of the vendor AmdCcxZenZpPei/Dxe (sp4r2 report CCX_ZP).
//!
//! The BSP applies the reset register table before MP
//! init ([`bsp_init`]); every CPU, BSP included, runs [`cpu_init`] during MP
//! init. Data that needs SMN (PSP BARs, CAC weights) is read by the BSP
//! before MP init and handed to every CPU in a [`CpuPolicy`], since the
//! APs can't use the SMN index/data pair in parallel.
use super::{
    self as ccx, CacWeights, Ccx, Error, Inputs, L3Owner, Mca, Thread,
    cache::CacheControl,
    msr::{self, DpmCfg, MsrUpdate},
};
use crate::zeppelin::{ECAM, ECAM_BUSES, MAX_DIES, Revision};
use fstart_arch::x86::cpu::msr::MsrAccess;

mod tables;
use tables::MSR_TABLE;

/// The Zeppelin parameters of the Family 17h CCX init.
const CCX: Ccx = Ccx {
    mca: Mca {
        // MISC1 of banks 15 and 16.
        banks: 17..=u8::MAX,
        no_misc0: 15..=16,
        // LS SystemReadDataErrorT0/T1.
        ctl_masks: &[(0, (1 << 9) | (1 << 10))],
        l3_banks: 7..=14,
        l3_owner: L3Owner::DiePrimary,
    },
    ecam: ECAM,
    ecam_buses: ECAM_BUSES,
    // ACPI C-state control I/O (vendor DXE PCD 0x14d).
    cstate_io_base: 0x813,
    // Enabled, 2047 * 1.31 ms.
    watchdog: Some(0x9),
    dpm_cfg: DpmCfg::CFG_WEIGHTS_VALID::SET.value,
    tail: &[],
};

/// Which threads apply a reset table record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every thread.
    Thread,
    /// The primary thread of each core.
    Core,
    /// The first thread of each core complex.
    Complex,
}

/// One record of the vendor CCX reset register table: the threads that
/// apply it and the [`Revision::mask`] bits it applies to (0: all).
pub type MsrRecord = MsrUpdate<(Scope, u8)>;

/// The reset register table of the vendor CCX code, for this thread.
pub fn msr_table(revision: Revision, thread: Thread) -> impl Iterator<Item = MsrUpdate> {
    let applies = move |&(scope, revisions): &(Scope, u8)| {
        (revisions == 0 || revisions & revision.mask() != 0)
            && match scope {
                Scope::Thread => true,
                Scope::Core => thread.core_primary(),
                Scope::Complex => thread.complex_primary(),
            }
    };
    MSR_TABLE
        .iter()
        .filter(move |r| applies(&r.when))
        .map(|r| r.unconditional())
}

/// The BSP, before MP init.
pub fn bsp_init(cpu: &impl MsrAccess, revision: Revision, thread: Thread) {
    msr::apply(cpu, msr_table(revision, thread));
}

/// What every CPU needs that only the BSP can read through SMN.
#[derive(Debug, Clone, Copy)]
pub struct CpuPolicy {
    pub revision: Revision,
    /// PSP MMIO base of each die, 0 if not enabled ([`crate::psp::bars`]).
    pub psp_bars: [u32; MAX_DIES],
    /// From [`crate::smu::v9::cac_weights`]; `None` if reading them failed.
    pub cac_weights: Option<CacWeights>,
}

/// `zeppelin_native_cpu_init`: every CPU, BSP included, after its microcode
/// update. The SMT siblings share some of the MSRs, so the platform must not
/// run this in parallel on two CPUs.
pub fn cpu_init(
    cpu: &(impl MsrAccess + CacheControl),
    policy: &CpuPolicy,
    thread: Thread,
) -> Result<(), Error> {
    let inputs = Inputs {
        // RDRAND needs the PSP MMIO base of the core's own die.
        psp_bar: policy
            .psp_bars
            .get(usize::from(thread.die()))
            .filter(|&&bar| bar != 0)
            .map(|&bar| bar.into()),
        cac_weights: policy.cac_weights.as_ref(),
    };
    ccx::cpu_init(
        cpu,
        &CCX,
        thread,
        &inputs,
        msr_table(policy.revision, thread),
    )
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::ccx::{
        cache::testing::{DISABLE, ENABLE, FLUSH},
        msr::{
            CSTATE_BASE_ADDRESS, CSTATE_CONFIG, DPM_CFG, DPM_WAC_DATA, PSP_ADDRESS, PSTATE_0,
            mca_config, mca_ctl_mask, mca_misc0, mca_misc1,
        },
    };
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::{
        amd::msr::{MMIO_CFG_BASE, SYSCFG},
        msr::{
            MCG_CAP,
            testing::{MsrSpace, fingerprint},
        },
    };
    use std::vec::Vec;

    type Cpu = RefCell<MsrSpace>;
    fn cpu(values: impl IntoIterator<Item = (u32, u64)>) -> Cpu {
        MsrSpace::new(values)
    }
    fn written(cpu: &Cpu, index: u32) -> bool {
        cpu.borrow().written(index)
    }
    fn get(cpu: &Cpu, index: u32) -> u64 {
        cpu.borrow().get(index)
    }

    #[test]
    fn table_scope_and_revision_filtering() {
        // Thread 1 of core 0 on B2: only the per-thread records.
        let cpu = cpu([]);
        bsp_init(&cpu, Revision::B2, Thread::from_cpuid(1 << 24, 1 << 8));
        assert_eq!(cpu.borrow().writes.len(), 3);
        // The first thread of the second complex of die 1 gets every B2 record.
        let cpu = self::cpu([]);
        let thread = Thread::from_cpuid(0x18 << 24, 1 << 8);
        assert!(thread.complex_primary() && thread.core_primary() && thread.die() == 1);
        bsp_init(&cpu, Revision::B2, thread);
        let applicable = MSR_TABLE
            .iter()
            .filter(|r| r.when.1 == 0 || r.when.1 & Revision::B2.mask() != 0)
            .count();
        assert_eq!(cpu.borrow().writes.len(), applicable);
        assert_eq!(get(&cpu, 0xc0011093), 0x480060); // the B1/B2 variant
        assert_eq!(get(&cpu, 0xc0011097), 0xff6);
    }

    #[test]
    fn cpu_init_uses_die_psp_bar_cac_weights_and_mca_geometry() {
        let cpu = self::cpu([
            (MCG_CAP.index(), 23),
            (SYSCFG.index(), (1 << 21) | (1 << 23)),
            (DPM_CFG.index(), 0x5),
            (mca_config(2).index(), 0b111 | (1 << 33)),
            (mca_misc0(15).index(), 0xff00_0001),
        ]);
        let policy = CpuPolicy {
            revision: Revision::B2,
            psp_bars: [0xe010_0000, 0xd010_0000],
            cac_weights: Some(core::array::from_fn(|i| i as u64 * 3)),
        };
        // Primary thread of core 0, die 1.
        cpu_init(&cpu, &policy, Thread::from_cpuid(0x10 << 24, 1 << 8)).unwrap();
        assert_eq!(get(&cpu, PSP_ADDRESS.index()), 0xd010_0000);
        assert_eq!(get(&cpu, MMIO_CFG_BASE.index()), 0xf000_001d);
        assert_eq!(get(&cpu, SYSCFG.index()), 1 << 21 | 1 << 22);
        let cache: Vec<_> = cpu
            .borrow()
            .writes
            .iter()
            .copied()
            .filter(|&op| [DISABLE, FLUSH, ENABLE].contains(&op))
            .collect();
        assert_eq!(cache, [DISABLE, FLUSH, ENABLE]);
        assert_eq!(
            get(&cpu, mca_config(2).index()),
            0b111 | (1 << 32) | (1 << 34)
        );
        assert_eq!(get(&cpu, mca_misc0(15).index()), 0xff00_0001);
        assert!(written(&cpu, mca_ctl_mask(14).index()));
        assert_eq!(get(&cpu, DPM_CFG.index()), 0x5 | (1 << 63));
        let weights: Vec<_> = cpu
            .borrow()
            .writes
            .iter()
            .filter(|&&(i, _)| i == DPM_WAC_DATA.index())
            .map(|&(_, v)| v)
            .collect();
        assert_eq!(weights, policy.cac_weights.unwrap());

        // The SMT sibling skips the per-core MSRs; die 0 without a PSP BAR.
        let cpu = self::cpu([
            (MCG_CAP.index(), 23),
            (SYSCFG.index(), (1 << 21) | (1 << 22)),
        ]);
        let policy = CpuPolicy {
            psp_bars: [0, 0],
            ..policy
        };
        cpu_init(&cpu, &policy, Thread::from_cpuid(0x3 << 24, 1 << 8)).unwrap();
        assert!(!written(&cpu, PSP_ADDRESS.index()));
        assert!(!cpu.borrow().writes.contains(&DISABLE));
        assert!(!written(&cpu, DPM_WAC_DATA.index()) && !written(&cpu, CSTATE_CONFIG.index()));
        assert!(!written(&cpu, mca_ctl_mask(14).index()));
        assert_eq!(get(&cpu, CSTATE_BASE_ADDRESS.index()), 0x813);

        let cpu = self::cpu([(MCG_CAP.index(), 16)]);
        assert_eq!(
            cpu_init(&cpu, &policy, Thread::from_cpuid(0, 1 << 8)),
            Err(Error::McaBanks(16))
        );
        assert!(cpu.borrow().writes.is_empty());
    }

    #[test]
    fn write_sequences_are_unchanged() {
        // Pinned on the hardware-tested code before the typed MSR rework:
        // (APIC ID, revision, SYSCFG) -> cpu_init and bsp_init (length, digest).
        let cases = [
            (
                0x10,
                Revision::B2,
                (1 << 21) | (1 << 23),
                (134, 0x4329f304954015ca),
                (22, 0x38e81d7893c4f837),
            ),
            (
                0x3,
                Revision::B2,
                (1 << 21) | (1 << 22),
                (57, 0xd1e292fa4458145c),
                (3, 0x1edfe4c9b53c7328),
            ),
            (
                0x18,
                Revision::B1,
                1 << 21,
                (124, 0x79490a5d34fc463),
                (20, 0x49fbbac9039bf8ea),
            ),
            (
                0,
                Revision::A0,
                0,
                (130, 0x92cedecea808ffcf),
                (22, 0xf039cdd47bebd5ed),
            ),
        ];
        for (apic, revision, syscfg, init, bsp) in cases {
            let thread = Thread::from_cpuid(apic << 24, 1 << 8);
            let cpu = self::cpu(
                [
                    (MCG_CAP.index(), 23),
                    (SYSCFG.index(), syscfg),
                    (DPM_CFG.index(), 0x5),
                    (PSTATE_0.index(), 0x8000_0000_1234_5678),
                ]
                .into_iter()
                .chain((0..23).flat_map(|bank| {
                    [
                        (mca_config(bank).index(), u64::from(bank % 8) | (1 << 33)),
                        (mca_misc0(bank).index(), 0xff00_0001 + u64::from(bank)),
                        (mca_misc1(bank).index(), 0xff00_0002),
                        (mca_ctl_mask(bank).index(), 0x100),
                    ]
                })),
            );
            let policy = CpuPolicy {
                revision,
                psp_bars: [0xe010_0000, 0xd010_0000],
                cac_weights: Some(core::array::from_fn(|i| i as u64 * 3 + 1)),
            };
            cpu_init(&cpu, &policy, thread).unwrap();
            let writes = &cpu.borrow().writes;
            assert_eq!(
                (writes.len(), fingerprint(writes)),
                init,
                "cpu_init {apic:#x}"
            );
            let cpu = self::cpu([]);
            bsp_init(&cpu, revision, thread);
            let writes = &cpu.borrow().writes;
            assert_eq!(
                (writes.len(), fingerprint(writes)),
                bsp,
                "bsp_init {apic:#x}"
            );
        }
    }
}
