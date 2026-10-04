// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native CPU cold-flow composition. Not SMM setup or a complete silicon flow.
use crate::{
    cpu::{self, NativeCpu},
    layout,
    memory::Memory,
    mp,
};
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_arch::x86::mp::{
    ApLauncher, CpuDriver, MpConfig, MpError, MpFailStop, MpHandle, PreparedAps,
};
use fstart_driver_amd::picasso::{
    cpu::Msr,
    nbio_late, power,
    registers::Smn,
    topology::{self, Topology},
};
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

pub struct Inputs<'a> {
    /// Already applied topology: no policy reset/reconfiguration here.
    pub topology: Topology,
    /// Caller-verified complete physical APIC IDs, BSP first, NOT ThreadEn slots.
    pub apic_ids: &'a [u32],
    /// One exact, independently trusted patch selected for this CPU signature.
    pub patch: &'a [u8],
    pub memory: &'a Memory,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LateResponse {
    pub command: u32,
    pub status: u32,
}
/// Bounded source-backed late-policy diagnostics, never proof features activated.
#[derive(Debug)]
pub struct LateResponses {
    entries: [LateResponse; 8],
    count: usize,
}
impl LateResponses {
    fn new() -> Self {
        Self {
            entries: [LateResponse {
                command: 0,
                status: 0,
            }; 8],
            count: 0,
        }
    }
    fn push(&mut self, command: u32, status: u32) -> bool {
        let Some(entry) = self.entries.get_mut(self.count) else {
            return false;
        };
        *entry = LateResponse { command, status };
        self.count += 1;
        true
    }
    pub fn entries(&self) -> &[LateResponse] {
        &self.entries[..self.count]
    }
}
#[derive(Debug)]
pub enum Error {
    Topology(topology::Error),
    CpuCount {
        expected: usize,
        actual: usize,
    },
    AlreadyAttempted,
    Cpu(cpu::Error),
    Mp(mp::ResidentError),
    ArchitecturalMp(MpError),
    ApRelease {
        code: u32,
        cause: Option<mp::ReleaseError>,
    },
    IncompleteCpu,
    Smu {
        cause: power::Error,
        responses: LateResponses,
    },
    DiagnosticsOverflow {
        response: LateResponse,
        prefix: LateResponses,
    },
}
/// Terminal cold-flow failure, including uncertain submission/partial hardware.
/// # Safety
/// NEVER return or unwind. Preserve BSP stack/borrowed driver/configuration/code,
/// mappings and all permanent resources until reset, including live AP borrows.
/// Retain/emit reported diagnostics. Do not retry, roll back, reset links or reclaim.
/// Own safe reset/SMI/fault policy;
/// halting alone is not cancellation/global quiescence or SMM/DMA isolation.
pub unsafe trait FailStop {
    fn fail_stop(&self, error: Error) -> !;
}
struct MpFailure<'a, F>(&'a F);
// SAFETY: the enclosing unsafe FailStop contract includes the MP lifetime and
// terminal handling contract; this adapter preserves the actual typed error.
unsafe impl<F: FailStop> MpFailStop for MpFailure<'_, F> {
    fn fail_stop(&self, error: MpError) -> ! {
        self.0.fail_stop(Error::ArchitecturalMp(error))
    }
}
fn release_error(code: u32, cause: Option<mp::ReleaseError>) -> Error {
    Error::ApRelease { code, cause }
}
struct TerminalRelease<'a, S, D, F> {
    inner: mp::ColdRelease<'a, S, D>,
    failure: &'a F,
}
// SAFETY: the inner unsafe ColdRelease owns native publication/reset/cache
// obligations. Failure is terminal before code-only architectural mapping;
// retaining all live MP borrows is part of FailStop's unsafe contract.
unsafe impl<S: Smn, D: FnMut(u32), F: FailStop> ApLauncher for TerminalRelease<'_, S, D, F> {
    fn validate_cpu_count(&mut self, count: u16) -> Result<(), u32> {
        match self.inner.validate_cpu_count(count) {
            Ok(()) => Ok(()),
            Err(code) => self
                .failure
                .fail_stop(release_error(code, self.inner.failure().copied())),
        }
    }
    fn launch(&mut self, prepared: &PreparedAps) -> Result<(), u32> {
        match self.inner.launch(prepared) {
            Ok(()) => Ok(()),
            Err(code) => self
                .failure
                .fail_stop(release_error(code, self.inner.failure().copied())),
        }
    }
}
/// Architectural CPU initialization and callback return completed. This permits
/// later platform work, NOT immediate CCX-done/SMM/graphics/general completion.
#[must_use]
pub struct CpusInitialized {
    mp: MpHandle,
}
impl CpusInitialized {
    pub fn mp(&self) -> &MpHandle {
        &self.mp
    }
    /// Consume the CPU-completion result at the correct later native phase.
    /// # Safety
    /// Complete ALL CCX-notification prerequisites in native silicon/pre-
    /// enumeration/power-policy and CPU/SMM flows at their proper phase first.
    /// CPU completion alone does not establish protected SMM services or these
    /// other prerequisites. Do not forge them with a no-op hook/boolean promise.
    /// Own SMN exclusively, retain all mappings/resources and establish actual
    /// cache/event/reset/NMI/SMI/SMM/DMA exclusions. Delay is calibrated/non-
    /// unwinding; fail-stop cannot return/unwind/retry/reclaim. Graphics is actual
    /// earlier discovery, not synthetic enablement. Retain/emit every returned
    /// nonstandard response; a completed request need not activate its features.
    pub unsafe fn finish_ccx(
        self,
        graphics: bool,
        smn: &mut impl Smn,
        delay_us: &mut impl FnMut(u32),
        failure: &impl FailStop,
    ) -> CcxNotified {
        // Private consumed token, obtainable only after actual MP return plus
        // NativeCpu::complete(), not a public fabricated completion boolean.
        let responses = notify(true, graphics, smn, delay_us, failure)
            .unwrap_or_else(|error| failure.fail_stop(error));
        CcxNotified {
            cpus: self,
            responses,
        }
    }
}
/// CPU completion plus transport-completed late notifications, not SMM/security
/// or successful nonstandard SMU feature activation. No second notification API.
#[must_use = "retain or emit every nonstandard late response before discarding the result"]
pub struct CcxNotified {
    cpus: CpusInitialized,
    responses: LateResponses,
}
impl CcxNotified {
    pub fn mp(&self) -> &MpHandle {
        self.cpus.mp()
    }
    pub fn late_responses(&self) -> &[LateResponse] {
        self.responses.entries()
    }
}
fn count(topology: Topology, ids: &[u32]) -> Result<u16, Error> {
    let expected = topology.thread_slots().map_err(Error::Topology)?.len();
    if ids.len() != expected {
        return Err(Error::CpuCount {
            expected,
            actual: ids.len(),
        });
    }
    // NativeCpu repeats complete-set uniqueness/BSP/CPUID agreement before staging.
    Ok(expected as u16)
}
fn claim(flag: &AtomicBool) -> Result<(), Error> {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| Error::AlreadyAttempted)
}
fn notify(
    complete: bool,
    graphics: bool,
    smn: &mut impl Smn,
    delay_us: &mut impl FnMut(u32),
    failure: &impl FailStop,
) -> Result<LateResponses, Error> {
    if !complete {
        return Err(Error::IncompleteCpu);
    }
    let mut responses = LateResponses::new();
    let result = nbio_late::ccx_done(smn, graphics, delay_us, &mut |command, status| {
        if !responses.push(command, status) {
            let prefix = core::mem::replace(&mut responses, LateResponses::new());
            failure.fail_stop(Error::DiagnosticsOverflow {
                response: LateResponse { command, status },
                prefix,
            });
        }
    });
    match result {
        Ok(()) => Ok(responses),
        Err(cause) => Err(Error::Smu { cause, responses }),
    }
}
/// One attempt: cached complete count -> trusted resident patch preparation ->
/// resident coordinated MP publication/SMU release -> callback-return completion
/// -> NativeCpu completion check. Returns a private CPU-phase result; CCX-done
/// is a separately consumed later phase so SMM/other prerequisites can be met.
/// No PSP boot-done, SMM, graphics, PCI enumeration, payload or OS handoff here.
/// # Safety
/// ALL NativeCpu::prepare_staged, microcode::stage, ColdRelease::new and
/// initialize_resident contracts hold for the exact CPU/reset revision, complete
/// physical CPU set, APs-held/BSP role, cache/MSR/CLFLUSH/errata semantics, stable
/// WB/UC mappings/coherent resources, event/reset/NMI/SMI/SMM/DMA exclusions and
/// measured BSP/AP stack budgets. Numerical checks do not establish those facts.
/// Native early initialization and topology policy already completed correctly.
/// Patch source independently trusted/immutable/disjoint through copy; map and
/// ALL reservations persist in every allocator/payload until reset, also errors.
/// Own MSR/SMN exclusively; do not use the BSP MSR owner while MP callbacks hold
/// local access. Delay is real/calibrated and non-unwinding. Failure cannot return
/// or unwind. An unknown/active prior boot flow may not be reconstructed/reset.
/// Return does not establish the SMM/silicon prerequisites of finish_ccx.
pub unsafe fn initialize_cpus(
    input: Inputs<'_>,
    msr: &mut impl Msr,
    smn: &mut impl Smn,
    delay_us: &mut impl FnMut(u32),
    failure: &impl FailStop,
) -> CpusInitialized {
    let total =
        count(input.topology, input.apic_ids).unwrap_or_else(|error| failure.fail_stop(error));
    claim(&ATTEMPTED).unwrap_or_else(|error| failure.fail_stop(error));
    let cpu = unsafe {
        NativeCpu::prepare_staged(
            msr,
            smn,
            input.topology,
            input.apic_ids,
            input.patch,
            input.memory,
        )
    }
    .unwrap_or_else(|error| failure.fail_stop(Error::Cpu(error)));
    let drivers: &[&dyn CpuDriver] = &[&cpu];
    let config = MpConfig {
        cpu_drivers: drivers,
        num_cpus: total,
        max_cpus: layout::MP_CPU_CAPACITY,
    };
    let mp = {
        let mut launcher = TerminalRelease {
            inner: unsafe { mp::ColdRelease::new(smn, input.topology, delay_us) },
            failure,
        };
        unsafe {
            mp::initialize_resident(&config, &MpFailure(failure), &mut launcher, input.memory)
        }
        .unwrap_or_else(|error| failure.fail_stop(Error::Mp(error)))
    };
    if mp.num_cpus() != total {
        failure.fail_stop(Error::CpuCount {
            expected: usize::from(total),
            actual: usize::from(mp.num_cpus()),
        });
    }
    if !cpu.complete() {
        failure.fail_stop(Error::IncompleteCpu);
    }
    CpusInitialized { mp }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_driver_amd::picasso::smu;
    use std::{collections::BTreeMap, vec::Vec};
    struct Stop;
    // SAFETY: terminal host abort; never a panic/unwind/retry surrogate.
    unsafe impl FailStop for Stop {
        fn fail_stop(&self, error: Error) -> ! {
            // Host-only retention through terminal process abort; never called
            // by these pure-gate/immediate synchronous mock success cases.
            std::mem::forget(std::boxed::Box::new(error));
            std::process::abort()
        }
    }
    struct Deny;
    impl Smn for Deny {
        fn read(&mut self, _: u32) -> u32 {
            panic!("incomplete CPUs must not read SMN");
        }
        fn write(&mut self, _: u32, _: u32) {
            panic!("incomplete CPUs must not write SMN");
        }
    }
    #[test]
    fn cached_count_and_completion_gate_including_singleton() {
        let eight = Topology {
            cores_per_complex: 4,
            complexes: 1,
            threads_per_core: 2,
        };
        assert!(matches!(
            count(eight, &[0]),
            Err(Error::CpuCount {
                expected: 8,
                actual: 1
            })
        ));
        assert_eq!(count(eight, &[0, 1, 4, 5, 8, 9, 12, 13]).unwrap(), 8);
        let one = Topology {
            cores_per_complex: 1,
            complexes: 1,
            threads_per_core: 1,
        };
        assert_eq!(count(one, &[42]).unwrap(), 1);
        assert!(
            count(
                Topology {
                    threads_per_core: 0,
                    ..one
                },
                &[42]
            )
            .is_err()
        );
        assert!(matches!(
            notify(
                false,
                false,
                &mut Deny,
                &mut |_| panic!("no delay before CPU completion"),
                &Stop
            ),
            Err(Error::IncompleteCpu)
        ));
        let flag = AtomicBool::new(false);
        assert!(claim(&flag).is_ok());
        assert!(matches!(claim(&flag), Err(Error::AlreadyAttempted)));
    }
    struct Mailbox {
        registers: BTreeMap<u32, u32>,
        frames: Vec<(u32, [u32; 6])>,
        unavailable_after: usize,
    }
    impl Smn for Mailbox {
        fn read(&mut self, address: u32) -> u32 {
            *self.registers.get(&address).unwrap_or(&0)
        }
        fn write(&mut self, address: u32, value: u32) {
            self.registers.insert(address, value);
            if address == smu::MESSAGE {
                let arguments = core::array::from_fn(|i| self.read(smu::ARGUMENTS + i as u32 * 4));
                self.frames.push((value, arguments));
                // Immediate synchronous CPU-only fake completion, not physical PSP/SMU evidence.
                self.registers.insert(
                    smu::RESPONSE,
                    if self.frames.len() > self.unavailable_after {
                        u32::MAX
                    } else {
                        2
                    },
                );
            }
        }
    }
    fn mailbox(unavailable_after: usize) -> Mailbox {
        Mailbox {
            registers: BTreeMap::from([
                (smu::RESPONSE, 1),
                (0x0005d5bc, 0x62e >> 5),
                (0x0005d5b8, (0x62e & 31) << 27),
            ]),
            frames: Vec::new(),
            unavailable_after,
        }
    }
    #[test]
    fn release_diagnostics_distinguish_unavailable_thread_enable_from_smu_timeout() {
        struct ReleaseMock {
            unavailable: bool,
            submitted: bool,
            writes: usize,
        }
        impl Smn for ReleaseMock {
            fn read(&mut self, address: u32) -> u32 {
                match address {
                    0x0005a818 if self.unavailable => u32::MAX,
                    smu::RESPONSE if !self.submitted => 1,
                    _ => 0,
                }
            }
            fn write(&mut self, address: u32, _: u32) {
                self.writes += 1;
                if address == smu::MESSAGE {
                    self.submitted = true;
                }
            }
        }
        let topology = Topology {
            cores_per_complex: 1,
            complexes: 1,
            threads_per_core: 2,
        };
        let mut unavailable = ReleaseMock {
            unavailable: true,
            submitted: false,
            writes: 0,
        };
        // RAM-only mock transport: no real AP reset, PreparedAps fabrication or
        // bootstrap publication. These are driver cause/forwarding witnesses.
        let cause = unsafe {
            topology::launch(&mut unavailable, topology, 1, &mut |_| {
                panic!("no delay on unavailable ThreadEn")
            })
        }
        .unwrap_err();
        assert_eq!(unavailable.writes, 0);
        assert!(matches!(
            release_error(3, Some(mp::ReleaseError::Topology(cause))),
            Error::ApRelease {
                code: 3,
                cause: Some(mp::ReleaseError::Topology(topology::Error::Unavailable(
                    0x0005a818
                )))
            }
        ));
        let mut timeout = ReleaseMock {
            unavailable: false,
            submitted: false,
            writes: 0,
        };
        let mut delays = 0;
        let cause = unsafe { topology::launch(&mut timeout, topology, 1, &mut |_| delays += 1) }
            .unwrap_err();
        assert!(timeout.submitted);
        assert_eq!(delays, 100_000); // mock iterations, not a physical deadline
        assert!(matches!(
            release_error(3, Some(mp::ReleaseError::Topology(cause))),
            Error::ApRelease {
                code: 3,
                cause: Some(mp::ReleaseError::Topology(topology::Error::Smu(
                    smu::Error::CompletionTimeout
                )))
            }
        ));
    }
    #[test]
    fn source_order_retains_all_nonstandard_responses_and_failure_prefix() {
        let mut transport = mailbox(usize::MAX);
        let records = notify(
            true,
            false,
            &mut transport,
            &mut |_| panic!("immediate fake needs no delay"),
            &Stop,
        )
        .unwrap();
        let commands: Vec<_> = transport.frames.iter().map(|frame| frame.0).collect();
        assert_eq!(commands, [0x35, 5, 0x0e, 0x53, 0x3a, 0x42, 0x32, 5]);
        assert_eq!(records.entries().len(), 8);
        assert!(
            records
                .entries()
                .iter()
                .zip(commands)
                .all(|(entry, command)| entry.command == command && entry.status == 2)
        );
        let mut transport = mailbox(1);
        let error = notify(
            true,
            false,
            &mut transport,
            &mut |_| panic!("unavailable fake must not delay"),
            &Stop,
        )
        .unwrap_err();
        assert!(
            matches!(error,Error::Smu { cause: power::Error::Smu(smu::Error::Unavailable),responses } if responses.entries()==[LateResponse { command: 0x35, status: 2 }])
        );
        assert_eq!(transport.frames.len(), 2);
        let mut full = LateResponses::new();
        for i in 0..8 {
            assert!(full.push(i, 2));
        }
        assert!(!full.push(99, 2)); // caller must terminal-stop rather than drop diagnostics
        assert_eq!(full.entries().len(), 8);
    }
}
