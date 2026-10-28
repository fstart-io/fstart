//! Native SMM MCA observer: CPU-local capture, shared bounded reporting.
//!
//! No heap, flash writes, OS buffers, GHES, address-to-DIMM guesses, bank
//! clearing or all-CPU barrier. The first records survive in locked SMRAM
//! until reset; this is deliberately not a persistent error store.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use fstart_arch::x86::cpu::amd::msr::SMM_BASE;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_driver_amd::sp3::ccx::ras::{self, LocalSmiStatus, Record};
use fstart_driver_amd::sp3::fch::{Mmio, Window};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_ipmi::SelRecord;
use tock_registers::interfaces::Readable;
use zerocopy::little_endian::{U16, U32};

/// A firmware-exclusive KCS endpoint, not the interface exposed to the OS.
#[derive(Debug, Clone, Copy)]
pub struct SelTransport {
    pub port: u16,
    pub spacing: u16,
}

/// Compile-time board policy, independent of the small installer config ABI.
/// Enabling this is experimental until native #MC handoff is hardware-tested.
pub trait Policy {
    const OBSERVE_MCE: bool;
    /// Restricted, correctable-memory EINJ in platform-owned scratch RAM.
    const MEMORY_EINJ: bool = false;
    const SEL: Option<SelTransport> = None;
}

/// Default for boards that have not opted into MCA observation.
pub struct OsFirst;
impl Policy for OsFirst {
    const OBSERVE_MCE: bool = false;
}

// SP3 supports at most 2 sockets * 64 cores * 2 threads. Indices outside
// this bound are left OS-first and never have redirection enabled.
const MAX_CPUS: usize = 256;
const RECORDS: usize = 64;
const MAX_SEL_ATTEMPTS: u32 = 8;
/// At most 20 ms of polling delays for one entire SEL transaction, not per byte.
const SEL_POLL_BUDGET: u32 = 2_000;
/// Only a transport command: never sufficient to establish an MCA cause.
pub const MCE_SMI_COMMAND: u8 = 0x80;
/// Diagnostic I/O ABI: OUT AL,DX with RAX=DIAG_REQUEST, RDX=B2, RCX selector.
/// 0 reads PFEH/cloak/mask into RAX/RCX/RDX. 1 requests one benign SEL probe;
/// 2 reads its result (0 idle, 1 queued, 4 running, 2 success, 3 failure).
/// 3 checks saved-SMBASE offsets (RAX=FF00 match, RCX=FE00 match, RDX=revision).
/// No pointers, error injection or arbitrary MSRs are accepted.
pub const DIAG_COMMAND: u8 = 0xd1;
pub const DIAG_REQUEST: u64 = 0x4653_5241_5300_00d1;
static PROBE: AtomicU8 = AtomicU8::new(0);
static CPU_STATE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
static REFUSED_CPUS: AtomicU32 = AtomicU32::new(0);
static SEL_FAILED: AtomicBool = AtomicBool::new(false);
static JOURNAL: Journal = Journal::new();

struct Journal {
    locked: AtomicBool,
    dropped: AtomicU32,
    state: UnsafeCell<State>,
}

struct State {
    records: [Record; RECORDS],
    count: usize,
    pending: usize,
    attempts: u32,
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
                attempts: 0,
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

    #[inline(always)]
    fn push(&self, record: Record) {
        let stored = self
            .with(|state| {
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
            if state.pending >= state.count || state.attempts >= MAX_SEL_ATTEMPTS {
                return None;
            }
            let record = *state.records.get(state.pending)?;
            state.pending += 1;
            state.attempts += 1;
            Some(record)
        })
        .flatten()
    }
}

/// Every entrant executes this, including CPUs that do not own FCH dispatch.
/// Arming happens on the installer's per-CPU self-SMI, after SMRAM is locked.
/// No CPU waits for a sibling and a journal collision never delays native #MC.
pub(super) fn enter<P: Policy>(logical_cpu: u32, generation: Generation) {
    if P::OBSERVE_MCE {
        enter_enabled(logical_cpu, generation);
    }
}

// Keep SMRAM static references in their defining crate. Cross-crate inlining
// into the board entry can materialize a GOT access, forbidden in PIC SMM.
#[inline(never)]
fn enter_enabled(logical_cpu: u32, generation: Generation) {
    let Some(state) = CPU_STATE.get(logical_cpu as usize) else {
        return;
    };
    let cpu = crate::hw::msr();
    let smbase = cpu.reg(SMM_BASE).get() as usize;
    // SAFETY: this thread's locked SMBASE window, entirely identity-mapped.
    let saved = unsafe {
        fstart_smm::X86SaveState::from_top(
            (smbase + 0x10000) as *mut u8,
            fstart_smm::X86SaveStateFormat::Amd64,
        )
    };
    if state.load(Ordering::Relaxed) == 0 {
        let stepping = core::arch::x86_64::__cpuid(1).eax as u8 & 0xf;
        let armed = saved.revision().is_ok()
            && ras::arm(
                &cpu,
                generation,
                stepping,
                fstart_driver_amd::sp3::fch::acpimmio::SMI_CMD as u16,
                MCE_SMI_COMMAND,
            )
            .is_ok();
        state.store(if armed { 1 } else { 2 }, Ordering::Relaxed);
        if !armed {
            REFUSED_CPUS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if state.load(Ordering::Relaxed) != 1 {
        return;
    }
    // SAFETY: this thread's installed AMD64 SMBASE; local status at +FEC4
    // is in its locked save-state window, mapped by the SMM identity tables.
    let save = unsafe { Mmio::new(smbase) };
    let status = save
        .reg::<u32, LocalSmiStatus::Register>(ras::LOCAL_SMI_STATUS)
        .get();
    diagnostics(&cpu, saved);
    ras::observe(&cpu, logical_cpu, status, |record| JOURNAL.push(record));
    if LocalSmiStatus::WRMSR.is_set(status) {
        use fstart_smm::save_state::Amd64Register::{Rax, Rcx, Rdx, Rip};
        if let (Ok(index), Ok(low), Ok(high), Ok(rip)) = (
            saved.read_amd64(Rcx),
            saved.read_amd64(Rax),
            saved.read_amd64(Rdx),
            saved.read_amd64(Rip),
        ) {
            let value = (high as u32 as u64) << 32 | low as u32 as u64;
            if !ras::replay_clear(&cpu, index as u32, value) {
                // A WRMSR trap saves the following RIP (the donor replays
                // without adjusting it). Retry its two-byte opcode outside
                // SMM, with the original GPRs/HWCR and native fault behavior.
                // If #MC is pending too, hand that off first; its handler or
                // the retried WRMSR will re-enter without MCE_REDIRECT set.
                if !LocalSmiStatus::MCE_REDIRECT.is_set(status) {
                    ras::disarm(&cpu);
                    state.store(2, Ordering::Relaxed);
                    REFUSED_CPUS.fetch_add(1, Ordering::Relaxed);
                }
                let _ = saved.write_amd64(Rip, rip.wrapping_sub(2));
            }
        }
    }
}

fn diagnostics<M: MsrAccess>(cpu: &M, saved: fstart_smm::X86SaveState) {
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
        Ok(selector @ (1 | 2)) => {
            if selector == 1 {
                let _ = PROBE.compare_exchange(0, 1, Ordering::Release, Ordering::Relaxed);
            }
            let _ = saved.write_amd64(Rax, u64::from(PROBE.load(Ordering::Acquire)));
        }
        _ => {}
    }
}

// Standard system events work on the bench BMC; OEM E0 entries are silently
// acknowledged without storage. FF means no particular SDR sensor is named.
fn event(sensor_type: u8, event_data: [u8; 3]) -> SelRecord {
    SelRecord {
        record_id: U16::ZERO,
        record_type: 2,
        timestamp: U32::ZERO,
        generator_id: U16::new(1),
        event_message_revision: 4,
        sensor_type,
        sensor_number: 0xff,
        event_dir_type: 0x6f,
        event_data,
    }
}

fn sel(record: &Record) -> SelRecord {
    let uncorrected = ras::McaStatus::UNCORRECTED.is_set(record.status)
        || ras::McaStatus::DEFERRED.is_set(record.status);
    // Processor: uncorrectable/correctable machine check. Event bytes 2/3
    // are OEM data: logical CPU index and bank, not guessed DIMM locations.
    event(
        7,
        [
            if uncorrected { 0xab } else { 0xac },
            record.cpu as u8,
            record.bank as u8,
        ],
    )
}

/// Shared owner only: at most one attempt per SMI, eight error reports and
/// one explicitly requested benign probe per boot. The
/// first failure trips a circuit breaker. There is no retry, abort recovery,
/// SEL-info query or wait for BMC readiness. A late entrant's queued record
/// may wait until a later SMI; OS handoff never depends on this delivery.
pub(super) fn flush<P: Policy>(timer: crate::hw::Tsc) {
    let Some(transport) = P::SEL.filter(|_| P::OBSERVE_MCE) else {
        return;
    };
    flush_enabled(transport, timer);
}

#[inline(never)]
fn flush_enabled(transport: SelTransport, timer: crate::hw::Tsc) {
    if SEL_FAILED.load(Ordering::Relaxed) {
        if PROBE.load(Ordering::Acquire) == 1 {
            PROBE.store(3, Ordering::Release);
        }
        return;
    }
    let (record, probe) = if let Some(record) = JOURNAL.next() {
        (sel(&record), false)
    } else if PROBE
        .compare_exchange(1, 4, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        // Entry added to auxiliary log, OEM data 'FS': a transport check,
        // never a manufactured machine-check record. At most once per boot.
        (event(0x12, [0xa3, b'F', b'S']), true)
    } else {
        return;
    };
    // SAFETY: board policy reserves a distinct, decoded KCS endpoint for
    // firmware; this is the single shared-dispatch owner.
    let mut kcs =
        unsafe { fstart_driver_ipmi::kcs::Kcs::new(transport.port, transport.spacing, timer) }
            .with_poll_budget(SEL_POLL_BUDGET);
    let success = kcs.add_sel(&record).is_ok();
    if !success {
        SEL_FAILED.store(true, Ordering::Relaxed);
    }
    if probe {
        PROBE.store(if success { 2 } else { 3 }, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
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
        for bank in 0..MAX_SEL_ATTEMPTS {
            assert_eq!(journal.next().unwrap().bank, bank);
        }
        assert!(journal.next().is_none());
        assert_eq!(
            journal.with(|s| s.count),
            Some(RECORDS),
            "delivery does not erase evidence"
        );
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
        diagnostics(&Cpu, saved);
        assert_eq!(saved.read_amd64(Rax), Ok(DIAG_REQUEST));
        saved.write_amd64(Rdx, 0xb2).unwrap();
        diagnostics(&Cpu, saved);
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
            sel(&record).as_bytes(),
            &[0, 0, 2, 0, 0, 0, 0, 1, 0, 4, 7, 255, 0x6f, 0xab, 255, 63]
        );
    }
}
