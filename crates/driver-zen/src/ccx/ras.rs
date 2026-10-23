//! Per-thread MCA observation in SMM, without taking the OS's MCA banks.
//!
//! Unlike the vendor's cloaking/WRMSR-interception scheme, this observer
//! leaves every bank visible and unchanged. Only #MC is redirected; corrected
//! and deferred interrupts, thresholds, AER and recovery remain OS-owned.
//!
//! Register facts: AMD #55570, MSRC001_0022/0056 and SMMxFEC4;
//! OpenSIL cd8edaee, xUSL/Include/MsrReg.h (PFEH_CFG fields). Donor check:
//! Rome AmdRasSspSmm/ee8175ef-4c60-82af-f5e2-8a9ef1fc0a9a,
//! cca68f7070ef, 0x6b0 and 0x934 (SMM, instruction-checked).

use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, Msr, MsrAccess};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use crate::soc::Generation;

register_bitfields![u64,
    pub MceRedirect [
        VECTOR_ENABLE OFFSET(8) NUMBITS(1) [],
        SMI_ENABLE OFFSET(9) NUMBITS(1) []
    ],
    pub SmiIoCycle [
        PORT OFFSET(0) NUMBITS(16) [],
        DATA OFFSET(16) NUMBITS(8) [],
        ENABLE OFFSET(25) NUMBITS(1) [],
        READ OFFSET(26) NUMBITS(1) []
    ],
    pub PfehConfig [
        ENABLE OFFSET(0) NUMBITS(1) [],
        DEFERRED_ON_EXIT OFFSET(1) NUMBITS(1) [],
        THRESHOLD_ON_EXIT OFFSET(2) NUMBITS(1) [],
        MCE_ON_EXIT OFFSET(3) NUMBITS(1) []
    ],
    pub McaStatus [
        DEFERRED OFFSET(44) NUMBITS(1) [],
        SYNDROME_VALID OFFSET(53) NUMBITS(1) [],
        CONTEXT_CORRUPT OFFSET(57) NUMBITS(1) [],
        ADDRESS_VALID OFFSET(58) NUMBITS(1) [],
        UNCORRECTED OFFSET(61) NUMBITS(1) [],
        VALID OFFSET(63) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub LocalSmiStatus [
        MCE_REDIRECT OFFSET(8) NUMBITS(1) [],
        WRMSR OFFSET(11) NUMBITS(1) []
    ]
];

pub const LOCAL_SMI_STATUS: u32 = 0xfec4;
pub const MCE_REDIRECT: Msr<MceRedirect::Register> = Msr::new(0xc001_0022);
pub const SMI_IO_CYCLE: Msr<SmiIoCycle::Register> = Msr::new(0xc001_0056);
pub const PFEH_CONFIG: Msr<PfehConfig::Register> = Msr::new(0xc001_0120);
pub const PFEH_CLOAK: Msr = Msr::new(0xc001_0121);
pub const PFEH_DEFERRED_MASK: Msr = Msr::new(0xc001_0122);
const MCG_STATUS: Msr = Msr::new(0x17a);
/// A bank mask is 64 bits. Refuse larger implementations, rather than
/// silently enabling a handler that cannot inspect all their banks.
pub const MAX_BANKS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmError {
    UnsupportedCpu,
    UnsupportedBanks,
    AlreadyOwned,
    Readback,
}

/// Arm the executing thread, only after its permanent SMM entry is installed.
/// Existing PFEH, interrupt redirection and local-trigger ownership is refused.
/// Naples before B2 is deliberately excluded (the vendor warns against B1 PFEH).
/// The caller must arrange to inspect this CPU's local status on *every* SMI.
pub fn arm<M: MsrAccess>(
    cpu: &M,
    generation: Generation,
    stepping: u8,
    port: u16,
    command: u8,
) -> Result<(), ArmError> {
    if generation == Generation::Naples && stepping < 2 {
        return Err(ArmError::UnsupportedCpu);
    }
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT);
    if banks == 0
        || banks > u64::from(MAX_BANKS)
        || !cpu
            .reg(Msr::new(super::mca::config(0)))
            .is_set(super::mca::McaConfig::MCAX)
    {
        return Err(ArmError::UnsupportedBanks);
    }
    let redirect = cpu.reg(MCE_REDIRECT).get();
    let trigger = cpu.reg(SMI_IO_CYCLE).get();
    let pfeh = cpu.reg(PFEH_CONFIG).get();
    if MceRedirect::VECTOR_ENABLE.is_set(redirect)
        || MceRedirect::SMI_ENABLE.is_set(redirect)
        || SmiIoCycle::ENABLE.is_set(trigger)
        || PfehConfig::ENABLE.is_set(pfeh)
        || cpu.reg(PFEH_CLOAK).get() != 0
        || cpu.reg(PFEH_DEFERRED_MASK).get() != 0
    {
        return Err(ArmError::AlreadyOwned);
    }
    let trigger_new = (SmiIoCycle::PORT.val(u64::from(port))
        + SmiIoCycle::DATA.val(u64::from(command))
        + SmiIoCycle::ENABLE::SET
        + SmiIoCycle::READ::CLEAR)
        .modify(trigger);
    let pfeh_new = PfehConfig::ENABLE::SET.modify(pfeh);
    let redirect_new = MceRedirect::SMI_ENABLE::SET.modify(redirect);
    // Establish the target and exit mechanism before redirecting any #MC.
    // Bank cloaking and the deferred interrupt mask remain zero.
    cpu.reg(SMI_IO_CYCLE).set(trigger_new);
    cpu.reg(PFEH_CONFIG).set(pfeh_new);
    cpu.reg(MCE_REDIRECT).set(redirect_new);
    if cpu.reg(SMI_IO_CYCLE).get() != trigger_new
        || cpu.reg(PFEH_CONFIG).get() != pfeh_new
        || cpu.reg(MCE_REDIRECT).get() != redirect_new
    {
        // Stop redirection before undoing its target.
        cpu.reg(MCE_REDIRECT).set(redirect);
        cpu.reg(PFEH_CONFIG).set(pfeh);
        cpu.reg(SMI_IO_CYCLE).set(trigger);
        return Err(ArmError::Readback);
    }
    Ok(())
}

/// Execute only an OS-requested zero status clear. PFEH may intercept these
/// writes even with no banks cloaked. Never turn an arbitrary saved RCX into
/// an unchecked WRMSR in SMM: unsupported writes must retry in normal mode.
/// Facts: Rome AmdRasSspSmm (GUID/hash above), 0xafc, instruction-checked;
/// saved RCX/RAX/RDX at SMMxFFF0/FFF8/FFE8, no vendor RIP adjustment on replay.
pub fn replay_clear<M: MsrAccess>(cpu: &M, index: u32, value: u64) -> bool {
    if value != 0 {
        return false;
    }
    let banks = (cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32).min(MAX_BANKS);
    let legacy = index
        .checked_sub(0x400)
        .is_some_and(|off| off < 0x80 && off / 4 < banks && off % 4 == 1);
    let extended = index.checked_sub(0xc000_2000).is_some_and(|off| {
        let bank = off / 16;
        bank < banks
            && (off % 16 == 1
                || (off % 16 == 8
                    && super::mca::McaConfig::DEFERRED_LOGGING_SUPPORTED
                        .is_set(cpu.read(super::mca::config(bank)))))
    });
    if index != MCG_STATUS.index() && !legacy && !extended {
        return false;
    }
    cpu.write(index, 0);
    true
}

/// Permanently yield this thread back to native MCA. Used before retrying an
/// unsupported intercepted WRMSR outside SMM, where normal #GP semantics apply.
pub fn disarm<M: MsrAccess>(cpu: &M) {
    cpu.reg(MCE_REDIRECT).modify(MceRedirect::SMI_ENABLE::CLEAR);
    cpu.reg(PFEH_CONFIG).modify(
        PfehConfig::ENABLE::CLEAR
            + PfehConfig::DEFERRED_ON_EXIT::CLEAR
            + PfehConfig::THRESHOLD_ON_EXIT::CLEAR
            + PfehConfig::MCE_ON_EXIT::CLEAR,
    );
    cpu.reg(SMI_IO_CYCLE).modify(SmiIoCycle::ENABLE::CLEAR);
}

/// Raw evidence, not an assertion that a DIMM failed or that recovery is safe.
/// `cpu` is the installer's logical index, not a socket or an APIC ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub cpu: u32,
    pub bank: u32,
    pub status: u64,
    pub address: u64,
    pub misc0: u64,
    pub config: u64,
    pub ipid: u64,
    pub syndrome: u64,
    pub deferred_status: u64,
    pub deferred_address: u64,
    pub misc1: u64,
    pub global_status: u64,
}

impl Record {
    pub const EMPTY: Self = Self {
        cpu: 0,
        bank: 0,
        status: 0,
        address: 0,
        misc0: 0,
        config: 0,
        ipid: 0,
        syndrome: 0,
        deferred_status: 0,
        deferred_address: 0,
        misc1: 0,
        global_status: 0,
    };
}

/// Inspect a redirected #MC on this thread and arrange native #MC on RSM.
/// No bank, threshold, MCG_STATUS, mask or OS save-state register is written.
/// The sink must be bounded and must not wait for another SMM entrant.
/// A full/failed sink cannot prevent the native handoff, even with no valid bank.
#[inline(always)]
pub fn observe<M: MsrAccess>(
    cpu: &M,
    logical_cpu: u32,
    local_status: u32,
    mut sink: impl FnMut(Record),
) {
    if !LocalSmiStatus::MCE_REDIRECT.is_set(local_status) {
        return;
    }
    cpu.reg(PFEH_CONFIG).modify(PfehConfig::MCE_ON_EXIT::SET);
    let global_status = cpu.reg(MCG_STATUS).get();
    let banks = (cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32).min(MAX_BANKS);
    for bank in 0..banks {
        let base = 0xc000_2000 + bank * 16;
        let status = cpu.reg(Msr::<McaStatus::Register>::new(base + 1)).extract();
        if !status.is_set(McaStatus::VALID) {
            continue;
        }
        let config = cpu.read(base + 4);
        let ipid = cpu.read(base + 5);
        let deferred_status = if super::mca::McaConfig::DEFERRED_LOGGING_SUPPORTED.is_set(config) {
            cpu.read(base + 8)
        } else {
            0
        };
        sink(Record {
            cpu: logical_cpu,
            bank,
            status: status.get(),
            address: if status.is_set(McaStatus::ADDRESS_VALID) {
                cpu.read(base + 2)
            } else {
                0
            },
            misc0: cpu.read(base + 3),
            config,
            ipid,
            syndrome: if status.is_set(McaStatus::SYNDROME_VALID) {
                cpu.read(base + 6)
            } else {
                0
            },
            deferred_status,
            deferred_address: if McaStatus::VALID.is_set(deferred_status)
                && McaStatus::ADDRESS_VALID.is_set(deferred_status)
            {
                cpu.read(base + 9)
            } else {
                0
            },
            misc1: if super::mca::McaIpid::HARDWARE_ID.read(ipid) == 0x96 {
                cpu.read(base + 10)
            } else {
                0
            },
            global_status,
        });
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;
    use std::vec::Vec;

    #[test]
    fn arm_leaves_banks_os_owned_and_preserves_reserved_fields() {
        let cpu = MsrSpace::new([
            (0x179, 1),
            (0xc0002004, 1),
            (SMI_IO_CYCLE.index(), 1 << 40),
            (PFEH_CONFIG.index(), 1 << 40),
        ]);
        arm(&cpu, Generation::Rome, 0, 0xb2, 0x80).unwrap();
        assert_eq!(cpu.read(SMI_IO_CYCLE.index()), (1 << 40) | 0x0280_00b2);
        assert_eq!(cpu.read(MCE_REDIRECT.index()), 1 << 9);
        assert_eq!(cpu.read(PFEH_CONFIG.index()), (1 << 40) | 1);
        let space = cpu.into_inner();
        assert!(!space.written(PFEH_CLOAK.index()));
        assert!(!space.written(PFEH_DEFERRED_MASK.index()));
        assert!(!space.written(0xc0002001));
    }

    #[test]
    fn rejects_old_naples_capacity_and_other_owners_without_writes() {
        for (generation, step, extra, error) in [
            (Generation::Naples, 1, (0, 0), ArmError::UnsupportedCpu),
            (Generation::Rome, 0, (0x179, 65), ArmError::UnsupportedBanks),
            (
                Generation::Milan,
                0,
                (PFEH_CLOAK.index(), 1),
                ArmError::AlreadyOwned,
            ),
            (
                Generation::Milan,
                0,
                (MCE_REDIRECT.index(), 1 << 8),
                ArmError::AlreadyOwned,
            ),
            (
                Generation::Milan,
                0,
                (SMI_IO_CYCLE.index(), 1 << 25),
                ArmError::AlreadyOwned,
            ),
        ] {
            let cpu = MsrSpace::new([(0x179, 1), (0xc0002004, 1), extra]);
            assert_eq!(arm(&cpu, generation, step, 0xb2, 0x80), Err(error));
            assert!(!cpu.into_inner().written(PFEH_CONFIG.index()));
        }
    }

    #[test]
    fn unrelated_smi_does_not_scan_or_manufacture_a_machine_check() {
        let cpu = MsrSpace::new([]);
        observe(&cpu, 0, 0, |_| panic!("not an MCA SMI"));
        assert!(!cpu.into_inner().written(PFEH_CONFIG.index()));
    }

    #[test]
    fn captures_umc_and_hands_off_without_clearing_any_evidence() {
        let status = (1 << 63) | (1 << 61) | (1 << 58) | (1 << 53);
        let cpu = MsrSpace::new([
            (0x179, 2),
            (0x17a, 5),
            (0xc0002001, status),
            (0xc0002002, 0x1234),
            (0xc0002004, 5),
            (0xc0002005, 0x96 << 32),
            (0xc0002006, 0x77),
            (0xc0002008, (1 << 63) | (1 << 58)),
            (0xc0002009, 0x4567),
            (0xc000200a, 0xaa),
            (PFEH_CONFIG.index(), 1),
        ]);
        let mut records = Vec::new();
        observe(&cpu, 255, 1 << 8, |r| records.push(r));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cpu, 255);
        assert_eq!(records[0].address, 0x1234);
        assert_eq!(records[0].syndrome, 0x77);
        assert_eq!(records[0].deferred_address, 0x4567);
        assert_eq!(records[0].misc1, 0xaa);
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 9);
        let space = cpu.into_inner();
        for msr in [0x17a, 0xc0002001, 0xc0002008, PFEH_CLOAK.index()] {
            assert!(!space.written(msr));
        }
    }

    #[test]
    fn only_os_requested_zero_status_clears_are_replayed() {
        for (index, value, allowed) in [
            (0x401, 0, true),
            (0xc0002001, 0, true),
            (0xc0002008, 0, true),
            (0x17a, 0, true),
            (0xc0002001, 1, false),
            (0xc0002018, 0, false),
            (0xc0002021, 0, false),
            (0xc0002004, 0, false),
            (0xc0010015, 0, false),
        ] {
            let cpu = MsrSpace::new([(0x179, 2), (0xc0002004, 5)]);
            assert_eq!(replay_clear(&cpu, index, value), allowed);
            assert_eq!(cpu.into_inner().writes.len(), usize::from(allowed));
        }
    }

    #[test]
    fn bad_readback_rolls_back_redirection_before_its_target() {
        use core::cell::RefCell;
        use fstart_arch::x86::cpu::msr::testing::MsrModel;
        struct Faulty(MsrSpace);
        impl MsrModel for Faulty {
            fn read(&mut self, index: u32) -> u64 {
                self.0.get(index)
            }
            fn write(&mut self, index: u32, value: u64) {
                self.0.writes.push((index, value));
                if index != PFEH_CONFIG.index() {
                    self.0.values.insert(index, value);
                }
            }
        }
        let cpu = RefCell::new(Faulty(
            MsrSpace::new([(0x179, 1), (0xc0002004, 1)]).into_inner(),
        ));
        assert_eq!(
            arm(&cpu, Generation::Rome, 0, 0xb2, 0x80),
            Err(ArmError::Readback)
        );
        let space = cpu.into_inner().0;
        let indices: Vec<_> = space.writes.iter().map(|&(index, _)| index).collect();
        assert_eq!(
            indices,
            [
                SMI_IO_CYCLE.index(),
                PFEH_CONFIG.index(),
                MCE_REDIRECT.index(),
                MCE_REDIRECT.index(),
                PFEH_CONFIG.index(),
                SMI_IO_CYCLE.index()
            ]
        );
        assert_eq!(space.get(MCE_REDIRECT.index()), 0);
        assert_eq!(space.get(SMI_IO_CYCLE.index()), 0);
    }

    #[test]
    fn absent_or_dropped_records_still_request_native_mce() {
        let cpu = MsrSpace::new([(0x179, 1), (PFEH_CONFIG.index(), 1)]);
        observe(&cpu, 0, 1 << 8, |_| {});
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 9);
        cpu.write(0xc0002001, 1 << 63);
        observe(&cpu, 0, 1 << 8, |_| {});
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 9);
    }
}
