// SPDX-License-Identifier: MIT OR Apache-2.0
//! CCX initialization of the vendor AmdCcxZenZpPei/Dxe (sp4r2 report CCX_ZP).
//!
//! The BSP applies the reset register table before MP
//! init ([`bsp_init`]); every CPU, BSP included, runs [`cpu_init`] during MP
//! init. Data that needs SMN (PSP BARs, CAC weights) is read by the BSP
//! before MP init and handed to every CPU in a [`CpuPolicy`], since the
//! APs can't use the SMN index/data pair in parallel.
use super::{ECAM, ECAM_BUSES, MAX_DIES, Revision, ccx_tables::MSR_TABLE};
use crate::fam17h::{
    cache::CacheControl,
    msr::{
        self, BU_CFG, BuCfg, CPU_WDT_CFG, CSTATE_BASE_ADDRESS, CSTATE_CONFIG, DPM_CFG,
        DPM_WAC_DATA, DPM_WAC_INDEX, DpmCfg, MCA_INTR_CFG, McaConfig, McaIntrCfg, McaMisc,
        MsrUpdate, PSP_ADDRESS, PSTATE_0, mca_config, mca_ctl_mask, mca_misc0, mca_misc1,
    },
};
use fstart_arch::x86::cpu::{
    amd::msr::{SYSCFG, Syscfg},
    msr::{MCG_CAP, McgCap, MsrAccess},
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// ACPI C-state control I/O (vendor DXE PCD 0x14d).
const CSTATE_IO_BASE: u64 = 0x813;
/// Enabled, 2047 * 1.31 ms.
const CPU_WDT_2047_1_31MS: u64 = 0x9;
const CSTATE_CONFIG_CC6: u64 = 0x48_4848;

/// MCA banks [`cpu_init`] accesses: MISC1 of banks 15 and 16.
const MIN_MCA_BANKS: u8 = 17;

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

/// One record of the vendor CCX reset register table.
#[derive(Debug, Clone, Copy)]
pub struct MsrRecord {
    pub scope: Scope,
    pub msr: u32,
    pub clear: u64,
    pub set: u64,
    /// [`Revision::mask`] bits it applies to, 0: all.
    pub revisions: u8,
}

/// The executing thread's place in the topology. Initial APIC ID:
/// socket << 6 | die << 4 | complex << 3 | core * threads + thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread {
    apic_id: u8,
    threads_per_core: u8,
}
impl Thread {
    /// From CPUID(1).EBX and CPUID(8000_001E).EBX of the executing thread.
    pub const fn from_cpuid(leaf1_ebx: u32, topology_ebx: u32) -> Self {
        Self {
            apic_id: (leaf1_ebx >> 24) as u8,
            threads_per_core: ((topology_ebx >> 8) & 0xff) as u8 + 1,
        }
    }
    pub const fn apic_id(self) -> u8 {
        self.apic_id
    }
    /// The die (APIC ID [5:4]) the thread is on.
    pub const fn die(self) -> u8 {
        (self.apic_id >> 4) & 3
    }
    pub const fn core_primary(self) -> bool {
        self.apic_id.is_multiple_of(self.threads_per_core)
    }
    pub const fn complex_primary(self) -> bool {
        self.apic_id & 7 == 0
    }
    const fn die_primary(self) -> bool {
        self.apic_id & 0xf == 0
    }
}

/// The reset register table of the vendor CCX code, for this thread.
pub fn apply_msr_table(cpu: &impl MsrAccess, revision: Revision, thread: Thread) {
    MSR_TABLE
        .iter()
        .filter(|r| r.revisions == 0 || r.revisions & revision.mask() != 0)
        .filter(|r| match r.scope {
            Scope::Thread => true,
            Scope::Core => thread.core_primary(),
            Scope::Complex => thread.complex_primary(),
        })
        .for_each(|r| MsrUpdate::new(r.msr, r.clear, r.set).apply(cpu));
}

/// The BSP, before MP init.
pub fn bsp_init(cpu: &impl MsrAccess, revision: Revision, thread: Thread) {
    apply_msr_table(cpu, revision, thread);
}

/// Number of CAC weights the SMU has and the DPM weight MSRs take.
pub const CAC_WEIGHT_COUNT: usize = 21;
pub type CacWeights = [u64; CAC_WEIGHT_COUNT];

/// What every CPU needs that only the BSP can read through SMN.
#[derive(Debug, Clone, Copy)]
pub struct CpuPolicy {
    pub revision: Revision,
    /// PSP MMIO base of each die, 0 if not enabled ([`super::psp::bars`]).
    pub psp_bars: [u32; MAX_DIES],
    /// From [`super::smu::cac_weights`]; `None` if reading them failed.
    pub cac_weights: Option<CacWeights>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer MCA banks than the init accesses; nothing was written.
    McaBanks(u8),
}

/// `zeppelin_native_cpu_init`: every CPU, BSP included, after its microcode
/// update. The SMT siblings share some of the MSRs, so the platform must not
/// run this in parallel on two CPUs.
pub fn cpu_init(
    cpu: &(impl MsrAccess + CacheControl),
    policy: &CpuPolicy,
    thread: Thread,
) -> Result<(), Error> {
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u8;
    if banks < MIN_MCA_BANKS {
        return Err(Error::McaBanks(banks));
    }

    apply_msr_table(cpu, policy.revision, thread);

    // The APs start with the reset value of MMIO_CONF_BASE.
    msr::enable_ecam(cpu, ECAM, ECAM_BUSES);

    // RDRAND needs the PSP MMIO base of the core's own die.
    if let Some(&bar) = policy.psp_bars.get(usize::from(thread.die()))
        && bar != 0
    {
        cpu.reg(PSP_ADDRESS).set(bar.into());
    }

    // Memory above 4 GiB is write-back. The early cache plan usually set it
    // already; only toggle the cache when there is something to change.
    let syscfg = cpu.reg(SYSCFG).extract();
    if syscfg.is_set(Syscfg::TOM2_EN) && !syscfg.is_set(Syscfg::TOM2_WB) {
        cpu.disable();
        cpu.flush();
        cpu.reg(SYSCFG).modify(Syscfg::TOM2_WB::SET);
        cpu.enable();
    }

    mca_init(cpu, banks, thread);

    if thread.core_primary() {
        if let Some(weights) = &policy.cac_weights {
            load_cac_weights(cpu, weights);
        }
        cpu.reg(CSTATE_CONFIG).set(CSTATE_CONFIG_CC6);
        cpu.reg(BU_CFG).modify(BuCfg::COMBINE_CR0_CD::SET);
    }
    cpu.reg(CSTATE_BASE_ADDRESS).set(CSTATE_IO_BASE);
    cpu.reg(CPU_WDT_CFG).set(CPU_WDT_2047_1_31MS);
    // Rewrite P0 so the new settings take effect.
    cpu.reg(PSTATE_0).set(cpu.reg(PSTATE_0).get());

    // Memory encryption isn't set up, so leave SME disabled. With SMEE set
    // the MTRR setup would need to account for the encryption bit.
    cpu.reg(SYSCFG).modify(Syscfg::SMEE::CLEAR);
    Ok(())
}

fn mca_init(cpu: &impl MsrAccess, banks: u8, thread: Thread) {
    for bank in 0..u32::from(banks) {
        let config = cpu.reg(mca_config(bank)).extract();
        let mut value = config;
        value.modify(McaConfig::MCAX_ENABLE::CLEAR);
        if config.is_set(McaConfig::TRANSPARENT_LOGGING_SUPPORTED) {
            value.modify(McaConfig::TRANSPARENT_LOGGING_ENABLE::CLEAR);
        }
        if banks <= 32 {
            value.modify(McaConfig::MCAX_ENABLE::SET);
        }
        if config.is_set(McaConfig::DEFERRED_LOGGING_SUPPORTED) {
            value.modify(McaConfig::LOG_DEFERRED_IN_MCA_STAT::SET);
        }
        cpu.reg(mca_config(bank)).set(value.get());

        if bank != 15 && bank != 16 {
            cpu.reg(mca_misc0(bank))
                .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
        }
    }

    cpu.reg(mca_misc1(15))
        .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
    cpu.reg(mca_misc1(16))
        .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
    cpu.reg(MCA_INTR_CFG)
        .modify(McaIntrCfg::THRESHOLD_LVT_VALID::SET);
    // LS SystemReadDataErrorT0/T1.
    MsrUpdate::new(mca_ctl_mask(0).index(), 0, (1 << 9) | (1 << 10)).apply(cpu);
    // The L3 banks, once per die.
    if thread.die_primary() {
        (7..=14).for_each(|bank| MsrUpdate::new(mca_ctl_mask(bank).index(), 0, 1 << 7).apply(cpu));
    }
}

fn load_cac_weights(cpu: &impl MsrAccess, weights: &CacWeights) {
    let saved = cpu.reg(DPM_CFG).extract();
    cpu.reg(DPM_CFG).set(0);
    for (index, &weight) in (0..).zip(weights) {
        cpu.reg(DPM_WAC_INDEX).set(index);
        cpu.reg(DPM_WAC_DATA).set(weight);
    }
    let mut valid = saved;
    valid.modify(DpmCfg::CFG_WEIGHTS_VALID::SET);
    cpu.reg(DPM_CFG).set(valid.get());
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::fam17h::cache::testing::{DISABLE, ENABLE, FLUSH};
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::{
        amd::msr::MMIO_CFG_BASE,
        msr::testing::{MsrSpace, fingerprint},
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
        apply_msr_table(&cpu, Revision::B2, Thread::from_cpuid(1 << 24, 1 << 8));
        assert_eq!(cpu.borrow().writes.len(), 3);
        // The first thread of the second complex of die 1 gets every B2 record.
        let cpu = self::cpu([]);
        let thread = Thread::from_cpuid(0x18 << 24, 1 << 8);
        assert!(thread.complex_primary() && thread.core_primary() && thread.die() == 1);
        apply_msr_table(&cpu, Revision::B2, thread);
        let applicable = MSR_TABLE
            .iter()
            .filter(|r| r.revisions == 0 || r.revisions & Revision::B2.mask() != 0)
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
