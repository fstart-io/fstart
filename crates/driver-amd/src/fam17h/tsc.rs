// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TSC frequency from the hardware-selected highest P-state.
use super::msr::{PSTATE_0, PSTATE_CURRENT_LIMIT, PstateCurLim, PstateDef};
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
use tock_registers::{LocalRegisterCopy, interfaces::Readable};

/// Core MHz of a P-state definition, `None` if it isn't enabled.
pub fn pstate_mhz(value: u64) -> Option<u32> {
    let pstate = LocalRegisterCopy::<u64, PstateDef::Register>::new(value);
    if !pstate.is_set(PstateDef::PSTATE_EN) {
        return None;
    }
    let fid = pstate.read(PstateDef::CPU_FID) as u32;
    let divisor = pstate.read(PstateDef::CPU_DFS_ID) as u32;
    if divisor == 0 {
        return Some(4000);
    }
    let mhz =
        if (8..=0x1a).contains(&divisor) || divisor > 0x1a && divisor <= 0x3e && divisor & 1 == 0 {
            200 * fid / divisor
        } else {
            25 * fid
        };
    Some(if mhz == 0 { 4000 } else { mhz })
}
/// # Safety
/// Must run in ring 0 on a supported Family 17h CPU with invariant TSC.
pub unsafe fn frequency_mhz() -> Option<u32> {
    let cpu = unsafe { NativeMsr::new() };
    let highest = cpu
        .reg(PSTATE_CURRENT_LIMIT)
        .read(PstateCurLim::CUR_PSTATE_LIMIT);
    pstate_mhz(cpu.reg(PSTATE_0.offset(highest as u32)).get())
}
#[cfg(target_arch = "x86_64")]
pub fn delay_us(mhz: u32, microseconds: u32) {
    let ticks = u64::from(mhz) * u64::from(microseconds);
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}
