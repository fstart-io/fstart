// SPDX-License-Identifier: MIT OR Apache-2.0
//! One-shot Picasso SMU release after architectural bootstrap publication.
use fstart_arch::x86::mp::{AP_BOOTSTRAP_ADDRESS, ApLauncher, PreparedAps};
use fstart_driver_amd::picasso::{registers::Smn, topology};

#[derive(Debug)]
pub enum Error {
    AlreadySubmitted,
    Bootstrap,
    CpuCount { expected: usize, actual: u16 },
    Topology(topology::Error),
}

/// BSP-owned SMN transport and calibrated delay. There is no AP-side SMN owner,
/// INIT/SIPI fallback, implicit retry, image patch or cleanup after submission.
pub struct ColdRelease<'a, S, D> {
    smn: &'a mut S,
    topology: topology::Topology,
    delay_us: &'a mut D,
    submitted: bool,
    failure: Option<Error>,
}
impl<'a, S: Smn, D: FnMut(u32)> ColdRelease<'a, S, D> {
    /// # Safety
    /// Cold boot only: all APs are held for release, BSP occupies ThreadEn slot0,
    /// and `topology` is the exact complete enabled CPU set after any required
    /// policy reset. Validate native AP reset/dispatch and target cache/control
    /// semantics, exclude early NMI/SMI, and reserve/map/publish all resident MP
    /// resources until reset. Own SMN exclusively; delay must be calibrated.
    /// This constructor performs no hardware writes and attests none of those
    /// facts. An active/unknown prior release cannot be reset to discover them.
    pub unsafe fn new(smn: &'a mut S, topology: topology::Topology, delay_us: &'a mut D) -> Self {
        Self {
            smn,
            topology,
            delay_us,
            submitted: false,
            failure: None,
        }
    }

    pub fn failure(&self) -> Option<&Error> {
        self.failure.as_ref()
    }

    fn diagnostic<T>(&mut self, result: Result<T, Error>) -> Result<T, u32> {
        result.map_err(|error| {
            // Diagnostic namespace only, not SMU mailbox reply encodings.
            let code = match &error {
                Error::AlreadySubmitted => 1,
                Error::Bootstrap => 2,
                Error::Topology(_) => 3,
                Error::CpuCount { .. } => 4,
            };
            self.failure = Some(error);
            code
        })
    }

    fn release(&mut self, prepared: &PreparedAps) -> Result<(), Error> {
        if self.submitted {
            return Err(Error::AlreadySubmitted);
        }
        // Consume even rejected attempts; no reconstruction/retry is implied.
        self.submitted = true;
        preflight(
            self.topology,
            prepared.expected_aps(),
            prepared.coordinated_cache(),
            prepared.entry_address(),
        )?;
        // SAFETY: constructor's cold/reset/cache/map/exclusion contract and the
        // engine's sealed publication ordering precede this one-shot release.
        let released = unsafe {
            topology::launch(
                self.smn,
                self.topology,
                usize::from(prepared.expected_aps()),
                self.delay_us,
            )
        }
        .map_err(Error::Topology)?;
        if released.len() != usize::from(prepared.expected_aps()) {
            return Err(Error::Bootstrap);
        }
        // Released bits do NOT constitute restoration/check-in/CPU completion.
        Ok(())
    }
}
// SAFETY: unsafe construction supplies target prerequisites; release checks a
// complete set and coordinated publication before the existing driver sequence.
// Any error is reported for terminal MP handling, never retried or rolled back.
unsafe impl<S: Smn, D: FnMut(u32)> ApLauncher for ColdRelease<'_, S, D> {
    fn validate_cpu_count(&mut self, num_cpus: u16) -> Result<(), u32> {
        let result = if self.submitted {
            Err(Error::AlreadySubmitted)
        } else {
            validate_count(self.topology, num_cpus)
        };
        self.diagnostic(result)
    }

    fn launch(&mut self, prepared: &PreparedAps) -> Result<(), u32> {
        let result = self.release(prepared);
        self.diagnostic(result)
    }
}

fn validate_count(topology: topology::Topology, total: u16) -> Result<(), Error> {
    let expected = topology.thread_slots().map_err(Error::Topology)?.len();
    if usize::from(total) != expected {
        return Err(Error::CpuCount {
            expected,
            actual: total,
        });
    }
    Ok(())
}

fn preflight(
    topology: topology::Topology,
    expected: u16,
    coordinated: bool,
    entry: u64,
) -> Result<(), Error> {
    validate_count(topology, expected.checked_add(1).ok_or(Error::Bootstrap)?)?;
    if !coordinated || entry != AP_BOOTSTRAP_ADDRESS {
        return Err(Error::Bootstrap);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_count_validation_rejects_bsp_only_eight_threads_without_hardware() {
        struct NoHardware;
        impl Smn for NoHardware {
            fn read(&mut self, _: u32) -> u32 {
                panic!("count validation read SMN")
            }
            fn write(&mut self, _: u32, _: u32) {
                panic!("count validation wrote SMN")
            }
        }
        let mut transport = NoHardware;
        let mut delay = |_| panic!("count validation used hardware delay");
        let eight = topology::Topology {
            cores_per_complex: 4,
            complexes: 1,
            threads_per_core: 2,
        };
        // SAFETY: cached-only test path, with no actual native submission.
        let mut wrong = unsafe { ColdRelease::new(&mut transport, eight, &mut delay) };
        assert_eq!(wrong.validate_cpu_count(1), Err(4));
        assert!(matches!(
            wrong.failure(),
            Some(Error::CpuCount {
                expected: 8,
                actual: 1
            })
        ));
        assert!(!wrong.submitted);
        let singleton = topology::Topology {
            cores_per_complex: 1,
            complexes: 1,
            threads_per_core: 1,
        };
        // SAFETY: same cached-only test path; physical contracts not attested.
        let mut single = unsafe { ColdRelease::new(&mut transport, singleton, &mut delay) };
        assert_eq!(single.validate_cpu_count(1), Ok(()));
        assert!(!single.submitted);
        assert!(single.failure().is_none());
    }

    #[test]
    fn cold_release_requires_complete_set_and_coordinated_resident_entry() {
        let topology = topology::Topology {
            cores_per_complex: 4,
            complexes: 1,
            threads_per_core: 2,
        };
        assert!(preflight(topology, 7, true, 0x8000).is_ok());
        for (expected, coordinated, entry) in [
            (6, true, 0x8000),
            (8, true, 0x8000),
            (7, false, 0x8000),
            (7, true, 0x30000),
        ] {
            assert!(preflight(topology, expected, coordinated, entry).is_err());
        }
    }
}
