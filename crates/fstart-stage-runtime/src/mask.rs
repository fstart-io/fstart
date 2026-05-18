//! A small fixed-size bitset keyed by [`DeviceId`].
//!
//! Used by the executor to track which devices have been constructed or
//! initialised so a later capability does not redo the work. Replaces
//! the codegen-side `Vec<String>` tracking that lived in
//! `fstart-codegen::stage_gen::generate_fstart_main`.

use fstart_types::DeviceId;

/// A 256-bit bitset over [`DeviceId`] (the full `u8` range).
///
/// `Copy`, stack-only, zero heap — fits the executor's `no_std`
/// constraints. At four 64-bit words, operations compile to a handful
/// of instructions on every supported architecture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceMask([u64; 4]);

impl DeviceMask {
    /// An empty mask (no devices set).
    pub const fn new() -> Self {
        Self([0; 4])
    }

    /// Mark `id` as present.
    #[inline]
    pub fn set(&mut self, id: DeviceId) {
        let (word, bit) = Self::slot(id);
        self.0[word] |= 1u64 << bit;
    }

    /// Clear `id`.
    #[inline]
    pub fn clear(&mut self, id: DeviceId) {
        let (word, bit) = Self::slot(id);
        self.0[word] &= !(1u64 << bit);
    }

    /// Check whether `id` is present.
    #[inline]
    pub fn contains(&self, id: DeviceId) -> bool {
        let (word, bit) = Self::slot(id);
        self.0[word] & (1u64 << bit) != 0
    }

    /// Union of `self` and `other`.
    #[inline]
    pub fn union_with(&mut self, other: &DeviceMask) {
        for i in 0..4 {
            self.0[i] |= other.0[i];
        }
    }

    /// Build a mask from a slice of [`DeviceId`]s — used by the codegen
    /// to construct the `persistent_inited` mask from a plan's
    /// `&'static [DeviceId]` table.
    pub fn from_slice(ids: &[DeviceId]) -> Self {
        let mut m = Self::new();
        for id in ids {
            m.set(*id);
        }
        m
    }

    #[inline]
    const fn slot(id: DeviceId) -> (usize, u32) {
        let idx = id as usize;
        (idx / 64, (idx % 64) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mask_contains_nothing() {
        let m = DeviceMask::new();
        for id in 0u8..=255 {
            assert!(!m.contains(id), "empty mask should not contain {id}");
        }
    }

    #[test]
    fn set_and_contains_roundtrip() {
        let mut m = DeviceMask::new();
        m.set(0);
        m.set(63);
        m.set(64);
        m.set(127);
        m.set(255);
        assert!(m.contains(0));
        assert!(m.contains(63));
        assert!(m.contains(64));
        assert!(m.contains(127));
        assert!(m.contains(255));
        assert!(!m.contains(1));
        assert!(!m.contains(62));
        assert!(!m.contains(128));
        assert!(!m.contains(254));
    }

    #[test]
    fn clear_removes_bit() {
        let mut m = DeviceMask::new();
        m.set(42);
        assert!(m.contains(42));
        m.clear(42);
        assert!(!m.contains(42));
    }

    #[test]
    fn union_merges_masks() {
        let mut a = DeviceMask::new();
        a.set(1);
        a.set(100);
        let mut b = DeviceMask::new();
        b.set(2);
        b.set(100);
        a.union_with(&b);
        assert!(a.contains(1));
        assert!(a.contains(2));
        assert!(a.contains(100));
        assert!(!a.contains(3));
    }

    #[test]
    fn from_slice_builds_mask() {
        let m = DeviceMask::from_slice(&[3, 7, 9]);
        assert!(m.contains(3));
        assert!(m.contains(7));
        assert!(m.contains(9));
        assert!(!m.contains(0));
        assert!(!m.contains(8));
    }
}
