//! Per-thread MCA work in SMM, without taking the OS's MCA banks.
//!
//! Two sources reach SMM through the thread's SMI trigger I/O cycle
//! ([`arm_trigger`], to the FCH's SMI command port, PPR 55901
//! §2.1.18.1.3):
//!
//! - thresholding: SMM counts corrected errors in a bank's MCA_MISC0 block
//!   with an SMI at the overflow and locks the block for the OS
//!   ([`arm_thresholds`], MCA_MISC0[Locked]: "BIOS should set this bit if
//!   ThresholdIntType is set to SMI"; Linux leaves locked blocks alone),
//!   then records and re-arms an overflowed counter ([`observe_mca`]). The
//!   OS still reads, logs and clears every bank;
//! - optionally machine checks, with platform-first error handling (PFEH,
//!   PPR 55901 vol. 2 §3.4, [`arm`]): #MC redirected to SMI, recorded
//!   ([`observe`]) and percolated back to the OS on RSM
//!   (`GenerateMceOnExit`). PFEH also sends deferred errors to SMM, which
//!   percolates them too, makes MCA_MISCx RAZ/WI for the OS, and turns the
//!   OS's writes to a bank's status into SMIs, which SMM replays
//!   ([`replay_clear`]). That SMI is the trigger I/O cycle's: it arrives a
//!   few instructions after the WRMSR (on Linux 7.3 after
//!   `native_write_msr` zeroed its registers and returned), so the saved
//!   registers may no longer name the write. Unlike the vendor's cloaking
//!   scheme no bank is cloaked, no status cleared, and the deferred-
//!   interrupt mask stays zero.
//!
//! Register facts: AMD #55570 and PPR 55901, MSRC001_0022/0056/0120 and
//! SMMxFEC4; OpenSIL cd8edaee, xUSL/Include/MsrReg.h (PFEH_CFG fields).
//! Donor check: Rome AmdRasSspSmm/ee8175ef-4c60-82af-f5e2-8a9ef1fc0a9a,
//! cca68f7070ef, 0x6b0 and 0x934 (SMM, instruction-checked).

use fstart_arch::x86::cpu::amd::msr::{HWCR, Hwcr};
use fstart_arch::x86::cpu::msr::{MCG_CAP, MCG_STATUS, McgCap, Msr, MsrAccess};
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use crate::ccx::msr::{McaConfig, McaIpid, McaMisc, mca_config, mca_ipid, mca_misc0};
use crate::cpu::Silicon;

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
        OVERFLOW OFFSET(62) NUMBITS(1) [],
        VALID OFFSET(63) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub LocalSmiStatus [
        MCE_REDIRECT OFFSET(8) NUMBITS(1) [],
        WRMSR OFFSET(11) NUMBITS(1) [],
        /// A deferred error or a thresholding overflow (DF::SmiSourceRegister[2]).
        SMI_SRC_MCA OFFSET(18) NUMBITS(1) []
    ]
];

pub const LOCAL_SMI_STATUS: u32 = 0xfec4;
pub const MCE_REDIRECT: Msr<MceRedirect::Register> = Msr::new(0xc001_0022);
pub const SMI_IO_CYCLE: Msr<SmiIoCycle::Register> = Msr::new(0xc001_0056);
pub const PFEH_CONFIG: Msr<PfehConfig::Register> = Msr::new(0xc001_0120);
pub const PFEH_CLOAK: Msr = Msr::new(0xc001_0121);
pub const PFEH_DEFERRED_MASK: Msr = Msr::new(0xc001_0122);
/// A bank mask is 64 bits. Refuse larger implementations, rather than
/// silently enabling a handler that cannot inspect all their banks.
pub const MAX_BANKS: u32 = 64;
/// MCA_IPID[HardwareID] of a unified memory controller's bank.
pub const UMC_HARDWARE_ID: u64 = 0x96;
/// Which threads share a bank's thresholding counter (MCA_MISC0), by the
/// PPRs' register instances: a counter one register instance names per
/// thread, per CCX or per die. PPR 54945 (ZP B2, Naples), "MCA::LS::
/// MCA_MISC0_LS" and the other core banks: `_lthree[1:0]_core[3:0]_
/// thread[1:0]` (one per thread, "Thread 0" in their titles), L3:
/// `_ccx0_inst7`... (per CCX), UMC, CS, PIE: `_instUMC_umc0`, `_instCS0`,
/// `_instPIE` (per die); PPR 55901 (Genoa B2) and 57228 (Siena A2) alike:
/// `_ccd[]_lthree[]_core[7:0]_thread[1:0]` for LS, IF, L2, DE, EX and FP,
/// `_ccd[]_lthree[]_inst7`... for the L3, no CCD for UMC, CS, PIE,
/// NBIO (`_iohub[1:0]_nbio[1:0]`), PCIE, SMU (`MPIO`, `MP1`) and PSP
/// (`MP0`): the IOD's, one per socket and node. Rome and Milan sit
/// between the two (their PPRs, 55803 and 55898, are not in the tree):
/// the same core and L3 instances, an IOD per socket. Linux decodes the
/// same pairs (`smca_hwid_mcatypes`) and keeps every bank per CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One thread's: SMT siblings each have their own (LS, IF, L2, DE,
    /// EX, FP).
    Thread,
    /// The threads of a CCX, which share its L3 (L3).
    Ccx,
    /// The node's (a Naples die, a socket's IOD): UMC, CS, PIE, NBIO,
    /// PCIE, SMU, PSP.
    Node,
}

impl Scope {
    /// The scope of the bank with MCA_IPID `ipid`, by {HardwareID,
    /// McaType} (PPR 55901 3.1.5.5, Table 42), or `None` for a bank
    /// whose counter's scope is not established here: SMM does not arm
    /// it. MP5 (01h, 2) and PB (05h, 0) have an instance per CCD on Genoa
    /// besides the IOD's, under the same pair; the I/O blocks the PPRs
    /// list without a threshold (NBIF, SATA, USB, SHUB, the PCS and PHY
    /// banks), MPDMA and every unknown pair are left out too.
    pub fn of(ipid: u64) -> Option<Self> {
        match (
            McaIpid::HARDWARE_ID.read(ipid),
            McaIpid::MCA_TYPE.read(ipid),
        ) {
            (CORE_HARDWARE_ID, 0x0 | 0x10 | 0x1 | 0x2 | 0x3 | 0x5 | 0x6) => Some(Self::Thread),
            (CORE_HARDWARE_ID, L3_MCA_TYPE) => Some(Self::Ccx),
            (UMC_HARDWARE_ID, 0x0 | 0x1)
            | (0x2e, 0x0..=0x2)
            | (0x18, 0x0)
            | (0x46, 0x0 | 0x1)
            | (0x01, 0x0 | 0x1)
            | (0xff, 0x0 | 0x1) => Some(Self::Node),
            _ => None,
        }
    }
}

/// MCA_IPID[HardwareID] of a core's banks: LS (McaType 0, 10h on Genoa),
/// IF (1), L2 (2), DE (3), EX (5), FP (6) and the L3 ([`L3_MCA_TYPE`]).
pub const CORE_HARDWARE_ID: u64 = 0xb0;
/// MCA_IPID[McaType] of an L3 slice's bank, under [`CORE_HARDWARE_ID`].
pub const L3_MCA_TYPE: u64 = 0x07;

/// Where the executing thread sits: what [`Scope`]s it shares. An
/// MCA_IPID names a bank's instance within its scope, not a register:
/// every core's LS bank has the same IPID (the Onyx: a reservation by
/// node and IPID armed only the first core's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sharing {
    /// The extended APIC ID (CPUID 8000_001E EAX), unique in the system.
    pub apic_id: u32,
    /// log2 of the threads sharing the L3 (CPUID 8000_001D, its level-3
    /// leaf, EAX\[25:14\] + 1).
    pub ccx_shift: u32,
    /// The node (CPUID 8000_001E ECX\[7:0\]).
    pub node: u32,
}

impl Sharing {
    /// The executing thread's.
    pub fn current() -> Self {
        let log2 = |n: u32| n.max(1).next_power_of_two().trailing_zeros();
        let (apic_id, _, ecx, _) = fstart_arch::x86::cpuid(0x8000_001e);
        let l3 = (0..8)
            .map(|leaf| fstart_arch::x86::cpuid_count(0x8000_001d, leaf).0)
            .take_while(|eax| eax & 0x1f != 0)
            .find(|eax| eax >> 5 & 7 == 3)
            .map_or(1, |eax| (eax >> 14 & 0xfff) + 1);
        Self {
            apic_id,
            ccx_shift: log2(l3),
            node: ecx & 0xff,
        }
    }

    /// The bank with MCA_IPID `ipid` as one counter: its [`Scope`]'s
    /// instance (the thread or the CCX by the APIC ID, the node) and its
    /// IPID. Two threads have the same key for a bank exactly when they
    /// share its counter; `None` for a bank of no known scope.
    pub fn key(&self, ipid: u64) -> Option<(u64, u64)> {
        let scope = match Scope::of(ipid)? {
            Scope::Thread => 1 << 32 | u64::from(self.apic_id),
            Scope::Ccx => 2 << 32 | u64::from(self.apic_id >> self.ccx_shift),
            Scope::Node => 3 << 32 | u64::from(self.node),
        };
        Some((scope, ipid))
    }
}
/// The largest thresholding count: ErrCnt starts at FFFh - count, and a
/// start of FFFh is not supported.
pub const MAX_THRESHOLD: u16 = 0xffe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmError {
    UnsupportedCpu,
    UnsupportedBanks,
    AlreadyOwned,
    Readback,
}

/// The banks this thread sees: MCG_CAP's count, at most [`MAX_BANKS`].
fn banks<M: MsrAccess>(cpu: &M) -> u32 {
    (cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32).min(MAX_BANKS)
}

/// MCAX register `offset` of `bank`.
const fn mcax(bank: u32, offset: u32) -> u32 {
    0xc000_2000 + bank * 16 + offset
}
/// The thread's SMI trigger I/O cycle: a write of `command` to `port`
/// (the FCH's SMI command port) for its local SMI sources. Refused if
/// someone else owns it.
pub fn arm_trigger<M: MsrAccess>(cpu: &M, port: u16, command: u8) -> Result<(), ArmError> {
    let trigger = cpu.reg(SMI_IO_CYCLE).get();
    if SmiIoCycle::ENABLE.is_set(trigger) {
        return Err(ArmError::AlreadyOwned);
    }
    let new = (SmiIoCycle::PORT.val(u64::from(port))
        + SmiIoCycle::DATA.val(u64::from(command))
        + SmiIoCycle::ENABLE::SET
        + SmiIoCycle::READ::CLEAR)
        .modify(trigger);
    cpu.reg(SMI_IO_CYCLE).set(new);
    if cpu.reg(SMI_IO_CYCLE).get() != new {
        cpu.reg(SMI_IO_CYCLE).set(trigger);
        return Err(ArmError::Readback);
    }
    Ok(())
}

/// Arm PFEH on the executing thread, only after its permanent SMM entry is
/// installed: the trigger I/O cycle, PFEH and #MC redirected to SMI.
/// Existing PFEH, interrupt redirection and local-trigger ownership is refused.
/// Naples before B2 is deliberately excluded (the vendor warns against B1 PFEH).
/// The caller must arrange to inspect this CPU's local status on *every* SMI.
pub fn arm<M: MsrAccess>(
    cpu: &M,
    generation: Silicon,
    stepping: u8,
    port: u16,
    command: u8,
) -> Result<(), ArmError> {
    if generation == Silicon::Zp && stepping < 2 {
        return Err(ArmError::UnsupportedCpu);
    }
    let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT);
    if banks == 0 || banks > u64::from(MAX_BANKS) || !cpu.reg(mca_config(0)).is_set(McaConfig::MCAX)
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

/// PFEH is on: the thread's MCA events come to SMM.
pub fn pfeh<M: MsrAccess>(cpu: &M) -> bool {
    cpu.reg(PFEH_CONFIG).is_set(PfehConfig::ENABLE)
}

/// Run `f` with HWCR.McStatusWrEn set: a locked thresholding block's
/// fields are writable only so.
fn status_writable<M: MsrAccess, T>(cpu: &M, f: impl FnOnce() -> T) -> T {
    let hwcr = cpu.reg(HWCR);
    let saved = hwcr.get();
    hwcr.set(Hwcr::MC_STATUS_WR_EN::SET.modify(saved));
    let result = f();
    hwcr.set(saved);
    result
}

/// MCA_MISC0 counting `count` errors from now on, with an SMI at the
/// overflow, locked for the OS; the other fields as they are.
fn counting(misc: u64, count: u16) -> u64 {
    let count = u64::from(count.clamp(1, MAX_THRESHOLD));
    (McaMisc::ERR_CNT.val(0xfff - count)
        + McaMisc::OVRFLW::CLEAR
        + McaMisc::THRESHOLD_INT_TYPE::Smi
        + McaMisc::CNT_EN::SET
        + McaMisc::LOCKED::SET)
        .modify(misc)
}

/// A thresholding block SMM may count with: valid, with a counter that
/// can interrupt and that nobody locked.
fn countable(misc: LocalRegisterCopy<u64, McaMisc::Register>) -> bool {
    misc.is_set(McaMisc::VALID)
        && misc.is_set(McaMisc::CNT_P)
        && misc.is_set(McaMisc::INT_P)
        && !misc.is_set(McaMisc::LOCKED)
}

/// Count `count` corrected errors in the MCA_MISC0 thresholding block of
/// every bank of this thread that has one free, with an SMI at the
/// overflow, and lock the block for the OS (MCA_MISC0[Locked], writable
/// only with HWCR.McStatusWrEn). With [`arm_trigger`] (or [`arm`]) done
/// first. Returns the banks armed.
///
/// A shared bank (an SMT core's banks, a CCX's L3, a socket's UMCs) is
/// one block whichever thread sees it, and its counter one register: one
/// thread writes it. `reserve` (the bank and its MCA_IPID) makes this
/// thread the only one arming, or refuses (then the bank is left); under
/// the reservation the block is checked again (a sibling may have locked
/// it meanwhile) and armed, then `publish`ed with whether it was. The
/// caller's ownership (then `observe_mca`'s `owns`) is by the block, not
/// by the thread that armed it.
pub fn arm_thresholds<M: MsrAccess, T>(
    cpu: &M,
    count: u16,
    mut reserve: impl FnMut(u32, u64) -> Option<T>,
    mut publish: impl FnMut(T, bool),
) -> u64 {
    status_writable(cpu, || {
        let mut armed = 0;
        for bank in 0..banks(cpu) {
            let misc = cpu.reg(mca_misc0(bank));
            if !countable(misc.extract()) {
                continue;
            }
            let Some(reservation) = reserve(bank, cpu.reg(mca_ipid(bank)).get()) else {
                continue;
            };
            // Another thread of the share may have locked it before.
            let value = misc.extract();
            let countable = countable(value);
            if countable {
                misc.set(counting(value.get(), count));
                armed |= 1 << bank;
            }
            publish(reservation, countable);
        }
        armed
    })
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
    let banks = banks(cpu);
    let legacy = index
        .checked_sub(0x400)
        .is_some_and(|off| off < 0x80 && off / 4 < banks && off % 4 == 1);
    let extended = index.checked_sub(0xc000_2000).is_some_and(|off| {
        let bank = off / 16;
        bank < banks
            && (off % 16 == 1
                || (off % 16 == 8
                    && cpu
                        .reg(mca_config(bank))
                        .is_set(McaConfig::DEFERRED_LOGGING_SUPPORTED)))
    });
    if index != MCG_STATUS.index() && !legacy && !extended {
        return false;
    }
    cpu.write(index, 0);
    true
}

/// Permanently yield this thread's machine checks back to native MCA: no
/// #MC redirection, no PFEH. The trigger I/O cycle stays (its local
/// sources may still fire: PPR 57228 calls disabling it then undefined),
/// and so do the thresholds SMM counts.
pub fn disarm<M: MsrAccess>(cpu: &M) {
    cpu.reg(MCE_REDIRECT).modify(MceRedirect::SMI_ENABLE::CLEAR);
    cpu.reg(PFEH_CONFIG).modify(
        PfehConfig::ENABLE::CLEAR
            + PfehConfig::DEFERRED_ON_EXIT::CLEAR
            + PfehConfig::THRESHOLD_ON_EXIT::CLEAR
            + PfehConfig::MCE_ON_EXIT::CLEAR,
    );
}

/// What brought a [`Record`] to light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A machine-check exception redirected to SMI.
    MachineCheck,
    /// A thresholding counter overflowed: `count` corrected errors.
    Threshold,
    /// A deferred error (uncorrected, not consumed).
    Deferred,
}

/// Raw evidence, not an assertion that a DIMM failed or that recovery is safe.
/// `cpu` is the SMM installer's logical index of the thread that read it,
/// not a socket or an APIC ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
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
        kind: Kind::MachineCheck,
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

    /// Uncorrected: logged as uncorrected or deferred, in MCA_STATUS or
    /// in MCA_DESTAT.
    pub fn uncorrected(&self) -> bool {
        let status = LocalRegisterCopy::<u64, McaStatus::Register>::new(self.status);
        status.is_set(McaStatus::UNCORRECTED)
            || status.is_set(McaStatus::DEFERRED)
            || McaStatus::VALID.is_set(self.deferred_status)
    }

    /// The processor context is corrupt: the error is fatal.
    pub fn fatal(&self) -> bool {
        let status = LocalRegisterCopy::<u64, McaStatus::Register>::new(self.status);
        status.is_set(McaStatus::UNCORRECTED) && status.is_set(McaStatus::CONTEXT_CORRUPT)
    }

    /// The bank is a memory controller's (a DRAM error).
    pub fn memory(&self) -> bool {
        McaIpid::HARDWARE_ID.read(self.ipid) == UMC_HARDWARE_ID
    }

    /// The same error as `other`, read again (from the same bank).
    pub fn same(&self, other: &Self) -> bool {
        (self.bank, self.ipid, self.status, self.address)
            == (other.bank, other.ipid, other.status, other.address)
            && (self.deferred_status, self.deferred_address)
                == (other.deferred_status, other.deferred_address)
    }
}

/// Read `bank` of this thread into a [`Record`] of `kind`, if it logged
/// an error in MCA_STATUS or (deferred) in MCA_DESTAT. Writes nothing.
pub fn read<M: MsrAccess>(cpu: &M, logical_cpu: u32, bank: u32, kind: Kind) -> Option<Record> {
    let status = cpu
        .reg(Msr::<McaStatus::Register>::new(mcax(bank, 1)))
        .extract();
    let config = cpu.read(mcax(bank, 4));
    let deferred_status = if McaConfig::DEFERRED_LOGGING_SUPPORTED.is_set(config) {
        cpu.read(mcax(bank, 8))
    } else {
        0
    };
    if !status.is_set(McaStatus::VALID) && !McaStatus::VALID.is_set(deferred_status) {
        return None;
    }
    let ipid = cpu.read(mcax(bank, 5));
    Some(Record {
        kind,
        cpu: logical_cpu,
        bank,
        status: status.get(),
        address: if status.is_set(McaStatus::ADDRESS_VALID) {
            cpu.read(mcax(bank, 2))
        } else {
            0
        },
        misc0: cpu.read(mcax(bank, 3)),
        config,
        ipid,
        syndrome: if status.is_set(McaStatus::SYNDROME_VALID) {
            cpu.read(mcax(bank, 6))
        } else {
            0
        },
        deferred_status,
        deferred_address: if McaStatus::VALID.is_set(deferred_status)
            && McaStatus::ADDRESS_VALID.is_set(deferred_status)
        {
            cpu.read(mcax(bank, 9))
        } else {
            0
        },
        misc1: if McaIpid::HARDWARE_ID.read(ipid) == UMC_HARDWARE_ID {
            cpu.read(mcax(bank, 10))
        } else {
            0
        },
        global_status: cpu.reg(MCG_STATUS).get(),
    })
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
    for bank in 0..banks(cpu) {
        let Some(record) = read(cpu, logical_cpu, bank, Kind::MachineCheck) else {
            continue;
        };
        if LocalRegisterCopy::<u64, McaStatus::Register>::new(record.status)
            .is_set(McaStatus::VALID)
        {
            sink(record);
        }
    }
}

/// What an `SmiSrcMca` SMI found on this thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McaEvents {
    /// SMM's thresholding counters that overflowed, recorded (and re-armed).
    pub thresholds: u64,
    /// Overflowed counters that are not SMM's (locked by someone else):
    /// not recorded, not written.
    pub unowned: u64,
    /// Banks holding a deferred error.
    pub deferred: u64,
}

/// Inspect a deferred-error or thresholding SMI (`SmiSrcMca`) on this
/// thread: record every overflowed counter SMM owns (its MCA_STATUS holds
/// the last of the errors it counted) and every bank holding a deferred
/// error, re-arm SMM's overflowed counters for `rearm` more errors (none:
/// they stay overflowed and quiet), and with PFEH percolate the
/// deferred-error and thresholding interrupts to the OS on RSM (both if
/// nothing was found, so no event is lost). MCA state is not written: the
/// OS still reads and clears every bank.
///
/// `owns` (the bank and its MCA_IPID) says whether an overflowed block is
/// one SMM armed ([`arm_thresholds`]), on any thread of its share, and
/// takes it for this SMI; a block it does not own (locked before SMM
/// armed, or by another owner) is neither recorded nor written. A block
/// another thread of its share handled meanwhile (no longer overflowed)
/// is left too.
#[inline(always)]
pub fn observe_mca<M: MsrAccess>(
    cpu: &M,
    logical_cpu: u32,
    local_status: u32,
    rearm: Option<u16>,
    mut owns: impl FnMut(u32, u64) -> bool,
    mut sink: impl FnMut(Record),
) -> Option<McaEvents> {
    if !LocalSmiStatus::SMI_SRC_MCA.is_set(local_status) {
        return None;
    }
    let overflowed = |value: LocalRegisterCopy<u64, McaMisc::Register>| {
        value.is_set(McaMisc::VALID)
            && value.is_set(McaMisc::CNT_P)
            && value.is_set(McaMisc::OVRFLW)
    };
    let mut events = McaEvents::default();
    for bank in 0..banks(cpu) {
        let misc = cpu.reg(mca_misc0(bank));
        let mut threshold = false;
        if overflowed(misc.extract()) {
            if !owns(bank, cpu.reg(mca_ipid(bank)).get()) {
                events.unowned |= 1 << bank;
            } else if let value = misc.extract()
                && overflowed(value)
            {
                threshold = true;
                events.thresholds |= 1 << bank;
                if let Some(record) = read(cpu, logical_cpu, bank, Kind::Threshold) {
                    sink(record);
                }
                if let Some(count) = rearm {
                    status_writable(cpu, || misc.set(counting(value.get(), count)));
                }
            }
        }
        if !threshold
            && let Some(record) = read(cpu, logical_cpu, bank, Kind::Deferred)
                .filter(Record::uncorrected)
                .filter(|r| {
                    McaStatus::DEFERRED.is_set(r.status)
                        || McaStatus::VALID.is_set(r.deferred_status)
                })
        {
            events.deferred |= 1 << bank;
            sink(record);
        }
    }
    if !pfeh(cpu) {
        return Some(events);
    }
    let both = events == McaEvents::default();
    let pfeh = cpu.reg(PFEH_CONFIG);
    if both || events.deferred != 0 {
        pfeh.modify(PfehConfig::DEFERRED_ON_EXIT::SET);
    }
    if both || events.thresholds != 0 || events.unowned != 0 {
        pfeh.modify(PfehConfig::THRESHOLD_ON_EXIT::SET);
    }
    Some(events)
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
        arm(&cpu, Silicon::Ssp, 0, 0xb2, 0x80).unwrap();
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
            (Silicon::Zp, 1, (0, 0), ArmError::UnsupportedCpu),
            (Silicon::Ssp, 0, (0x179, 65), ArmError::UnsupportedBanks),
            (
                Silicon::Gn,
                0,
                (PFEH_CLOAK.index(), 1),
                ArmError::AlreadyOwned,
            ),
            (
                Silicon::Gn,
                0,
                (MCE_REDIRECT.index(), 1 << 8),
                ArmError::AlreadyOwned,
            ),
            (
                Silicon::Gn,
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
            arm(&cpu, Silicon::Ssp, 0, 0xb2, 0x80),
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

    #[test]
    fn thresholds_count_in_smm_only_where_a_free_counter_exists() {
        // Valid | CntP | IntP, BlkPtr 1; bank 1 is locked; bank 2 has no counter.
        let free = (1 << 63) | (1 << 62) | (1 << 60) | (1 << 24);
        let cpu = MsrSpace::new([
            (0x179, 3),
            (0xc0002003, free),
            (0xc0002013, free | (1 << 61)),
            (0xc0002023, 1 << 63),
        ]);
        assert_eq!(arm_thresholds(&cpu, 10, |_, _| Some(()), |_, _| {}), 0b1);
        // ErrCnt FFF - 10, IntType SMI, CntEn, Locked; BlkPtr kept.
        assert_eq!(
            cpu.read(0xc0002003),
            free | (0xff5 << 32) | (2 << 49) | (1 << 51) | (1 << 61)
        );
        // Written with McStatusWrEn, which is clear again.
        assert_eq!(cpu.read(HWCR.index()), 0);
        let space = cpu.into_inner();
        assert!(space.writes.contains(&(HWCR.index(), 1 << 18)));
        assert!(!space.written(0xc0002013));
        assert!(!space.written(0xc0002023));
    }

    #[test]
    fn threshold_overflow_is_recorded_rearmed_and_percolated() {
        let overflowed =
            (1 << 63) | (1 << 62) | (1 << 60) | (0xfff << 32) | (1 << 48) | (2 << 49) | (1 << 51);
        let ce = (1 << 63) | (1 << 58);
        let cpu = MsrSpace::new([
            (0x179, 2),
            (0xc0002001, ce),
            (0xc0002002, 0x1000),
            (0xc0002003, overflowed),
            (0xc0002005, 0x96 << 32),
            (PFEH_CONFIG.index(), 1),
        ]);
        let mut records = Vec::new();
        assert_eq!(
            observe_mca(&cpu, 7, 0, Some(10), |_, _| true, |r| records.push(r)),
            None
        );
        let events =
            observe_mca(&cpu, 7, 1 << 18, Some(10), |_, _| true, |r| records.push(r)).unwrap();
        assert_eq!(
            events,
            McaEvents {
                thresholds: 1,
                ..McaEvents::default()
            }
        );
        assert_eq!(records.len(), 1);
        assert_eq!(
            (records[0].kind, records[0].cpu, records[0].address),
            (Kind::Threshold, 7, 0x1000)
        );
        assert!(records[0].memory() && !records[0].uncorrected());
        // Re-armed for ten more, overflow cleared; threshold LVT on exit only.
        assert_eq!(cpu.read(0xc0002003) >> 32 & 0xfff, 0xff5);
        assert_eq!(cpu.read(0xc0002003) & (1 << 48), 0);
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 1 | 4);
        let space = cpu.into_inner();
        assert!(!space.written(0xc0002001), "the OS still owns the status");
    }

    /// In one SMI: bank 0's counter, which SMM armed, and bank 1's, which
    /// was locked before (another owner's), both overflowed. SMM records
    /// and re-arms its own and leaves the other as it is.
    #[test]
    fn only_smms_overflowed_counters_are_recorded_and_written() {
        let armed = (1 << 63) | (1 << 62) | (1 << 61) | (1 << 60) | (2 << 49) | (1 << 51);
        let overflowed = armed | (0xfff << 32) | (1 << 48);
        // Locked, interrupting through the APIC (IntType 1), overflowed.
        let foreign = (1 << 63) | (1 << 62) | (1 << 61) | (1 << 60) | (1 << 49) | (1 << 51);
        let foreign = foreign | (0xfff << 32) | (1 << 48);
        let ce = (1 << 63) | (1 << 58);
        let cpu = MsrSpace::new([
            (0x179, 2),
            (0xc0002001, ce),
            (0xc0002003, overflowed),
            (0xc0002005, 0x96 << 32 | 1),
            (0xc0002011, ce),
            (0xc0002013, foreign),
            (0xc0002015, 0x96 << 32 | 2),
            (PFEH_CONFIG.index(), 1),
        ]);
        let mut records = Vec::new();
        let mut asked = Vec::new();
        let events = observe_mca(
            &cpu,
            3,
            1 << 18,
            Some(10),
            |bank, ipid| {
                asked.push((bank, ipid));
                ipid == 0x96 << 32 | 1
            },
            |r| records.push(r),
        )
        .unwrap();
        assert_eq!(
            events,
            McaEvents {
                thresholds: 0b01,
                unowned: 0b10,
                deferred: 0
            }
        );
        assert_eq!(asked, [(0, 0x96 << 32 | 1), (1, 0x96 << 32 | 2)]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bank, 0);
        assert_eq!(cpu.read(0xc0002003) >> 32 & 0xfff, 0xff5);
        assert_eq!(cpu.read(0xc0002003) & (1 << 48), 0);
        // The OS's threshold interrupt on exit, for both.
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 1 | 4);
        let space = cpu.into_inner();
        assert!(!space.written(0xc0002013), "not SMM's counter");
        assert!(!space.written(0xc0002011));
    }

    /// Arming asks for each free block, with its IPID; a refused one stays
    /// free.
    #[test]
    fn a_block_arming_could_not_claim_stays_free() {
        let free = (1 << 63) | (1 << 62) | (1 << 60);
        let cpu = MsrSpace::new([
            (0x179, 2),
            (0xc0002003, free),
            (0xc0002005, 7),
            (0xc0002013, free),
            (0xc0002015, 8),
        ]);
        let mut published = Vec::new();
        let armed = arm_thresholds(
            &cpu,
            10,
            |bank, ipid| (ipid == 8).then_some(bank),
            |bank, armed| published.push((bank, armed)),
        );
        assert_eq!(armed, 0b10);
        assert_eq!(published, [(1, true)]);
        assert!(!cpu.into_inner().written(0xc0002003));
    }

    /// Two threads of a share arm its one block at once: the one whose
    /// reservation holds writes it, the other leaves it. And a thread
    /// that reserved a block a sibling locked in the meantime finds it
    /// locked under the reservation, leaves it and publishes it as not
    /// armed.
    #[test]
    fn aliases_arming_one_block_write_it_once() {
        let free = (1 << 63) | (1 << 62) | (1 << 60);
        let shared = MsrSpace::new([(0x179, 1), (0xc0002003, free), (0xc0002005, 0x96 << 32)]);
        let reserved = core::cell::Cell::new(false);
        let mut sibling = None;
        let first = arm_thresholds(
            &shared,
            10,
            |_, _| {
                reserved.set(true);
                // The sibling arms now, on the same block.
                sibling = Some(arm_thresholds(
                    &shared,
                    10,
                    |_, _| (!reserved.get()).then_some(()),
                    |_, _| panic!("the sibling reserved nothing"),
                ));
                Some(())
            },
            |_, armed| assert!(armed),
        );
        assert_eq!((first, sibling), (1, Some(0)));
        let misc_writes = |space: &MsrSpace| {
            space
                .writes
                .iter()
                .filter(|&&(index, _)| index == 0xc0002003)
                .count()
        };
        assert_eq!(misc_writes(&shared.borrow()), 1);

        let late = MsrSpace::new([(0x179, 1), (0xc0002003, free), (0xc0002005, 0x96 << 32)]);
        let mut published = None;
        let armed = arm_thresholds(
            &late,
            10,
            |_, _| {
                // The sibling locked it between the check and the
                // reservation.
                late.borrow_mut().values.insert(0xc0002003, free | 1 << 61);
                Some(())
            },
            |_, armed| published = Some(armed),
        );
        assert_eq!((armed, published), (0, Some(false)));
        assert_eq!(misc_writes(&late.borrow()), 0);
    }

    #[test]
    fn deferred_error_is_recorded_and_its_interrupt_percolated() {
        let deferred = (1 << 63) | (1 << 44) | (1 << 58);
        let cpu = MsrSpace::new([
            (0x179, 1),
            (0xc0002001, deferred),
            (0xc0002004, 4),
            (0xc0002008, deferred),
            (0xc0002009, 0x2000),
            (PFEH_CONFIG.index(), 1),
        ]);
        let mut records = Vec::new();
        let events = observe_mca(&cpu, 0, 1 << 18, None, |_, _| true, |r| records.push(r)).unwrap();
        assert_eq!(events.deferred, 1);
        assert_eq!(records[0].kind, Kind::Deferred);
        assert_eq!(records[0].deferred_address, 0x2000);
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 1 | 2);
    }

    #[test]
    fn without_pfeh_a_threshold_smi_is_rearmed_but_not_percolated() {
        let overflowed = (1 << 63) | (1 << 62) | (1 << 61) | (1 << 60) | (0xfff << 32) | (1 << 48);
        let cpu = MsrSpace::new([(0x179, 1), (0xc0002003, overflowed)]);
        let events = observe_mca(&cpu, 0, 1 << 18, Some(1), |_, _| true, |_| {}).unwrap();
        assert_eq!(events.thresholds, 1);
        assert_eq!(cpu.read(0xc0002003) >> 32 & 0xfff, 0xffe);
        assert!(!cpu.into_inner().written(PFEH_CONFIG.index()));
    }

    /// Each pair decodes to its PPR scope: the core banks per thread, the
    /// L3 (B0h with McaType 7, not a HardwareID 7) per CCX, the IOD's and
    /// a die's per node; a pair of no established scope, none.
    #[test]
    fn a_bank_s_scope_is_its_hardware_id_and_type() {
        let ipid = |hwid: u64, kind: u64| kind << 48 | hwid << 32 | 0x0035_0f00;
        for kind in [0x0, 0x10, 0x1, 0x2, 0x3, 0x5, 0x6] {
            assert_eq!(
                Scope::of(ipid(0xb0, kind)),
                Some(Scope::Thread),
                "{kind:#x}"
            );
        }
        assert_eq!(Scope::of(ipid(0xb0, 0x7)), Some(Scope::Ccx));
        for (hwid, kind) in [
            (0x96, 0),
            (0x96, 1),
            (0x2e, 0),
            (0x2e, 1),
            (0x2e, 2),
            (0x18, 0),
            (0x46, 0),
            (0x46, 1),
            (0x01, 0),
            (0x01, 1),
            (0xff, 0),
            (0xff, 1),
        ] {
            assert_eq!(
                Scope::of(ipid(hwid, kind)),
                Some(Scope::Node),
                "{hwid:#x}/{kind}"
            );
        }
        for (hwid, kind) in [
            (0x07, 0),
            (0xb0, 0x4),
            (0x01, 2),
            (0x05, 0),
            (0x01, 3),
            (0x6c, 0),
            (0x2e, 4),
            (0x00, 0),
        ] {
            assert_eq!(Scope::of(ipid(hwid, kind)), None, "{hwid:#x}/{kind}");
        }
    }

    /// SMT siblings do not share a thread's counter; a CCX's cores share
    /// its L3's, other CCXs do not; a node's threads share its UMC's;
    /// a bank of no known scope has no key.
    #[test]
    fn a_banks_key_is_its_hardware_instance() {
        let thread = |apic_id, node| Sharing {
            apic_id,
            ccx_shift: 4,
            node,
        };
        let ls = 0x10 << 48 | 0xb0 << 32;
        let l3 = 0x07 << 48 | 0xb0 << 32 | 3;
        let umc = 0x96 << 32 | 0x0035_0f00;
        let (t0, t1, other_core, other_ccx, other_node) = (
            thread(4, 0),
            thread(5, 0),
            thread(6, 0),
            thread(20, 0),
            thread(0x84, 1),
        );
        assert_ne!(t0.key(ls), t1.key(ls), "SMT siblings: a counter each");
        assert_ne!(
            t0.key(ls),
            other_core.key(ls),
            "the same IPID, another core"
        );
        assert_eq!(t0.key(l3), t1.key(l3), "one L3 for the CCX");
        assert_eq!(t0.key(l3), other_core.key(l3), "across its cores");
        assert_ne!(t0.key(l3), other_ccx.key(l3));
        assert_eq!(t0.key(umc), other_ccx.key(umc), "one node");
        assert_ne!(t0.key(umc), other_node.key(umc));
        assert_eq!(t0.key(0x02 << 48 | 0x01 << 32), None, "MP5");
    }

    #[test]
    fn the_trigger_alone_is_armed_once() {
        let cpu = MsrSpace::new([(SMI_IO_CYCLE.index(), 1 << 40)]);
        arm_trigger(&cpu, 0xb2, 0x80).unwrap();
        assert_eq!(cpu.read(SMI_IO_CYCLE.index()), (1 << 40) | 0x0280_00b2);
        assert_eq!(arm_trigger(&cpu, 0xb2, 0x80), Err(ArmError::AlreadyOwned));
        let space = cpu.into_inner();
        assert!(!space.written(PFEH_CONFIG.index()));
        assert!(!space.written(MCE_REDIRECT.index()));
    }

    #[test]
    fn mca_smi_without_evidence_percolates_both_interrupts() {
        let cpu = MsrSpace::new([(0x179, 1), (PFEH_CONFIG.index(), 1)]);
        let events = observe_mca(
            &cpu,
            0,
            1 << 18,
            Some(10),
            |_, _| true,
            |_| panic!("no evidence"),
        );
        assert_eq!(events, Some(McaEvents::default()));
        assert_eq!(cpu.read(PFEH_CONFIG.index()), 1 | 2 | 4);
    }
}
