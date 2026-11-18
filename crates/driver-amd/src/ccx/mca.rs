//! Machine-check bank setup, every thread (CCX_SP3 §3.11, instruction-
//! checked: ZP `0x6074`, SSP `0x6bf4`, GN `0x7f40`).
//!
//! This only acknowledges MCAX and sets the error masks the donors set.
//! Thresholds and deferred-error interrupts stay OS-owned. Optional SMM
//! observation of #MC is armed separately by [`super::ras`] after SMM install.

use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, Msr, MsrAccess};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, RegisterLongName};

use super::topology::{Scope, Thread};
use crate::ccx::msr::{
    MCA_INTR_CFG, McaConfig, McaIpid, McaMisc, mca_config, mca_ctl_mask, mca_ipid, mca_misc0,
    mca_misc1,
};
use crate::cpu::Silicon;

/// MCA_CTL_MASK of the eight L3 banks (7..=14).
const L3_BANKS: core::ops::RangeInclusive<u32> = 7..=14;
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

fn modify<M: MsrAccess + ?Sized, R: RegisterLongName>(
    cpu: &M,
    msr: Msr<R>,
    f: impl FnOnce(u64) -> u64,
) {
    let reg = cpu.reg(msr);
    reg.set(f(reg.get()));
}

/// An MCA_CTL_MASK setting for the banks of one kind of IP: its SMCA
/// hardware ID and MCA type (MCA_IPID), and the error bits to mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpMask {
    pub hardware_id: u64,
    pub mca_type: u64,
    pub bits: u64,
}

/// Milan's masks: CS (coherent slave) v2 bit 1, and the L3's bit 7 only
/// (erratum 1160 forbids bits 5/6).
const GN_MASKS: [IpMask; 2] = [
    IpMask {
        hardware_id: 0x2e,
        mca_type: 2,
        bits: 0x2,
    },
    IpMask {
        hardware_id: 0xb0,
        mca_type: 7,
        bits: 0x80,
    },
];

/// The NBIO's PCIe sideband errors (MCA_CTL_MASK_NBIO\[1\],
/// PCIE_Sideband): the PCIe errors its root ports also log as machine
/// checks. Masked when the OS handles PCIe errors through AER (openSIL
/// `SetNbioMcaCtlMask`, Ras.c:57, for any `PcieAerReportMechanism` but 0,
/// whose default is OS-first, NbioData.c:127; Oxide masks it too): the OS
/// then sees each error once, from AER, and not again as a corrected
/// machine check.
pub const NBIO_PCIE_SIDEBAND: IpMask = IpMask {
    hardware_id: 0x18,
    mca_type: 0,
    bits: 1 << 1,
};

/// Set each of `masks` in the executing thread's banks of its IP.
pub fn mask_by_ip<M: MsrAccess + ?Sized>(cpu: &M, masks: &[IpMask]) {
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
    (0..banks).for_each(|b| {
        let id = cpu.reg(mca_ipid(b)).extract();
        let ip = (id.read(McaIpid::HARDWARE_ID), id.read(McaIpid::MCA_TYPE));
        masks
            .iter()
            .filter(|m| (m.hardware_id, m.mca_type) == ip)
            .for_each(|m| modify(cpu, mca_ctl_mask(b), |v| v | m.bits));
    });
}

/// Set up the MCA banks of the executing thread.
pub fn init<M: MsrAccess + ?Sized>(cpu: &M, thread: &Thread) {
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
    (0..banks).for_each(|b| modify(cpu, mca_config(b), |v| configure(v, banks)));
    let clear_blk_ptr = |v| McaMisc::BLK_PTR.val(0).modify(v);
    // The first thread of a Naples die / Rome CCD owns the L3 masks.
    let l3_owner = thread.in_scope(Scope::Die);
    match thread.silicon() {
        Silicon::Zp => {
            // Erratum 931: MISC0[BlkPtr] = 0 except on the UMC banks,
            // whose MISC1 BlkPtr is cleared instead.
            (0..banks)
                .filter(|b| !ZP_UMC_BANKS.contains(b))
                .for_each(|b| modify(cpu, mca_misc0(b), clear_blk_ptr));
            ZP_UMC_BANKS
                .iter()
                .for_each(|&b| modify(cpu, mca_misc1(b), clear_blk_ptr));
            // Bit 3 is reserved in PPR 54945; the donor sets it.
            modify(cpu, MCA_INTR_CFG, |v| v | 0x8);
            modify(cpu, mca_ctl_mask(0), |v| v | 0x600);
            if l3_owner {
                L3_BANKS.for_each(|b| modify(cpu, mca_ctl_mask(b), |v| v | 0x80));
            }
        }
        Silicon::Ssp => {
            if l3_owner {
                L3_BANKS.for_each(|b| modify(cpu, mca_ctl_mask(b), |v| v | 0x80));
                modify(cpu, mca_ctl_mask(0x19), |v| v | 0x3c0);
            }
        }
        Silicon::Gn => mask_by_ip(cpu, &GN_MASKS),
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
            (mca_ipid(0).index(), 0x00b0 << 32),
            (mca_ipid(1).index(), 0x0007_00b0 << 32),
            (mca_ipid(2).index(), 0x0002_002e << 32),
        ]);
        init(&cpu, &Thread::new(Silicon::Gn, 1, true, false));
        let cpu = cpu.into_inner();
        assert!(!cpu.written(mca_ctl_mask(0).index()));
        assert_eq!(cpu.get(mca_ctl_mask(1).index()), 0x80);
        assert_eq!(cpu.get(mca_ctl_mask(2).index()), 0x2);
        assert_eq!(cpu.get(mca_config(2).index()), 1 << 32);
    }

    #[test]
    fn nbio_pcie_errors_masked_in_the_nbio_banks_only() {
        let cpu = MsrSpace::new([
            (0x179, 3),
            (mca_ipid(0).index(), 0x0000_0018 << 32),
            (mca_ipid(1).index(), 0x0002_002e << 32),
            (mca_ipid(2).index(), 0x0000_0018 << 32),
            (mca_ctl_mask(2).index(), 0x4),
        ]);
        mask_by_ip(&cpu, &[NBIO_PCIE_SIDEBAND]);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(mca_ctl_mask(0).index()), 0x2);
        assert!(!cpu.written(mca_ctl_mask(1).index()));
        assert_eq!(cpu.get(mca_ctl_mask(2).index()), 0x6);
    }

    #[test]
    fn naples_die_owner() {
        let cpu = MsrSpace::new([
            (0x179, 23),
            (mca_misc0(15).index(), 0xff00_0000),
            (mca_misc0(3).index(), 0xff00_0000),
        ]);
        init(&cpu, &Thread::new(Silicon::Zp, 0x10, true, false));
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(mca_misc0(3).index()), 0);
        assert_eq!(
            cpu.get(mca_misc0(15).index()),
            0xff00_0000,
            "UMC keeps MISC0 BlkPtr"
        );
        assert_eq!(cpu.get(mca_ctl_mask(14).index()), 0x80);
        assert_eq!(cpu.get(mca_ctl_mask(0).index()), 0x600);
        // Rome: L3 masks and C001_0419 only on the first thread of a CCD.
        let cpu = MsrSpace::new([(0x179, 23)]);
        init(&cpu, &Thread::new(Silicon::Ssp, 0x08, true, false));
        assert!(!cpu.into_inner().written(mca_ctl_mask(0x19).index()));
        let cpu = MsrSpace::new([(0x179, 23)]);
        init(&cpu, &Thread::new(Silicon::Ssp, 0x10, true, false));
        assert_eq!(cpu.into_inner().get(0xc001_0419), 0x3c0);
    }
}
