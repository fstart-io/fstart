//! `IA32_FEATURE_CONTROL` policy shared by Intel CPU model drivers.
//!
//! Follows coreboot's defaults (`ENABLE_VMX=y`, `SET_IA32_FC_LOCK_BIT=y`):
//! enable VMX outside SMX when the CPU has it, then lock the register. A
//! register that is already locked is left as found.

use crate::x86::msr::{rdmsr, wrmsr};

const IA32_FEATURE_CONTROL: u32 = 0x03a;
/// Lock bit; the register is read-only until the next reset once set.
pub const LOCK: u64 = 1 << 0;
const VMX_OUTSIDE_SMX: u64 = 1 << 2;
const CPUID_1_ECX_VMX: u32 = 1 << 5;

/// Enable VMX when supported, OR in `extra` model-specific enable bits (for
/// example [`SmrrPair::feature_control_bits`](super::smrr::SmrrPair::feature_control_bits)),
/// and lock `IA32_FEATURE_CONTROL` on the current CPU.
///
/// # Safety
///
/// The current CPU must implement `IA32_FEATURE_CONTROL` and every bit in
/// `extra`.
pub unsafe fn enable_and_lock(extra: u64) {
    // SAFETY: the caller guarantees the MSR exists.
    let current = unsafe { rdmsr(IA32_FEATURE_CONTROL) };
    if current & LOCK != 0 {
        return;
    }
    let (_, _, ecx, _) = crate::x86::cpuid(1);
    let vmx = if ecx & CPUID_1_ECX_VMX != 0 {
        VMX_OUTSIDE_SMX
    } else {
        0
    };
    // SAFETY: the register is unlocked and the caller vouches for `extra`.
    unsafe { wrmsr(IA32_FEATURE_CONTROL, current | vmx | extra | LOCK) };
}
