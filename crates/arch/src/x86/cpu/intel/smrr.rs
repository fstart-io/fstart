//! Intel System Management Range Register programming.
//!
//! Which register pair a CPU uses is a CPU-model fact supplied by its model
//! driver through [`SmmCpu`](super::smm::SmmCpu); this module only knows the
//! two architectural layouts and how to test whether they are usable.

use crate::x86::msr::{rdmsr, wrmsr};

const IA32_MTRR_CAP: u32 = 0x0fe;
const IA32_FEATURE_CONTROL: u32 = 0x03a;
const CORE2_SMRR_PHYS_BASE: u32 = 0x0a0;
const CORE2_SMRR_PHYS_MASK: u32 = 0x0a1;
const IA32_SMRR_PHYS_BASE: u32 = 0x1f2;
const IA32_SMRR_PHYS_MASK: u32 = 0x1f3;

const CPUID_MTRR: u32 = 1 << 12;
const MTRR_CAP_SMRR: u64 = 1 << 11;
const MTRR_TYPE_WRITE_BACK: u64 = 6;
const MTRR_PHYS_MASK_VALID: u64 = 1 << 11;
const ADDRESS_MASK_32: u64 = 0xffff_f000;

/// SMRR register layout of a CPU model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmrrPair {
    /// `IA32_SMRR_PHYSBASE/PHYSMASK` (`0x1f2/0x1f3`).
    Architectural,
    /// The Core 2 / early Atom pair (`0xa0/0xa1`). Usable only after the CPU
    /// driver sets [`Self::feature_control_bits`] and locks the register. Its base
    /// register reserves bits 11:0, so the memory type is implicit.
    Core2Alternative,
}

/// `IA32_FEATURE_CONTROL` bit that unlocks the [`SmrrPair::Core2Alternative`]
/// registers on the CPUs that have them.
const FEATURE_CONTROL_SMRR_ENABLE: u64 = 1 << 3;

fn mtrr_cap_has_smrr() -> bool {
    let (_, _, _, edx) = crate::x86::cpuid(1);
    // SAFETY: CPUID reports MTRR support, so IA32_MTRR_CAP exists.
    edx & CPUID_MTRR != 0 && unsafe { rdmsr(IA32_MTRR_CAP) } & MTRR_CAP_SMRR != 0
}

impl SmrrPair {
    /// `IA32_FEATURE_CONTROL` bits a CPU driver must set before locking the
    /// register so this pair becomes usable on the current CPU.
    #[must_use]
    pub fn feature_control_bits(self) -> u64 {
        match self {
            Self::Core2Alternative if mtrr_cap_has_smrr() => FEATURE_CONTROL_SMRR_ENABLE,
            Self::Core2Alternative | Self::Architectural => 0,
        }
    }

    pub(super) const fn to_raw(pair: Option<Self>) -> u32 {
        match pair {
            None => 0,
            Some(Self::Architectural) => 1,
            Some(Self::Core2Alternative) => 2,
        }
    }

    pub(super) const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Architectural),
            2 => Some(Self::Core2Alternative),
            _ => None,
        }
    }

    /// Whether the current CPU can program this pair: `IA32_MTRR_CAP`
    /// advertises SMRR and, for the Core 2 pair, `IA32_FEATURE_CONTROL` is
    /// locked with SMRR enabled. Mirrors coreboot's gen1 `smmrelocate.c`.
    pub(super) fn usable_on_current_cpu(self) -> bool {
        if !mtrr_cap_has_smrr() {
            return false;
        }
        match self {
            Self::Architectural => true,
            Self::Core2Alternative => {
                // SAFETY: the model driver named a CPU with this MSR.
                let feature = unsafe { rdmsr(IA32_FEATURE_CONTROL) };
                feature & super::feature_control::LOCK != 0
                    && feature & FEATURE_CONTROL_SMRR_ENABLE != 0
            }
        }
    }

    /// Program this CPU's SMRR while running in the relocation SMI.
    ///
    /// # Safety
    ///
    /// Must run in SMM on a CPU for which [`Self::usable_on_current_cpu`]
    /// returned true.
    pub(super) unsafe fn program(self, range: SmrrRange) -> Result<(), SmrrError> {
        let (base_msr, mask_msr, base) = match self {
            Self::Core2Alternative => (
                CORE2_SMRR_PHYS_BASE,
                CORE2_SMRR_PHYS_MASK,
                range.base & !0xfff,
            ),
            Self::Architectural => (IA32_SMRR_PHYS_BASE, IA32_SMRR_PHYS_MASK, range.base),
        };
        // SAFETY: the caller established that this pair is usable.
        unsafe {
            wrmsr(base_msr, base);
            wrmsr(mask_msr, range.mask);
        }
        // SAFETY: same registers as above.
        let (observed_base, observed_mask) = unsafe { (rdmsr(base_msr), rdmsr(mask_msr)) };
        if observed_base != base || observed_mask != range.mask {
            return Err(SmrrError::VerificationFailed);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SmrrError {
    /// TSEG is not a naturally aligned power of two below 4 GiB.
    InvalidRange,
    /// The registers did not read back the programmed values.
    VerificationFailed,
}

/// Encoded SMRR base/mask covering TSEG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SmrrRange {
    base: u64,
    mask: u64,
}

impl SmrrRange {
    pub(super) fn new(base: u64, size: u32) -> Result<Self, SmrrError> {
        let size = u64::from(size);
        let end = base.checked_add(size).ok_or(SmrrError::InvalidRange)?;
        if size < 0x1000
            || !size.is_power_of_two()
            || !base.is_multiple_of(size)
            || end > 0x1_0000_0000
        {
            return Err(SmrrError::InvalidRange);
        }
        Ok(Self {
            base: (base & ADDRESS_MASK_32) | MTRR_TYPE_WRITE_BACK,
            mask: (!(size - 1) & ADDRESS_MASK_32) | MTRR_PHYS_MASK_VALID,
        })
    }

    pub(super) const fn from_raw(base: u64, mask: u64) -> Self {
        Self { base, mask }
    }

    pub(super) const fn base(self) -> u64 {
        self.base
    }

    pub(super) const fn mask(self) -> u64 {
        self.mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_encodes_tseg_ranges() {
        let range = SmrrRange::new(0x7f80_0000, 0x0080_0000).unwrap();
        assert_eq!(range.base, 0x7f80_0006);
        assert_eq!(range.mask, 0xff80_0800);
        assert_eq!(
            SmrrRange::new(0x7f90_0000, 0x0080_0000),
            Err(SmrrError::InvalidRange)
        );
        assert_eq!(
            SmrrRange::new(0x7f80_0000, 0x0060_0000),
            Err(SmrrError::InvalidRange)
        );
    }

    #[test]
    fn raw_pair_encoding_round_trips() {
        for pair in [
            None,
            Some(SmrrPair::Architectural),
            Some(SmrrPair::Core2Alternative),
        ] {
            assert_eq!(SmrrPair::from_raw(SmrrPair::to_raw(pair)), pair);
        }
    }
}
