//! SMM communication-buffer isolation and bounds validation.
//!
//! Before an SMM handler dereferences any OS-supplied buffer (such as a UEFI
//! variable communication buffer or nested descriptor payload), it must prove
//! that the address range:
//!
//! - is non-null and properly aligned;
//! - does not wrap around `u64::MAX`;
//! - stays within the address space mapped by SMM (e.g. the low 4 GiB
//!   identity map);
//! - never touches or overlaps SMRAM/TSEG or forbidden MMIO windows (SPI
//!   flash controller, LAPIC, etc.); and
//! - when an allowlist is configured, stays strictly within an allowed region.
//!
//! All operations are `no_std`, allocation-free, panic-free, and backed by
//! Kani formal proofs against `u128` reference specifications.
//!
//! # TOCTOU / Double-Fetch Safety
//!
//! Validating a communication buffer range only proves that its physical
//! addresses are outside SMRAM/MMIO and within the mapped window. Because the
//! OS or DMA masters can concurrently modify non-SMRAM memory, SMM handlers
//! MUST copy validated untrusted bytes into SMRAM once before parsing headers,
//! lengths, or signatures, and must never re-read fields from the external
//! buffer after validation.
//!
//! Likewise, [`MemoryRange::subrange`] permits a zero-length sentinel at
//! `outer.end()` (which may equal the start of an adjacent forbidden range);
//! callers must check `!sub.is_empty()` / `sub.len()` before dereferencing.

/// Default upper exclusive bound of the SMM 4 GiB identity map.
pub const SMM_IDENTITY_MAP_END: u64 = 1u64 << 32;

/// Errors returned when validating an untrusted SMM communication buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommError {
    /// The buffer base address is zero.
    NullBase,
    /// `base + len` overflows `u64`.
    Overflow,
    /// The buffer ends above the policy's addressable limit.
    AboveAddressLimit,
    /// The alignment argument is not a power of two, or `base` is misaligned.
    Misaligned,
    /// The buffer overlaps (or, if zero-length, points into) a forbidden range
    /// such as SMRAM or protected MMIO.
    OverlapsForbidden,
    /// The buffer is not contained in any allowed region.
    OutsideAllowed,
    /// A requested sub-slice exceeds its parent validated buffer.
    SubrangeOutOfBounds,
    /// A non-empty buffer was required, but `len == 0`.
    ZeroLength,
}

/// Half-open physical address range `[base, base + len)` guaranteed not to
/// overflow `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRange {
    base: u64,
    len: u64,
}

impl MemoryRange {
    /// Construct `[base, base + len)`, returning `None` if `base + len`
    /// overflows `u64`.
    #[must_use]
    pub const fn new(base: u64, len: u64) -> Option<Self> {
        if base.checked_add(len).is_some() {
            Some(Self { base, len })
        } else {
            None
        }
    }

    /// Construct `[start, end)`, returning `None` if `end < start`.
    #[must_use]
    pub const fn from_bounds(start: u64, end: u64) -> Option<Self> {
        match end.checked_sub(start) {
            Some(len) => Some(Self { base: start, len }),
            None => None,
        }
    }

    /// Inclusive start address.
    #[must_use]
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Whether the range contains zero bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Exclusive end address (`base + len`). Cannot overflow by construction.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.base.wrapping_add(self.len)
    }

    /// Whether two half-open ranges share at least one byte.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        !self.is_empty() && !other.is_empty() && self.base < other.end() && other.base < self.end()
    }

    /// Whether `self` overlaps `forbidden`, or (when `self` is empty) points
    /// inside a non-empty `forbidden` range.
    #[must_use]
    pub const fn touches_or_overlaps(self, forbidden: Self) -> bool {
        if forbidden.is_empty() {
            return false;
        }
        if self.is_empty() {
            self.base >= forbidden.base && self.base < forbidden.end()
        } else {
            self.base < forbidden.end() && forbidden.base < self.end()
        }
    }

    /// Whether every byte of `inner` (and its base/end endpoints) lies within
    /// `self`.
    #[must_use]
    pub const fn contains(self, inner: Self) -> bool {
        inner.base >= self.base && inner.end() <= self.end()
    }

    /// Carve a validated sub-range `[base + offset, base + offset + len)` from
    /// an already-validated outer buffer.
    pub const fn subrange(self, offset: u64, len: u64) -> Result<Self, CommError> {
        let Some(sub_end) = offset.checked_add(len) else {
            return Err(CommError::SubrangeOutOfBounds);
        };
        if sub_end > self.len {
            return Err(CommError::SubrangeOutOfBounds);
        }
        // `self.base + self.len <= u64::MAX` and `offset + len <= self.len`,
        // so `self.base + offset` and `self.base + offset + len` cannot wrap.
        let base = self.base.wrapping_add(offset);
        Ok(Self { base, len })
    }
}

/// Policy describing which physical memory ranges an SMM handler may treat as
/// untrusted communication buffers.
#[derive(Debug, Clone, Copy)]
pub struct SmmBufferPolicy<'a> {
    forbidden: &'a [MemoryRange],
    allowed: Option<&'a [MemoryRange]>,
    max_end: u64,
}

impl<'a> SmmBufferPolicy<'a> {
    /// Create a policy rejecting all `forbidden` ranges and any access ending
    /// above `max_end`.
    #[must_use]
    pub const fn new(forbidden: &'a [MemoryRange], max_end: u64) -> Self {
        Self {
            forbidden,
            allowed: None,
            max_end,
        }
    }

    /// Additionally require every validated buffer to be contained inside one
    /// of the `allowed` ranges. Forbidden ranges still take precedence.
    #[must_use]
    pub const fn with_allowed(mut self, allowed: &'a [MemoryRange]) -> Self {
        self.allowed = Some(allowed);
        self
    }

    /// Validate `[base, base + len)` with power-of-two `align`.
    pub fn validate(&self, base: u64, len: u64, align: u64) -> Result<MemoryRange, CommError> {
        if base == 0 {
            return Err(CommError::NullBase);
        }
        if align == 0 || !align.is_power_of_two() || (base & (align - 1)) != 0 {
            return Err(CommError::Misaligned);
        }
        let range = MemoryRange::new(base, len).ok_or(CommError::Overflow)?;
        if range.end() > self.max_end {
            return Err(CommError::AboveAddressLimit);
        }
        for &forbidden in self.forbidden {
            if range.touches_or_overlaps(forbidden) {
                return Err(CommError::OverlapsForbidden);
            }
        }
        if let Some(allowed) = self.allowed {
            let mut permitted = false;
            for &region in allowed {
                if !region.is_empty() && region.contains(range) {
                    permitted = true;
                    break;
                }
            }
            if !permitted {
                return Err(CommError::OutsideAllowed);
            }
        }
        Ok(range)
    }

    /// Validate a non-empty buffer `[base, base + len)` with power-of-two
    /// `align`.
    pub fn validate_non_empty(
        &self,
        base: u64,
        len: u64,
        align: u64,
    ) -> Result<MemoryRange, CommError> {
        if len == 0 {
            return Err(CommError::ZeroLength);
        }
        self.validate(base, len, align)
    }
}

/// Validate that `[base, base + len)` is non-null, does not wrap, stays within
/// `max_phys_end` (e.g. [`SMM_IDENTITY_MAP_END`] for the 4 GiB SMM identity
/// map; for non-empty ranges this also prevents 32-bit `as usize` truncation on
/// i686, whereas an empty sentinel at `base == SMM_IDENTITY_MAP_END` requires
/// checking `!range.is_empty()` before converting), and is completely disjoint
/// from `smram`.
pub fn validate_outside_smram(
    base: u64,
    len: u64,
    smram: MemoryRange,
    max_phys_end: u64,
) -> Result<MemoryRange, CommError> {
    let forbidden = [smram];
    SmmBufferPolicy::new(&forbidden, max_phys_end).validate(base, len, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_smram_overlap_wraparound_and_above_limit() {
        let smram = MemoryRange::new(0x7f00_0000, 0x0100_0000).unwrap();
        let mmio = MemoryRange::new(0xfec0_0000, 0x0140_0000).unwrap();
        let forbidden = [smram, mmio];
        let policy = SmmBufferPolicy::new(&forbidden, 1u64 << 32);

        // Clean RAM buffer below TSEG.
        let ok = policy.validate_non_empty(0x1000_0000, 0x1000, 8).unwrap();
        assert_eq!(ok.base(), 0x1000_0000);
        assert_eq!(ok.end(), 0x1000_1000);

        // Adjacent to TSEG start/end is allowed.
        assert!(policy.validate_non_empty(0x7eff_f000, 0x1000, 1).is_ok());
        assert!(policy.validate_non_empty(0x8000_0000, 0x1000, 1).is_ok());

        // Overlapping TSEG start, interior, enclosing, or zero-length inside.
        assert_eq!(
            policy.validate_non_empty(0x7eff_f800, 0x1000, 1),
            Err(CommError::OverlapsForbidden)
        );
        assert_eq!(
            policy.validate_non_empty(0x7f00_1000, 0x100, 1),
            Err(CommError::OverlapsForbidden)
        );
        assert_eq!(
            policy.validate_non_empty(0x7eff_0000, 0x0200_0000, 1),
            Err(CommError::OverlapsForbidden)
        );
        assert_eq!(
            policy.validate(0x7f00_0000, 0, 1),
            Err(CommError::OverlapsForbidden)
        );

        // MMIO overlap and >4 GiB limit.
        assert_eq!(
            policy.validate_non_empty(0xfed1_0000, 0x1000, 1),
            Err(CommError::OverlapsForbidden)
        );
        assert_eq!(
            policy.validate_non_empty(0x1_0000_0000, 0x1000, 1),
            Err(CommError::AboveAddressLimit)
        );

        // Wraparound, null, misalignment, zero-length.
        assert_eq!(
            policy.validate_non_empty(u64::MAX - 0x10, 0x20, 1),
            Err(CommError::Overflow)
        );
        assert_eq!(
            policy.validate_non_empty(0, 0x10, 1),
            Err(CommError::NullBase)
        );
        assert_eq!(
            policy.validate_non_empty(0x1001, 0x10, 4),
            Err(CommError::Misaligned)
        );
        assert_eq!(
            policy.validate_non_empty(0x1000, 0, 1),
            Err(CommError::ZeroLength)
        );
    }

    #[test]
    fn allowed_list_enforces_containment_while_forbidden_takes_precedence() {
        let smram = MemoryRange::new(0x7f00_0000, 0x0100_0000).unwrap();
        let comm_buf = MemoryRange::new(0x7e00_0000, 0x0001_0000).unwrap();
        // Deliberately include a buggy allowed range that covers SMRAM to
        // verify forbidden always wins.
        let allowed = [
            comm_buf,
            MemoryRange::new(0x7e00_0000, 0x0200_0000).unwrap(),
        ];
        let forbidden = [smram];
        let policy = SmmBufferPolicy::new(&forbidden, 1u64 << 32).with_allowed(&allowed);

        let outer = policy.validate_non_empty(0x7e00_0000, 0x4000, 16).unwrap();
        let sub = outer.subrange(0x100, 0x200).unwrap();
        assert_eq!(sub.base(), 0x7e00_0100);
        assert_eq!(sub.end(), 0x7e00_0300);
        assert_eq!(
            outer.subrange(0x3f00, 0x200),
            Err(CommError::SubrangeOutOfBounds)
        );

        // Outside allowed.
        assert_eq!(
            policy.validate_non_empty(0x1000_0000, 0x100, 1),
            Err(CommError::OutsideAllowed)
        );
        // Inside the buggy allowed region, but overlaps SMRAM: still rejected.
        assert_eq!(
            policy.validate_non_empty(0x7f00_0000, 0x100, 1),
            Err(CommError::OverlapsForbidden)
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn overlaps_u128(a_base: u64, a_len: u64, b_base: u64, b_len: u64) -> bool {
        let a_start = u128::from(a_base);
        let a_end = a_start + u128::from(a_len);
        let b_start = u128::from(b_base);
        let b_end = b_start + u128::from(b_len);
        a_len != 0 && b_len != 0 && a_start < b_end && b_start < a_end
    }

    #[kani::proof]
    fn verify_memory_range_overlaps_matches_u128_spec() {
        let a_base: u64 = kani::any();
        let a_len: u64 = kani::any();
        let b_base: u64 = kani::any();
        let b_len: u64 = kani::any();

        let Some(a) = MemoryRange::new(a_base, a_len) else {
            assert!(u128::from(a_base) + u128::from(a_len) > u128::from(u64::MAX));
            return;
        };
        let Some(b) = MemoryRange::new(b_base, b_len) else {
            assert!(u128::from(b_base) + u128::from(b_len) > u128::from(u64::MAX));
            return;
        };

        assert_eq!(u128::from(a.end()), u128::from(a_base) + u128::from(a_len));
        assert_eq!(u128::from(b.end()), u128::from(b_base) + u128::from(b_len));
        assert_eq!(a.overlaps(b), overlaps_u128(a_base, a_len, b_base, b_len));
        assert_eq!(a.overlaps(b), b.overlaps(a));
    }

    #[kani::proof]
    fn verify_memory_range_subrange_matches_u128_spec() {
        let base: u64 = kani::any();
        let len: u64 = kani::any();
        let offset: u64 = kani::any();
        let sub_len: u64 = kani::any();

        let Some(outer) = MemoryRange::new(base, len) else {
            return;
        };

        let outer_start = u128::from(base);
        let outer_end = outer_start + u128::from(len);
        let sub_start = outer_start + u128::from(offset);
        let sub_end = sub_start + u128::from(sub_len);
        let in_bounds = sub_end <= outer_end;

        match outer.subrange(offset, sub_len) {
            Ok(sub) => {
                assert!(in_bounds);
                assert_eq!(u128::from(sub.base()), sub_start);
                assert_eq!(u128::from(sub.end()), sub_end);
                assert!(outer.contains(sub));
            }
            Err(CommError::SubrangeOutOfBounds) => {
                assert!(!in_bounds);
            }
            Err(_) => unreachable!(),
        }
    }

    #[kani::proof]
    fn verify_policy_rejects_any_overlap_with_smram_or_mmio() {
        let smram_base: u64 = kani::any();
        let smram_len: u64 = kani::any();
        let mmio_base: u64 = kani::any();
        let mmio_len: u64 = kani::any();
        let max_end: u64 = kani::any();
        let base: u64 = kani::any();
        let len: u64 = kani::any();
        let align: u64 = kani::any();

        let Some(smram) = MemoryRange::new(smram_base, smram_len) else {
            return;
        };
        let Some(mmio) = MemoryRange::new(mmio_base, mmio_len) else {
            return;
        };
        let forbidden = [smram, mmio];
        let policy = SmmBufferPolicy::new(&forbidden, max_end);

        if let Ok(range) = policy.validate(base, len, align) {
            kani::cover!(true, "valid buffer accepted by policy");
            assert!(range.base() != 0);
            assert!(align.is_power_of_two());
            assert_eq!(range.base() % align, 0);

            let r_start = u128::from(range.base());
            let r_end = r_start + u128::from(range.len());
            assert!(r_end <= u128::from(u64::MAX));
            assert!(r_end <= u128::from(max_end));

            // No byte in `range` can lie in SMRAM or MMIO.
            assert!(!overlaps_u128(
                range.base(),
                range.len(),
                smram_base,
                smram_len
            ));
            assert!(!overlaps_u128(
                range.base(),
                range.len(),
                mmio_base,
                mmio_len
            ));

            // Even for a zero-length buffer, `base` cannot point inside a
            // non-empty forbidden range.
            if smram_len != 0 {
                assert!(range.base() < smram_base || range.base() >= smram.end());
            }
            if mmio_len != 0 {
                assert!(range.base() < mmio_base || range.base() >= mmio.end());
            }
        }
    }

    #[kani::proof]
    fn verify_policy_forbidden_overrides_allowed_and_subrange_stays_isolated() {
        let smram_base: u64 = kani::any();
        let smram_len: u64 = kani::any();
        let allow_base: u64 = kani::any();
        let allow_len: u64 = kani::any();
        let max_end: u64 = kani::any();
        let base: u64 = kani::any();
        let len: u64 = kani::any();
        let sub_off: u64 = kani::any();
        let sub_len: u64 = kani::any();

        let Some(smram) = MemoryRange::new(smram_base, smram_len) else {
            return;
        };
        let Some(allow) = MemoryRange::new(allow_base, allow_len) else {
            return;
        };
        let forbidden = [smram];
        let allowed = [allow];
        let policy = SmmBufferPolicy::new(&forbidden, max_end).with_allowed(&allowed);

        if let Ok(outer) = policy.validate_non_empty(base, len, 1) {
            assert!(!allow.is_empty());
            assert!(allow.contains(outer));
            assert!(!outer.overlaps(smram));

            if let Ok(sub) = outer.subrange(sub_off, sub_len) {
                kani::cover!(!sub.is_empty(), "non-empty allowed subrange accepted");
                assert!(allow.contains(sub));
                assert!(!sub.overlaps(smram));
                // Non-empty subranges (and empty subranges strictly inside `outer`)
                // cannot even point at the first byte of SMRAM; an empty subrange at
                // `outer.end()` is a one-past-the-end sentinel referencing 0 bytes.
                if !sub.is_empty() || sub.base() < outer.end() {
                    assert!(!sub.touches_or_overlaps(smram));
                }
                assert!(sub.end() <= max_end);
            }
        }
    }
}
