// SPDX-License-Identifier: MIT OR Apache-2.0
//! Event types (PFP 1.06 §10.4.1; the values below `0x8000_0000` are shared
//! with the TPM 1.2 specifications) and their event data.

pub const EV_POST_CODE: u32 = 0x01;
pub const EV_NO_ACTION: u32 = 0x03;
pub const EV_SEPARATOR: u32 = 0x04;
pub const EV_ACTION: u32 = 0x05;
pub const EV_EVENT_TAG: u32 = 0x06;
pub const EV_S_CRTM_CONTENTS: u32 = 0x07;
pub const EV_S_CRTM_VERSION: u32 = 0x08;
pub const EV_PLATFORM_CONFIG_FLAGS: u32 = 0x0A;
pub const EV_NONHOST_CODE: u32 = 0x0F;
pub const EV_NONHOST_CONFIG: u32 = 0x10;
pub const EV_NONHOST_INFO: u32 = 0x11;
pub const EV_POST_CODE2: u32 = 0x13;
pub const EV_EFI_ACTION: u32 = 0x8000_0007;
pub const EV_EFI_PLATFORM_FIRMWARE_BLOB: u32 = 0x8000_0008;
pub const EV_EFI_PLATFORM_FIRMWARE_BLOB2: u32 = 0x8000_000A;
pub const EV_EFI_HANDOFF_TABLES2: u32 = 0x8000_000B;

/// `BlobDescription` strings of `EV_POST_CODE2` (PFP table 13).
pub mod post_code {
    pub const POST_CODE: &str = "POST CODE";
    pub const SMM_CODE: &str = "SMM CODE";
    pub const ACPI_DATA: &str = "ACPI DATA";
    pub const EMBEDDED_DRIVER: &str = "Embedded UEFI Driver";
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
        let description = &description.as_bytes()[..description.len().min(MAX_EVENT_DATA - 17)];
        let mut data = Self::new();
        data.push(&[description.len() as u8])
            .push(description)
            .push(&base.to_le_bytes())
            .push(&length.to_le_bytes());
        data
    }

    /// `UEFI_PLATFORM_FIRMWARE_BLOB` (TCG EFI Platform 1.22): base and length.
    pub fn blob(base: u64, length: u64) -> Self {
        let mut data = Self::new();
        data.push(&base.to_le_bytes()).push(&length.to_le_bytes());
        data
    }

    /// `TCG_PCClientTaggedEvent`: tag, data size, data.
    pub fn tagged(tag: u32, data: &[u8]) -> Self {
        let data = &data[..data.len().min(MAX_EVENT_DATA - 8)];
        let mut event = Self::new();
        event
            .push(&tag.to_le_bytes())
            .push(&(data.len() as u32).to_le_bytes())
            .push(data);
        event
    }

    /// A NUL-terminated UCS-2 string, as `EV_S_CRTM_VERSION` takes it.
    pub fn ucs2(text: &str) -> Self {
        let mut data = Self::new();
        for unit in text.encode_utf16() {
            data.push(&unit.to_le_bytes());
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
