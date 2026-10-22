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
        self, MCA_CONFIG, MCA_MISC0, MCA_MISC1, MCG_CAP, McaConfig, Msr, PSP_ADDRESS, SYSCFG,
        Syscfg, mca_ctl_mask, mcax,
    },
};
use tock_registers::LocalRegisterCopy;

const CSTATE_BASE_ADDRESS: u32 = 0xc001_0073;
/// ACPI C-state control I/O (vendor DXE PCD 0x14d).
const CSTATE_IO_BASE: u64 = 0x813;
const CPU_WDT_CFG: u32 = 0xc001_0074;
/// Enabled, 2047 * 1.31 ms.
const CPU_WDT_2047_1_31MS: u64 = 0x9;
const CSTATE_CONFIG: u32 = 0xc001_0296;
const CSTATE_CONFIG_CC6: u64 = 0x48_4848;
const MCA_INTR_CFG: u32 = 0xc000_0410;
const PSTATE_0: u32 = 0xc001_0064;
const BU_CFG: u32 = 0xc001_1023;
/// TW_CFG::CombineCr0Cd.
const COMBINE_CR0_CD: u64 = 1 << 49;
const DPM_CFG: u32 = 0xc001_1074;
const DPM_CFG_WEIGHTS_VALID: u64 = 1 << 63;
const DPM_WAC_INDEX: u32 = 0xc001_1076;
const DPM_WAC_DATA: u32 = 0xc001_1077;

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
pub fn apply_msr_table(msr: &mut impl Msr, revision: Revision, thread: Thread) {
    MSR_TABLE
        .iter()
        .filter(|r| r.revisions == 0 || r.revisions & revision.mask() != 0)
        .filter(|r| match r.scope {
            Scope::Thread => true,
            Scope::Core => thread.core_primary(),
            Scope::Complex => thread.complex_primary(),
        })
        .for_each(|r| msr.update(r.msr, r.clear, r.set));
}

/// The BSP, before MP init.
pub fn bsp_init(msr: &mut impl Msr, revision: Revision, thread: Thread) {
    apply_msr_table(msr, revision, thread);
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
    cpu: &mut (impl Msr + CacheControl),
    policy: &CpuPolicy,
    thread: Thread,
) -> Result<(), Error> {
    let banks = (cpu.read(MCG_CAP) & 0xff) as u8;
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
        cpu.write(PSP_ADDRESS, bar.into());
    }

    // Memory above 4 GiB is write-back. The early cache plan usually set it
    // already; only toggle the cache when there is something to change.
    let syscfg = LocalRegisterCopy::<u64, Syscfg::Register>::new(cpu.read(SYSCFG));
    if syscfg.is_set(Syscfg::TOM2_EN) && !syscfg.is_set(Syscfg::TOM2_WB) {
        cpu.disable();
        cpu.flush();
        cpu.update(SYSCFG, 0, Syscfg::TOM2_WB::SET.value);
        cpu.enable();
    }

    mca_init(cpu, banks, thread);

    if thread.core_primary() {
        if let Some(weights) = &policy.cac_weights {
            load_cac_weights(cpu, weights);
        }
        cpu.write(CSTATE_CONFIG, CSTATE_CONFIG_CC6);
        cpu.update(BU_CFG, 0, COMBINE_CR0_CD);
    }
    cpu.write(CSTATE_BASE_ADDRESS, CSTATE_IO_BASE);
    cpu.write(CPU_WDT_CFG, CPU_WDT_2047_1_31MS);
    // Rewrite P0 so the new settings take effect.
    let pstate0 = cpu.read(PSTATE_0);
    cpu.write(PSTATE_0, pstate0);

    // Memory encryption isn't set up, so leave SME disabled. With SMEE set
    // the MTRR setup would need to account for the encryption bit.
    cpu.update(SYSCFG, Syscfg::SMEE::SET.mask(), 0);
    Ok(())
}

fn mca_init(msr: &mut impl Msr, banks: u8, thread: Thread) {
    for bank in 0..u32::from(banks) {
        let config =
            LocalRegisterCopy::<u64, McaConfig::Register>::new(msr.read(mcax(bank, MCA_CONFIG)));
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
        msr.write(mcax(bank, MCA_CONFIG), value.get());

        if bank != 15 && bank != 16 {
            msr.update(mcax(bank, MCA_MISC0), 0xff00_0000, 0);
        }
    }

    msr.update(mcax(15, MCA_MISC1), 0xff00_0000, 0);
    msr.update(mcax(16, MCA_MISC1), 0xff00_0000, 0);
    msr.update(MCA_INTR_CFG, 0, 1 << 3);
    // LS SystemReadDataErrorT0/T1.
    msr.update(mca_ctl_mask(0), 0, (1 << 9) | (1 << 10));
    // The L3 banks, once per die.
    if thread.die_primary() {
        (7..=14).for_each(|bank| msr.update(mca_ctl_mask(bank), 0, 1 << 7));
    }
}

fn load_cac_weights(msr: &mut impl Msr, weights: &CacWeights) {
    let saved = msr.read(DPM_CFG);
    msr.write(DPM_CFG, 0);
    for (index, &weight) in (0..).zip(weights) {
        msr.write(DPM_WAC_INDEX, index);
        msr.write(DPM_WAC_DATA, weight);
    }
    msr.write(DPM_CFG, saved | DPM_CFG_WEIGHTS_VALID);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{collections::BTreeMap, vec::Vec};

    #[derive(Default)]
    struct Cpu {
        values: BTreeMap<u32, u64>,
        writes: Vec<(u32, u64)>,
        cache_toggles: Vec<&'static str>,
    }
    impl Msr for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            self.values.get(&index).copied().unwrap_or(0)
        }
        fn write(&mut self, index: u32, value: u64) {
            self.values.insert(index, value);
            self.writes.push((index, value));
        }
    }
    impl CacheControl for Cpu {
        fn disable(&mut self) {
            self.cache_toggles.push("disable");
        }
        fn flush(&mut self) {
            self.cache_toggles.push("flush");
        }
        fn enable(&mut self) {
            self.cache_toggles.push("enable");
        }
    }
    fn written(cpu: &Cpu, index: u32) -> bool {
        cpu.writes.iter().any(|&(i, _)| i == index)
    }

    #[test]
    fn table_scope_and_revision_filtering() {
        // Thread 1 of core 0 on B2: only the per-thread records.
        let mut cpu = Cpu::default();
        apply_msr_table(&mut cpu, Revision::B2, Thread::from_cpuid(1 << 24, 1 << 8));
        assert_eq!(cpu.writes.len(), 3);
        // The first thread of the second complex of die 1 gets every B2 record.
        let mut cpu = Cpu::default();
        let thread = Thread::from_cpuid(0x18 << 24, 1 << 8);
        assert!(thread.complex_primary() && thread.core_primary() && thread.die() == 1);
        apply_msr_table(&mut cpu, Revision::B2, thread);
        let applicable = MSR_TABLE
            .iter()
            .filter(|r| r.revisions == 0 || r.revisions & Revision::B2.mask() != 0)
            .count();
        assert_eq!(cpu.writes.len(), applicable);
        assert_eq!(cpu.values[&0xc0011093], 0x480060); // the B1/B2 variant
        assert_eq!(cpu.values[&0xc0011097], 0xff6);
    }

    #[test]
    fn cpu_init_uses_die_psp_bar_cac_weights_and_mca_geometry() {
        let mut cpu = Cpu::default();
        cpu.values.insert(MCG_CAP, 23);
        cpu.values.insert(SYSCFG, (1 << 21) | (1 << 23));
        cpu.values.insert(DPM_CFG, 0x5);
        cpu.values.insert(mcax(2, MCA_CONFIG), 0b111 | (1 << 33));
        cpu.values.insert(mcax(15, MCA_MISC0), 0xff00_0001);
        let policy = CpuPolicy {
            revision: Revision::B2,
            psp_bars: [0xe010_0000, 0xd010_0000],
            cac_weights: Some(core::array::from_fn(|i| i as u64 * 3)),
        };
        // Primary thread of core 0, die 1.
        cpu_init(&mut cpu, &policy, Thread::from_cpuid(0x10 << 24, 1 << 8)).unwrap();
        assert_eq!(cpu.values[&PSP_ADDRESS], 0xd010_0000);
        assert_eq!(cpu.values[&msr::MMCONFIG], 0xf000_001d);
        assert_eq!(cpu.values[&SYSCFG], 1 << 21 | 1 << 22);
        assert_eq!(cpu.cache_toggles, ["disable", "flush", "enable"]);
        assert_eq!(
            cpu.values[&mcax(2, MCA_CONFIG)],
            0b111 | (1 << 32) | (1 << 34)
        );
        assert_eq!(cpu.values[&mcax(15, MCA_MISC0)], 0xff00_0001);
        assert!(written(&cpu, mca_ctl_mask(14)));
        assert_eq!(cpu.values[&DPM_CFG], 0x5 | DPM_CFG_WEIGHTS_VALID);
        let weights: Vec<_> = cpu
            .writes
            .iter()
            .filter(|&&(i, _)| i == DPM_WAC_DATA)
            .map(|&(_, v)| v)
            .collect();
        assert_eq!(weights, policy.cac_weights.unwrap());

        // The SMT sibling skips the per-core MSRs; die 0 without a PSP BAR.
        let mut cpu = Cpu::default();
        cpu.values.insert(MCG_CAP, 23);
        cpu.values.insert(SYSCFG, (1 << 21) | (1 << 22));
        let policy = CpuPolicy {
            psp_bars: [0, 0],
            ..policy
        };
        cpu_init(&mut cpu, &policy, Thread::from_cpuid(0x3 << 24, 1 << 8)).unwrap();
        assert!(!written(&cpu, PSP_ADDRESS));
        assert!(cpu.cache_toggles.is_empty());
        assert!(!written(&cpu, DPM_WAC_DATA) && !written(&cpu, CSTATE_CONFIG));
        assert!(!written(&cpu, mca_ctl_mask(14)));
        assert_eq!(cpu.values[&CSTATE_BASE_ADDRESS], 0x813);

        let mut cpu = Cpu::default();
        cpu.values.insert(MCG_CAP, 16);
        assert_eq!(
            cpu_init(&mut cpu, &policy, Thread::from_cpuid(0, 1 << 8)),
            Err(Error::McaBanks(16))
        );
        assert!(cpu.writes.is_empty());
    }
}
