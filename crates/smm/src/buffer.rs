//! Checks of the memory an SMM handler is asked to touch.
//!
//! An SMM handler must not let code outside SMM name the memory it reads
//! or writes: a pointer the OS controls (a BAR it programmed, a buffer
//! address it wrote, a pointer read from either) may aim at SMRAM, and
//! the handler then discloses or corrupts it (the "confused deputy" the
//! WSMT's `COMM_BUFFER_NESTED_PTR_PROTECTION` promises against). Every
//! such address is checked with [`Region`]: inside a range the firmware
//! recorded before SMM was locked, and outside SMRAM
//! ([`crate::SmmContext::smram`]).

/// A physical range `[base, base + size)`. Free of panics: every
/// computation saturates or is checked, for SMM.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
}

impl Region {
    pub const EMPTY: Self = Self { base: 0, size: 0 };

    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }

    /// The exclusive end, `None` if it overflows.
    pub const fn end(&self) -> Option<u64> {
        self.base.checked_add(self.size)
    }

    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Whether `[base, base + size)` lies wholly inside this region. An
    /// empty or overflowing range is never inside.
    pub const fn contains(&self, base: u64, size: u64) -> bool {
        let (Some(end), Some(own_end)) = (base.checked_add(size), self.end()) else {
            return false;
        };
        size != 0 && base >= self.base && end <= own_end
    }

    /// Whether `[base, base + size)` shares a byte with this region. A
    /// range whose end overflows overlaps everything above its base.
    pub const fn overlaps(&self, base: u64, size: u64) -> bool {
        if size == 0 || self.size == 0 {
            return false;
        }
        let end = match base.checked_add(size) {
            Some(end) => end,
            None => u64::MAX,
        };
        let own_end = match self.end() {
            Some(end) => end,
            None => u64::MAX,
        };
        base < own_end && self.base < end
    }
}

/// Whether `[base, base + size)` is a buffer an SMM handler may touch for
/// code outside SMM: non-empty, not wrapping, inside one of `allowed` and
/// sharing no byte with any of `protected` (SMRAM first of all).
pub fn permitted(base: u64, size: u64, allowed: &[Region], protected: &[Region]) -> bool {
    allowed.iter().any(|region| region.contains(base, size))
        && !protected.iter().any(|region| region.overlaps(base, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMRAM: Region = Region::new(0x7f00_0000, 0x100_0000);

    #[test]
    fn inside_and_overlap_are_exact_at_the_edges() {
        let window = Region::new(0x1000, 0x1000);
        assert!(window.contains(0x1000, 0x1000));
        assert!(window.contains(0x1ffc, 4));
        assert!(!window.contains(0x1ffd, 4));
        assert!(!window.contains(0xfff, 4));
        assert!(!window.contains(0x1000, 0));
        assert!(window.overlaps(0xffc, 5));
        assert!(!window.overlaps(0xffc, 4));
        assert!(!window.overlaps(0x2000, 4));
        assert!(window.overlaps(0x1fff, 1));
    }

    #[test]
    fn wrapping_ranges_are_refused() {
        let all = Region::new(0, u64::MAX);
        assert!(!all.contains(u64::MAX - 3, 8));
        assert!(SMRAM.overlaps(0x7000_0000, u64::MAX));
        assert!(!permitted(u64::MAX - 3, 8, &[all], &[SMRAM]));
    }

    /// A buffer inside an allowed window that reaches into SMRAM, or one
    /// wholly in SMRAM, is refused, also when the window (wrongly) covers
    /// SMRAM.
    #[test]
    fn smram_is_never_permitted() {
        let low = Region::new(0x7000_0000, 0x1000_0000);
        assert!(permitted(0x7000_0000, 0x1_0000, &[low], &[SMRAM]));
        assert!(!permitted(0x7eff_f000, 0x2000, &[low], &[SMRAM]));
        assert!(!permitted(0x7f00_0000, 4, &[low], &[SMRAM]));
        assert!(!permitted(0x7fff_fffc, 4, &[low], &[SMRAM]));
        assert!(!permitted(0x9000_0000, 4, &[low], &[SMRAM]));
        assert!(!permitted(0x7000_0000, 4, &[], &[SMRAM]));
    }
}
