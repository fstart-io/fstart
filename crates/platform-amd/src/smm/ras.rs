// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMM machine-check handler of the SoCs with an SMI trigger for their
//! MCA (SP3: [`crate::sp3::smm`], RS: [`crate::rs::smm`]), per the board's
//! [`Policy`] (`Board::RAS`).
//!
//! Every CPU that enters SMM runs [`enter`]. On its first SMI (the
//! installer's self-SMI, after SMRAM is locked) it arms its SMI trigger
//! I/O cycle to the FCH's SMI command port and the thresholding counters
//! of its banks (`ccx::ras::arm_thresholds`: [`Policy::threshold`]
//! corrected errors each, then an SMI; the blocks are locked for the OS,
//! which keeps the banks), or with [`Policy::observe_mce`] PFEH as well
//! (`ccx::ras::arm`). On every SMI it reads its local SMI status: a
//! counter overflow or a deferred error (SmiSrcMca) or a machine check
//! (MceRedirSts) is recorded in a journal in SMRAM, the counter re-armed,
//! and with PFEH the event percolated back to the OS on RSM. An OS write
//! to a bank status PFEH intercepts is replayed if its saved registers
//! still name a clear; otherwise the CPU leaves PFEH (the vendors'
//! AmdPlatformRasRsSmm "MCE to Software SMI error handler" takes the same
//! sources but cloaks the banks and reports to the OS through GHES).
//!
//! The journal goes to the board's BMC ([`deliver`], on the rendezvous
//! owner only): an [`Event`] at a time through the board's hook, which
//! owns the transport (a KCS channel the OS does not use, or the OS's when
//! it is idle) and may use [`sel`] for the record. Delivery is bounded:
//! [`SEL_PER_BOOT`] events per boot, one pass per SMI of at most
//! [`SEL_PER_SMI`] events and [`DELIVERY_US`], however many CPUs come to
//! own its rendezvous (what is left waits for a later SMI), the first failure stops it, a
//! counter is re-armed at most [`REARMS_PER_CPU`] times per CPU (an error
//! storm then stays with the OS, whose banks still log it). The owner
//! waits at most [`ENTER_WAIT_US`] for CPUs still doing their local work,
//! never for one that has not entered.
//!
//! A thresholding counter is SMM's if SMM armed its block, and every write
//! to it is one CPU's at a time: the handler keeps every armed block by
//! its node and MCA_IPID ([`Blocks`]), reserved by the one CPU that arms
//! it (a shared bank, an SMT core's, a CCX's L3, a socket's UMCs, is one
//! block every CPU of the share sees), then claimed by whichever CPU of
//! the share handles its overflow; a block someone else locked is never
//! written.
//!
//! No heap, flash writes, OS buffers, GHES, address-to-DIMM guesses, bank
//! clearing or all-CPU barrier. The first records survive in locked SMRAM
//! until reset; this is deliberately not a persistent error store.

/// A board's RAS policy ([`crate::board::Board::RAS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Corrected errors a bank counts before its thresholding interrupt
    /// (the vendors' "MCA error thresh count" default is 10); 0 counts
    /// none. Nonzero: SMM counts them, reports each overflow and leaves
    /// the errors in the banks for the OS; zero: the OS counts, with its
    /// own limit.
    pub threshold: u16,
    /// Machine checks and deferred errors observed in SMM too (PFEH),
    /// then percolated to the OS. Experimental: PFEH turns every OS write
    /// to a bank status into an SMI that arrives after the write, whose
    /// registers Linux 7.3 has already cleared (`ccx::ras`).
    pub observe_mce: bool,
}

impl Policy {
    /// OS-first: no SMM counting, observation or SEL.
    pub const OS_FIRST: Self = Self {
        threshold: 0,
        observe_mce: false,
    };
    /// SMM counts corrected errors with the vendors' threshold of 10.
    pub const THRESHOLDS: Self = Self {
        threshold: 10,
        observe_mce: false,
    };

    /// SMM handles machine-check events at all.
    pub const fn smm(&self) -> bool {
        self.threshold != 0 || self.observe_mce
    }
}

use fstart_driver_amd::ccx::ras::{Kind, Record};
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_ipmi::SelRecord;
use zerocopy::little_endian::{U16, U32};

/// What the handler, and the boot for the last boot's errors, report to
/// the board's BMC (`Board::BMC_EVENT`).
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// A machine check, a deferred error or a thresholding overflow, or
    /// an error of the last boot.
    Mca(&'a Record),
    /// The one benign transport check the diagnostic ABI requests.
    Probe,
}

/// A board without a BMC to report to.
pub fn no_bmc(_: &Event<'_>, _: Tsc) -> bool {
    false
}

/// The KCS poll budget of one event (`Kcs::with_poll_budget`): 25000
/// readiness checks 10 us apart, about 250 ms. The S8040's BMC takes
/// about 25 ms to answer an event.
pub const SEL_POLL_BUDGET: u32 = 25_000;

/// The generator of the records: system software ID 10h, the first of
/// the SMI handler's (IPMI 2.0 §5.5, table 5-4).
const GENERATOR: u16 = 0x10 << 1 | 1;

// Standard system events work on the bench BMCs; OEM E0 entries are
// silently acknowledged without storage. FF names no SDR sensor.
fn event(sensor_type: u8, event_data: [u8; 3]) -> SelRecord {
    SelRecord {
        record_id: U16::ZERO,
        record_type: 2,
        timestamp: U32::ZERO,
        generator_id: U16::new(GENERATOR),
        event_message_revision: 4,
        sensor_type,
        sensor_number: 0xff,
        event_dir_type: 0x6f,
        event_data,
    }
}

/// A standard SEL record of `event` (IPMI 2.0 table 42-3). A DRAM
/// error is a memory event (sensor type 0Ch): correctable ECC logging
/// limit reached (05h) for a counter overflow, uncorrectable ECC (01h)
/// for an uncorrected one, correctable ECC (00h) otherwise; any other
/// bank's is a processor event (07h): uncorrectable (0Bh) or
/// correctable (0Ch) machine check. Event data 2 and 3 are OEM data:
/// the logical CPU that read the bank and the bank, not a guessed DIMM.
pub fn sel(event: &Event<'_>) -> SelRecord {
    let record = match event {
        Event::Mca(record) => record,
        // Entry added to auxiliary log, OEM data 'FS': a transport
        // check, never a manufactured machine-check record.
        Event::Probe => return self::event(0x12, [0xa3, b'F', b'S']),
    };
    let (sensor, offset) = match (record.memory(), record.uncorrected()) {
        (true, true) => (0x0c, 0x01),
        (true, false) if record.kind == Kind::Threshold => (0x0c, 0x05),
        (true, false) => (0x0c, 0x00),
        (false, true) => (0x07, 0x0b),
        (false, false) => (0x07, 0x0c),
    };
    self::event(sensor, [0xa0 | offset, record.cpu as u8, record.bank as u8])
}

#[cfg(feature = "smm")]
pub use handler::{
    DELIVERY_US, DIAG_COMMAND, DIAG_REQUEST, ENTER_WAIT_US, MCE_SMI_COMMAND, REARMS_PER_CPU,
    RETRY_MS, RETRY_SMIS, SEL_PER_BOOT, SEL_PER_SMI, deliver, enter,
};

#[cfg(feature = "smm")]
mod handler {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

    use fstart_arch::x86::cpu::amd::msr::SMM_BASE;
    use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
    use fstart_driver_amd::ccx::ras::{self, Kind, LocalSmiStatus, Record};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::cpu::Silicon;
    use fstart_driver_amd::window::{Mmio, Window};
    use tock_registers::interfaces::Readable;

    use super::{Event, Policy};

    /// Logical CPUs the handler tracks: two Genoa sockets of 96 cores and
    /// two threads fit. Indices beyond stay OS-first, never armed.
    const MAX_CPUS: usize = 512;
    const RECORDS: usize = 64;
    /// Events sent to the BMC per boot.
    pub const SEL_PER_BOOT: u32 = 16;
    /// Events sent in one SMI, whatever comes in meanwhile.
    pub const SEL_PER_SMI: u32 = 2;
    /// The time one SMI spends delivering: no event starts after it (one
    /// event takes at most [`super::SEL_POLL_BUDGET`] polls).
    pub const DELIVERY_US: u64 = 50_000;
    /// The period of the SMI that retries what waits for delivery.
    pub const RETRY_MS: u16 = 100;
    /// Periodic SMIs a waiting report gets, from its record or the last
    /// event that went out ([`Retry`]).
    pub const RETRY_SMIS: u32 = 16;
    /// The longest the delivering owner waits for CPUs still doing their
    /// local work (and journaling) in this SMI.
    pub const ENTER_WAIT_US: u64 = 1000;
    /// Thresholding blocks SMM can own: every thread's core banks, every
    /// L3 slice and socket bank of two 96-core sockets.
    const BLOCKS: usize = 4096;
    /// Thresholding re-arms per CPU per boot.
    pub const REARMS_PER_CPU: u8 = 32;
    /// The I/O cycle data of the local SMI trigger; never sufficient to
    /// establish an MCA cause.
    pub const MCE_SMI_COMMAND: u8 = 0x80;
    /// Diagnostic I/O ABI: OUT AL,DX with RAX=DIAG_REQUEST, RDX=B2, RCX selector.
    /// 0 reads PFEH/cloak/mask into RAX/RCX/RDX. 1 requests one benign SEL probe;
    /// 2 reads its result (0 idle, 1 queued, 4 running, 2 success, 3 failure).
    /// 3 checks saved-SMBASE offsets (RAX=FF00 match, RCX=FE00 match, RDX=revision).
    /// 4 reads RAX=records journaled, RCX=dropped, RDX=refused CPUs.
    /// 7 reads RAX=the banks whose counters this CPU armed, RCX=the blocks
    /// SMM armed on every CPU, RDX=the banks with a counter this CPU left
    /// alone, their scope not known.
    /// No pointers, error injection or arbitrary MSRs are accepted.
    pub const DIAG_COMMAND: u8 = 0xd1;
    pub const DIAG_REQUEST: u64 = 0x4653_5241_5300_00d1;

    /// Per CPU: 0 not tried, [`TRIGGER`] armed, with [`PFEH`] too, or
    /// [`REFUSED`].
    static CPU_STATE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
    const TRIGGER: u8 = 1;
    const PFEH: u8 = 2;
    const REFUSED: u8 = 4;
    static REARMS: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
    /// CPUs doing their local work now.
    static ENTERING: AtomicU32 = AtomicU32::new(0);
    /// Per CPU: the banks whose counters it armed ([`ras::arm_thresholds`]).
    static OWNED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    /// Per CPU: the banks with a counter it left alone, their scope not
    /// known ([`ras::Scope::of`]).
    static UNSCOPED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static ARMED: Blocks = Blocks::new();
    static REFUSED_CPUS: AtomicU32 = AtomicU32::new(0);
    static PROBE: AtomicU8 = AtomicU8::new(0);
    /// The delivery lock.
    static DELIVERING: AtomicBool = AtomicBool::new(false);
    static SEL_FAILED: AtomicBool = AtomicBool::new(false);
    static JOURNAL: Journal = Journal::new();
    static RETRY: Retry = Retry::new();

    /// The thresholding blocks SMM armed, each by its hardware instance
    /// (`ras::Sharing::key`: its scope, the thread, the CCX or the node,
    /// and its MCA_IPID), with one writer at a time: a shared block's
    /// counter (a CCX's L3, a node's UMC) is one register every CPU of the
    /// share reaches, and only those; a core bank's is its thread's. Arming is
    /// one CPU at a time ([`Self::reserve`] to [`Self::publish`]): it
    /// checks the block again, a sibling may have locked it, and arms it,
    /// under the arming lock. After that a CPU of the share claims it for
    /// every write (an overflow's re-arm) and releases it. A block of
    /// another instance, even with the same IPID, is another entry: its
    /// arming authorizes no write to this one. Append only.
    pub(super) struct Blocks {
        /// Held from a block's [`Self::reserve`] to its [`Self::publish`].
        arming: AtomicBool,
        len: AtomicUsize,
        ipid: [AtomicU64; BLOCKS],
        scope: [AtomicU64; BLOCKS],
        state: [AtomicU8; BLOCKS],
    }

    /// A CPU writes the block (it claimed it).
    const HELD: u8 = 0;
    /// SMM's, nobody writing it.
    const SMMS: u8 = 1;

    /// How long a CPU tries for the arming lock: much more than all the
    /// CPUs' arming takes, which is a few MSR accesses per block.
    const ARMING_TRIES: u32 = 1 << 26;

    /// The block a CPU holds the arming lock for ([`Blocks::reserve`]).
    pub(super) struct Reservation {
        scope: u64,
        ipid: u64,
    }

    impl Blocks {
        pub(super) const fn new() -> Self {
            Self {
                arming: AtomicBool::new(false),
                len: AtomicUsize::new(0),
                ipid: [const { AtomicU64::new(0) }; BLOCKS],
                scope: [const { AtomicU64::new(0) }; BLOCKS],
                state: [const { AtomicU8::new(HELD) }; BLOCKS],
            }
        }

        /// Take the arming lock for the block `(scope, ipid)`: no other CPU
        /// arms a block until [`Self::publish`]. `None` if it stays taken
        /// (a CPU that never published): then this one arms nothing.
        pub(super) fn reserve(&self, (scope, ipid): (u64, u64)) -> Option<Reservation> {
            self.reserve_within((scope, ipid), ARMING_TRIES)
        }

        fn reserve_within(&self, (scope, ipid): (u64, u64), mut tries: u32) -> Option<Reservation> {
            while self
                .arming
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                tries = tries.checked_sub(1)?;
                core::hint::spin_loop();
            }
            // A full table takes no new block: one SMM armed but could
            // not claim would never be re-armed.
            if self.find((scope, ipid)).is_none() && self.len.load(Ordering::Relaxed) >= BLOCKS {
                self.arming.store(false, Ordering::Release);
                return None;
            }
            Some(Reservation { scope, ipid })
        }

        /// The reserving CPU armed the block, or found it locked: an armed
        /// one is SMM's from now on (its entry added if it has none), and
        /// the arming lock is free again.
        pub(super) fn publish(&self, reservation: Reservation, armed: bool) {
            let Reservation { scope, ipid } = reservation;
            if armed && self.find((scope, ipid)).is_none() {
                let index = self.len.load(Ordering::Relaxed);
                if let (Some(slot), Some(instance), Some(state)) = (
                    self.ipid.get(index),
                    self.scope.get(index),
                    self.state.get(index),
                ) {
                    slot.store(ipid, Ordering::Relaxed);
                    instance.store(scope, Ordering::Relaxed);
                    state.store(SMMS, Ordering::Relaxed);
                    self.len.store(index + 1, Ordering::Release);
                }
            }
            self.arming.store(false, Ordering::Release);
        }

        // No indexing here: the SMM image takes no panic formatting code.
        fn find(&self, (scope, ipid): (u64, u64)) -> Option<usize> {
            let len = self.len.load(Ordering::Acquire);
            self.scope
                .iter()
                .zip(&self.ipid)
                .take(len)
                .position(|(instance, slot)| {
                    instance.load(Ordering::Relaxed) == scope
                        && slot.load(Ordering::Relaxed) == ipid
                })
        }

        /// Take the SMM block `key` to write it; `None` if it is not SMM's
        /// or another CPU has it now (and handles it).
        pub(super) fn claim(&self, key: (u64, u64)) -> Option<usize> {
            let index = self.find(key)?;
            self.state
                .get(index)?
                .compare_exchange(SMMS, HELD, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
                .then_some(index)
        }

        pub(super) fn release(&self, index: usize) {
            if let Some(state) = self.state.get(index) {
                state.store(SMMS, Ordering::Release);
            }
        }
    }

    struct Journal {
        locked: AtomicBool,
        dropped: AtomicU32,
        state: UnsafeCell<State>,
    }

    struct State {
        records: [Record; RECORDS],
        count: usize,
        pending: usize,
        sent: u32,
    }

    // SAFETY: every access to state is behind `with`'s acquire/release lock;
    // producers never spin and no reference to the contents escapes its closure.
    unsafe impl Sync for Journal {}

    impl Journal {
        const fn new() -> Self {
            Self {
                locked: AtomicBool::new(false),
                dropped: AtomicU32::new(0),
                state: UnsafeCell::new(State {
                    records: [Record::EMPTY; RECORDS],
                    count: 0,
                    pending: 0,
                    sent: 0,
                }),
            }
        }

        fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                return None;
            }
            struct Unlock<'a>(&'a AtomicBool);
            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _unlock = Unlock(&self.locked);
            // SAFETY: exclusive access until Unlock drops; `f` cannot leak the borrow.
            Some(f(unsafe { &mut *self.state.get() }))
        }

        /// Keep `record`, unless it is a deferred error already kept (it
        /// stays in its bank until the OS clears it, and another SMI of
        /// the bank's thread reads it again).
        #[inline(always)]
        fn push(&self, record: Record) {
            let stored = self
                .with(|state| {
                    let kept = state.records.get(..state.count).unwrap_or_default();
                    if record.kind == Kind::Deferred && kept.iter().any(|r| r.same(&record)) {
                        return true;
                    }
                    let Some(slot) = state.records.get_mut(state.count) else {
                        return false;
                    };
                    *slot = record;
                    state.count += 1;
                    true
                })
                .unwrap_or(false);
            if !stored {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn next(&self) -> Option<Record> {
            self.with(|state| {
                if state.pending >= state.count || state.sent >= SEL_PER_BOOT {
                    return None;
                }
                let record = *state.records.get(state.pending)?;
                state.pending += 1;
                state.sent += 1;
                Some(record)
            })
            .flatten()
        }

        /// Whether a record waits for delivery (a journal another CPU
        /// holds now counts as one).
        fn waiting(&self) -> bool {
            self.with(|state| state.pending < state.count && state.sent < SEL_PER_BOOT)
                .unwrap_or(true)
        }
    }

    /// Every entrant runs this, including CPUs that do not own the FCH
    /// dispatch: the local MCA work of `policy`, which journals what it
    /// finds for the owner to deliver ([`deliver`]). Arming happens on the
    /// installer's per-CPU self-SMI, after SMRAM is locked. No CPU waits
    /// for a sibling and a journal collision never delays native #MC.
    #[inline(always)]
    pub fn enter(policy: &Policy, logical_cpu: u32, silicon: Silicon) {
        if policy.smm() {
            ENTERING.fetch_add(1, Ordering::AcqRel);
            enter_enabled(logical_cpu, silicon, policy.threshold, policy.observe_mce);
            ENTERING.fetch_sub(1, Ordering::AcqRel);
        }
    }

    // Keep SMRAM static references in their defining crate. Cross-crate
    // inlining into the board entry can materialize a GOT access, forbidden
    // in PIC SMM.
    #[inline(never)]
    fn enter_enabled(logical_cpu: u32, silicon: Silicon, threshold: u16, observe_mce: bool) {
        let index = logical_cpu as usize;
        let (Some(state), Some(rearms), Some(owned)) =
            (CPU_STATE.get(index), REARMS.get(index), OWNED.get(index))
        else {
            return;
        };
        // SAFETY: SMM, ring 0.
        let cpu = unsafe { NativeMsr::new() };
        let smbase = cpu.reg(SMM_BASE).get() as usize;
        // SAFETY: this thread's locked SMBASE window, entirely identity-mapped.
        let saved = unsafe {
            fstart_smm::X86SaveState::from_top(
                (smbase + 0x10000) as *mut u8,
                fstart_smm::X86SaveStateFormat::Amd64,
            )
        };
        if state.load(Ordering::Relaxed) == 0 {
            let stepping = fstart_arch::x86::cpuid(1).0 as u8 & 0xf;
            let (port, command) = (fstart_driver_amd::fch::smi::APM_CNT, MCE_SMI_COMMAND);
            let armed = saved.revision().is_ok()
                && if observe_mce {
                    ras::arm(&cpu, silicon, stepping, port, command)
                } else {
                    ras::arm_trigger(&cpu, port, command)
                }
                .is_ok();
            if armed && threshold != 0 {
                let sharing = ras::Sharing::current();
                let mut unscoped = 0;
                let banks = ras::arm_thresholds(
                    &cpu,
                    threshold,
                    |bank, ipid| match sharing.key(ipid) {
                        Some(key) => ARMED.reserve(key),
                        // A counter of no known scope is not armed.
                        None => {
                            unscoped |= 1 << bank;
                            None
                        }
                    },
                    |reservation, armed| ARMED.publish(reservation, armed),
                );
                owned.store(banks, Ordering::Relaxed);
                if let Some(slot) = UNSCOPED.get(index) {
                    slot.store(unscoped, Ordering::Relaxed);
                }
            }
            state.store(
                match (armed, observe_mce) {
                    (false, _) => REFUSED,
                    (true, false) => TRIGGER,
                    (true, true) => TRIGGER | PFEH,
                },
                Ordering::Relaxed,
            );
            if !armed {
                REFUSED_CPUS.fetch_add(1, Ordering::Relaxed);
            }
        }
        let armed = state.load(Ordering::Relaxed);
        if armed & TRIGGER == 0 {
            return;
        }
        // SAFETY: this thread's installed AMD64 SMBASE; local status at +FEC4
        // is in its locked save-state window, mapped by the SMM identity tables.
        let save = unsafe { Mmio::new(smbase) };
        let status = save
            .reg::<u32, LocalSmiStatus::Register>(ras::LOCAL_SMI_STATUS)
            .get();
        diagnostics(&cpu, index, saved);
        let mut sink = |record| {
            JOURNAL.push(record);
            RETRY.renew();
        };
        if armed & PFEH != 0 {
            ras::observe(&cpu, logical_cpu, status, &mut sink);
        }
        let rearm = (threshold != 0 && rearms.load(Ordering::Relaxed) < REARMS_PER_CPU)
            .then_some(threshold);
        // The counters SMM armed, this CPU's (`owned`) or one of a CPU it
        // shares the bank with: each claimed for this SMI, then released.
        let sharing = ras::Sharing::current();
        let mut claimed = heapless::Vec::<usize, { ras::MAX_BANKS as usize }>::new();
        let owns = |_, ipid| {
            let Some(key) = sharing.key(ipid) else {
                return false;
            };
            ARMED.claim(key).is_some_and(|index| {
                claimed
                    .push(index)
                    .map_err(|_| ARMED.release(index))
                    .is_ok()
            })
        };
        let events = ras::observe_mca(&cpu, logical_cpu, status, rearm, owns, &mut sink);
        claimed.iter().for_each(|&index| ARMED.release(index));
        if events.is_some_and(|events| events.thresholds != 0) && rearm.is_some() {
            rearms.fetch_add(1, Ordering::Relaxed);
        }
        if armed & PFEH != 0 && LocalSmiStatus::WRMSR.is_set(status) {
            use fstart_smm::save_state::Amd64Register::{Rax, Rcx, Rdx};
            // The trigger's SMI arrives after the write: the registers
            // are the ones at the SMI, which may no longer name it.
            let replayed = match (
                saved.read_amd64(Rcx),
                saved.read_amd64(Rax),
                saved.read_amd64(Rdx),
            ) {
                (Ok(index), Ok(low), Ok(high)) => {
                    let value = (high as u32 as u64) << 32 | low as u32 as u64;
                    ras::replay_clear(&cpu, index as u32, value)
                }
                _ => false,
            };
            if !replayed {
                // The write is lost (PFEH turned it into this SMI); the
                // OS's next poll logs the bank again and clears it, now
                // natively: this CPU leaves PFEH for good.
                ras::disarm(&cpu);
                state.store(TRIGGER, Ordering::Relaxed);
                REFUSED_CPUS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn diagnostics<M: MsrAccess>(cpu: &M, cpu_index: usize, saved: fstart_smm::X86SaveState) {
        use fstart_smm::save_state::Amd64Register::{Rax, Rcx, Rdx};
        if saved.read_amd64(Rax) != Ok(DIAG_REQUEST) || saved.read_amd64(Rdx) != Ok(0xb2) {
            return;
        }
        match saved.read_amd64(Rcx) {
            Ok(0) => {
                let _ = saved.write_amd64(Rax, cpu.reg(ras::PFEH_CONFIG).get());
                let _ = saved.write_amd64(Rcx, cpu.reg(ras::PFEH_CLOAK).get());
                let _ = saved.write_amd64(Rdx, cpu.reg(ras::PFEH_DEFERRED_MASK).get());
            }
            Ok(3) => {
                let base = cpu.reg(SMM_BASE).get();
                // SAFETY: this CPU's locked, mapped save-state window.
                let memory = unsafe { Mmio::new(base as usize) };
                let _ = saved.write_amd64(Rax, u64::from(memory.read32(0xff00) == base as u32));
                let _ = saved.write_amd64(Rcx, u64::from(memory.read32(0xfe00) == base as u32));
                let _ = saved.write_amd64(Rdx, u64::from(saved.revision().unwrap_or(0)));
            }
            Ok(4) => {
                let count = JOURNAL.with(|state| state.count).unwrap_or(usize::MAX);
                let _ = saved.write_amd64(Rax, count as u64);
                let _ = saved.write_amd64(Rcx, JOURNAL.dropped.load(Ordering::Relaxed).into());
                let _ = saved.write_amd64(Rdx, REFUSED_CPUS.load(Ordering::Relaxed).into());
            }
            Ok(7) => {
                let owned = OWNED
                    .get(cpu_index)
                    .map_or(0, |owned| owned.load(Ordering::Relaxed));
                let unscoped = UNSCOPED
                    .get(cpu_index)
                    .map_or(0, |unscoped| unscoped.load(Ordering::Relaxed));
                let _ = saved.write_amd64(Rax, owned);
                let _ = saved.write_amd64(Rcx, ARMED.len.load(Ordering::Relaxed) as u64);
                let _ = saved.write_amd64(Rdx, unscoped);
            }
            Ok(selector @ (1 | 2)) => {
                if selector == 1 {
                    let _ = PROBE.compare_exchange(0, 1, Ordering::Release, Ordering::Relaxed);
                }
                let _ = saved.write_amd64(Rax, u64::from(PROBE.load(Ordering::Acquire)));
            }
            _ => {}
        }
    }

    /// On the rendezvous owner, with the FCH's ACPI block `acpi`: send
    /// what the journal holds to the board's BMC through `bmc`, in one pass
    /// per SMI ([`pass`]: on its `first` owner, `SmmContext::first_owner`)
    /// of at most [`SEL_PER_SMI`] events and [`DELIVERY_US`], whatever
    /// comes in meanwhile. First it waits at most [`ENTER_WAIT_US`] for the
    /// CPUs still journaling. What is left waits for the FCH's periodic SMI
    /// ([`Retry`]), which runs while a report waits, at most
    /// [`RETRY_SMIS`] times since the last record or event, and for one
    /// period after an SMI that not every CPU (`cpus`, the handler's
    /// `SmmContext::cpus_held`) had reached the rendezvous for: one may
    /// still journal. The first failed event stops delivery until reset
    /// (no wait for the BMC); OS handoff never depends on it.
    #[inline(always)]
    pub fn deliver<W: Window>(
        policy: &Policy,
        acpi: &W,
        first: bool,
        cpus: impl Fn() -> Option<(u32, u32)>,
        mut bmc: impl FnMut(&Event<'_>, Tsc) -> bool,
    ) {
        use fstart_core::services::Timer;
        use fstart_driver_amd::fch::smi::{self as fch_smi, source};
        if !policy.smm() {
            return;
        }
        // SAFETY: SMM, ring 0; the P-state MSRs.
        let timer = Tsc::native(&unsafe { NativeMsr::new() });
        let start = timer.timestamp_us();
        while entering() && timer.timestamp_us().wrapping_sub(start) < ENTER_WAIT_US {
            core::hint::spin_loop();
        }
        // Before the FCH's dispatch clears it.
        let tick = [source::SHORT_TIMER, source::LONG_TIMER]
            .into_iter()
            .any(|s| fch_smi::is_pending(acpi, s));
        let start = timer.timestamp_us();
        pass(
            first,
            |event| bmc(event, timer),
            || timer.timestamp_us().wrapping_sub(start) < DELIVERY_US,
        );
        let held = cpus().is_some_and(|(inside, total)| inside >= total);
        if let Some(period) = retry(tick, held) {
            fch_smi::periodic(acpi, period);
        }
    }

    #[inline(never)]
    fn entering() -> bool {
        ENTERING.load(Ordering::Acquire) != 0
    }

    /// What the owner does with the periodic SMI after its pass.
    // The statics stay in this crate (no GOT in the board's SMM image).
    #[inline(never)]
    fn retry(tick: bool, held: bool) -> Option<Option<u16>> {
        let waiting = !SEL_FAILED.load(Ordering::Relaxed)
            && (JOURNAL.waiting() || PROBE.load(Ordering::Acquire) == 1);
        RETRY.clock(tick, waiting, held)
    }

    /// The periodic SMI that delivers what waits without another SMI: a
    /// record a CPU journaled after its SMI's pass (it reached SMM late),
    /// or one the pass had no time or budget for. Bounded: a waiting
    /// report gets [`RETRY_SMIS`] periods from its record or the last
    /// event that went out; an SMI not every CPU reached the rendezvous
    /// for gets one period, in which a late record arrives (and renews
    /// it). The owner's decision, after its pass, sees every CPU parked
    /// for it (each did its local work) or else keeps one more period.
    struct Retry {
        running: AtomicBool,
        left: AtomicU32,
    }

    impl Retry {
        const fn new() -> Self {
            Self {
                running: AtomicBool::new(false),
                left: AtomicU32::new(0),
            }
        }

        /// A record came, or an event went out.
        fn renew(&self) {
            self.left.store(RETRY_SMIS, Ordering::Relaxed);
        }

        /// On an owner, after its pass: `tick` if this SMI is the periodic
        /// one, `waiting` if a report waits, `held` if every CPU was
        /// parked for the owner. The periodic SMI's new period
        /// (`Some(None)`: stop it), if it changes.
        fn clock(&self, tick: bool, waiting: bool, held: bool) -> Option<Option<u16>> {
            let running = self.running.load(Ordering::Relaxed);
            if running && tick {
                let _ = self
                    .left
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
            }
            let wanted = (waiting && self.left.load(Ordering::Relaxed) != 0) || (!held && !tick);
            if wanted == running {
                return None;
            }
            self.running.store(wanted, Ordering::Relaxed);
            Some(wanted.then_some(RETRY_MS))
        }
    }

    /// One SMI's delivery, on its `first` owner only: a CPU that comes to
    /// own the rendezvous after another owner of its SMI (late, even with
    /// a source that latched meanwhile) delivers nothing, so one SMI has
    /// one pass whatever its owners. Then events, under the delivery lock,
    /// while fewer than [`SEL_PER_SMI`] went out and `in_time` holds, and
    /// none failed.
    fn pass(first: bool, mut bmc: impl FnMut(&Event<'_>) -> bool, in_time: impl Fn() -> bool) {
        if !first || !begin() {
            return;
        }
        let mut sent = 0;
        while in_time() && sent < SEL_PER_SMI && !SEL_FAILED.load(Ordering::Relaxed) {
            let Some(event) = next() else {
                break;
            };
            let ok = match event {
                Some(record) => bmc(&Event::Mca(&record)),
                None => bmc(&Event::Probe),
            };
            done(event.is_none(), ok);
            sent += 1;
            if !ok {
                break;
            }
        }
        end();
    }

    /// Take delivery, unless another CPU has it or the BMC failed (read
    /// under the lock).
    #[inline(never)]
    fn begin() -> bool {
        if DELIVERING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        if SEL_FAILED.load(Ordering::Relaxed) {
            if PROBE.load(Ordering::Acquire) == 1 {
                PROBE.store(3, Ordering::Release);
            }
            DELIVERING.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// The next event: a record (`Some`), or the requested probe (`None`).
    #[inline(never)]
    fn next() -> Option<Option<Record>> {
        if let Some(record) = JOURNAL.next() {
            return Some(Some(record));
        }
        PROBE
            .compare_exchange(1, 4, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
            .then_some(None)
    }

    /// An event went out (under the lock).
    #[inline(never)]
    fn done(probe: bool, ok: bool) {
        if ok {
            RETRY.renew();
        } else {
            SEL_FAILED.store(true, Ordering::Relaxed);
        }
        if probe {
            PROBE.store(if ok { 2 } else { 3 }, Ordering::Release);
        }
    }

    #[inline(never)]
    fn end() {
        DELIVERING.store(false, Ordering::Release);
    }

    #[cfg(test)]
    mod tests {
        use super::super::sel;
        use super::*;
        use zerocopy::IntoBytes;

        #[test]
        fn journal_preserves_first_evidence_and_reports_overflow() {
            let journal = Journal::new();
            for bank in 0..RECORDS as u32 + 3 {
                journal.push(Record {
                    bank,
                    status: 1 << 63,
                    ..Record::EMPTY
                });
            }
            assert_eq!(journal.dropped.load(Ordering::Relaxed), 3);
            journal
                .with(|s| {
                    assert_eq!(s.count, RECORDS);
                    assert_eq!(s.records[0].bank, 0);
                    assert_eq!(s.records[RECORDS - 1].bank, RECORDS as u32 - 1);
                })
                .unwrap();
            for bank in 0..SEL_PER_BOOT {
                assert_eq!(journal.next().unwrap().bank, bank);
            }
            assert!(journal.next().is_none());
            assert!(!journal.waiting(), "the per-boot budget is spent");
            assert_eq!(
                journal.with(|s| s.count),
                Some(RECORDS),
                "delivery does not erase evidence"
            );
        }

        /// One test for every use of the handler's statics (the journal,
        /// the delivery lock): SEL_PER_SMI events per SMI and one pass per
        /// SMI, on its first owner: a late entrant that owns the
        /// rendezvous after it (even with a fresh source, which it
        /// dispatches) delivers nothing, in time or not; what is left for
        /// later SMIs; nothing after a failure.
        #[test]
        fn delivery_is_bounded_per_smi_and_carries_the_rest_over() {
            for bank in 0..6 {
                JOURNAL.push(Record {
                    bank,
                    status: 1 << 63,
                    ..Record::EMPTY
                });
            }
            let banks = |first, in_time: &dyn Fn() -> bool, ok: bool| {
                let mut banks = std::vec::Vec::new();
                pass(
                    first,
                    |event| {
                        if let Event::Mca(record) = event {
                            banks.push(record.bank);
                        }
                        ok
                    },
                    in_time,
                );
                banks
            };
            // The SMI's first owner; then a late entrant of the same SMI
            // owning it next: nothing, in time or not.
            assert_eq!(banks(true, &|| true, true), [0, 1]);
            assert!(banks(false, &|| true, true).is_empty());
            assert!(banks(false, &|| false, true).is_empty());
            assert!(JOURNAL.waiting());
            assert_eq!(banks(true, &|| true, true), [2, 3]);
            // The SMI's time is up after the first event.
            let calls = core::cell::Cell::new(0);
            let in_time = || {
                calls.set(calls.get() + 1);
                calls.get() < 2
            };
            assert_eq!(banks(true, &in_time, true), [4]);
            // Out of time before the first: nothing, and it stays queued.
            assert!(banks(true, &|| false, true).is_empty());
            assert!(JOURNAL.waiting());
            // A failed event ends delivery, read under the lock.
            assert_eq!(banks(true, &|| true, false), [5]);
            JOURNAL.push(Record {
                bank: 6,
                status: 1 << 63,
                ..Record::EMPTY
            });
            assert!(banks(true, &|| true, true).is_empty());
            assert!(!DELIVERING.load(Ordering::Relaxed));
            // Another CPU holding the lock: nothing.
            SEL_FAILED.store(false, Ordering::Relaxed);
            DELIVERING.store(true, Ordering::Relaxed);
            assert!(banks(true, &|| true, true).is_empty());
            DELIVERING.store(false, Ordering::Relaxed);
            assert_eq!(banks(true, &|| true, true), [6]);
            assert!(!JOURNAL.waiting());
            // A failure stops the retries: nothing waits for them.
            SEL_FAILED.store(true, Ordering::Relaxed);
            JOURNAL.push(Record {
                bank: 7,
                status: 1 << 63,
                ..Record::EMPTY
            });
            RETRY.renew();
            assert_eq!(retry(false, true), None, "not running, not wanted");
            SEL_FAILED.store(false, Ordering::Relaxed);
            assert_eq!(retry(false, true), Some(Some(RETRY_MS)));
            assert_eq!(banks(true, &|| true, true), [7]);
            assert_eq!(retry(true, true), Some(None), "delivered: stopped");
        }

        /// A record journaled after its SMI's pass, by a CPU that reached
        /// SMM late, with no SMI after it: the owner saw a CPU missing at
        /// its decision and keeps the periodic SMI for one period, in
        /// which the record is found and delivered, then stops. A report
        /// the channel never takes gets RETRY_SMIS periods from its record.
        #[test]
        fn a_late_record_is_retried_without_another_smi() {
            let retry = Retry::new();
            // The SMI's owner: nothing waits, one CPU is not parked yet.
            assert_eq!(retry.clock(false, false, false), Some(Some(RETRY_MS)));
            // That CPU journals now, after the pass.
            retry.renew();
            // The periodic SMI: the record waits; every CPU is held.
            assert_eq!(retry.clock(true, true, true), None, "still running");
            // Delivered on that SMI's pass: nothing waits, stop.
            assert_eq!(retry.clock(true, false, true), Some(None));
            // Every CPU held and nothing waiting: no periodic SMI at all.
            assert_eq!(retry.clock(false, false, true), None);
            // A periodic SMI's own missing CPU adds no period.
            assert_eq!(retry.clock(false, false, false), Some(Some(RETRY_MS)));
            assert_eq!(retry.clock(true, false, false), Some(None));
            // A report that never goes out: RETRY_SMIS periods, then none.
            retry.renew();
            assert_eq!(retry.clock(false, true, true), Some(Some(RETRY_MS)));
            for _ in 1..RETRY_SMIS {
                assert_eq!(retry.clock(true, true, true), None);
            }
            assert_eq!(retry.clock(true, true, true), Some(None), "spent");
            assert_eq!(retry.clock(false, true, true), None, "until renewed");
            retry.renew();
            assert_eq!(retry.clock(false, true, true), Some(Some(RETRY_MS)));
        }

        /// One writer at a time, and only the block's own sharers: while a
        /// CPU arms a block, no other CPU arms one (its reservation waits,
        /// here in vain). An L3 counter the CCX's cores share is one block
        /// (a true alias): another core of the CCX claims it, another
        /// CCX's core with the same IPID does not. A core bank's counter is
        /// the thread's: its SMT sibling's, with the same IPID, is another
        /// block, which the first thread's arming does not authorize.
        #[test]
        fn a_block_is_its_hardware_instance_with_one_writer() {
            let thread = |apic_id| ras::Sharing {
                apic_id,
                ccx_shift: 4,
                node: 0,
            };
            let key = |sharing: ras::Sharing, ipid| sharing.key(ipid).unwrap();
            let (t0, sibling, other_core, other_ccx) =
                (thread(4), thread(5), thread(6), thread(0x14));
            let l3 = 0x07 << 48 | 0xb0 << 32 | 3;
            let blocks = Blocks::new();
            let reserved = blocks.reserve(key(t0, l3)).unwrap();
            assert!(
                blocks.reserve_within(key(other_core, l3), 1000).is_none(),
                "the CCX's other core waits"
            );
            assert_eq!(blocks.claim(key(t0, l3)), None, "not armed yet");
            blocks.publish(reserved, true);
            // The other core finds the block locked: nothing new.
            let alias = blocks.reserve_within(key(other_core, l3), 1000).unwrap();
            blocks.publish(alias, false);
            // Another CCX's L3 with the same IPID, locked by another owner.
            let foreign = blocks.reserve_within(key(other_ccx, l3), 1000).unwrap();
            blocks.publish(foreign, false);
            assert_eq!(blocks.len.load(Ordering::Relaxed), 1);
            assert_eq!(
                blocks.claim(key(other_ccx, l3)),
                None,
                "the same IPID in another CCX: not SMM's"
            );
            let claim = blocks.claim(key(other_core, l3)).unwrap();
            assert_eq!(blocks.claim(key(t0, l3)), None, "the alias has it");
            blocks.release(claim);
            assert_eq!(blocks.claim(key(t0, l3)), Some(claim));
            blocks.release(claim);
            // A core bank: t0 armed its own; its sibling's is not t0's.
            let ls = 0x10 << 48 | 0xb0 << 32;
            let own = blocks.reserve(key(t0, ls)).unwrap();
            blocks.publish(own, true);
            assert_eq!(
                blocks.claim(key(sibling, ls)),
                None,
                "the sibling's own counter"
            );
            assert!(blocks.claim(key(t0, ls)).is_some());
            // A node's UMC: one block for every thread of the node.
            let umc = 0x96 << 32 | 0x0035_0f00;
            let armed = blocks.reserve(key(t0, umc)).unwrap();
            blocks.publish(armed, true);
            assert!(blocks.claim(key(thread(0x40), umc)).is_some());
        }

        /// A full table takes no new block, so none is armed that SMM
        /// could not claim; one it holds is found again.
        #[test]
        fn a_full_table_arms_no_new_block() {
            let blocks = Blocks::new();
            let key = |n: u64| (3 << 32, 0x96 << 32 | n);
            for n in 0..BLOCKS as u64 {
                let reservation = blocks.reserve(key(n)).unwrap();
                blocks.publish(reservation, true);
            }
            assert!(blocks.reserve(key(BLOCKS as u64)).is_none());
            assert!(!blocks.arming.load(Ordering::Relaxed), "the lock is free");
            let again = blocks.reserve(key(0)).unwrap();
            blocks.publish(again, false);
        }

        #[test]
        fn a_deferred_error_read_again_is_kept_once() {
            let journal = Journal::new();
            let deferred = Record {
                kind: Kind::Deferred,
                bank: 3,
                status: 1 << 63 | 1 << 44,
                ..Record::EMPTY
            };
            journal.push(deferred);
            journal.push(Record { cpu: 9, ..deferred });
            journal.push(Record {
                kind: Kind::Threshold,
                ..deferred
            });
            journal.push(Record {
                kind: Kind::Threshold,
                ..deferred
            });
            assert_eq!(journal.with(|s| s.count), Some(3));
        }

        #[test]
        fn contended_producer_drops_instead_of_waiting() {
            let journal = Journal::new();
            journal.with(|_| journal.push(Record::EMPTY)).unwrap();
            assert_eq!(journal.dropped.load(Ordering::Relaxed), 1);
            assert_eq!(journal.with(|s| s.count), Some(0));
        }

        #[test]
        fn diagnostics_only_return_registers_to_the_requesting_context() {
            use fstart_smm::save_state::Amd64Register::{Rax, Rcx, Rdx};
            use fstart_smm::{X86SaveState, X86SaveStateFormat};
            struct Cpu;
            impl MsrAccess for Cpu {
                fn read(&self, index: u32) -> u64 {
                    match index {
                        0xc0010120 => 1,
                        0xc0010121 | 0xc0010122 => 0,
                        _ => panic!("unexpected MSR"),
                    }
                }
                fn write(&self, _: u32, _: u64) {
                    panic!("diagnostics must not write MSRs");
                }
            }
            let mut memory = [0u8; 0x400];
            memory[0x2fc..0x300].copy_from_slice(&0x30064u32.to_le_bytes());
            let saved = unsafe {
                X86SaveState::from_top(memory.as_mut_ptr().add(0x400), X86SaveStateFormat::Amd64)
            };
            saved.write_amd64(Rax, DIAG_REQUEST).unwrap();
            saved.write_amd64(Rcx, 0).unwrap();
            saved.write_amd64(Rdx, 0xb3).unwrap();
            diagnostics(&Cpu, 0, saved);
            assert_eq!(saved.read_amd64(Rax), Ok(DIAG_REQUEST));
            saved.write_amd64(Rdx, 0xb2).unwrap();
            diagnostics(&Cpu, 0, saved);
            assert_eq!(saved.read_amd64(Rax), Ok(1));
            assert_eq!(saved.read_amd64(Rcx), Ok(0));
            assert_eq!(saved.read_amd64(Rdx), Ok(0));
        }

        #[test]
        fn standard_sel_has_no_guessed_dimm_or_sensor() {
            let record = Record {
                cpu: 255,
                bank: 63,
                status: 0xdead_beef_1234_5678,
                ..Record::EMPTY
            };
            assert_eq!(
                sel(&Event::Mca(&record)).as_bytes(),
                &[0, 0, 2, 0, 0, 0, 0, 0x21, 0, 4, 7, 255, 0x6f, 0xab, 255, 63]
            );
        }

        #[test]
        fn dram_errors_are_memory_events() {
            let umc = Record {
                cpu: 3,
                bank: 21,
                status: 1 << 63,
                ipid: 0x96 << 32,
                ..Record::EMPTY
            };
            let threshold = Record {
                kind: Kind::Threshold,
                ..umc
            };
            let uncorrected = Record {
                status: 1 << 63 | 1 << 61,
                ..umc
            };
            assert_eq!(
                sel(&Event::Mca(&threshold)).as_bytes()[10..],
                [0xc, 0xff, 0x6f, 0xa5, 3, 21]
            );
            assert_eq!(
                sel(&Event::Mca(&uncorrected)).as_bytes()[10..],
                [0xc, 0xff, 0x6f, 0xa1, 3, 21]
            );
            assert_eq!(
                sel(&Event::Probe).as_bytes()[10..],
                [0x12, 0xff, 0x6f, 0xa3, b'F', b'S']
            );
        }
    }
}
