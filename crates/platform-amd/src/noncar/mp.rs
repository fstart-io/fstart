// SPDX-License-Identifier: MIT OR Apache-2.0
//! MP init of a noncar SoC with the architectural MP engine: the SMU
//! releases the APs ([`fstart_driver_amd::smu::threads::launch`]), every
//! CPU loads its microcode and runs the SoC's CCX init
//! ([`crate::cpu::Fam17hCpu`]).
use super::Soc;
use crate::cpu::{Fam17hCpu, PatchPage};
use core::sync::atomic::{AtomicU32, Ordering};
use fstart_arch::x86::{
    cpu::msr::NativeMsr,
    mp::{self, CpuDriver, MpConfig, MpError, MpHandle},
};
use fstart_driver_amd::{
    ccx::{Thread, tsc::Tsc},
    df::Die,
    smu::threads::{self, Policy},
};
use fstart_pci::ecam::GlobalEcam;
use heapless::Vec;

/// The microcode patch the CPUs load: firmware BSS in the reserved region.
static mut MICROCODE: core::mem::MaybeUninit<PatchPage> = core::mem::MaybeUninit::uninit();

/// The CPUs MP init brought up.
pub struct Cpus {
    pub mp: MpHandle,
    /// APIC IDs, BSP first.
    pub apic_ids: Vec<u32, 32>,
}

/// Microcode and the CCX init `init` on every CPU, the SMU releases the
/// APs of `dies`.
pub fn start<S: Soc, E>(
    dies: &[Die<S, GlobalEcam>],
    init: impl Fn(&NativeMsr, Thread) -> Result<(), E> + Send + Sync,
    clock: Tsc,
) -> Result<Cpus, &'static str> {
    let threads = threads::thread_count(dies, Policy::DEFAULT).map_err(|_| "CPU topology")?;
    // SAFETY: the boot flash is mapped below 4 GiB.
    let flash = unsafe {
        core::slice::from_raw_parts(
            S::LAYOUT.flash.base as *const u8,
            S::LAYOUT.flash.size as usize,
        )
    };
    // SAFETY: BSP only, before the APs start; the page is reserved BSS.
    let cpu = unsafe {
        Fam17hCpu::<S, _>::new(
            S::CPU_NAME,
            flash,
            &mut *core::ptr::addr_of_mut!(MICROCODE),
            init,
        )
    };
    let drivers: [&dyn CpuDriver; 1] = [&cpu];
    let mut launch = |aps: u16| {
        // SAFETY: the reset vector sends APs to the SIPI trampoline the MP
        // engine installed before calling this.
        unsafe { threads::launch(dies, Policy::DEFAULT, usize::from(aps), &mut { clock }) }.map_err(
            |error| {
                if let threads::Error::ApCount { started, requested } = error {
                    fstart_log::error!("{}: started {} of {} APs", S::NAME, started, requested);
                }
                1
            },
        )
    };
    let mp = mp::mp_init_with_launcher(
        &MpConfig {
            cpu_drivers: &drivers,
            num_cpus: threads as u16,
            max_cpus: S::MAX_CPUS,
        },
        &mut launch,
    );
    let mp = match mp {
        Ok(mp) => {
            fstart_log::info!(
                "{}: {} CPUs up, {} CPU init errors",
                S::NAME,
                mp.num_cpus(),
                cpu.errors()
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
                    fstart_log::error!("mp: SMU release failed")
                }
                MpError::HardwareCpuCountExceedsCapacity { reported, .. } => {
                    fstart_log::error!("mp: {} CPUs responded", reported)
                }
                _ => fstart_log::error!("mp: setup failed"),
            }
            return Err("MP init");
        }
    };
    let apic_ids: [AtomicU32; 32] = [const { AtomicU32::new(u32::MAX) }; 32];
    mp.scope(|scope| {
        scope.scatter(&|cpu: u32| {
            if let Some(slot) = apic_ids.get(cpu as usize) {
                slot.store(fstart_arch::x86::cpuid(1).1 >> 24, Ordering::Release);
            }
        })
    });
    let apic_ids = apic_ids
        .iter()
        .map(|id| id.load(Ordering::Acquire))
        .take(usize::from(mp.num_cpus()))
        .collect();
    Ok(Cpus { mp, apic_ids })
}
