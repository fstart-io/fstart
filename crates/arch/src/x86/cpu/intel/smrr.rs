//! Intel System Management Range Register programming.
//!
//! Which register pair a CPU uses is a CPU-model fact supplied by its model
//! driver through [`SmmCpu`](super::smm::SmmCpu); this module only knows the
//! two architectural layouts and how to test whether they are usable.

use super::msr_register::Msr;
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    CPUID_1_EDX [ MTRR OFFSET(12) NUMBITS(1) [] ]
];
register_bitfields![u64,
    MTRR_CAP [ SMRR OFFSET(11) NUMBITS(1) [] ],
    SMRR_BASE [ MEMORY_TYPE OFFSET(0) NUMBITS(8) [], ADDRESS OFFSET(12) NUMBITS(20) [] ],
    SMRR_MASK [ VALID OFFSET(11) NUMBITS(1) [], ADDRESS OFFSET(12) NUMBITS(20) [] ]
];

const IA32_MTRR_CAP: u32 = 0x0fe;
const CORE2_SMRR_PHYS_BASE: u32 = 0x0a0;
const CORE2_SMRR_PHYS_MASK: u32 = 0x0a1;
const IA32_SMRR_PHYS_BASE: u32 = 0x1f2;
const IA32_SMRR_PHYS_MASK: u32 = 0x1f3;

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

fn mtrr_cap_has_smrr() -> bool {
    let (_, _, _, edx) = crate::x86::cpuid(1);
    // SAFETY: CPUID reports MTRR support, so IA32_MTRR_CAP exists.
    LocalRegisterCopy::<u32, CPUID_1_EDX::Register>::new(edx).is_set(CPUID_1_EDX::MTRR)
        && unsafe { Msr::<MTRR_CAP::Register>::new(IA32_MTRR_CAP).read() }.is_set(MTRR_CAP::SMRR)
}

impl SmrrPair {
    /// `IA32_FEATURE_CONTROL` bits a CPU driver must set before locking the
    /// register so this pair becomes usable on the current CPU.
    #[must_use]
    pub fn feature_control_bits(self) -> u64 {
        match self {
            Self::Core2Alternative if mtrr_cap_has_smrr() => {
                super::feature_control::FEATURE_CONTROL::SMRR_ENABLE::SET.value
            }
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
                let feature = unsafe {
                    Msr::<super::feature_control::FEATURE_CONTROL::Register>::new(
                        super::feature_control::IA32_FEATURE_CONTROL,
                    )
                    .read()
                };
                feature.is_set(super::feature_control::FEATURE_CONTROL::LOCK)
                    && feature.is_set(super::feature_control::FEATURE_CONTROL::SMRR_ENABLE)
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
                SMRR_BASE::ADDRESS.val(range.base >> 12).value, // Type is reserved.
            ),
            Self::Architectural => (IA32_SMRR_PHYS_BASE, IA32_SMRR_PHYS_MASK, range.base),
        };
        let base_reg = Msr::<SMRR_BASE::Register>::new(base_msr);
        let mask_reg = Msr::<SMRR_MASK::Register>::new(mask_msr);
        // SAFETY: the caller established that this pair is usable.
        unsafe {
            base_reg.write(base);
            mask_reg.write(range.mask);
        }
        // SAFETY: same registers as above.
        let (observed_base, observed_mask) = unsafe { (base_reg.read(), mask_reg.read()) };
        if observed_base.get() != base || observed_mask.get() != range.mask {
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
            base: SMRR_BASE::ADDRESS.val(base >> 12).value | SMRR_BASE::MEMORY_TYPE.val(6).value,
            mask: SMRR_MASK::ADDRESS.val((!(size - 1) >> 12) & 0xfffff).value
                | SMRR_MASK::VALID::SET.value,
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
