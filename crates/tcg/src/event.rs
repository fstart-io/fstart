// SPDX-License-Identifier: MIT OR Apache-2.0
//! Event types (PFP 1.06 §10.4.1; the values below `0x8000_0000` are shared
//! with the TPM 1.2 specifications) and their event data.

pub use fstart_core::services::measure::kind::*;
pub use fstart_core::services::measure::post_code;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// `UEFI_PLATFORM_FIRMWARE_BLOB` base and length (16 bytes).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct FirmwareBlobRange {
    pub base: U64,
    pub length: U64,
}

/// `TCG_PCClientTaggedEvent` prefix: tag and data size (8 bytes).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct TaggedEventPrefix {
    pub tag: U32,
    pub data_size: U32,
}

/// Largest event data this crate builds on the stack.
pub const MAX_EVENT_DATA: usize = 256;

/// Event data assembled in place.
pub struct EventData {
    buf: [u8; MAX_EVENT_DATA],
    len: usize,
}

impl EventData {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_EVENT_DATA],
            len: 0,
        }
    }

    /// Append, truncating at [`MAX_EVENT_DATA`].
    pub fn push(&mut self, bytes: &[u8]) -> &mut Self {
        let n = bytes.len().min(MAX_EVENT_DATA - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        self
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// `UEFI_PLATFORM_FIRMWARE_BLOB2` (PFP §10.2.5): a sized description,
    /// then base and length.
    pub fn blob2(description: &str, base: u64, length: u64) -> Self {
        let range = FirmwareBlobRange {
            base: U64::new(base),
            length: U64::new(length),
        };
        let description = &description.as_bytes()[..description
            .len()
            .min(MAX_EVENT_DATA - 1 - core::mem::size_of::<FirmwareBlobRange>())];
        let mut data = Self::new();
        data.push(&[description.len() as u8])
            .push(description)
            .push(range.as_bytes());
        data
    }

    /// `UEFI_PLATFORM_FIRMWARE_BLOB` (TCG EFI Platform 1.22): base and length.
    pub fn blob(base: u64, length: u64) -> Self {
        let range = FirmwareBlobRange {
            base: U64::new(base),
            length: U64::new(length),
        };
        let mut data = Self::new();
        data.push(range.as_bytes());
        data
    }

    /// `TCG_PCClientTaggedEvent`: tag, data size, data.
    pub fn tagged(tag: u32, data: &[u8]) -> Self {
        let data = &data[..data
            .len()
            .min(MAX_EVENT_DATA - core::mem::size_of::<TaggedEventPrefix>())];
        let prefix = TaggedEventPrefix {
            tag: U32::new(tag),
            data_size: U32::new(data.len() as u32),
        };
        let mut event = Self::new();
        event.push(prefix.as_bytes()).push(data);
        event
    }

    /// A NUL-terminated UCS-2 string, as `EV_S_CRTM_VERSION` takes it.
    pub fn ucs2(text: &str) -> Self {
        let mut data = Self::new();
        for unit in text.encode_utf16() {
            data.push(U16::new(unit).as_bytes());
        }
        data.push(&[0, 0]);
        data
    }
}

impl Default for EventData {
    fn default() -> Self {
        Self::new()
    }
}
