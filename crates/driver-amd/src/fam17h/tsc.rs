// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso TSC frequency from the hardware-selected highest P-state.
pub const fn pstate_mhz(value: u64) -> Option<u32> {
    if value >> 63 == 0 {
        return None;
    }
    let fid = value as u32 & 0xff;
    let divisor = (value >> 8) as u32 & 0x3f;
    if divisor == 0 {
        return Some(4000);
    }
    let mhz = if divisor >= 8 && divisor <= 0x1a
        || divisor > 0x1a && divisor <= 0x3e && divisor & 1 == 0
    {
        200 * fid / divisor
    } else {
        25 * fid
    };
    Some(if mhz == 0 { 4000 } else { mhz })
}
#[cfg(target_arch = "x86_64")]
/// # Safety
/// Must run in ring 0 on a supported Picasso CPU with invariant TSC.
pub unsafe fn frequency_mhz() -> Option<u32> {
    unsafe fn rdmsr(index: u32) -> u64 {
        let (lo, hi): (u32, u32);
        unsafe {
            core::arch::asm!("rdmsr", in("ecx") index, out("eax") lo, out("edx") hi, options(nostack));
        }
        u64::from(lo) | (u64::from(hi) << 32)
    }
    let highest = unsafe { rdmsr(0xc0010061) } as u32 & 7;
    pstate_mhz(unsafe { rdmsr(0xc0010064 + highest) })
}
#[cfg(target_arch = "x86_64")]
pub fn delay_us(mhz: u32, microseconds: u32) {
    let ticks = u64::from(mhz) * u64::from(microseconds);
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}
