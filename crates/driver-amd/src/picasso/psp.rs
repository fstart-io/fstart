// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso's PSP mailbox: the shared Family 17h Gen2 protocol at the fixed
//! Picasso PSP aperture.
pub use crate::fam17h::psp::*;

#[cfg(target_arch = "x86_64")]
impl Native {
    /// The mailbox at Picasso's PSP aperture [`super::nbio::PSP_MMIO_BASE`].
    /// # Safety
    /// As [`Native::at`], with the Picasso PSP BAR low=fd100101/high=0.
    pub unsafe fn new() -> Self {
        unsafe { Self::at(super::nbio::PSP_MMIO_BASE as usize) }
    }
}
