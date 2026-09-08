//! Initial-image location metadata. This is bounded transport, never key or
//! signature policy. Only uncompressed initial images may contain a patch site.

use serde::{Deserialize, Serialize};

pub const LOCATOR_MAGIC: [u8; 8] = *b"FSTART01";
pub const LOCATOR_VERSION: u32 = 7;
pub const LOCATOR_SIZE: usize = 40;

/// Existing x86 pre-Rust offsets through `microcode_size` are retained. The last
/// word locates the packed filesystem within a platform-owned media window.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct LocatorBlock {
    pub magic: [u8; 8],
    pub version: u32,
    pub manifest_offset: u32,
    pub manifest_size: u32,
    pub total_image_size: u32,
    pub anchor_offset: u32,
    pub microcode_offset: u32,
    pub microcode_size: u32,
    pub image_offset: u32,
}

const _: () = {
    assert!(core::mem::size_of::<LocatorBlock>() == LOCATOR_SIZE);
    assert!(core::mem::offset_of!(LocatorBlock, anchor_offset) == 24);
    assert!(core::mem::offset_of!(LocatorBlock, microcode_offset) == 28);
    assert!(core::mem::offset_of!(LocatorBlock, microcode_size) == 32);
};

/// Media-relative values carried through a protected predecessor handoff.
/// Expected keys/family/minimum version deliberately cannot be transported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaLocator {
    pub image_offset: u64,
    pub image_size: u64,
    pub root_offset: u64,
    pub root_size: u64,
    pub microcode_offset: u64,
    pub microcode_size: u64,
}

impl MediaLocator {
    /// Validate before a media read against the independently supplied platform
    /// window/device capacity. A carried image size never enlarges that window.
    pub fn validate(self, media_capacity: u64) -> bool {
        let within = |offset: u64, size: u64, limit: u64| {
            offset.checked_add(size).is_some_and(|end| end <= limit)
        };
        self.image_size != 0
            && within(self.image_offset, self.image_size, media_capacity)
            && self.root_size == 512
            && within(self.root_offset, self.root_size, self.image_size)
            && if self.microcode_size == 0 {
                self.microcode_offset == 0
            } else {
                self.microcode_offset != 0
                    && within(self.microcode_offset, self.microcode_size, self.image_size)
            }
    }
}

impl LocatorBlock {
    pub const fn placeholder() -> Self {
        Self {
            magic: LOCATOR_MAGIC,
            version: LOCATOR_VERSION,
            manifest_offset: 0,
            manifest_size: 0,
            total_image_size: 0,
            anchor_offset: 0,
            microcode_offset: 0,
            microcode_size: 0,
            image_offset: 0,
        }
    }

    pub fn write_to(self, output: &mut [u8]) {
        assert!(output.len() >= LOCATOR_SIZE, "short locator output");
        output[..8].copy_from_slice(&self.magic);
        for (value, bytes) in [
            self.version,
            self.manifest_offset,
            self.manifest_size,
            self.total_image_size,
            self.anchor_offset,
            self.microcode_offset,
            self.microcode_size,
            self.image_offset,
        ]
        .into_iter()
        .zip(output[8..LOCATOR_SIZE].chunks_exact_mut(4))
        {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Decode exactly one fixed-size locator, including from an unaligned
    /// handoff buffer. Version 6 combined trust/location anchors are not accepted.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != LOCATOR_SIZE || bytes[..8] != LOCATOR_MAGIC {
            return None;
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if word(8) != LOCATOR_VERSION {
            return None;
        }
        Some(Self {
            magic: LOCATOR_MAGIC,
            version: LOCATOR_VERSION,
            manifest_offset: word(12),
            manifest_size: word(16),
            total_image_size: word(20),
            anchor_offset: word(24),
            microcode_offset: word(28),
            microcode_size: word(32),
            image_offset: word(36),
        })
    }

    pub fn from_media(media: MediaLocator) -> Option<Self> {
        Some(Self {
            manifest_offset: media.root_offset.try_into().ok()?,
            manifest_size: media.root_size.try_into().ok()?,
            total_image_size: media.image_size.try_into().ok()?,
            microcode_offset: media.microcode_offset.try_into().ok()?,
            microcode_size: media.microcode_size.try_into().ok()?,
            image_offset: media.image_offset.try_into().ok()?,
            ..Self::placeholder()
        })
    }

    pub fn media(self) -> MediaLocator {
        MediaLocator {
            image_offset: self.image_offset.into(),
            image_size: self.total_image_size.into(),
            root_offset: self.manifest_offset.into(),
            root_size: self.manifest_size.into(),
            microcode_offset: self.microcode_offset.into(),
            microcode_size: self.microcode_size.into(),
        }
    }
}

/// Volatile snapshot of a build-patched locator. Copying forty bytes avoids
/// borrowing mutable media while later code validates and uses its bounds.
#[derive(Clone, Copy)]
pub struct LocatorRef<'a> {
    value: LocatorBlock,
    lifetime: core::marker::PhantomData<&'a [u8]>,
}

impl<'a> LocatorRef<'a> {
    /// # Safety
    /// The forty bytes must stay readable while this snapshot is taken.
    pub unsafe fn read_volatile(data: &'a [u8]) -> Option<Self> {
        if data.len() != LOCATOR_SIZE {
            return None;
        }
        let mut bytes = [0; LOCATOR_SIZE];
        for (index, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: the checked input slice contains each byte.
            *byte = unsafe { core::ptr::read_volatile(data.as_ptr().add(index)) };
        }
        Some(Self {
            value: LocatorBlock::parse(&bytes)?,
            lifetime: core::marker::PhantomData,
        })
    }
    pub fn manifest_offset(self) -> u32 {
        self.value.manifest_offset
    }
    pub fn manifest_size(self) -> u32 {
        self.value.manifest_size
    }
    pub fn total_image_size(self) -> u32 {
        self.value.total_image_size
    }
    pub fn microcode_offset(self) -> u32 {
        self.value.microcode_offset
    }
    pub fn microcode_size(self) -> u32 {
        self.value.microcode_size
    }
    pub fn image_offset(self) -> u32 {
        self.value.image_offset
    }
    pub fn media(self) -> MediaLocator {
        self.value.media()
    }
    pub fn value(self) -> LocatorBlock {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_locator_is_bounded_by_platform_capacity() {
        let valid = MediaLocator {
            image_offset: 0x21000,
            image_size: 0x80000,
            root_offset: 0x7fe00,
            root_size: 512,
            microcode_offset: 0,
            microcode_size: 0,
        };
        assert!(valid.validate(0x100000));
        assert!(!valid.validate(0xa0fff));
        for bad in [
            MediaLocator {
                image_offset: u64::MAX,
                ..valid
            },
            MediaLocator {
                root_offset: u64::MAX,
                ..valid
            },
            MediaLocator {
                microcode_offset: u64::MAX,
                microcode_size: 1,
                ..valid
            },
            MediaLocator {
                root_size: 0,
                ..valid
            },
            MediaLocator {
                image_size: 0,
                ..valid
            },
        ] {
            assert!(!bad.validate(u64::MAX));
        }
    }

    #[test]
    fn locator_wire_keeps_x86_offsets_and_rejects_wrong_version_or_length() {
        let locator = LocatorBlock {
            anchor_offset: 0x11223344,
            microcode_offset: 0x55667788,
            microcode_size: 0x99aabbcc,
            ..LocatorBlock::placeholder()
        };
        let mut bytes = [0; LOCATOR_SIZE];
        locator.write_to(&mut bytes);
        assert_eq!(
            &bytes[24..36],
            &[
                0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55, 0xcc, 0xbb, 0xaa, 0x99
            ]
        );
        assert_eq!(
            LocatorBlock::parse(&bytes).unwrap().microcode_size,
            0x99aabbcc
        );
        assert!(LocatorBlock::parse(&bytes[..39]).is_none());
        bytes[8] = 6;
        assert!(LocatorBlock::parse(&bytes).is_none());
    }
}
