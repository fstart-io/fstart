// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read back the actual CPU set and power definitions on every thread;
//! ACPI must not derive CPU IDs from counts or copy the BSP's untested policy.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use fstart_arch::x86::{cpu::msr::NativeMsr, mp::MpHandle};
use fstart_driver_amd::ccx::power::{self, CstateInfo, Pstate, Voltage};

pub struct Snapshot {
    pub ids: Vec<u32>,
    pub pstates: heapless::Vec<Pstate, 8>,
    /// The C-states every thread has (`_CST`).
    pub cstates: Option<CstateInfo>,
}

/// Every thread must report the BSP's CPUID signature and the same SVI3
/// P-state definitions. `MAX` bounds the platform's CPU storage.
pub fn snapshot<const MAX: usize>(mp: &MpHandle) -> Result<Snapshot, ()> {
    // SAFETY: checked Rs CPL0 BSP, read-only architectural power state.
    let msr = unsafe { NativeMsr::new() };
    let pstates = power::pstates(&msr, Voltage::Svi3);
    if pstates.is_empty() || usize::from(mp.num_cpus()) > MAX {
        return Err(());
    }
    let signature = fstart_arch::x86::cpuid(1).0;
    let cstates = power::cstate_info(&msr);
    let ids = [const { AtomicU32::new(u32::MAX) }; MAX];
    let bad = AtomicBool::new(false);
    mp.scope(|scope| {
        scope.scatter(&|index| {
            // SAFETY: the selected current CPU, at CPL0; reads only.
            let cpu = unsafe { NativeMsr::new() };
            let id = fstart_arch::x86::cpuid(0x8000001e).0;
            ids[index as usize].store(id, Ordering::Release);
            if fstart_arch::x86::cpuid(1).0 != signature
                || power::pstates(&cpu, Voltage::Svi3) != pstates
                || power::cstate_info(&cpu) != cstates
            {
                bad.store(true, Ordering::Release);
            }
        })
    });
    let ids: Vec<u32> = ids[..usize::from(mp.num_cpus())]
        .iter()
        .map(|v| v.load(Ordering::Acquire))
        .collect();
    if bad.load(Ordering::Acquire)
        || ids
            .iter()
            .enumerate()
            .any(|(n, id)| *id == u32::MAX || ids[..n].contains(id))
    {
        return Err(());
    }
    fstart_log::info!(
        "rs: ACPI CPU audit {} unique IDs, {} matching SVI3 states",
        ids.len(),
        pstates.len()
    );
    if let Some(c) = cstates {
        fstart_log::info!(
            "rs: C-state I/O={:#x} CC6={} on every thread",
            c.io_base,
            c.cc6
        );
    }
    Ok(Snapshot {
        ids,
        pstates,
        cstates,
    })
}
