//! MP bring-up: the SMU releases each thread once into the reset vector,
//! which the reset image routes to the arch crate's SIPI trampoline
//! (`entry::ApRoute`); every thread then runs the CCX per-thread steps
//! (CCX §3.7, CCX_SP3 §2 steps 5–7) through the driver's CPU driver
//! (`ccx::mp::Fam17hCpu::multi_socket`). This module supplies the release
//! ([`release_all`]).

use fstart_driver_amd::ccx::release::{Launch, ReleaseError};
use fstart_driver_amd::ccx::topology::Thread;
use fstart_driver_amd::cpu::Silicon;

use super::hw::msr;

/// The executing thread as the CCX code sees it.
pub fn this_thread(generation: Silicon) -> Thread {
    Thread::current(generation, &msr())
}

/// The executing thread's (extended) APIC ID, valid in xAPIC and x2APIC.
pub fn apic_id() -> u32 {
    fstart_arch::x86::cpuid(0x8000_001e).0
}

/// Release every thread of `order` (one SMU THREAD_ENABLE bit + apply
/// message each). `launch` sends one; the arch MP core waits for the
/// check-ins (bounded: 50 ms per AP; AGESA waits forever, CCX §3.7).
/// The error is the failing launch's index in `order` + 1.
pub fn release_all(
    order: &[Launch],
    mut launch: impl FnMut(&Launch) -> Result<(), ReleaseError>,
) -> Result<(), u32> {
    order
        .iter()
        .enumerate()
        .try_for_each(|(i, l)| launch(l).map_err(|_| i as u32 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_stops_at_the_first_failure() {
        let l = |bit| Launch {
            socket: 0,
            die: 0,
            thread_enable: 0x3008_1018,
            bit,
        };
        let order = [l(1), l(2), l(3)];
        let mut sent = std::vec::Vec::new();
        let r = release_all(&order, |l| {
            sent.push(l.bit);
            if l.bit == 2 {
                Err(ReleaseError::AlreadyRunning)
            } else {
                Ok(())
            }
        });
        assert_eq!(r, Err(2));
        assert_eq!(sent, [1, 2]);
        assert_eq!(release_all(&order[..1], |_| Ok(())), Ok(()));
    }
}
