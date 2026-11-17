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
//! An error storm is the vendors' leaky bucket ([`Storm`]): more
//! thresholding SMIs than the bucket holds and SMM stops interrupting.
//! Every CPU then counts without an interrupt and the rendezvous owner
//! ([`own`]) runs the FCH's periodic SMI, on which every CPU polls its
//! counters (`ccx::ras::poll_thresholds`: an overflow is still recorded
//! and the counter re-armed); after [`Storm::smis`] polls without a
//! counted error the counters interrupt again. The OS keeps every bank
//! throughout: Linux polls them itself (its `check_interval`, halved
//! while it finds errors, down to 10 ms) and decodes them (EDAC); the
//! locked blocks are SMM's in both modes, so Linux's own thresholding
//! interrupt and storm control stay off for them.
//!
//! The journal goes to the board's BMC, from the rendezvous owner only
//! ([`own`]): an [`Event`] at a time through the board's hook, which owns
//! the transport and may use [`sel`] for the record. The owner waits (at
//! most [`HOLD_US`]) for every CPU to have done its local work, which
//! journals what it found. On the OS's channel ([`Policy::shared_bmc`])
//! it delivers only with every CPU held in SMM (so the OS cannot start a
//! transaction meanwhile) and the interface idle; a transaction of the
//! OS's in progress is not touched: the report waits for the next
//! periodic SMI, which the owner keeps running while one waits. Delivery
//! is bounded: [`SEL_PER_BOOT`] events per boot, at most [`SEL_PER_SMI`]
//! events and none started after [`DELIVERY_US`] in one SMI however many
//! CPUs come to own its rendezvous (what is left waits for a later SMI),
//! the first failure stops it.
//!
//! A thresholding counter is SMM's if SMM armed its block, and every write
//! to it is one CPU's at a time: the handler keeps every armed block by
//! its node and MCA_IPID ([`Blocks`]), reserved by the one CPU that arms
//! it (a shared bank, an SMT core's, a CCX's L3, a socket's UMCs, is one
//! block every CPU of the share sees), then claimed for every write: an
//! overflow's re-arm by whichever CPU of the share handles it, a switch
//! between interrupts and polling and a poll by the CPU that armed it. A
//! block someone else locked is never written.
//!
//! No heap, flash writes, OS buffers, GHES, address-to-DIMM guesses or
//! bank clearing. The first records survive in locked SMRAM until reset;
//! this is deliberately not a persistent error store.

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
    /// When the counters stop interrupting.
    pub storm: Storm,
    /// The board's BMC channel is the OS's too: report only while every
    /// CPU is in SMM and the interface idle.
    pub shared_bmc: bool,
}

/// The storm limit, the vendors' leaky bucket (AMD PBS "RAS": "SMI
/// Threshold" 5 per "SMI Scale" 1000 ms, "SMI Period" 1000 ms, the S8040
/// V2.00 setup defaults; AmdPlatformRasRsSmm `0x95f4` counts thresholding
/// SMIs and leaks one per scale/threshold, at the threshold `0x5700`
/// re-arms the counters without an interrupt and registers the periodic
/// SMI, `0x5830` polls on it and returns to interrupts after a quiet
/// bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storm {
    /// Thresholding SMIs (of every CPU) the bucket holds.
    pub smis: u16,
    /// The time one SMI takes to leak out, times [`Storm::smis`], in ms.
    pub window_ms: u16,
    /// The polling period in ms (1 to 32767); also when a report the
    /// OS's channel deferred is tried again.
    pub poll_ms: u16,
}

impl Storm {
    /// The vendors' defaults.
    pub const VENDOR: Self = Self {
        smis: 5,
        window_ms: 1000,
        poll_ms: 1000,
    };
}

impl Policy {
    /// OS-first: no SMM counting, observation or SEL.
    pub const OS_FIRST: Self = Self {
        threshold: 0,
        observe_mce: false,
        storm: Storm::VENDOR,
        shared_bmc: false,
    };
    /// SMM counts corrected errors with the vendors' threshold of 10 and
    /// their storm limit, reporting on a channel the OS does not use.
    pub const THRESHOLDS: Self = Self {
        threshold: 10,
        observe_mce: false,
        storm: Storm::VENDOR,
        shared_bmc: false,
    };

    /// SMM handles machine-check events at all.
    pub const fn smm(&self) -> bool {
        self.threshold != 0 || self.observe_mce
    }
}

/// The leaky bucket of [`Storm`]: [`Bucket::hit`] for each thresholding
/// SMI, true when it overflows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bucket {
    level: u32,
    /// Since when the bucket leaks: the time of the last whole SMI it
    /// leaked, or of the SMI that found it empty.
    last_us: u64,
}

impl Bucket {
    /// Count an SMI at `now_us`, after leaking one per `window_ms / smis`
    /// since the bucket last leaked: whole SMIs, the rest of the time
    /// carried over to the next hit, so arrivals closer than that still
    /// leak. True (and empty again) when it reaches [`Storm::smis`], as the
    /// vendor's `0x95f4`. The SMI that arrives is never leaked itself.
    pub fn hit(&mut self, storm: &Storm, now_us: u64) -> bool {
        let smis = u32::from(storm.smis);
        if smis == 0 {
            return false;
        }
        let per_us = u64::from(storm.window_ms) * 1000 / u64::from(smis);
        if self.level != 0 && per_us != 0 {
            let leaked = now_us.saturating_sub(self.last_us) / per_us;
            self.level -= leaked.min(u64::from(self.level)) as u32;
            self.last_us = if self.level == 0 {
                // Empty: it leaks nothing until the next SMI is in it.
                now_us
            } else {
                self.last_us + leaked * per_us
            };
        } else if self.level == 0 {
            self.last_us = now_us;
        }
        self.level += 1;
        if self.level >= smis {
            self.level = 0;
            return true;
        }
        false
    }
}

/// What the board's BMC hook did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The BMC took it.
    Sent,
    /// The channel is in use (the OS's transaction): try again later.
    Busy,
    /// The BMC did not take it.
    Failed,
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
pub fn no_bmc(_: &Event<'_>, _: Tsc) -> Delivery {
    Delivery::Failed
}

/// The KCS poll budget of one event (`Kcs::with_poll_budget`), the abort
/// of a failed one included: 25000 readiness checks 10 us apart, about
/// 250 ms in all. The S8040's BMC takes about 25 ms to answer an event.
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
    DELIVERY_US, DIAG_COMMAND, DIAG_REQUEST, HOLD_US, MCE_SMI_COMMAND, RETRY_MS, RETRY_SMIS,
    SEL_PER_BOOT, SEL_PER_SMI, enter, own,
};

#[cfg(feature = "smm")]
mod handler {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{
        AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
    };

    use fstart_arch::x86::cpu::amd::msr::SMM_BASE;
    use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
    use fstart_core::services::Timer;
    use fstart_driver_amd::ccx::ras::{self, Kind, LocalSmiStatus, Record};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::cpu::Silicon;
    use fstart_driver_amd::fch::smi::{self as fch_smi, source};
    use fstart_driver_amd::window::{Mmio, Window};
    use tock_registers::interfaces::Readable;

    use super::{Bucket, Delivery, Event, Policy};

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
    /// The period of the SMI that retries what waits for delivery, when
    /// no storm sets it.
    pub const RETRY_MS: u16 = 100;
    /// Periodic SMIs a waiting report gets, from its record or the last
    /// event that went out ([`Retry`]).
    pub const RETRY_SMIS: u32 = 16;
    /// Thresholding blocks SMM can own: every thread's core banks, every
    /// L3 slice and socket bank of two 96-core sockets.
    const BLOCKS: usize = 4096;
    /// How long the rendezvous owner waits for every CPU to be in SMM, its
    /// local work done (EDK2 waits up to its PcdCpuSmmApSyncTimeout). A CPU
    /// that never comes (wait-for-SIPI) makes it give up each time: the
    /// storm clock then waits, and so does a report on the OS's channel.
    pub const HOLD_US: u64 = 2000;
    /// The I/O cycle data of the local SMI trigger; never sufficient to
    /// establish an MCA cause.
    pub const MCE_SMI_COMMAND: u8 = 0x80;
    /// Diagnostic I/O ABI: OUT AL,DX with RAX=DIAG_REQUEST, RDX=B2, RCX selector.
    /// 0 reads PFEH/cloak/mask into RAX/RCX/RDX. 1 requests one benign SEL probe;
    /// 2 reads its result (0 idle, 1 queued, 4 running, 2 success, 3 failure).
    /// 3 checks saved-SMBASE offsets (RAX=FF00 match, RCX=FE00 match, RDX=revision).
    /// 4 reads RAX=records journaled, RCX=dropped, RDX=refused CPUs.
    /// 5 reads the storm state: RAX=polling | periodic SMI on << 1 | this CPU
    /// polling << 2, RCX=switches to polling | periodic SMIs << 32,
    /// RDX=reports deferred (channel busy) | owner waits that timed out << 32.
    /// 6 reads the owner's wait: RAX=CPUs counted at the last timeout,
    /// RCX=the longest wait that succeeded (us), RDX=CPUs the handler counts.
    /// 7 reads RAX=the banks whose counters this CPU armed, RCX=the blocks
    /// SMM armed on every CPU, RDX=the banks with a counter this CPU left
    /// alone, their scope not known.
    /// No pointers, error injection or arbitrary MSRs are accepted.
    pub const DIAG_COMMAND: u8 = 0xd1;
    pub const DIAG_REQUEST: u64 = 0x4653_5241_5300_00d1;

    /// Per CPU: 0 not tried, [`TRIGGER`] armed, with [`PFEH`] too, or
    /// [`REFUSED`]; [`POLLING`] while its counters count without an
    /// interrupt.
    static CPU_STATE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
    const TRIGGER: u8 = 1;
    const PFEH: u8 = 2;
    const REFUSED: u8 = 4;
    const POLLING: u8 = 8;
    /// Per CPU: the banks whose counters it armed ([`ras::arm_thresholds`]).
    static BANKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    /// Per CPU: the banks with a counter it left alone, their scope not
    /// known ([`ras::Scope::of`]).
    static UNSCOPED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static ARMED: Blocks = Blocks::new();
    /// Per CPU: the sum of its counters at its last poll.
    static COUNTED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
    static REFUSED_CPUS: AtomicU32 = AtomicU32::new(0);
    static PROBE: AtomicU8 = AtomicU8::new(0);
    /// The delivery lock.
    static DELIVERING: AtomicBool = AtomicBool::new(false);
    static SEL_FAILED: AtomicBool = AtomicBool::new(false);
    static JOURNAL: Journal = Journal::new();
    static RETRY: Retry = Retry::new();

    /// The storm: the counters count without an interrupt.
    static STORM: AtomicBool = AtomicBool::new(false);
    static BUCKET: TryLock<Bucket> = TryLock::new(Bucket {
        level: 0,
        last_us: 0,
    });
    /// A poll counted an error since the last periodic SMI.
    static ACTIVITY: AtomicBool = AtomicBool::new(false);
    /// The rendezvous owner's: the periodic SMI runs, quiet polls left.
    static PERIODIC: AtomicBool = AtomicBool::new(false);
    /// Its period while it runs.
    static PERIOD_MS: AtomicU16 = AtomicU16::new(0);
    static QUIET_LEFT: AtomicU32 = AtomicU32::new(0);
    static SWITCHES: AtomicU32 = AtomicU32::new(0);
    static TICKS: AtomicU32 = AtomicU32::new(0);
    static DEFERRED: AtomicU32 = AtomicU32::new(0);
    static HOLD_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
    static HOLD_LAST: AtomicU32 = AtomicU32::new(0);
    static HOLD_LONGEST_US: AtomicU32 = AtomicU32::new(0);
    static HOLD_TOTAL: AtomicU32 = AtomicU32::new(0);

    /// A value one CPU at a time works on; a CPU that finds it taken goes
    /// on without it (it never spins: a sibling may wait in a #MC).
    struct TryLock<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    // SAFETY: every access to the value is behind `with`'s acquire/release
    // lock; no reference to it escapes the closure.
    unsafe impl<T: Send> Sync for TryLock<T> {}

    impl<T> TryLock<T> {
        const fn new(value: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                value: UnsafeCell::new(value),
            }
        }

        fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
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
            Some(f(unsafe { &mut *self.value.get() }))
        }
    }

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

    /// The counters SMM armed that this CPU claimed for its writes in this
    /// SMI, this CPU's or a CPU's it shares the bank with; released when
    /// dropped. A block SMM did not arm, or another CPU writes now, is not
    /// this one's to write.
    struct Claims {
        sharing: ras::Sharing,
        claimed: heapless::Vec<usize, { ras::MAX_BANKS as usize }>,
    }

    impl Claims {
        fn new() -> Self {
            Self {
                sharing: ras::Sharing::current(),
                claimed: heapless::Vec::new(),
            }
        }

        /// Claim the block of the bank with MCA_IPID `ipid`.
        fn owns(&mut self, ipid: u64) -> bool {
            let Some(key) = self.sharing.key(ipid) else {
                return false;
            };
            ARMED.claim(key).is_some_and(|index| {
                self.claimed
                    .push(index)
                    .map_err(|_| ARMED.release(index))
                    .is_ok()
            })
        }
    }

    impl Drop for Claims {
        fn drop(&mut self) {
            self.claimed.iter().for_each(|&index| ARMED.release(index));
        }
    }

    /// `ras::observe_mca` with the counters SMM armed ([`Claims`]).
    fn observe_owned(
        cpu: &NativeMsr,
        logical_cpu: u32,
        status: u32,
        rearm: Option<u16>,
        sink: impl FnMut(Record),
    ) -> Option<ras::McaEvents> {
        let mut claims = Claims::new();
        ras::observe_mca(
            cpu,
            logical_cpu,
            status,
            rearm,
            |_, ipid| claims.owns(ipid),
            sink,
        )
    }

    struct Journal {
        dropped: AtomicU32,
        state: TryLock<State>,
    }

    struct State {
        records: [Record; RECORDS],
        count: usize,
        pending: usize,
        sent: u32,
    }

    impl Journal {
        const fn new() -> Self {
            Self {
                dropped: AtomicU32::new(0),
                state: TryLock::new(State {
                    records: [Record::EMPTY; RECORDS],
                    count: 0,
                    pending: 0,
                    sent: 0,
                }),
            }
        }

        fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
            self.state.with(f)
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

        /// The next record to send, left in place until [`Journal::sent`].
        fn peek(&self) -> Option<Record> {
            self.with(|state| {
                if state.pending >= state.count || state.sent >= SEL_PER_BOOT {
                    return None;
                }
                state.records.get(state.pending).copied()
            })
            .flatten()
        }

        /// The record [`Journal::peek`] returned is done with.
        fn sent(&self) {
            // A producer holds the lock only to store a record.
            while self
                .with(|state| {
                    state.pending += 1;
                    state.sent += 1;
                })
                .is_none()
            {
                core::hint::spin_loop();
            }
        }

        fn waiting(&self) -> bool {
            self.with(|state| state.pending < state.count && state.sent < SEL_PER_BOOT)
                .unwrap_or(true)
        }
    }

    /// Every entrant runs this, including CPUs that do not own the FCH
    /// dispatch: the local MCA work of `policy`, which journals what it
    /// finds for the owner to deliver ([`own`]). Arming happens on the
    /// installer's per-CPU self-SMI, after SMRAM is locked. No CPU waits
    /// for a sibling here and a journal collision never delays native #MC.
    #[inline(always)]
    pub fn enter(policy: &Policy, logical_cpu: u32, silicon: Silicon) {
        if policy.smm() {
            enter_enabled(logical_cpu, silicon, *policy);
        }
    }

    // Keep SMRAM static references in their defining crate. Cross-crate
    // inlining into the board entry can materialize a GOT access, forbidden
    // in PIC SMM.
    #[inline(never)]
    fn enter_enabled(logical_cpu: u32, silicon: Silicon, policy: Policy) {
        let index = logical_cpu as usize;
        let (Some(state), Some(banks), Some(counted)) =
            (CPU_STATE.get(index), BANKS.get(index), COUNTED.get(index))
        else {
            return;
        };
        let (threshold, observe_mce) = (policy.threshold, policy.observe_mce);
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
                let armed = ras::arm_thresholds(
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
                banks.store(armed, Ordering::Relaxed);
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
        let unscoped = UNSCOPED
            .get(index)
            .map_or(0, |unscoped| unscoped.load(Ordering::Relaxed));
        diagnostics(&cpu, saved, armed, banks.load(Ordering::Relaxed), unscoped);
        let mut sink = |record| {
            JOURNAL.push(record);
            RETRY.renew();
        };
        if armed & PFEH != 0 {
            ras::observe(&cpu, logical_cpu, status, &mut sink);
        }
        if threshold == 0 {
            observe_owned(&cpu, logical_cpu, status, None, &mut sink);
        } else {
            let banks = banks.load(Ordering::Relaxed);
            let storm = STORM.load(Ordering::Acquire);
            let was_polling = armed & POLLING != 0;
            if storm != was_polling {
                // The counters change modes: what they latched is sampled
                // and journaled first, never reset unseen. Each under its
                // claim: a sibling may be re-arming its overflow. One that
                // could not be claimed is switched on a later SMI, this
                // CPU still in its old mode.
                let mut claims = Claims::new();
                let poll = ras::switch_counting(
                    &cpu,
                    logical_cpu,
                    banks,
                    threshold,
                    !storm,
                    |_, ipid| claims.owns(ipid),
                    &mut sink,
                );
                drop(claims);
                counted.store(poll.counted, Ordering::Relaxed);
                if storm && poll.overflowed != 0 {
                    ACTIVITY.store(true, Ordering::Release);
                }
                if poll.skipped == 0 {
                    state.fetch_xor(POLLING, Ordering::Relaxed);
                }
            }
            if storm {
                if was_polling {
                    let mut claims = Claims::new();
                    let poll = ras::poll_thresholds(
                        &cpu,
                        logical_cpu,
                        banks,
                        threshold,
                        false,
                        |_, ipid| claims.owns(ipid),
                        &mut sink,
                    );
                    drop(claims);
                    // A sum that left a block out (a sibling held it) is not
                    // the last one's to compare with: no activity from it.
                    let changed = poll.skipped == 0
                        && counted.swap(poll.counted, Ordering::Relaxed) != poll.counted;
                    if poll.overflowed != 0 || changed {
                        ACTIVITY.store(true, Ordering::Release);
                    }
                }
                // A deferred error's SMI (PFEH). The counters are the
                // poll's while it lasts: they are not claimed here, so no
                // CPU's poll finds one taken by an observer.
                ras::observe_mca(&cpu, logical_cpu, status, None, |_, _| false, &mut sink);
            } else if observe_owned(&cpu, logical_cpu, status, Some(threshold), &mut sink)
                .is_some_and(|events| events.thresholds != 0)
            {
                // SAFETY: SMM, ring 0; the P-state MSRs.
                let now = Tsc::native(&cpu).timestamp_us();
                if BUCKET.with(|bucket| bucket.hit(&policy.storm, now)) == Some(true) {
                    STORM.store(true, Ordering::Release);
                    SWITCHES.fetch_add(1, Ordering::Relaxed);
                }
            }
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
                state.fetch_and(!PFEH, Ordering::Relaxed);
                REFUSED_CPUS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn diagnostics<M: MsrAccess>(
        cpu: &M,
        saved: fstart_smm::X86SaveState,
        state: u8,
        banks: u64,
        unscoped: u64,
    ) {
        use fstart_smm::save_state::Amd64Register::{Rax, Rcx, Rdx};
        if saved.read_amd64(Rax) != Ok(DIAG_REQUEST) || saved.read_amd64(Rdx) != Ok(0xb2) {
            return;
        }
        let load = |counter: &AtomicU32| u64::from(counter.load(Ordering::Relaxed));
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
                let _ = saved.write_amd64(Rcx, load(&JOURNAL.dropped));
                let _ = saved.write_amd64(Rdx, load(&REFUSED_CPUS));
            }
            Ok(5) => {
                let flags = u64::from(STORM.load(Ordering::Relaxed))
                    | u64::from(PERIODIC.load(Ordering::Relaxed)) << 1
                    | u64::from(state & POLLING != 0) << 2;
                let _ = saved.write_amd64(Rax, flags);
                let _ = saved.write_amd64(Rcx, load(&SWITCHES) | load(&TICKS) << 32);
                let _ = saved.write_amd64(Rdx, load(&DEFERRED) | load(&HOLD_TIMEOUTS) << 32);
            }
            Ok(6) => {
                let _ = saved.write_amd64(Rax, load(&HOLD_LAST));
                let _ = saved.write_amd64(Rcx, load(&HOLD_LONGEST_US));
                let _ = saved.write_amd64(Rdx, load(&HOLD_TOTAL));
            }
            Ok(7) => {
                let _ = saved.write_amd64(Rax, banks);
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

    /// The rendezvous owner's part, with the FCH's ACPI block `acpi`,
    /// before the FCH's dispatch: wait (at most [`HOLD_US`]) until every
    /// CPU has done its local work and is parked for this one (`cpus`, the
    /// handler's `SmmContext::cpus_held`: the CPUs that acknowledged this
    /// owner's rendezvous, not ones of a previous SMI still on their way
    /// out); on the periodic SMI take the storm's clock; on the SMI's
    /// `first` owner (`SmmContext::first_owner`) deliver the journal
    /// through `bmc` (on the OS's channel only if every CPU is held); then
    /// run the periodic SMI while the storm lasts, a CPU's counters are
    /// not in its mode yet, or a report waits ([`Retry`]).
    #[inline(always)]
    pub fn own<W: Window>(
        policy: &Policy,
        acpi: &W,
        first: bool,
        cpus: impl Fn() -> Option<(u32, u32)>,
        bmc: impl FnMut(&Event<'_>, Tsc) -> Delivery,
    ) {
        if !policy.smm() {
            return;
        }
        // SAFETY: SMM, ring 0; the P-state MSRs.
        let timer = Tsc::native(&unsafe { NativeMsr::new() });
        let start = timer.timestamp_us();
        let (held, seen) = loop {
            let waited = timer.timestamp_us().wrapping_sub(start);
            match cpus() {
                Some((inside, total)) if inside >= total => break (true, (inside, total, waited)),
                Some((inside, total)) if waited >= HOLD_US => {
                    break (false, (inside, total, waited));
                }
                Some(_) => core::hint::spin_loop(),
                None => break (false, (0, 0, waited)),
            }
        };
        let tick = [source::SHORT_TIMER, source::LONG_TIMER]
            .into_iter()
            .any(|s| fch_smi::is_pending(acpi, s));
        if before_delivery(policy, held, seen) {
            deliver(timer, first, bmc);
        }
        if let Some(periodic) = clock(policy, tick, held) {
            fch_smi::periodic(acpi, periodic);
        }
    }

    /// Whether the owner delivers now: on the OS's channel only with
    /// every CPU held. Counts what waits.
    // The statics stay in this crate (no GOT in the board's SMM image).
    #[inline(never)]
    fn before_delivery(
        policy: &Policy,
        held: bool,
        (inside, total, waited): (u32, u32, u64),
    ) -> bool {
        HOLD_TOTAL.store(total, Ordering::Relaxed);
        if held {
            HOLD_LONGEST_US.fetch_max(waited.min(u32::MAX.into()) as u32, Ordering::Relaxed);
        } else {
            HOLD_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            HOLD_LAST.store(inside, Ordering::Relaxed);
        }
        if policy.shared_bmc && !held {
            if JOURNAL.waiting() {
                DEFERRED.fetch_add(1, Ordering::Relaxed);
            }
            return false;
        }
        true
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
        left: AtomicU32,
    }

    impl Retry {
        const fn new() -> Self {
            Self {
                left: AtomicU32::new(0),
            }
        }

        /// A record came, or an event went out.
        fn renew(&self) {
            self.left.store(RETRY_SMIS, Ordering::Relaxed);
        }

        /// On an owner, after its pass: whether reports want the periodic
        /// SMI, `running` or not: `tick` if this SMI is the periodic one,
        /// `waiting` if a report waits, `held` if every CPU was parked for
        /// the owner.
        fn wants(&self, tick: bool, running: bool, waiting: bool, held: bool) -> bool {
            if running && tick {
                let _ = self
                    .left
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
            }
            (waiting && self.left.load(Ordering::Relaxed) != 0) || (!held && !tick)
        }
    }

    /// Whether an armed CPU's counters are not in the storm's mode yet
    /// (polling during it, interrupting after it).
    fn lagging() -> bool {
        let storm = STORM.load(Ordering::Acquire);
        CPU_STATE.iter().any(|state| {
            let state = state.load(Ordering::Relaxed);
            state & TRIGGER != 0 && (state & POLLING != 0) != storm
        })
    }

    /// The periodic SMI's clock, on the rendezvous owner after delivery:
    /// the storm's polls, the CPUs' switches back after it and the reports
    /// that wait ([`Retry`]). What to do with it (`Some(None)`: stop it,
    /// `Some(Some(ms))`: run it every `ms`), if anything.
    #[inline(never)]
    fn clock(policy: &Policy, tick: bool, held: bool) -> Option<Option<u16>> {
        let waiting = !SEL_FAILED.load(Ordering::Relaxed)
            && (JOURNAL.waiting() || PROBE.load(Ordering::Acquire) == 1);
        let storm = STORM.load(Ordering::Acquire);
        let running = PERIODIC.load(Ordering::Relaxed);
        let reports = RETRY.wants(tick, running, waiting, held);
        if tick {
            TICKS.fetch_add(1, Ordering::Relaxed);
        }
        // Every CPU polled on this SMI, before this owner: its activity
        // is in. Without them all, wait for a full tick.
        if storm && running && tick && held {
            if ACTIVITY.swap(false, Ordering::AcqRel) {
                QUIET_LEFT.store(policy.storm.smis.into(), Ordering::Relaxed);
            } else if QUIET_LEFT.load(Ordering::Relaxed) <= 1 {
                // The bucket's time without an error: interrupts again,
                // which every CPU sets on its next SMI.
                STORM.store(false, Ordering::Release);
            } else {
                QUIET_LEFT.fetch_sub(1, Ordering::Relaxed);
            }
        }
        // After the storm the periodic SMI runs until every CPU's counters
        // are back on interrupts: a CPU whose switch left a block a sibling
        // held (`ras::switch_counting`'s `skipped`) is still polling and
        // switches on the next SMI. That every CPU was held proves only
        // that its local work ended, not that it switched.
        // Reports alone retry at their own period.
        let polls = STORM.load(Ordering::Acquire) || lagging();
        let wanted = polls || reports;
        let period = if polls {
            policy.storm.poll_ms
        } else {
            RETRY_MS
        };
        if wanted == running && (!wanted || PERIOD_MS.load(Ordering::Relaxed) == period) {
            return None;
        }
        PERIODIC.store(wanted, Ordering::Relaxed);
        PERIOD_MS.store(period, Ordering::Relaxed);
        if wanted {
            QUIET_LEFT.store(policy.storm.smis.into(), Ordering::Relaxed);
            ACTIVITY.store(false, Ordering::Relaxed);
        }
        Some(wanted.then_some(period))
    }

    /// On the rendezvous owner: send what the journal holds to the board's
    /// BMC through `bmc`, in one pass per SMI ([`pass`]) of at most
    /// [`SEL_PER_SMI`] events and [`DELIVERY_US`], whatever comes in
    /// meanwhile; what is left waits for a later SMI. A busy channel
    /// leaves the event for a later SMI; the first failed event stops
    /// delivery until reset (no retry, no wait for the BMC); OS handoff
    /// never depends on it.
    #[inline(always)]
    fn deliver(timer: Tsc, first: bool, mut bmc: impl FnMut(&Event<'_>, Tsc) -> Delivery) {
        let start = timer.timestamp_us();
        pass(
            first,
            |event| bmc(event, timer),
            || timer.timestamp_us().wrapping_sub(start) < DELIVERY_US,
        );
    }

    /// One SMI's delivery, on its `first` owner only: a CPU that comes to
    /// own the rendezvous after another owner of its SMI (late, even with
    /// a source that latched meanwhile) delivers nothing, so one SMI has
    /// one pass whatever its owners. Then events, under the delivery lock,
    /// while fewer than [`SEL_PER_SMI`] went out, `in_time` holds, the
    /// channel is free and none failed.
    fn pass(first: bool, mut bmc: impl FnMut(&Event<'_>) -> Delivery, in_time: impl Fn() -> bool) {
        if !first || !begin() {
            return;
        }
        let mut sent = 0;
        while in_time() && sent < SEL_PER_SMI && !SEL_FAILED.load(Ordering::Relaxed) {
            let Some(event) = next() else {
                break;
            };
            let delivery = match event {
                Some(record) => bmc(&Event::Mca(&record)),
                None => bmc(&Event::Probe),
            };
            done(event.is_none(), delivery);
            if delivery != Delivery::Sent {
                break;
            }
            sent += 1;
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
        if let Some(record) = JOURNAL.peek() {
            return Some(Some(record));
        }
        PROBE
            .compare_exchange(1, 4, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
            .then_some(None)
    }

    /// What became of an event (under the lock).
    #[inline(never)]
    fn done(probe: bool, delivery: Delivery) {
        match (probe, delivery) {
            (true, Delivery::Busy) => PROBE.store(1, Ordering::Release),
            (true, _) => PROBE.store(
                if delivery == Delivery::Sent { 2 } else { 3 },
                Ordering::Release,
            ),
            (false, Delivery::Busy) => {}
            (false, _) => JOURNAL.sent(),
        }
        match delivery {
            Delivery::Sent => RETRY.renew(),
            Delivery::Busy => {
                DEFERRED.fetch_add(1, Ordering::Relaxed);
            }
            Delivery::Failed => SEL_FAILED.store(true, Ordering::Relaxed),
        }
    }

    #[inline(never)]
    fn end() {
        DELIVERING.store(false, Ordering::Release);
    }

    #[cfg(test)]
    mod tests {
        use super::super::{Storm, sel};
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
                assert_eq!(journal.peek().unwrap().bank, bank);
                assert_eq!(journal.peek().unwrap().bank, bank, "kept until sent");
                journal.sent();
            }
            assert!(journal.peek().is_none());
            assert!(!journal.waiting(), "the per-boot budget is spent");
            assert_eq!(
                journal.with(|s| s.count),
                Some(RECORDS),
                "delivery does not erase evidence"
            );
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

        /// SEL_PER_SMI events per SMI, what is left for later SMIs, and no
        /// event after the SMI's time is up.
        /// The tests of the journal and delivery statics, one at a time.
        static STATICS: std::sync::Mutex<()> = std::sync::Mutex::new(());

        #[test]
        fn delivery_is_bounded_per_smi_and_carries_the_rest_over() {
            let _statics = STATICS.lock().unwrap();
            for bank in 0..6 {
                JOURNAL.push(Record {
                    bank: 100 + bank,
                    status: 1 << 63,
                    ..Record::EMPTY
                });
            }
            let banks = |first, in_time: &dyn Fn() -> bool, delivery: Delivery| {
                let mut banks = std::vec::Vec::new();
                pass(
                    first,
                    |event| {
                        if let Event::Mca(record) = event
                            && record.bank >= 100
                        {
                            banks.push(record.bank);
                        }
                        delivery
                    },
                    in_time,
                );
                banks
            };
            let sent = Delivery::Sent;
            assert_eq!(banks(true, &|| true, sent).len(), 2);
            // A late entrant of the same SMI owning it after the first
            // (even with a fresh source), in time or not: nothing.
            assert!(banks(false, &|| true, sent).is_empty());
            assert!(banks(false, &|| false, sent).is_empty());
            assert!(JOURNAL.waiting());
            assert_eq!(banks(true, &|| true, sent).len(), 2);
            // The SMI's time is up after the first event.
            let calls = core::cell::Cell::new(0);
            let in_time = || {
                calls.set(calls.get() + 1);
                calls.get() < 2
            };
            assert_eq!(banks(true, &in_time, sent).len(), 1);
            // Out of time before the first: nothing, and it stays queued.
            assert!(banks(true, &|| false, sent).is_empty());
            // A busy channel: the event stays for later.
            assert_eq!(banks(true, &|| true, Delivery::Busy).len(), 1);
            assert!(JOURNAL.waiting());
            assert_eq!(banks(true, &|| true, sent), [105]);
            assert!(!JOURNAL.waiting());
            // Another CPU holding the lock: nothing.
            JOURNAL.push(Record {
                bank: 106,
                status: 1 << 63,
                ..Record::EMPTY
            });
            DELIVERING.store(true, Ordering::Relaxed);
            assert!(banks(true, &|| true, sent).is_empty());
            DELIVERING.store(false, Ordering::Relaxed);
            // A failed event ends delivery, read under the lock.
            assert_eq!(banks(true, &|| true, Delivery::Failed), [106]);
            JOURNAL.push(Record {
                bank: 107,
                status: 1 << 63,
                ..Record::EMPTY
            });
            assert!(banks(true, &|| true, sent).is_empty());
            SEL_FAILED.store(false, Ordering::Relaxed);
            assert_eq!(banks(true, &|| true, sent), [107]);
            assert!(!DELIVERING.load(Ordering::Relaxed));
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
            diagnostics(&Cpu, saved, TRIGGER, 0, 0);
            assert_eq!(saved.read_amd64(Rax), Ok(DIAG_REQUEST));
            saved.write_amd64(Rdx, 0xb2).unwrap();
            diagnostics(&Cpu, saved, TRIGGER, 0, 0);
            assert_eq!(saved.read_amd64(Rax), Ok(1));
            assert_eq!(saved.read_amd64(Rcx), Ok(0));
            assert_eq!(saved.read_amd64(Rdx), Ok(0));
        }

        /// ACPI-MMIO as bytes, for the periodic SMI's registers.
        struct Acpi(core::cell::RefCell<[u8; 0x400]>);
        impl Acpi {
            fn new() -> Self {
                Self(core::cell::RefCell::new([0; 0x400]))
            }
            fn get(&self, off: u32, width: usize) -> u32 {
                let bytes = self.0.borrow();
                (0..width).fold(0, |v, i| v | u32::from(bytes[off as usize + i]) << (8 * i))
            }
            fn put(&self, off: u32, width: usize, value: u32) {
                let mut bytes = self.0.borrow_mut();
                for i in 0..width {
                    bytes[off as usize + i] = (value >> (8 * i)) as u8;
                }
            }
        }
        impl Window for Acpi {
            fn read32(&self, off: u32) -> u32 {
                self.get(off, 4)
            }
            fn write32(&self, off: u32, value: u32) {
                self.put(off, 4, value)
            }
            fn read16(&self, off: u32) -> u16 {
                self.get(off, 2) as u16
            }
            fn write16(&self, off: u32, value: u16) {
                self.put(off, 2, value.into())
            }
            fn read8(&self, off: u32) -> u8 {
                self.get(off, 1) as u8
            }
            fn write8(&self, off: u32, value: u8) {
                self.put(off, 1, value.into())
            }
        }

        /// The owner's storm clock: polling starts the periodic SMI, a
        /// poll that counted keeps it, a quiet bucket ends the storm, and
        /// the periodic SMI stops only once every armed CPU's counters are
        /// back on interrupts, not when every CPU was merely held.
        #[test]
        fn a_storm_polls_until_a_quiet_bucket() {
            let _statics = STATICS.lock().unwrap();
            let policy = Policy {
                storm: Storm {
                    smis: 2,
                    window_ms: 1000,
                    poll_ms: 1000,
                },
                ..Policy::THRESHOLDS
            };
            let acpi = Acpi::new();
            let timer = fch_smi::SMI_TIMER.offset;
            let periodic = |policy: &Policy, acpi: &Acpi, tick, held, reports: bool| {
                // A report waiting on the OS's channel, as `own` sees it.
                if reports {
                    JOURNAL.push(Record::EMPTY);
                    RETRY.renew();
                }
                let policy = Policy {
                    shared_bmc: true,
                    ..*policy
                };
                if let Some(action) = clock(&policy, tick, held) {
                    fch_smi::periodic(acpi, action);
                }
                if reports {
                    JOURNAL.with(|state| state.pending = state.count);
                }
            };
            periodic(&policy, &acpi, false, true, false);
            assert_eq!(acpi.get(timer, 2), 0, "no storm, no periodic SMI");
            STORM.store(true, Ordering::Release);
            periodic(&policy, &acpi, false, true, false);
            assert_eq!(acpi.get(timer, 2), 0x8000 | 1000);
            ACTIVITY.store(true, Ordering::Release);
            periodic(&policy, &acpi, true, true, false);
            assert!(STORM.load(Ordering::Acquire), "a poll counted errors");
            periodic(&policy, &acpi, true, false, false);
            assert!(STORM.load(Ordering::Acquire), "not every CPU polled");
            periodic(&policy, &acpi, true, true, false);
            assert!(STORM.load(Ordering::Acquire), "one quiet poll of two");
            // Two armed CPUs, polling.
            let cpus = [&CPU_STATE[0], &CPU_STATE[1]];
            cpus.iter()
                .for_each(|cpu| cpu.store(TRIGGER | POLLING, Ordering::Relaxed));
            periodic(&policy, &acpi, true, true, false);
            assert!(!STORM.load(Ordering::Acquire), "a quiet bucket");
            assert!(PERIODIC.load(Ordering::Relaxed), "the CPUs still poll");
            // The last recovery tick: every CPU held, but CPU 1's switch
            // skipped a block a sibling held, so it still polls: the
            // periodic SMI goes on.
            cpus[0].store(TRIGGER, Ordering::Relaxed);
            periodic(&policy, &acpi, true, true, false);
            assert!(
                PERIODIC.load(Ordering::Relaxed),
                "a CPU has not switched back"
            );
            assert_eq!(acpi.get(timer, 2) & 0x8000, 0x8000);
            cpus[1].store(TRIGGER, Ordering::Relaxed);
            periodic(&policy, &acpi, true, true, false);
            assert!(!PERIODIC.load(Ordering::Relaxed));
            assert_eq!(acpi.get(timer, 2) & 0x8000, 0);
            // A report that waits runs it too, at the retry's period.
            periodic(&policy, &acpi, false, true, true);
            assert!(PERIODIC.load(Ordering::Relaxed));
            assert_eq!(acpi.get(timer, 2), 0x8000 | u32::from(RETRY_MS));
            // A storm meanwhile polls at its own period.
            STORM.store(true, Ordering::Release);
            periodic(&policy, &acpi, false, true, false);
            assert_eq!(acpi.get(timer, 2), 0x8000 | 1000);
            STORM.store(false, Ordering::Release);
            periodic(&policy, &acpi, true, true, false);
            assert!(!PERIODIC.load(Ordering::Relaxed));
            assert_eq!(acpi.get(timer, 2) & 0x8000, 0);
        }

        /// A record journaled after its SMI's pass, by a CPU that reached
        /// SMM late, with no SMI after it: the owner saw a CPU missing at
        /// its decision and keeps the periodic SMI for one period, in
        /// which the record is found and delivered, then lets it stop. A
        /// report the channel never takes (the OS's busy channel) gets
        /// RETRY_SMIS periods from its record.
        #[test]
        fn a_late_record_is_retried_without_another_smi() {
            let retry = Retry::new();
            // The SMI's owner: nothing waits, one CPU is not parked yet.
            assert!(retry.wants(false, false, false, false));
            // That CPU journals now, after the pass.
            retry.renew();
            // The periodic SMI: the record waits; every CPU is held.
            assert!(retry.wants(true, true, true, true));
            // Delivered on that SMI's pass: nothing waits, stop.
            assert!(!retry.wants(true, true, false, true));
            // Every CPU held and nothing waiting: no periodic SMI at all.
            assert!(!retry.wants(false, false, false, true));
            // A periodic SMI's own missing CPU adds no period.
            assert!(!retry.wants(true, true, false, false));
            // A report that never goes out: RETRY_SMIS periods, then none.
            retry.renew();
            assert!(retry.wants(false, false, true, true));
            for _ in 1..RETRY_SMIS {
                assert!(retry.wants(true, true, true, true));
            }
            assert!(!retry.wants(true, true, true, true), "spent");
            assert!(!retry.wants(false, false, true, true), "until renewed");
            retry.renew();
            assert!(retry.wants(false, false, true, true));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bucket_takes_the_vendors_five_smis_per_second() {
        let storm = Storm::VENDOR;
        let mut bucket = Bucket::default();
        // One SMI every 200 ms leaks as fast as it fills.
        assert!((1..=20).all(|n| !bucket.hit(&storm, n * 200_000)));
        // Five within 200 ms do not.
        let mut bucket = Bucket::default();
        let hits: [bool; 5] = core::array::from_fn(|n| bucket.hit(&storm, 1 + n as u64 * 10_000));
        assert_eq!(hits, [false, false, false, false, true]);
        // It starts empty again.
        assert!(!bucket.hit(&storm, 60_000));
        // A storm limit of zero never polls.
        let off = Storm { smis: 0, ..storm };
        assert!((0..100).all(|n| !Bucket::default().hit(&off, n)));
    }

    /// Arrivals closer than a leak interval: five per second on average,
    /// 150 and 250 ms apart in turn. The time below an interval carries
    /// over, so they leak as fast as they come and never overflow; dropped
    /// at each hit, the bucket overflowed after seven.
    #[test]
    fn the_bucket_carries_the_time_below_a_leak_over() {
        let storm = Storm::VENDOR;
        let mut bucket = Bucket::default();
        let mut now = 1_000_000;
        for n in 0..1000 {
            assert!(!bucket.hit(&storm, now), "hit {n}");
            now += if n % 2 == 0 { 150_000 } else { 250_000 };
        }
    }

    /// After a quiet second the bucket is empty: the SMI that comes then is
    /// in it, not leaked with the quiet time, and five in quick succession
    /// overflow it at the fifth.
    #[test]
    fn the_bucket_leaks_before_it_takes_the_smi() {
        let storm = Storm::VENDOR;
        let mut bucket = Bucket::default();
        assert!(!bucket.hit(&storm, 1_000));
        assert!(!bucket.hit(&storm, 2_000));
        let hits: [bool; 5] =
            core::array::from_fn(|n| bucket.hit(&storm, 5_000_000 + n as u64 * 1_000));
        assert_eq!(hits, [false, false, false, false, true]);
    }
}
