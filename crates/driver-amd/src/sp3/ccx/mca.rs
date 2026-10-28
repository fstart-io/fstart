//! Machine-check bank setup, every thread (CCX_SP3 §3.11, instruction-
//! checked: ZP `0x6074`, SSP `0x6bf4`, GN `0x7f40`).
//!
//! This only acknowledges MCAX and sets the error masks the donors set.
//! Thresholds and deferred-error interrupts stay OS-owned. Optional SMM
//! observation of #MC is armed separately by [`super::ras`] after SMM install.

use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, MsrAccess};
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;

use super::topology::{Scope, Thread};
use crate::sp3::soc::Generation;

register_bitfields![u64,
    /// MCA_CONFIG (C000_2004 + 16·bank).
    pub McaConfig [
        MCAX OFFSET(0) NUMBITS(1) [],
        TRANSPARENT_LOGGING_SUPPORTED OFFSET(1) NUMBITS(1) [],
        DEFERRED_LOGGING_SUPPORTED OFFSET(2) NUMBITS(1) [],
        MCAX_ENABLE OFFSET(32) NUMBITS(1) [],
        TRANSPARENT_LOGGING_ENABLE OFFSET(33) NUMBITS(1) [],
        LOG_DEFERRED_IN_MCA_STAT OFFSET(34) NUMBITS(1) []
    ],
    /// MCA_IPID (C000_2005 + 16·bank).
    pub McaIpid [
        HARDWARE_ID OFFSET(32) NUMBITS(12) [],
        MCA_TYPE OFFSET(48) NUMBITS(16) []
    ],
    /// MCA_MISC0/1 (C000_2003 / C000_200A + 16·bank).
    pub McaMisc [
        BLK_PTR OFFSET(24) NUMBITS(8) []
    ]
];

pub(super) const fn config(bank: u32) -> u32 {
    0xc000_2004 + 16 * bank
}
const fn ipid(bank: u32) -> u32 {
    0xc000_2005 + 16 * bank
}
const fn misc0(bank: u32) -> u32 {
    0xc000_2003 + 16 * bank
}
const fn misc1(bank: u32) -> u32 {
    0xc000_200a + 16 * bank
}
/// MCA_CTL_MASK of a bank.
pub const fn ctl_mask(bank: u32) -> u32 {
    0xc001_0400 + bank
}
const MCA_INTR_CFG: u32 = 0xc000_0410;
/// MCA_CTL_MASK of the eight L3 banks (7..=14).
const L3_MASKS: core::ops::RangeInclusive<u32> = ctl_mask(7)..=ctl_mask(14);
/// Naples UMC banks: the only ones keeping MISC0\[BlkPtr\] (erratum 931).
const ZP_UMC_BANKS: [u32; 2] = [15, 16];

/// MCA_CONFIG after the donors' update: acknowledge MCAX (if at most 32
/// banks), no transparent logging, deferred errors also in MCA_STATUS.
pub fn configure(old: u64, banks: u32) -> u64 {
    let mut c = LocalRegisterCopy::<u64, McaConfig::Register>::new(old);
    let transparent = c.is_set(McaConfig::TRANSPARENT_LOGGING_SUPPORTED);
    let deferred = c.is_set(McaConfig::DEFERRED_LOGGING_SUPPORTED);
    c.modify(McaConfig::MCAX_ENABLE.val(u64::from(banks <= 32)));
    if transparent {
        c.modify(McaConfig::TRANSPARENT_LOGGING_ENABLE::CLEAR);
    }
    if deferred {
        c.modify(McaConfig::LOG_DEFERRED_IN_MCA_STAT::SET);
    }
    c.get()
}

fn modify<M: MsrAccess + ?Sized>(cpu: &M, msr: u32, f: impl FnOnce(u64) -> u64) {
    cpu.write(msr, f(cpu.read(msr)));
}

/// Set up the MCA banks of the executing thread.
pub fn init<M: MsrAccess + ?Sized>(cpu: &M, thread: &Thread) {
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
    (0..banks).for_each(|b| modify(cpu, config(b), |v| configure(v, banks)));
    let clear_blk_ptr = |v| McaMisc::BLK_PTR.val(0).modify(v);
    // The first thread of a Naples die / Rome CCD owns the L3 masks.
    let l3_owner = thread.in_scope(Scope::Die);
    match thread.generation() {
        Generation::Naples => {
            // Erratum 931: MISC0[BlkPtr] = 0 except on the UMC banks,
            // whose MISC1 BlkPtr is cleared instead.
            (0..banks)
                .filter(|b| !ZP_UMC_BANKS.contains(b))
                .for_each(|b| modify(cpu, misc0(b), clear_blk_ptr));
            ZP_UMC_BANKS
                .iter()
                .for_each(|&b| modify(cpu, misc1(b), clear_blk_ptr));
            // Bit 3 is reserved in PPR 54945; the donor sets it.
            modify(cpu, MCA_INTR_CFG, |v| v | 0x8);
            modify(cpu, ctl_mask(0), |v| v | 0x600);
            if l3_owner {
                L3_MASKS.for_each(|m| modify(cpu, m, |v| v | 0x80));
            }
        }
        Generation::Rome => {
            if l3_owner {
                L3_MASKS.for_each(|m| modify(cpu, m, |v| v | 0x80));
                modify(cpu, ctl_mask(0x19), |v| v | 0x3c0);
            }
        }
        Generation::Milan => (0..banks).for_each(|b| {
            let id = LocalRegisterCopy::<u64, McaIpid::Register>::new(cpu.read(ipid(b)));
            let set = match (id.read(McaIpid::HARDWARE_ID), id.read(McaIpid::MCA_TYPE)) {
                // CS (coherent slave) v2.
                (0x2e, 2) => 0x2,
                // L3; only bit 7 (erratum 1160 forbids bits 5/6).
                (0xb0, 7) => 0x80,
                _ => return,
            };
            modify(cpu, ctl_mask(b), |v| v | set);
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn config_bits() {
        // McaX | transparent | deferred supported, transparent enabled.
        assert_eq!(configure(0x2_0000_0007, 23), 0x5_0000_0007);
        assert_eq!(configure(0x1_0000_0001, 64), 0x1);
    }

    #[test]
    fn milan_masks_by_ipid() {
        let cpu = MsrSpace::new([
            (0x179, 3),
            (ipid(0), 0x00b0 << 32),
            (ipid(1), 0x0007_00b0 << 32),
            (ipid(2), 0x0002_002e << 32),
        ]);
        init(&cpu, &Thread::new(Generation::Milan, 1, true, false));
        let cpu = cpu.into_inner();
        assert!(!cpu.written(ctl_mask(0)));
        assert_eq!(cpu.get(ctl_mask(1)), 0x80);
        assert_eq!(cpu.get(ctl_mask(2)), 0x2);
        assert_eq!(cpu.get(config(2)), 1 << 32);
    }

    #[test]
    fn naples_die_owner() {
        let cpu = MsrSpace::new([
            (0x179, 23),
            (misc0(15), 0xff00_0000),
            (misc0(3), 0xff00_0000),
        ]);
        init(&cpu, &Thread::new(Generation::Naples, 0x10, true, false));
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(misc0(3)), 0);
        assert_eq!(cpu.get(misc0(15)), 0xff00_0000, "UMC keeps MISC0 BlkPtr");
        assert_eq!(cpu.get(ctl_mask(14)), 0x80);
        assert_eq!(cpu.get(ctl_mask(0)), 0x600);
        // Rome: L3 masks and C001_0419 only on the first thread of a CCD.
        let cpu = MsrSpace::new([(0x179, 23)]);
        init(&cpu, &Thread::new(Generation::Rome, 0x08, true, false));
        assert!(!cpu.into_inner().written(ctl_mask(0x19)));
        let cpu = MsrSpace::new([(0x179, 23)]);
        init(&cpu, &Thread::new(Generation::Rome, 0x10, true, false));
        assert_eq!(cpu.into_inner().get(0xc001_0419), 0x3c0);
    }
}
