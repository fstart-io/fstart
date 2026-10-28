//! Revision-guarded steps outside the tables (CCX_SP3 §3.4, §3.9, §3.10).
//!
//! Most errata workarounds the revision guides name a register for are table
//! records (ZP 1021/1033/1049/1091/1095/1100, see the generated tables).
//! What remains:
//!
//! - [`early`]: the donors' "BSP-early" step on the BSP and every AP before
//!   the tables: TW_CFG\[49\] (CombineCr0Cd) and the per-revision MCA DE
//!   mask / SSBD (ZP `0x11a8`, SSP `0x127c`, GN `0x1514`).
//! - [`all_threads`]: the DXE all-thread step after MP init: P0 rewrite and
//!   boost (in [`super::power`]), TW_CFG\[49\] on primary threads, Milan's
//!   fixed controls (GN `0x1df0`, instruction-checked).
//!
//! ZP erratum 931 is in [`super::mca`]; ZP 1158 (D18F5x104\[8\]) is DF
//! code; 1474 (CC6 off) is [`super::power::CstatePolicy::cc6`].

use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, SPEC_CTRL, SpecCtrl};
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::logical::{LogicalId, gn, ssp, zp};
use super::topology::Thread;
use crate::ccx::msr::{C001_10DD, IC_CFG, LS_CFG, LS_CFG3, MCODE_CTL, TW_CFG, TwCfg, mca_ctl_mask};
use crate::sp3::soc::Generation;

/// MCA_CTL_MASK of the DE bank.
const MCA_CTL_MASK_DE: Msr = mca_ctl_mask(3);

fn set<M: MsrAccess + ?Sized, R: RegisterLongName>(cpu: &M, msr: Msr<R>, mask: u64, on: bool) {
    let reg = cpu.reg(msr);
    reg.set(if on {
        reg.get() | mask
    } else {
        reg.get() & !mask
    });
}

/// The BSP-early step (BSP and every AP, before the tables run on APs).
pub fn early<M: MsrAccess + ?Sized>(cpu: &M, thread: &Thread, ids: &LogicalId) {
    let primary = thread.primary();
    match thread.generation() {
        Generation::Naples => {
            // ZP A0–B1 (and packages other than SP3/SP4/AM4).
            if primary && ids.cpu_is(zp::CPU, zp::A0 | zp::B0 | zp::B1) {
                set(cpu, MCA_CTL_MASK_DE, 1 << 3, true);
            }
        }
        Generation::Rome | Generation::Milan => {
            if !thread.smt() || !primary {
                cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
            }
            if primary && ids.core_is(ssp::CORE, ssp::A0) {
                set(cpu, MCA_CTL_MASK_DE, 1 << 3, true);
            }
            if !primary && ids.core_is(gn::CORE, gn::A0) {
                cpu.reg(SPEC_CTRL).modify(SpecCtrl::SSBD::SET);
            }
        }
    }
}

/// Milan's fixed all-thread controls; the defaults are the donor's PCD
/// defaults. Bit meanings beyond the names given are not public.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zen3Controls {
    /// PCD 0x263 (TRUE): IC_CFG bit 60 set, bit 59 clear.
    pub ic_cfg_60: bool,
    /// PCD 0x264 (FALSE): LS_CFG3\[26\] (EN_SPEC_ST_FILL). The B0+ table
    /// record sets it; this step then writes the PCD value.
    pub ls_cfg3_26: bool,
    /// PCD 0x302 (FALSE): C001_10DD\[13\].
    pub c001_10dd_13: bool,
    /// PCD 0x49 (FALSE): MCODE_CTL\[15\], Zen 3 core B0+.
    pub mcode_ctl_15: bool,
    /// PCD 0x3f (FALSE): set LS_CFG\[54\] (SPEC_LOCK_MAP_DIS), core B0+.
    pub spec_lock_map_dis: bool,
}

impl Default for Zen3Controls {
    fn default() -> Self {
        Self {
            ic_cfg_60: true,
            ls_cfg3_26: false,
            c001_10dd_13: false,
            mcode_ctl_15: false,
            spec_lock_map_dis: false,
        }
    }
}

/// The DXE all-thread step's CCX part (P0 and boost: [`super::power`]).
pub fn all_threads<M: MsrAccess + ?Sized>(
    cpu: &M,
    thread: &Thread,
    ids: &LogicalId,
    zen3: &Zen3Controls,
) {
    if thread.primary() {
        cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
    }
    if thread.generation() != Generation::Milan {
        return;
    }
    if zen3.ic_cfg_60 {
        let ic_cfg = cpu.reg(IC_CFG);
        ic_cfg.set((ic_cfg.get() & !(1 << 59)) | 1 << 60);
    }
    if ids.cpu_is(gn::CPU, gn::A0 | gn::B0 | gn::B1 | gn::B2) {
        set(cpu, LS_CFG3, 1 << 26, zen3.ls_cfg3_26);
        set(cpu, C001_10DD, 1 << 13, zen3.c001_10dd_13);
    }
    if ids.core_is(gn::CORE, gn::B0 | gn::B1 | gn::B2) {
        set(cpu, MCODE_CTL, 1 << 15, zen3.mcode_ctl_15);
        if zen3.spec_lock_map_dis {
            set(cpu, LS_CFG, 1 << 54, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Revision;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn early_per_revision() {
        let run = |rev: Revision, apic, smt| {
            let cpu = MsrSpace::new([]);
            let t = Thread::new(rev.generation(), apic, smt, false);
            early(&cpu, &t, &LogicalId::of(rev));
            cpu.into_inner()
        };
        assert_eq!(run(Revision::ZpB1, 0, true).get(MCA_CTL_MASK_DE.index()), 8);
        assert!(run(Revision::ZpB2, 0, true).writes.is_empty());
        let rome = run(Revision::SspA0, 0, true);
        assert_eq!(rome.get(MCA_CTL_MASK_DE.index()), 8);
        assert!(!rome.written(TW_CFG.index()), "primary thread with SMT");
        assert_eq!(
            run(Revision::SspB0, 0, false).get(TW_CFG.index()),
            TwCfg::COMBINE_CR0_CD::SET.value
        );
        let milan = run(Revision::GnA0, 1, true);
        assert_eq!(
            (milan.get(SPEC_CTRL.index()), milan.get(TW_CFG.index())),
            (SpecCtrl::SSBD::SET.value, TwCfg::COMBINE_CR0_CD::SET.value)
        );
        assert!(!run(Revision::GnB1, 1, true).written(SPEC_CTRL.index()));
    }

    #[test]
    fn milan_all_threads() {
        let cpu = MsrSpace::new([(LS_CFG3.index(), 1 << 26), (IC_CFG.index(), 1 << 59)]);
        let t = Thread::new(Generation::Milan, 0, true, true);
        all_threads(
            &cpu,
            &t,
            &LogicalId::of(Revision::GnB1),
            &Zen3Controls::default(),
        );
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(IC_CFG.index()), 1 << 60);
        assert_eq!(
            cpu.get(LS_CFG3.index()),
            0,
            "PCD 0x264 FALSE undoes the table bit"
        );
        assert!(cpu.written(MCODE_CTL.index()) && !cpu.written(LS_CFG.index()));
        assert_eq!(cpu.get(TW_CFG.index()), TwCfg::COMBINE_CR0_CD::SET.value);
    }
}
