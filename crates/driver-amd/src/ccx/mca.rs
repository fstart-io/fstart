//! Machine-check bank setup, every thread (CCX_SP3 §3.11, instruction-
//! checked: ZP `0x6074`, SSP `0x6bf4`, GN `0x7f40`), and the errors the
//! banks kept across the last reset ([`take_boot_errors`]).
//!
//! This only acknowledges MCAX and sets the error masks the donors set.
//! Thresholds and deferred-error interrupts stay OS-owned unless the SMM
//! handler counts them ([`super::ras`], armed after SMM install).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// [`configure`] every MCA_CONFIG of the executing thread (also WH's
/// McaConfigInit with TEL off, `f4e570`/`f4e6a0`).
pub fn configure_banks<M: MsrAccess + ?Sized>(cpu: &M) {
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
    (0..banks).for_each(|b| modify(cpu, mca_config(b), |v| configure(v, banks)));
}

/// Set up the MCA banks of the executing thread.
pub fn init<M: MsrAccess + ?Sized>(cpu: &M, thread: &Thread) {
    configure_banks(cpu);
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
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

/// The errors the banks of the threads held at boot: what the last boot
/// left before its reset (MCA_STATUS survives a warm reset, PPR 55901
/// "Reset: Cold"). Filled by [`take_boot_errors`] on every thread.
pub struct BootErrors {
    /// One thread reads and clears a bank at a time: the threads of a
    /// socket share its non-thread banks.
    claim: AtomicBool,
    count: AtomicUsize,
    /// Errors there was no room for, left in their banks.
    dropped: AtomicUsize,
    ready: [AtomicBool; BOOT_ERRORS],
    records: [UnsafeCell<super::ras::Record>; BOOT_ERRORS],
}

/// Boot errors kept; the rest are counted and dropped.
pub const BOOT_ERRORS: usize = 32;

// SAFETY: a slot is written once by the thread that took its index and
// read only after its `ready` flag is set.
unsafe impl Sync for BootErrors {}

impl BootErrors {
    pub const fn new() -> Self {
        Self {
            claim: AtomicBool::new(false),
            count: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            ready: [const { AtomicBool::new(false) }; BOOT_ERRORS],
            records: [const { UnsafeCell::new(super::ras::Record::EMPTY) }; BOOT_ERRORS],
        }
    }

    /// Keep `record`; false, and counted as dropped, if there is no room.
    fn push(&self, record: super::ras::Record) -> bool {
        let taken = self
            .count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < BOOT_ERRORS).then_some(count + 1)
            });
        let Ok(index) = taken else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        // SAFETY: the index is this thread's alone.
        unsafe { *self.records[index].get() = record };
        self.ready[index].store(true, Ordering::Release);
        true
    }

    /// Run `f` alone among the threads.
    fn claimed<T>(&self, f: impl FnOnce() -> T) -> T {
        while self
            .claim
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let result = f();
        self.claim.store(false, Ordering::Release);
        result
    }

    /// The errors kept and how many were dropped. A bank the threads of a
    /// share see is kept once already: the thread that takes it reads it
    /// again under the claim and clears it ([`take_boot_errors`]). Two
    /// records alike are two errors, of two threads' or sockets' banks.
    pub fn records(&self) -> (heapless::Vec<super::ras::Record, BOOT_ERRORS>, usize) {
        let mut records = heapless::Vec::new();
        for (slot, ready) in self.records.iter().zip(&self.ready) {
            if ready.load(Ordering::Acquire) {
                // SAFETY: published by `ready`; never written again.
                let _ = records.push(unsafe { *slot.get() });
            }
        }
        (records, self.dropped.load(Ordering::Acquire))
    }
}

impl Default for BootErrors {
    fn default() -> Self {
        Self::new()
    }
}

/// What the banks of every thread held at boot ([`take_boot_errors`]).
pub static BOOT: BootErrors = BootErrors::new();

/// On the executing thread (APIC ID `apic_id`): keep every uncorrected or
/// deferred error its banks hold from before the reset in [`BOOT`], for
/// the boot error record table, and clear it, so the OS does not log it
/// again as an error of this boot. Corrected errors stay for the OS, and
/// so does an error there is no room for.
pub fn take_boot_errors<M: MsrAccess + ?Sized>(cpu: &M, apic_id: u32) {
    take_boot_errors_into(&BOOT, cpu, apic_id);
}

fn take_boot_errors_into<M: MsrAccess + ?Sized>(errors: &BootErrors, cpu: &M, apic_id: u32) {
    use super::ras::{mca_destat, mca_status};
    let banks = (cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32).min(super::ras::MAX_BANKS);
    for bank in 0..banks {
        if boot_error(cpu, apic_id, bank).is_none() {
            continue;
        }
        // Read again under the claim: a sibling may have taken and
        // cleared a bank they share.
        errors.claimed(|| {
            let Some((record, in_status, in_destat)) = boot_error(cpu, apic_id, bank) else {
                return;
            };
            if !errors.push(record) {
                return;
            }
            if in_status {
                cpu.reg(mca_status(bank)).set(0);
            }
            if in_destat {
                cpu.reg(mca_destat(bank)).set(0);
            }
        });
    }
}

/// The boot error `bank` holds, if one of its two logs has one: MCA_STATUS
/// if it logged an uncorrected or deferred error, MCA_DESTAT if it logged
/// a deferred one. The two are independent: a corrected error in
/// MCA_STATUS next to a deferred one in MCA_DESTAT stays the OS's. The
/// record carries only the logs with a boot error, and says which they
/// are, for their clears.
fn boot_error<M: MsrAccess + ?Sized>(
    cpu: &M,
    apic_id: u32,
    bank: u32,
) -> Option<(super::ras::Record, bool, bool)> {
    use super::ras::{Kind, McaStatus, read};
    let mut record = read(cpu, apic_id, bank, Kind::Boot)?;
    let status = tock_registers::LocalRegisterCopy::<u64, McaStatus::Register>::new(record.status);
    let in_status = status.is_set(McaStatus::VALID)
        && (status.is_set(McaStatus::UNCORRECTED) || status.is_set(McaStatus::DEFERRED));
    let in_destat = McaStatus::VALID.is_set(record.deferred_status);
    if !in_status {
        (record.status, record.address, record.syndrome) = (0, 0, 0);
    }
    if !in_destat {
        (record.deferred_status, record.deferred_address) = (0, 0);
    }
    (in_status || in_destat).then_some((record, in_status, in_destat))
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

    /// A shared bank is taken once: the sibling finds it cleared. Two
    /// threads' (or sockets') banks holding errors alike are two errors,
    /// both kept.
    #[test]
    fn boot_errors_are_kept_once_and_cleared_corrected_ones_left() {
        let uc = (1 << 63) | (1 << 61) | (1 << 57);
        let ce = 1 << 63;
        let shared = || {
            MsrSpace::new([
                (0x179, 3),
                (0xc000_2001, uc),
                (0xc000_2005, 0x96 << 32),
                (0xc000_2011, ce),
            ])
        };
        let cpu = shared();
        take_boot_errors(&cpu, 0x21);
        // A sibling of the same socket finds the shared bank cleared.
        take_boot_errors(&cpu, 0x22);
        let (records, dropped) = BOOT.records();
        assert_eq!((records.len(), dropped), (1, 0));
        assert_eq!(
            (records[0].cpu, records[0].bank, records[0].status),
            (0x21, 0, uc)
        );
        assert!(records[0].fatal() && records[0].memory());
        assert_eq!(cpu.read(0xc000_2001), 0, "kept and cleared");
        assert_eq!(cpu.read(0xc000_2011), ce, "corrected: the OS's");
        // The other socket's bank, with the same IPID and status.
        let other = shared();
        take_boot_errors(&other, 0x81);
        let (records, _) = BOOT.records();
        assert_eq!(records.len(), 2);
        assert_eq!((records[1].cpu, records[1].status), (0x81, uc));
    }

    /// One bank, a corrected error in MCA_STATUS and a deferred one in
    /// MCA_DESTAT: the deferred error is the boot's, the corrected one the
    /// OS's, each register on its own.
    #[test]
    fn a_banks_two_logs_are_taken_independently() {
        use crate::ccx::ras::{McaStatus, Record};
        let ce = (1 << 63) | (1 << 58);
        let deferred = (1 << 63) | (1 << 44) | (1 << 58);
        let bank = || {
            MsrSpace::new([
                (0x179, 1),
                (0xc000_2001, ce),
                (0xc000_2002, 0x1000),
                (0xc000_2004, 1 << 2),
                (0xc000_2008, deferred),
                (0xc000_2009, 0x2000),
            ])
        };
        let errors = BootErrors::new();
        let cpu = bank();
        take_boot_errors_into(&errors, &cpu, 1);
        let (records, dropped) = errors.records();
        assert_eq!((records.len(), dropped), (1, 0));
        let record = records[0];
        assert_eq!((record.status, record.address), (0, 0));
        assert_eq!(
            (record.deferred_status, record.deferred_address),
            (deferred, 0x2000)
        );
        assert_eq!(cpu.read(0xc000_2001), ce, "the corrected error stays");
        assert_eq!(cpu.read(0xc000_2008), 0, "the deferred error is taken");

        // A valid deferred error in MCA_STATUS and nothing in MCA_DESTAT.
        let cpu = MsrSpace::new([(0x179, 1), (0xc000_2001, deferred), (0xc000_2004, 1 << 2)]);
        let errors = BootErrors::new();
        take_boot_errors_into(&errors, &cpu, 1);
        assert_eq!(errors.records().0[0].status, deferred);
        assert!(!cpu.into_inner().written(0xc000_2008));

        // No room: the evidence stays in the bank, counted as dropped.
        let full = BootErrors::new();
        for bank in 0..BOOT_ERRORS as u32 {
            assert!(full.push(Record {
                bank: bank + 1,
                status: 1 << 63 | 1 << 61,
                ..Record::EMPTY
            }));
        }
        let cpu = bank();
        take_boot_errors_into(&full, &cpu, 1);
        assert_eq!(full.records().1, 1);
        assert_eq!(cpu.read(0xc000_2008), deferred);
        assert!(!cpu.into_inner().written(0xc000_2001));
        assert!(McaStatus::VALID.is_set(deferred));
    }
}
