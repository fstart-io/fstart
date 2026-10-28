//! Core power management: C-states, CPU watchdog, CAC (DPM) weights, boost,
//! and the P-state / C-state / CPPC getters for ACPI (CCX_SP3 §3.9, §3.13).

use fstart_arch::x86::cpu::amd::msr::{
    HWCR, Hwcr, PSTATE_0, PSTATE_CURRENT_LIMIT, PstateCurLim, PstateDef, pstate_mhz,
};
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use heapless::Vec;
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::topology::Thread;
use crate::sp3::smu::{SMU_ARGS, SmuError, SmuMessages};
use crate::sp3::soc::Generation;

register_bitfields![u64,
    /// Core::X86::Msr::CSTATE_CFG (names: Oxide `ccx.h`).
    pub CstateCfg [
        CCR0_CC1DFSID OFFSET(0) NUMBITS(6) [],
        CCR0_CC6EN OFFSET(6) NUMBITS(1) [],
        CCR1_CC1DFSID OFFSET(8) NUMBITS(6) [],
        CCR1_CC6EN OFFSET(14) NUMBITS(1) [],
        CCR2_CC1DFSID OFFSET(16) NUMBITS(6) [],
        CCR2_CC6EN OFFSET(22) NUMBITS(1) []
    ],
    /// Core::X86::Msr::CStateBaseAddr.
    pub CstateBaseAddr [
        CSTATE_ADDR OFFSET(0) NUMBITS(16) []
    ],
    /// Core::X86::Msr::CpuWdtCfg.
    pub CpuWdtCfg [
        EN OFFSET(0) NUMBITS(1) [],
        TIME_BASE OFFSET(1) NUMBITS(2) [],
        COUNT_SEL OFFSET(3) NUMBITS(4) [],
        /// Rome/Milan only (reserved on Naples).
        SEVERITY OFFSET(7) NUMBITS(3) []
    ],
    /// Core::X86::Msr::DPM_CFG.
    pub DpmCfg [
        JITTER_EN OFFSET(60) NUMBITS(1) [],
        CFG_LOCKED OFFSET(63) NUMBITS(1) []
    ]
];

pub const CSTATE_CFG: Msr<CstateCfg::Register> = Msr::new(0xc001_0296);
pub const CSTATE_BASE_ADDR: Msr<CstateBaseAddr::Register> = Msr::new(0xc001_0073);
pub const CPU_WDT_CFG: Msr<CpuWdtCfg::Register> = Msr::new(0xc001_0074);
pub const DPM_CFG: Msr<DpmCfg::Register> = Msr::new(0xc001_1074);
pub const DPM_WAC_ACC_INDEX: Msr = Msr::new(0xc001_1076);
pub const DPM_WAC_DATA: Msr = Msr::new(0xc001_1077);

// --- C-states -------------------------------------------------------------

/// C-state policy (donor defaults: on, CC6 on, IO base 0x813).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CstatePolicy {
    pub enable: bool,
    /// Naples and Rome donors always enable CC6; disabling it is erratum
    /// 1474's workaround (ZP/Rome/Milan) and Milan's CBS option.
    pub cc6: bool,
    /// IO range trapped as C-state requests (8 ports, `_CST`).
    pub io_base: u16,
}

impl Default for CstatePolicy {
    fn default() -> Self {
        Self {
            enable: true,
            cc6: true,
            io_base: 0x813,
        }
    }
}

/// CSTATE_CFG: CC1 at DFS ID 8 on CCR0–2 and CC6 if enabled — the Milan
/// rule `0x80808 | cc6·0x404040`, which gives the fixed Naples/Rome value
/// 0x484848 with CC6 on.
pub fn cstate_cfg(cc6: bool) -> u64 {
    let cc6 = u64::from(cc6);
    (CstateCfg::CCR0_CC1DFSID.val(8)
        + CstateCfg::CCR1_CC1DFSID.val(8)
        + CstateCfg::CCR2_CC1DFSID.val(8)
        + CstateCfg::CCR0_CC6EN.val(cc6)
        + CstateCfg::CCR1_CC6EN.val(cc6)
        + CstateCfg::CCR2_CC6EN.val(cc6))
    .value
}

/// Program C-states on the executing thread (DXE MP callback, every
/// thread; CSTATE_CFG on primary threads).
pub fn init_cstates<M: MsrAccess + ?Sized>(cpu: &M, thread: &Thread, policy: &CstatePolicy) {
    if !policy.enable {
        return;
    }
    if thread.primary() {
        cpu.reg(CSTATE_CFG).set(cstate_cfg(policy.cc6));
    }
    cpu.reg(CSTATE_BASE_ADDR)
        .write(CstateBaseAddr::CSTATE_ADDR.val(policy.io_base.into()));
}

/// What `_CST` needs (AmdCcx C-state info protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CstateInfo {
    /// IO port for the deepest state (CC6 if enabled, else CC1).
    pub io_base: u16,
    pub cc6: bool,
}

pub fn cstate_info(policy: &CstatePolicy) -> Option<CstateInfo> {
    (policy.enable && policy.io_base != 0).then_some(CstateInfo {
        io_base: policy.io_base,
        cc6: policy.cc6,
    })
}

// --- Boost and P0 ------------------------------------------------------------

/// Core performance boost on/off (donor default on), and the P0 rewrite every
/// thread does after the SMU has set the P-states.
pub fn init_pstate<M: MsrAccess + ?Sized>(cpu: &M, boost: bool) {
    let p0 = cpu.reg(PSTATE_0);
    p0.set(p0.get());
    if !boost {
        cpu.reg(HWCR).modify(Hwcr::CPB_DIS::SET);
    }
}

// --- CPU watchdog ------------------------------------------------------------

/// CPU watchdog timeout: CpuWdtCountSel/TimeBase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wdt {
    pub count_sel: u8,
    pub time_base: u8,
}

/// Count multipliers by CpuWdtCountSel (PPR 54945/55803).
const WDT_COUNTS: [u64; 10] = [4095, 2047, 1023, 511, 255, 127, 63, 31, 8191, 16383];
/// Time bases in ns by CpuWdtTimeBase.
const WDT_BASES: [u64; 2] = [1_310_000, 1_280];
/// CountSel by increasing period (donor table, both directions walk it).
const WDT_ORDER: [u8; 10] = [7, 6, 5, 4, 3, 2, 1, 0, 8, 9];
/// DF watchdog multipliers and time bases (DfGlobalCtrl \[18:16\], \[5:4\];
/// ZP DXE `0x98f0`, SSP DXE `0x9850`, identical).
const DF_COUNTS: [u64; 8] = [31, 127, 255, 511, 1023, 2047, 4095, 16383];
const DF_BASES: [u64; 3] = [1_310_000, 1_280, 10];

impl Wdt {
    /// The donor default (PCD 0x100: 2047 × 1.31 ms).
    pub const DEFAULT: Self = Self {
        count_sel: 1,
        time_base: 0,
    };

    /// Timeout in ns; `None` for reserved encodings.
    pub fn period_ns(self) -> Option<u64> {
        Some(WDT_COUNTS.get(self.count_sel as usize)? * WDT_BASES.get(self.time_base as usize)?)
    }

    /// Next longer (`step = 1`) or shorter (`-1`) setting.
    fn step(self, longer: bool) -> Option<Self> {
        let pos = WDT_ORDER.iter().position(|&c| c == self.count_sel)?;
        match (longer, pos, self.time_base) {
            // Longest count: move to the coarser base, shortest count.
            (true, 9, 0) => None,
            (true, 9, b) => Some(Self {
                count_sel: WDT_ORDER[0],
                time_base: b - 1,
            }),
            (true, p, b) => Some(Self {
                count_sel: WDT_ORDER[p + 1],
                time_base: b,
            }),
            // Shortest count: move to the finer base, longest count.
            (false, 0, 1) => None,
            (false, 0, _) => Some(Self {
                count_sel: WDT_ORDER[9],
                time_base: 1,
            }),
            (false, p, b) => Some(Self {
                count_sel: WDT_ORDER[p - 1],
                time_base: b,
            }),
        }
    }
}

/// The DF CCM watchdog period from DF::DfGlobalCtrl (D18F0x3F8 of node 0),
/// `None` if disabled (time base 3).
pub fn df_wdt_ns(df_global_ctrl: u32) -> Option<u64> {
    let base = DF_BASES.get(((df_global_ctrl >> 4) & 3) as usize)?;
    Some(DF_COUNTS[((df_global_ctrl >> 16) & 7) as usize] * base)
}

/// The CPU watchdog setting to use given the DF watchdog: Naples wants the
/// CPU WDT *longer* than the DF's (raised step by step), Rome and Milan
/// *shorter* (lowered). `None` = leave the CPU WDT off, as the donors do
/// when no setting satisfies the rule (DXE ZP `0x1884`, SSP `0x1c0c`, GN
/// `0x211c`).
pub fn wdt_setting(generation: Generation, requested: Wdt, df_global_ctrl: u32) -> Option<Wdt> {
    let Some(df) = df_wdt_ns(df_global_ctrl) else {
        return Some(requested);
    };
    let longer = generation == Generation::Naples;
    let ok = |w: Wdt| {
        w.period_ns()
            .is_some_and(|p| if longer { p >= df } else { p < df })
    };
    // At most 20 settings exist: bounded.
    core::iter::successors(Some(requested), |w| w.step(longer))
        .take(WDT_ORDER.len() * WDT_BASES.len() + 1)
        .find(|&w| ok(w))
}

/// Program CpuWdtCfg (primary threads). `severity` is Rome/Milan's \[9:7\]
/// (PCD 0xff = keep the reset value).
pub fn init_wdt<M: MsrAccess + ?Sized>(cpu: &M, setting: Option<Wdt>, severity: Option<u8>) {
    let reg = cpu.reg(CPU_WDT_CFG);
    match setting {
        Some(w) => {
            let sev = severity.map_or(
                tock_registers::fields::FieldValue::<u64, CpuWdtCfg::Register>::none(),
                |s| CpuWdtCfg::SEVERITY.val(s.into()),
            );
            reg.modify(
                CpuWdtCfg::EN::SET
                    + CpuWdtCfg::TIME_BASE.val(w.time_base.into())
                    + CpuWdtCfg::COUNT_SEL.val(w.count_sel.into())
                    + sev,
            );
        }
        None => reg.modify(
            CpuWdtCfg::EN::CLEAR + CpuWdtCfg::TIME_BASE.val(0) + CpuWdtCfg::COUNT_SEL.val(0),
        ),
    }
}

// --- CAC weights -------------------------------------------------------------

/// Weight chains: 21 on Zen/Zen 2, 23 on Zen 3.
pub const fn cac_weight_count(generation: Generation) -> usize {
    match generation {
        Generation::Naples | Generation::Rome => 21,
        Generation::Milan => 23,
    }
}

/// The weights, read once from the SMU (BSP).
pub type CacWeights = Vec<u64, 23>;

/// Read the weights: V9 message 0x10 with `resp0 << 32 | resp1`
/// (AmdNbioSmuV9Dxe `0x1066`), V11 0x09 with `resp1 << 32 | resp0`
/// (SmuV11Dxe `0x37b8`).
pub fn read_cac_weights<M: SmuMessages + ?Sized>(
    generation: Generation,
    smu: &M,
) -> Result<CacWeights, SmuError> {
    let id = match generation {
        Generation::Naples => 0x10,
        _ => 0x09,
    };
    (0..cac_weight_count(generation) as u32)
        .map(|i| {
            let mut args = [0; SMU_ARGS];
            args[0] = i;
            smu.send(id, &mut args)?;
            let (lo, hi) = (u64::from(args[0]), u64::from(args[1]));
            Ok(match generation {
                Generation::Naples => lo << 32 | hi,
                _ => hi << 32 | lo,
            })
        })
        .collect()
}

/// Write the weights on a primary thread: DPM_CFG cleared while writing,
/// then restored with CFG_LOCKED.
pub fn write_cac_weights<M: MsrAccess + ?Sized>(cpu: &M, weights: &[u64]) {
    let saved = cpu.reg(DPM_CFG).get();
    cpu.reg(DPM_CFG).set(0);
    weights.iter().enumerate().for_each(|(i, &w)| {
        cpu.reg(DPM_WAC_ACC_INDEX).set(i as u64);
        cpu.reg(DPM_WAC_DATA).set(w);
    });
    cpu.reg(DPM_CFG).set(DpmCfg::CFG_LOCKED::SET.modify(saved));
}

// --- ACPI getters --------------------------------------------------------------

/// One `_PSS` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pstate {
    pub index: u8,
    pub mhz: u32,
    pub microvolts: u32,
    pub milliwatts: u32,
}

/// Decode a PStateDef value (formulas of the donors' P-state protocol).
pub fn decode_pstate(index: u8, raw: u64) -> Option<Pstate> {
    let r = LocalRegisterCopy::<u64, PstateDef::Register>::new(raw);
    if !r.is_set(PstateDef::PSTATE_EN) {
        return None;
    }
    let mhz = pstate_mhz(raw)?;
    // SVI2: 1.55 V − VID × 6.25 mV.
    let microvolts = 0xf8u32.saturating_sub(r.read(PstateDef::CPU_VID) as u32) * 6250;
    let div = [100u64, 1000, 10000].get(r.read(PstateDef::IDD_DIV) as usize)?;
    let milliwatts = (u64::from(microvolts) / 10 * r.read(PstateDef::IDD_VALUE) / div) as u32;
    Some(Pstate {
        index,
        mhz,
        microvolts,
        milliwatts,
    })
}

/// The enabled P-states up to PstateMaxVal, for `_PSS`.
pub fn pstates<M: MsrAccess + ?Sized>(cpu: &M) -> Vec<Pstate, 8> {
    let max = cpu
        .reg(PSTATE_CURRENT_LIMIT)
        .read(PstateCurLim::PSTATE_MAX_VAL) as u8;
    (0..=max)
        .filter_map(|i| decode_pstate(i, cpu.read(PSTATE_0.offset(i.into()).index())))
        .collect()
}

/// How the OS gets CPPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cppc {
    None,
    /// Rome: SMU shared-memory CPPC (PCCT), if the SMU feature is enabled.
    SharedMemory,
    /// Milan: CPPC MSRs (CPUID Fn8000_0008_EBX\[27\]).
    Msr,
}

/// CPPC availability. `smu_cppc` is the SMU's CPPC feature bit as enabled.
pub fn cppc(generation: Generation, cpuid_8000_0008_ebx: u32, smu_cppc: bool) -> Cppc {
    match generation {
        Generation::Naples => Cppc::None,
        Generation::Milan if cpuid_8000_0008_ebx & 1 << 27 != 0 => Cppc::Msr,
        _ if smu_cppc => Cppc::SharedMemory,
        _ => Cppc::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn cc6_values() {
        assert_eq!(cstate_cfg(true), 0x48_4848);
        assert_eq!(cstate_cfg(false), 0x08_0808);
        let cpu = MsrSpace::new([]);
        init_cstates(
            &cpu,
            &Thread::new(Generation::Milan, 1, true, false),
            &CstatePolicy::default(),
        );
        let cpu = cpu.into_inner();
        assert!(!cpu.written(0xc001_0296), "secondary thread");
        assert_eq!(cpu.get(0xc001_0073), 0x813);
    }

    #[test]
    fn wdt_rule() {
        // DF: 4095 × 1.31 ms ([18:16] = 6, [5:4] = 0) = 5.36 s.
        let df = 6 << 16;
        // Default 2047 × 1.31 ms = 2.68 s: already shorter on Rome.
        assert_eq!(
            wdt_setting(Generation::Rome, Wdt::DEFAULT, df),
            Some(Wdt::DEFAULT)
        );
        // Naples raises it to the first setting not shorter than the DF's.
        assert_eq!(
            wdt_setting(Generation::Naples, Wdt::DEFAULT, df),
            Some(Wdt {
                count_sel: 0,
                time_base: 0
            })
        );
        // DF 31 × 10 ns: nothing on the CPU is shorter, the WDT stays off.
        assert_eq!(wdt_setting(Generation::Milan, Wdt::DEFAULT, 2 << 4), None);
        // DF 127 × 1.28 µs: Rome walks down through the 1.31 ms counts,
        // switches to 1.28 µs and stops at 63 × 1.28 µs.
        assert_eq!(
            wdt_setting(Generation::Rome, Wdt::DEFAULT, 1 << 16 | 1 << 4),
            Some(Wdt {
                count_sel: 6,
                time_base: 1
            })
        );
        assert_eq!(
            wdt_setting(Generation::Rome, Wdt::DEFAULT, 3 << 4),
            Some(Wdt::DEFAULT)
        );

        let cpu = MsrSpace::new([(0xc001_0074, 0x280)]);
        init_wdt(&cpu, Some(Wdt::DEFAULT), None);
        assert_eq!(cpu.into_inner().get(0xc001_0074), 0x289);
    }

    #[test]
    fn cac_weights() {
        struct Smu;
        impl SmuMessages for Smu {
            fn send(&self, id: u32, args: &mut [u32; SMU_ARGS]) -> Result<(), SmuError> {
                assert_eq!(id, 0x09);
                *args = [0x100 + args[0], 0xa, 0, 0, 0, 0];
                Ok(())
            }
        }
        let w = read_cac_weights(Generation::Milan, &Smu).unwrap();
        assert_eq!(w.len(), 23);
        assert_eq!(w[2], 0xa_0000_0102);
        let cpu = MsrSpace::new([(0xc001_1074, 0x1000)]);
        write_cac_weights(&cpu, &w[..2]);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.writes[0], (0xc001_1074, 0));
        assert_eq!(cpu.writes[4], (0xc001_1077, 0xa_0000_0101));
        assert_eq!(cpu.get(0xc001_1074), 0x8000_0000_0000_1000);
    }

    #[test]
    fn pstate_decode() {
        // FID 0x8c, DID 8, VID 0x48, Idd 0x5a / 10: 3500 MHz, 1.1 V.
        let raw = 1 << 63 | 1 << 30 | 0x5a << 22 | 0x48 << 14 | 8 << 8 | 0x8c;
        let p = decode_pstate(0, raw).unwrap();
        assert_eq!((p.mhz, p.microvolts, p.milliwatts), (3500, 1_100_000, 9900));
        assert!(decode_pstate(1, 0x8c).is_none());
        let cpu = MsrSpace::new([(0xc001_0061, 0x10), (0xc001_0064, raw), (0xc001_0065, raw)]);
        assert_eq!(pstates(&cpu).len(), 2);
        assert_eq!(cppc(Generation::Milan, 1 << 27, false), Cppc::Msr);
        assert_eq!(cppc(Generation::Rome, 0, true), Cppc::SharedMemory);
    }
}
