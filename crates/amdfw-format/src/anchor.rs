// SPDX-License-Identifier: MIT OR Apache-2.0
//! The flash anchor: where a reset image finds its FFS in the SPI flash.
//!
//! The image builder patches it into the reset image; the reset image reads
//! it to map the FFS through ROM2 or ROM3. Both sides use this one
//! definition, so the builder needs no silicon driver.
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Magic and version of [`FlashAnchor`].
pub const ANCHOR_MAGIC: [u8; 8] = *b"SP3FLASH";
pub const ANCHOR_VERSION: u32 = 1;
pub const ANCHOR_SIZE: usize = 24;

/// Where the reset image's FFS lives in the SPI flash, as absolute flash
/// offsets (read through ROM3, so it does not matter which half ROM2
/// shows). The reset image links a [`FlashAnchor::placeholder`] at an
/// 8-byte aligned address; the image builder fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, align(8))]
pub struct FlashAnchor {
    pub magic: [u8; 8],
    pub version: U32,
    /// Size of the flash part.
    pub flash_size: U32,
    /// FFS start, flash offset.
    pub ffs_offset: U32,
    pub ffs_size: U32,
}

const _: () = assert!(core::mem::size_of::<FlashAnchor>() == ANCHOR_SIZE);

impl FlashAnchor {
    /// The unpatched site: magic and version, no FFS.
    pub const fn placeholder() -> Self {
        Self::new(0, 0, 0)
    }

    pub const fn new(flash_size: u32, ffs_offset: u32, ffs_size: u32) -> Self {
        Self {
            magic: ANCHOR_MAGIC,
            version: U32::new(ANCHOR_VERSION),
            flash_size: U32::new(flash_size),
            ffs_offset: U32::new(ffs_offset),
            ffs_size: U32::new(ffs_size),
        }
    }

    /// A patched anchor: right magic and version, an FFS inside a flash
    /// of 16, 32 or 64 MiB.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let anchor = Self::read_from_bytes(bytes.get(..ANCHOR_SIZE)?).ok()?;
        let flash = anchor.flash_size.get();
        let end = anchor.ffs_offset.get().checked_add(anchor.ffs_size.get())?;
        (anchor.magic == ANCHOR_MAGIC
            && anchor.version.get() == ANCHOR_VERSION
            && matches!(flash, 0x100_0000 | 0x200_0000 | 0x400_0000)
            && anchor.ffs_size.get() != 0
            && end <= flash)
            .then_some(anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: u32 = crate::HALF_SIZE as u32;

    #[test]
    fn anchor_placeholder_and_patched_value() {
        let placeholder = FlashAnchor::placeholder();
        assert_eq!(placeholder.as_bytes().len(), ANCHOR_SIZE);
        assert_eq!(&placeholder.as_bytes()[..8], b"SP3FLASH");
        assert!(FlashAnchor::parse(placeholder.as_bytes()).is_none());
        let patched = FlashAnchor::new(2 * HALF, 0x60_0000, 0x10_0000);
        assert_eq!(FlashAnchor::parse(patched.as_bytes()), Some(patched));
        let outside = FlashAnchor::new(HALF, 0xf0_0000, 0x20_0000);
        assert!(FlashAnchor::parse(outside.as_bytes()).is_none());
    }
}
