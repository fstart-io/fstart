//! Free-range resource pool and BAR overlap/sizing helpers for [`super::PciEcam`].

use super::{PciEcamError, PciFixedBar, PciFixedBarType};

/// Maximum number of free intervals kept for one resource type.
pub(super) const MAX_RESOURCE_RANGES: usize = 16;

#[derive(Debug, Clone, Copy)]
pub(super) struct ResourceRange {
    pub(super) base: u64,
    pub(super) end: u64,
}

/// Free-range allocator for one address type (MMIO32, MMIO64, or IO).
///
/// Unlike a plain bump allocator, this preserves holes occupied by fixed
/// chipset resources such as ECAM, and alignment gaps between BARs. This
/// matches coreboot's domain allocator, which subtracts fixed resources
/// before placing dynamic BARs.
#[derive(Debug, Clone, Copy)]
pub(super) struct ResourcePool {
    pub(super) ranges: [ResourceRange; MAX_RESOURCE_RANGES],
    pub(super) count: usize,
}

const EMPTY_RESOURCE_RANGE: ResourceRange = ResourceRange { base: 0, end: 0 };

pub(super) fn fixed_bars_overlap(left: &PciFixedBar, right: &PciFixedBar) -> bool {
    let same_space = matches!(
        (left.kind, right.kind),
        (PciFixedBarType::Io, PciFixedBarType::Io)
            | (
                PciFixedBarType::Memory32 | PciFixedBarType::Memory64,
                PciFixedBarType::Memory32 | PciFixedBarType::Memory64
            )
    );
    same_space
        && left.base < right.base.saturating_add(right.size)
        && right.base < left.base.saturating_add(left.size)
}

/// No movable bits means no BAR, except a 64-bit memory BAR of 4 GiB or
/// more: its low half has no movable address bits, only its type (64-bit
/// memory) identifies it, and its size lives in the upper half, which is
/// never a BAR of its own.
pub(super) const fn bar_unimplemented(ones: u32, moving_lo: u32) -> bool {
    moving_lo == 0 && ones & 0x7 != 0x4
}

pub(super) fn size_from_moving_bits(moving: u64, address_mask: u64) -> Result<u64, PciEcamError> {
    let address_bits = moving & address_mask;
    if address_bits == 0 {
        return Err(PciEcamError::ConfigError);
    }
    Ok(1u64 << address_bits.trailing_zeros())
}

impl ResourcePool {
    pub(super) const fn empty() -> Self {
        Self {
            ranges: [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES],
            count: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn new(base: u64, size: u64) -> Result<Self, PciEcamError> {
        let mut pool = Self {
            ranges: [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES],
            count: 0,
        };
        let end = base.checked_add(size).ok_or(PciEcamError::ConfigError)?;
        pool.push_range(base, end)?;
        Ok(pool)
    }

    pub(super) fn push_range(&mut self, base: u64, end: u64) -> Result<(), PciEcamError> {
        if base >= end {
            return Ok(());
        }
        if self.count == MAX_RESOURCE_RANGES {
            return Err(PciEcamError::ResourceRangeLimit);
        }
        self.ranges[self.count] = ResourceRange { base, end };
        self.count += 1;
        Ok(())
    }

    /// Remove a fixed address interval from the free space.
    pub(super) fn reserve_range(&mut self, base: u64, size: u64) -> Result<(), PciEcamError> {
        if size == 0 || self.count == 0 {
            return Ok(());
        }
        let end = base.checked_add(size).ok_or(PciEcamError::ConfigError)?;
        let old_ranges = self.ranges;
        let old_count = self.count;
        self.ranges = [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES];
        self.count = 0;

        for range in old_ranges.into_iter().take(old_count) {
            if end <= range.base || base >= range.end {
                self.push_range(range.base, range.end)?;
                continue;
            }
            if range.base < base {
                self.push_range(range.base, base.min(range.end))?;
            }
            if end < range.end {
                self.push_range(end.max(range.base), range.end)?;
            }
        }
        Ok(())
    }

    pub(super) fn free_bytes(&self) -> u64 {
        self.ranges[..self.count]
            .iter()
            .map(|range| range.end - range.base)
            .sum()
    }

    /// Whether `[base, end)` is free.
    pub(super) fn is_free(&self, base: u64, end: u64) -> bool {
        self.ranges[..self.count]
            .iter()
            .any(|range| range.base <= base && end <= range.end)
    }

    /// The size of its largest free range.
    pub(super) fn largest(&self) -> u64 {
        self.ranges[..self.count]
            .iter()
            .map(|range| range.end - range.base)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn allocate_aligned(&mut self, size: u64, align: u64) -> Option<u64> {
        self.allocate_at(size, align, 0)
    }

    /// Allocate `size` bytes, first fit, at an address that is `phase` above
    /// a multiple of `align` (a power of two).
    pub(super) fn allocate_at(&mut self, size: u64, align: u64, phase: u64) -> Option<u64> {
        if size == 0 || align == 0 || phase >= align {
            return None;
        }
        for idx in 0..self.count {
            let range = self.ranges[idx];
            let Some(aligned) = range
                .base
                .checked_add(phase.wrapping_sub(range.base) & (align - 1))
            else {
                continue;
            };
            let Some(end) = aligned.checked_add(size) else {
                continue;
            };
            if end > range.end {
                continue;
            }
            // Remove only the allocated interval, keeping its alignment
            // prefix available for smaller BARs. Commit atomically: if the
            // split exceeds the range capacity, try another candidate.
            let mut remaining = *self;
            if remaining.reserve_range(aligned, size).is_err() {
                continue;
            }
            *self = remaining;
            return Some(aligned);
        }
        None
    }
}
