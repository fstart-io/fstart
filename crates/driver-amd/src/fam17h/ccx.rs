// SPDX-License-Identifier: MIT OR Apache-2.0
//! The CCX init every Family 17h CPU runs after its microcode update: the
//! SoC's reset MSR table, MMCONFIG, the PSP base, TOM2 write-back, MCA, the DPM weights,
//! C-states and the P0 rewrite. What the SoCs do differently is a [`Ccx`]
//! parameter; their MSR tables stay theirs.
use super::{
    cache::CacheControl,
    msr::{
        self, BU_CFG, BuCfg, CPU_WDT_CFG, CSTATE_BASE_ADDRESS, CSTATE_CONFIG, DPM_CFG, DPM_WAC_DATA,
        DPM_WAC_INDEX, MCA_INTR_CFG, McaConfig, McaIntrCfg, McaMisc, MsrUpdate, PSP_ADDRESS,
        PSTATE_0, enable_ecam, mca_config, mca_ctl_mask, mca_misc0, mca_misc1, update,
    },
};
use core::ops::RangeInclusive;
use fstart_arch::x86::cpu::{
    amd::msr::{SYSCFG, Syscfg},
    msr::{MCG_CAP, McgCap, MsrAccess},
};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

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
    /// The executing thread.
    pub fn current() -> Self {
        Self::from_cpuid(
            fstart_arch::x86::cpuid(1).1,
            fstart_arch::x86::cpuid(0x8000_001e).1,
        )
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
    pub const fn die_primary(self) -> bool {
        self.apic_id & 0xf == 0
    }
}

/// Which CPUs mask the L3 MCA banks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L3Owner {
    /// Every CPU, when MCG_CAP[7:4] is clear (Picasso).
    NoExtraBanks,
    /// The first CPU of each die (Zeppelin).
    DiePrimary,
}

/// What a SoC's MCA init does its own way.
#[derive(Debug, Clone)]
pub struct Mca {
    /// The bank counts the init handles; it accesses MISC1 of banks 15/16.
    pub banks: RangeInclusive<u8>,
    /// Banks without a MISC0 threshold counter.
    pub no_misc0: RangeInclusive<u32>,
    /// MCA_CTL_MASK bits to set, by bank.
    pub ctl_masks: &'static [(u32, u64)],
    /// The L3 banks, whose bit 7 is masked.
    pub l3_banks: RangeInclusive<u32>,
    pub l3_owner: L3Owner,
}

/// What a SoC's CCX init does its own way.
#[derive(Debug, Clone)]
pub struct Ccx {
    pub mca: Mca,
    /// MMCONFIG: the APs start with its reset value.
    pub ecam: u64,
    pub ecam_buses: u16,
    /// CSTATE_BASE_ADDRESS: the ACPI C-state control I/O.
    pub cstate_io_base: u64,
    /// CPU_WDT_CFG, if the SoC programs the CPU watchdog.
    pub watchdog: Option<u64>,
    /// DPM_CFG bits set along with the weights.
    pub dpm_cfg: u64,
    /// Per-thread MSRs of the SoC, before the P0 rewrite.
    pub tail: &'static [MsrUpdate],
}

/// Number of CAC weights the SMU has and the DPM weight MSRs take.
pub const CAC_WEIGHT_COUNT: usize = 21;
pub type CacWeights = [u64; CAC_WEIGHT_COUNT];

/// What a CPU needs that only the BSP can read (through SMN).
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The PSP MMIO base of the CPU's die, for RDRAND.
    pub psp_bar: Option<u64>,
    pub cac_weights: Option<&'a CacWeights>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A bank count the init doesn't handle; nothing was written.
    McaBanks(u8),
}

/// The CCX init of the executing CPU, after its microcode update, with the
/// SoC's reset MSR `table` for it. The SMT siblings share some of the MSRs,
/// so the platform must not run this on two CPUs at once.
pub fn cpu_init(
    cpu: &(impl MsrAccess + CacheControl),
    soc: &Ccx,
    thread: Thread,
    inputs: &Inputs<'_>,
    table: impl IntoIterator<Item = MsrUpdate>,
) -> Result<(), Error> {
    let cap = mca_cap(cpu, &soc.mca)?;
    msr::apply(cpu, table);

    enable_ecam(cpu, soc.ecam, soc.ecam_buses);
    if let Some(bar) = inputs.psp_bar {
        cpu.reg(PSP_ADDRESS).set(bar);
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

    mca_setup(cpu, &soc.mca, cap, thread);

    if thread.core_primary() {
        if let Some(weights) = inputs.cac_weights {
            load_cac_weights(cpu, weights, soc.dpm_cfg);
        }
        cpu.reg(CSTATE_CONFIG).set(CSTATE_CONFIG_CC6);
        cpu.reg(BU_CFG).modify(BuCfg::COMBINE_CR0_CD::SET);
    }
    cpu.reg(CSTATE_BASE_ADDRESS).set(soc.cstate_io_base);
    if let Some(watchdog) = soc.watchdog {
        cpu.reg(CPU_WDT_CFG).set(watchdog);
    }
    msr::apply(cpu, soc.tail.iter().copied());
    // Rewrite P0 so the new settings take effect.
    cpu.reg(PSTATE_0).set(cpu.reg(PSTATE_0).get());

    // Memory encryption isn't set up, so leave SME disabled. With SMEE set
    // the MTRR setup would need to account for the encryption bit.
    cpu.reg(SYSCFG).modify(Syscfg::SMEE::CLEAR);
    Ok(())
}

/// CC6 enabled.
const CSTATE_CONFIG_CC6: u64 = 0x48_4848;

/// The MCA init alone, e.g. for the BSP before MP init.
pub fn mca_init(cpu: &impl MsrAccess, mca: &Mca, thread: Thread) -> Result<(), Error> {
    let cap = mca_cap(cpu, mca)?;
    mca_setup(cpu, mca, cap, thread);
    Ok(())
}

fn mca_cap(
    cpu: &impl MsrAccess,
    mca: &Mca,
) -> Result<LocalRegisterCopy<u64, McgCap::Register>, Error> {
    let cap = cpu.reg(MCG_CAP).extract();
    let banks = cap.read(McgCap::COUNT) as u8;
    if !mca.banks.contains(&banks) {
        return Err(Error::McaBanks(banks));
    }
    Ok(cap)
}

fn mca_setup(
    cpu: &impl MsrAccess,
    mca: &Mca,
    cap: LocalRegisterCopy<u64, McgCap::Register>,
    thread: Thread,
) {
    let banks = cap.read(McgCap::COUNT) as u32;
    for bank in 0..banks {
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

        if !mca.no_misc0.contains(&bank) {
            cpu.reg(mca_misc0(bank))
                .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
        }
    }

    for bank in [15, 16] {
        cpu.reg(mca_misc1(bank))
            .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
    }
    cpu.reg(MCA_INTR_CFG)
        .modify(McaIntrCfg::THRESHOLD_LVT_VALID::SET);
    for &(bank, bits) in mca.ctl_masks {
        msr::apply(cpu, [update(mca_ctl_mask(bank).index(), 0, bits)]);
    }
    let l3 = match mca.l3_owner {
        // MCG_CAP[7:4]: with the bank count limited to 17..=32 never clear.
        L3Owner::NoExtraBanks => (cap.get() >> 4) & 0xf == 0,
        L3Owner::DiePrimary => thread.die_primary(),
    };
    if l3 {
        for bank in mca.l3_banks.clone() {
            msr::apply(cpu, [update(mca_ctl_mask(bank).index(), 0, 1 << 7)]);
        }
    }
}

fn load_cac_weights(cpu: &impl MsrAccess, weights: &CacWeights, dpm_cfg: u64) {
    let saved = cpu.reg(DPM_CFG).get();
    cpu.reg(DPM_CFG).set(0);
    for (index, &weight) in (0..).zip(weights) {
        cpu.reg(DPM_WAC_INDEX).set(index);
        cpu.reg(DPM_WAC_DATA).set(weight);
    }
    cpu.reg(DPM_CFG).set(saved | dpm_cfg);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::cache::testing::{DISABLE, ENABLE, FLUSH};
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    const CCX: Ccx = Ccx {
        mca: Mca {
            banks: 17..=u8::MAX,
            no_misc0: 15..=16,
            ctl_masks: &[(0, (1 << 9) | (1 << 10))],
            l3_banks: 7..=14,
            l3_owner: L3Owner::DiePrimary,
        },
        ecam: 0xf000_0000,
        ecam_buses: 128,
        cstate_io_base: 0x813,
        watchdog: Some(9),
        dpm_cfg: 1 << 63,
        tail: &[update(0xc001_1022, 0, 1 << 13)],
    };

    fn cpu() -> RefCell<MsrSpace> {
        MsrSpace::new([
            (MCG_CAP.index(), 23),
            (SYSCFG.index(), (1 << 21) | (1 << 23)),
            (DPM_CFG.index(), 0x5),
            (mca_config(2).index(), 0b111 | (1 << 33)),
        ])
    }

    #[test]
    fn per_core_msrs_on_the_primary_thread_only() {
        let weights = core::array::from_fn(|i| i as u64 * 3);
        let inputs = Inputs {
            psp_bar: Some(0xd010_0000),
            cac_weights: Some(&weights),
        };
        let table = [update(0xc001_1020, 0, 1)];
        let primary = cpu();
        cpu_init(
            &primary,
            &CCX,
            Thread::from_cpuid(0x10 << 24, 1 << 8),
            &inputs,
            table,
        )
        .unwrap();
        let primary = primary.into_inner();
        assert_eq!(primary.writes[0], (0xc001_1020, 1));
        assert_eq!(primary.get(PSP_ADDRESS.index()), 0xd010_0000);
        assert_eq!(primary.get(SYSCFG.index()), (1 << 21) | (1 << 22));
        assert!(
            primary
                .writes
                .windows(4)
                .any(|w| w[0] == DISABLE && w[1] == FLUSH && w[3] == ENABLE)
        );
        assert_eq!(
            primary.get(mca_config(2).index()),
            0b111 | (1 << 32) | (1 << 34)
        );
        assert_eq!(primary.get(DPM_CFG.index()), 0x5 | (1 << 63));
        assert_eq!(primary.get(CSTATE_CONFIG.index()), CSTATE_CONFIG_CC6);
        assert!(primary.written(mca_ctl_mask(14).index()));
        assert_eq!(primary.get(0xc001_1022), 1 << 13);
        let tail = primary.writes.len() - 3;
        assert_eq!(primary.writes[tail].0, 0xc001_1022);
        assert_eq!(primary.writes[tail + 1].0, PSTATE_0.index());

        let sibling = cpu();
        let inputs = Inputs {
            psp_bar: None,
            ..inputs
        };
        cpu_init(
            &sibling,
            &CCX,
            Thread::from_cpuid(0x13 << 24, 1 << 8),
            &inputs,
            [],
        )
        .unwrap();
        let sibling = sibling.into_inner();
        assert!(!sibling.written(PSP_ADDRESS.index()));
        assert!(!sibling.written(DPM_WAC_DATA.index()) && !sibling.written(CSTATE_CONFIG.index()));
        assert!(!sibling.written(mca_ctl_mask(14).index()));
        assert_eq!(sibling.get(CSTATE_BASE_ADDRESS.index()), 0x813);
    }

    #[test]
    fn unsupported_bank_count_writes_nothing() {
        for banks in [16, 33] {
            let cpu = MsrSpace::new([(MCG_CAP.index(), banks)]);
            let mca = Mca {
                banks: 17..=32,
                ..CCX.mca
            };
            assert_eq!(
                mca_init(&cpu, &mca, Thread::from_cpuid(0, 1 << 8)),
                Err(Error::McaBanks(banks as u8))
            );
            assert!(cpu.borrow().writes.is_empty());
        }
    }

    #[test]
    fn l3_owner() {
        let masked = |cap: u64, apic: u32, l3_owner| {
            let cpu = MsrSpace::new([(MCG_CAP.index(), cap)]);
            let mca = Mca {
                banks: 0..=u8::MAX,
                l3_owner,
                ..CCX.mca
            };
            mca_init(&cpu, &mca, Thread::from_cpuid(apic << 24, 1 << 8)).unwrap();
            cpu.borrow().written(mca_ctl_mask(7).index())
        };
        assert!(masked(23, 0x10, L3Owner::DiePrimary));
        assert!(!masked(23, 0x18, L3Owner::DiePrimary));
        assert!(masked(15, 0x18, L3Owner::NoExtraBanks));
        assert!(!masked(23, 0, L3Owner::NoExtraBanks));
    }
}
