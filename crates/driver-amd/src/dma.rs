// SPDX-License-Identifier: MIT OR Apache-2.0
//! Caller-reserved RAM shared with retained AMD firmware, below 4 GiB.
//! Extracted from the DXIO DMA window so MPIO need not duplicate volatile
//! access and physical-extent checks. Protocol layout and cache ownership
//! remain the client's responsibility; this is not an allocator or MMIO.

use core::ptr::NonNull;
use core::sync::atomic::{Ordering, compiler_fence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidWindow;

/// Volatile shared RAM with a protocol-specific minimum capacity. Dropping
/// this handle NEVER frees memory or proves firmware has stopped accessing it.
pub struct Window<const MINIMUM: usize> {
    ptr: NonNull<u8>,
    len: usize,
    phys: u32,
}

impl<const MINIMUM: usize> Window<MINIMUM> {
    /// # Safety
    /// On success, `ptr..ptr+len` is caller-owned RAM, visible at `phys`,
    /// with no competing Rust references for this handle's lifetime. Keep
    /// its backing allocation reserved until firmware has released it, even
    /// after this handle is dropped. A transport timeout is NOT a release.
    /// The caller supplies protocol-specific alignment and cache maintenance.
    pub unsafe fn new(ptr: *mut u8, len: usize, phys: u64) -> Result<Self, InvalidWindow> {
        let fits = len >= MINIMUM
            && phys
                .checked_add(len as u64)
                .is_some_and(|end| end <= 1 << 32);
        match NonNull::new(ptr) {
            Some(ptr) if fits => Ok(Self {
                ptr,
                len,
                phys: phys as u32,
            }),
            _ => Err(InvalidWindow),
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Raw address for the owner's cache maintenance, not a new RAM borrow.
    pub const fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Physical address of an actual byte; never publish a one-past address
    /// that could wrap when a window ends at exactly 4 GiB.
    pub fn phys(&self, offset: usize) -> u32 {
        assert!(offset < self.len);
        self.phys + offset as u32
    }

    /// Caller must first acquire protocol ownership from firmware. This
    /// compiler fence does not replace CPU/device cache synchronization.
    pub fn write(&self, offset: usize, data: &[u8]) {
        assert!(offset <= self.len && data.len() <= self.len - offset);
        data.iter().enumerate().for_each(|(i, &b)| {
            // SAFETY: constructor RAM contract and checked bounds.
            unsafe { self.ptr.as_ptr().add(offset + i).write_volatile(b) }
        });
        compiler_fence(Ordering::SeqCst);
    }

    /// Read only after firmware ownership/completion and cache maintenance
    /// have been established by the protocol owner.
    pub fn read(&self, offset: usize, out: &mut [u8]) {
        assert!(offset <= self.len && out.len() <= self.len - offset);
        compiler_fence(Ordering::SeqCst);
        out.iter_mut().enumerate().for_each(|(i, b)| {
            // SAFETY: constructor RAM contract and checked bounds.
            *b = unsafe { self.ptr.as_ptr().add(offset + i).read_volatile() }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_geometry_and_volatile_round_trip() {
        let mut ram = [0u8; 32];
        let ptr = ram.as_mut_ptr();
        assert!(unsafe { Window::<32>::new(ptr, 31, 0x1000) }.is_err());
        assert!(unsafe { Window::<32>::new(core::ptr::null_mut(), 32, 0x1000) }.is_err());
        assert!(unsafe { Window::<32>::new(ptr, 32, 0xffff_fff0) }.is_err());
        assert!(unsafe { Window::<32>::new(ptr, 32, u64::MAX - 15) }.is_err());
        let window = unsafe { Window::<32>::new(ptr, 32, 0xffff_ffe0) }.unwrap();
        assert_eq!(window.phys(31), u32::MAX);
        window.write(7, &[1, 2, 3]);
        let mut out = [0; 3];
        window.read(7, &mut out);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(window.len(), 32);
        assert!(!window.is_empty());
        window.write(32, &[]);
        window.read(32, &mut []);
        assert!(std::panic::catch_unwind(|| window.phys(32)).is_err());
        assert!(std::panic::catch_unwind(|| window.write(usize::MAX, &[1])).is_err());
        assert!(std::panic::catch_unwind(|| window.write(31, &[1, 2])).is_err());
    }
}
