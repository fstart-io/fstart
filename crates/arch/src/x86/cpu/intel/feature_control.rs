//! `IA32_FEATURE_CONTROL` policy shared by Intel CPU model drivers.
//!
//! Follows coreboot's defaults (`ENABLE_VMX=y`, `SET_IA32_FC_LOCK_BIT=y`):
//! enable VMX outside SMX when the CPU has it, then lock the register. A
//! register that is already locked is left as found.

use super::msr_register::Msr;
use tock_registers::{LocalRegisterCopy, register_bitfields};

pub(super) const IA32_FEATURE_CONTROL: u32 = 0x03a;
register_bitfields![u32,
    CPUID_1_ECX [ VMX OFFSET(5) NUMBITS(1) [] ]
];
register_bitfields![u64,
    pub(super) FEATURE_CONTROL [
        LOCK OFFSET(0) NUMBITS(1) [],
        VMX_OUTSIDE_SMX OFFSET(2) NUMBITS(1) [],
        SMRR_ENABLE OFFSET(3) NUMBITS(1) []
    ]
];
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
    let register = Msr::<FEATURE_CONTROL::Register>::new(IA32_FEATURE_CONTROL);
    let current = unsafe { register.read() };
    if current.is_set(FEATURE_CONTROL::LOCK) {
        return;
    }
    let (_, _, ecx, _) = crate::x86::cpuid(1);
    let vmx = if LocalRegisterCopy::<u32, CPUID_1_ECX::Register>::new(ecx).is_set(CPUID_1_ECX::VMX)
    {
        FEATURE_CONTROL::VMX_OUTSIDE_SMX::SET.value
    } else {
        0
    };
    // SAFETY: the register is unlocked and the caller vouches for `extra`.
    unsafe { register.write(current.get() | vmx | extra | FEATURE_CONTROL::LOCK::SET.value) };
}
