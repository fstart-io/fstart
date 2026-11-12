// SPDX-License-Identifier: MIT OR Apache-2.0
//! MP init with the architectural MP engine, for every AMD flow: the SoC
//! releases the APs into the reset vector, which the flow routes to the
//! engine's SIPI trampoline; every CPU then runs the SoC's CPU driver.
use core::sync::atomic::{AtomicU32, Ordering};
use fstart_arch::x86::{
    mp::{self, CpuDriver, MpConfig, MpError, MpHandle},
    psp_reset::ApRoute,
};

/// Arm (or park) the AP path of the shared reset entry, which the SoC
/// releases the APs into.
fn route_aps(route: ApRoute) {
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    fstart_arch::x86::psp_reset::set_ap_route(route);
    #[cfg(not(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none")))]
    let _ = route;
}

/// The CPUs MP init brought up.
pub struct Cpus {
    pub mp: MpHandle,
    /// APIC IDs, BSP first.
    pub apic_ids: alloc::vec::Vec<u32>,
}

/// Bring up `threads` CPUs (the BSP included, `max_cpus` at most) with the
/// SoC's driver `cpu`; `release` lets the SoC release that many APs, and
/// `errors` counts the threads whose init failed. `name` is the log
/// prefix.
pub fn start(
    name: &'static str,
    cpu: &dyn CpuDriver,
    errors: impl Fn() -> u32,
    threads: u16,
    max_cpus: u16,
    mut release: impl FnMut(u16) -> Result<(), u32>,
) -> Result<Cpus, &'static str> {
    let drivers: [&dyn CpuDriver; 1] = [cpu];
    let route = ApRoute::to(mp::AP_BOOTSTRAP_ADDRESS).map_err(|_| "AP route")?;
    let mut launch = |aps: u16| {
        // The APs leave the reset entry for the SIPI trampoline the MP
        // engine installed before calling this.
        route_aps(route);
        release(aps)
    };
    let mp = mp::mp_init_with_launcher(
        &MpConfig {
            cpu_drivers: &drivers,
            num_cpus: threads,
            max_cpus,
        },
        &mut launch,
    );
    route_aps(ApRoute::PARK);
    let errors = errors();
    let mp = match mp {
        Ok(mp) => {
            fstart_log::info!(
                "{}: {} CPUs up, {} CPU init errors",
                name,
                mp.num_cpus(),
                errors
            );
            mp
        }
        Err(error) => {
            match error {
                MpError::NoApsResponded => fstart_log::error!("mp: no AP responded"),
                MpError::PartialBringup { expected, actual } => {
                    fstart_log::error!("mp: {} of {} APs responded", actual, expected)
                }
                MpError::PlatformLaunchFailed(_) => {
                    fstart_log::error!("mp: the release of the APs failed")
                }
                MpError::HardwareCpuCountExceedsCapacity { reported, .. } => {
                    fstart_log::error!("mp: {} CPUs responded", reported)
                }
                _ => fstart_log::error!("mp: setup failed"),
            }
            return Err("MP init");
        }
    };
    if errors != 0 {
        return Err("CPU initialization");
    }
    let ids: alloc::vec::Vec<AtomicU32> = (0..mp.num_cpus())
        .map(|_| AtomicU32::new(u32::MAX))
        .collect();
    mp.scope(|scope| {
        scope.scatter(&|cpu: u32| {
            if let Some(slot) = ids.get(cpu as usize) {
                // The extended APIC ID, valid in xAPIC and x2APIC.
                slot.store(fstart_arch::x86::cpuid(0x8000_001e).0, Ordering::Release);
            }
        })
    });
    let apic_ids = ids.iter().map(|id| id.load(Ordering::Acquire)).collect();
    Ok(Cpus { mp, apic_ids })
}
