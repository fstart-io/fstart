//! `IA32_FEATURE_CONTROL` policy shared by Intel CPU model drivers.
//!
//! Follows coreboot's defaults (`ENABLE_VMX=y`, `SET_IA32_FC_LOCK_BIT=y`):
//! enable VMX outside SMX when the CPU has it, then lock the register. A
//! register that is already locked is left as found.

use crate::x86::cpu::msr::{Msr, MsrAccess, NativeMsr};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

pub(super) const IA32_FEATURE_CONTROL: u32 = 0x03a;
register_bitfields![u32,
    CPUID_1_ECX [
        VMX OFFSET(5) NUMBITS(1) [],
        SMX OFFSET(6) NUMBITS(1) []
    ]
];
register_bitfields![u64,
    pub(super) FEATURE_CONTROL [
        LOCK OFFSET(0) NUMBITS(1) [],
        VMX_INSIDE_SMX OFFSET(1) NUMBITS(1) [],
        VMX_OUTSIDE_SMX OFFSET(2) NUMBITS(1) [],
        SMRR_ENABLE OFFSET(3) NUMBITS(1) []
    ]
];
fn feature_control_available(features: u32, extra: u64) -> bool {
    let features = LocalRegisterCopy::<u32, CPUID_1_ECX::Register>::new(features);
    features.is_set(CPUID_1_ECX::VMX)
        || features.is_set(CPUID_1_ECX::SMX)
        || LocalRegisterCopy::<u64, FEATURE_CONTROL::Register>::new(extra)
            .is_set(FEATURE_CONTROL::SMRR_ENABLE)
}

/// Enable VMX when supported, OR in `extra` model-specific enable bits (for
/// example [`SmrrPair::feature_control_bits`](super::smrr::SmrrPair::feature_control_bits)),
/// and lock `IA32_FEATURE_CONTROL` on the current CPU. Skip the MSR entirely
/// unless VMX, SMX, or a capability-backed alternative-SMRR enable requires it.
///
/// # Safety
///
/// The current CPU must be Intel and implement every bit in `extra`.
/// In particular, SMRR_ENABLE must come from a supported alternative-SMRR
/// capability, not merely a CPU model number.
pub unsafe fn enable_and_lock(extra: u64) {
    let (_, _, ecx, _) = crate::x86::cpuid(1);
    // SAFETY: configure checks CPUID or the caller's capability-backed SMRR
    // bit before accessing IA32_FEATURE_CONTROL.
    let cpu = unsafe { NativeMsr::new() };
    configure(&cpu, ecx, extra);
}

pub(super) fn configure(cpu: &impl MsrAccess, ecx: u32, extra: u64) {
    if !feature_control_available(ecx, extra) {
        return;
    }
    let register = cpu.reg(Msr::<FEATURE_CONTROL::Register>::new(IA32_FEATURE_CONTROL));
    if register.is_set(FEATURE_CONTROL::LOCK) {
        return;
    }
    let features = LocalRegisterCopy::<u32, CPUID_1_ECX::Register>::new(ecx);
    let vmx = if features.is_set(CPUID_1_ECX::VMX) || features.is_set(CPUID_1_ECX::SMX) {
        FEATURE_CONTROL::VMX_OUTSIDE_SMX::SET.value
            | FEATURE_CONTROL::VMX_INSIDE_SMX
                .val(u64::from(features.is_set(CPUID_1_ECX::SMX)))
                .value
    } else {
        0
    };
    // Unlocked bits are replaced cleanly so stale policy cannot survive locking.
    register.set(vmx | extra | FEATURE_CONTROL::LOCK::SET.value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocked_garbage_is_not_policy_and_lock_is_final() {
        let cpu = crate::x86::cpu::msr::testing::MsrSpace::new([(IA32_FEATURE_CONTROL, !1)]);
        configure(&cpu, 0x60, 0);
        configure(&cpu, 0x20, 0);
        assert_eq!(cpu.borrow().writes.as_slice(), &[(IA32_FEATURE_CONTROL, 7)]);
    }

    #[test]
    fn feature_control_probe_requires_an_advertised_or_model_backed_capability() {
        assert!(!feature_control_available(0, 0)); // Non-VMX/SMX Celeron.
        assert!(!feature_control_available(1 << 7, 0)); // EIST is unrelated.
        assert!(feature_control_available(1 << 5, 0)); // VMX.
        assert!(feature_control_available(1 << 6, 0)); // SMX.
        assert!(feature_control_available(
            0,
            FEATURE_CONTROL::SMRR_ENABLE::SET.value
        ));
        assert!(!feature_control_available(
            0,
            FEATURE_CONTROL::LOCK::SET.value
        ));
    }
}
