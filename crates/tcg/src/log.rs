// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TCG event log in a caller-provided buffer.
//!
//! - SHA-1 format (TPM 1.2): `TCG_PCClientPCREvent` entries (Conventional
//!   BIOS 1.21 §11.1.1, EFI Platform 1.22), after its Spec ID events.
//! - Crypto-agile format (TPM 2.0): a `TCG_PCClientPCREvent` carrying
//!   `TCG_EfiSpecIdEvent`, then `TCG_PCR_EVENT2` entries (PFP 1.06 §10).
//!
//! All fields are little-endian. The buffer past the last entry stays zero,
//! so a later boot component can find the end by the first entry whose PCR
//! index and event type are both zero (`EV_PREBOOT_CERT` is reserved).

use fstart_crypto::pcr::{MAX_DIGEST_SIZE, digest_size};

use crate::event::EV_NO_ACTION;

/// Banks a log can describe.
pub const MAX_BANKS: usize = 4;

/// Log entry layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Sha1,
    CryptoAgile,
}

/// The log does not have room for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// One digest per bank of a measurement.
#[derive(Clone, Copy)]
pub struct Digests {
    entries: [(u16, [u8; MAX_DIGEST_SIZE]); MAX_BANKS],
    count: usize,
}

impl Digests {
    pub const fn new() -> Self {
        Self {
            entries: [(0, [0; MAX_DIGEST_SIZE]); MAX_BANKS],
            count: 0,
        }
    }

    /// Add `digest` for `alg`; ignores banks beyond [`MAX_BANKS`].
    pub fn push(&mut self, alg: u16, digest: &[u8]) {
        if let Some((slot_alg, slot)) = self.entries.get_mut(self.count) {
            *slot_alg = alg;
            slot[..digest.len()].copy_from_slice(digest);
            self.count += 1;
        }
    }

    /// `(algorithm, digest)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &[u8])> {
        self.entries[..self.count]
            .iter()
            .map(|(alg, digest)| (*alg, &digest[..digest_size(*alg).unwrap_or(0)]))
    }

    pub fn get(&self, alg: u16) -> Option<&[u8]> {
        self.iter().find(|(a, _)| *a == alg).map(|(_, d)| d)
    }
}

impl Default for Digests {
    fn default() -> Self {
        Self::new()
    }
}

/// An event log being written.
pub struct EventLog {
    buf: &'static mut [u8],
    used: usize,
    last: Option<usize>,
    format: Format,
}

/// `TCG_EfiSpecIdEvent.platformClass` (TCG ACPI client/server).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PlatformClass {
    Client = 0,
    Server = 1,
}

impl EventLog {
    /// A SHA-1 log, starting with the Specification ID events (not
    /// extended): "Spec ID Event00" of the Conventional BIOS spec 1.21
    /// (§11.3.4.1) and, for a UEFI payload, "Spec ID Event02" of the EFI
    /// Platform spec 1.22 (§7.4); clears `buf`.
    pub fn sha1(buf: &'static mut [u8], class: PlatformClass, uefi: bool) -> Result<Self, Full> {
        buf.fill(0);
        let mut log = Self {
            buf,
            used: 0,
            last: None,
            format: Format::Sha1,
        };
        // signature, platformClass, specVersionMinor 2, specVersionMajor 1,
        // specErrata, reserved (Event00) or uintnSize (Event02),
        // vendorInfoSize 0.
        let spec_id = |signature: &[u8; 16], errata: u8, fifth: u8| {
            let mut event = [0u8; 25];
            event[..16].copy_from_slice(signature);
            event[16..20].copy_from_slice(&(class as u32).to_le_bytes());
            event[20..24].copy_from_slice(&[2, 1, errata, fifth]);
            event
        };
        log.append_sha1_entry(
            0,
            EV_NO_ACTION,
            &[0; 20],
            &spec_id(b"Spec ID Event00\0", 1, 0),
        )?;
        if uefi {
            log.append_sha1_entry(
                0,
                EV_NO_ACTION,
                &[0; 20],
                &spec_id(b"Spec ID Event02\0", 2, 2),
            )?;
        }
        Ok(log)
    }

    /// A crypto-agile log for `banks`, starting with the Specification ID
    /// Version event (PFP §10.4.5.1); clears `buf`.
    pub fn crypto_agile(
        buf: &'static mut [u8],
        banks: &[u16],
        class: PlatformClass,
    ) -> Result<Self, Full> {
        buf.fill(0);
        let mut log = Self {
            buf,
            used: 0,
            last: None,
            format: Format::CryptoAgile,
        };
        let mut spec_id = [0u8; 16 + 4 + 4 + 4 + 4 * MAX_BANKS + 1];
        spec_id[..16].copy_from_slice(b"Spec ID Event03\0");
        spec_id[16..20].copy_from_slice(&(class as u32).to_le_bytes());
        // familyVersionMinor 0, familyVersionMajor 2, specRevision 106,
        // uintnSize 2 (UINT64).
        spec_id[20..24].copy_from_slice(&[0, 2, 106, 2]);
        let banks = &banks[..banks.len().min(MAX_BANKS)];
        spec_id[24..28].copy_from_slice(&(banks.len() as u32).to_le_bytes());
        let mut at = 28;
        for &alg in banks {
            let size = digest_size(alg).unwrap_or(0) as u16;
            spec_id[at..at + 2].copy_from_slice(&alg.to_le_bytes());
            spec_id[at + 2..at + 4].copy_from_slice(&size.to_le_bytes());
            at += 4;
        }
        // vendorInfoSize 0.
        at += 1;
        log.append_sha1_entry(0, EV_NO_ACTION, &[0; 20], &spec_id[..at])?;
        Ok(log)
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Physical address of the log.
    pub fn base(&self) -> u64 {
        self.buf.as_ptr() as u64
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Bytes written so far.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Offset of the last entry.
    pub fn last_entry(&self) -> Option<usize> {
        self.last
    }

    /// Give the log area back: the whole buffer and the bytes used.
    pub fn into_area(self) -> (&'static mut [u8], usize) {
        (self.buf, self.used)
    }

    /// The entries written so far.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.used]
    }

    fn reserve(&mut self, size: usize) -> Result<&mut [u8], Full> {
        let start = self.used;
        let end = start.checked_add(size).ok_or(Full)?;
        if end > self.buf.len() {
            return Err(Full);
        }
        self.last = Some(start);
        self.used = end;
        Ok(&mut self.buf[start..end])
    }

    fn append_sha1_entry(
        &mut self,
        pcr: u32,
        event_type: u32,
        digest: &[u8],
        event: &[u8],
    ) -> Result<(), Full> {
        let entry = self.reserve(32 + event.len())?;
        entry[0..4].copy_from_slice(&pcr.to_le_bytes());
        entry[4..8].copy_from_slice(&event_type.to_le_bytes());
        entry[8..28].copy_from_slice(&digest[..20]);
        entry[28..32].copy_from_slice(&(event.len() as u32).to_le_bytes());
        entry[32..].copy_from_slice(event);
        Ok(())
    }

    /// Append an entry with the digests of every bank (only SHA-1 for the
    /// SHA-1 format).
    pub fn append(
        &mut self,
        pcr: u32,
        event_type: u32,
        digests: &Digests,
        event: &[u8],
    ) -> Result<(), Full> {
        match self.format {
            Format::Sha1 => {
                let digest = digests
                    .get(fstart_crypto::pcr::ALG_SHA1)
                    .unwrap_or(&[0; 20]);
                self.append_sha1_entry(pcr, event_type, digest, event)
            }
            Format::CryptoAgile => {
                let digest_bytes: usize = digests.iter().map(|(_, d)| 2 + d.len()).sum();
                let entry = self.reserve(12 + digest_bytes + 4 + event.len())?;
                entry[0..4].copy_from_slice(&pcr.to_le_bytes());
                entry[4..8].copy_from_slice(&event_type.to_le_bytes());
                entry[8..12].copy_from_slice(&(digests.count as u32).to_le_bytes());
                let mut at = 12;
                for (alg, digest) in digests.iter() {
                    entry[at..at + 2].copy_from_slice(&alg.to_le_bytes());
                    entry[at + 2..at + 2 + digest.len()].copy_from_slice(digest);
                    at += 2 + digest.len();
                }
                entry[at..at + 4].copy_from_slice(&(event.len() as u32).to_le_bytes());
                entry[at + 4..].copy_from_slice(event);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_crypto::pcr::{ALG_SHA1, ALG_SHA256};

    fn leak(size: usize) -> &'static mut [u8] {
        Box::leak(vec![0xAAu8; size].into_boxed_slice())
    }

    extern crate std;
    use std::boxed::Box;
    use std::vec;

    #[test]
    fn spec_id_event_layout() {
        let log = EventLog::crypto_agile(leak(256), &[ALG_SHA1, ALG_SHA256], PlatformClass::Server)
            .unwrap();
        let b = log.bytes();
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()),
            EV_NO_ACTION
        );
        assert_eq!(&b[8..28], &[0; 20]);
        let size = u32::from_le_bytes(b[28..32].try_into().unwrap()) as usize;
        assert_eq!(size, 16 + 4 + 4 + 4 + 8 + 1);
        assert_eq!(&b[32..48], b"Spec ID Event03\0");
        assert_eq!(b[48], 1);
        assert_eq!(&b[52..56], &[0, 2, 106, 2]);
        assert_eq!(&b[60..64], &[4, 0, 20, 0]);
        assert_eq!(&b[64..68], &[0x0b, 0, 32, 0]);
        assert_eq!(log.used(), 32 + size);
        assert_eq!(log.buf[log.used()], 0, "tail is cleared");
    }

    #[test]
    fn agile_entry_layout() {
        let mut log =
            EventLog::crypto_agile(leak(512), &[ALG_SHA256], PlatformClass::Client).unwrap();
        let start = log.used();
        let mut digests = Digests::new();
        digests.push(ALG_SHA256, &[7; 32]);
        log.append(4, 5, &digests, b"ev").unwrap();
        let e = &log.bytes()[start..];
        assert_eq!(e.len(), 12 + 34 + 4 + 2);
        assert_eq!(&e[0..12], &[4, 0, 0, 0, 5, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&e[12..14], &[0x0b, 0]);
        assert_eq!(&e[46..52], &[2, 0, 0, 0, b'e', b'v']);
        assert_eq!(log.last_entry(), Some(start));
    }

    #[test]
    fn sha1_spec_id_events() {
        let log = EventLog::sha1(leak(128), PlatformClass::Server, true).unwrap();
        let b = log.bytes();
        assert_eq!(b.len(), 2 * (32 + 25));
        assert_eq!(&b[4..8], &EV_NO_ACTION.to_le_bytes());
        assert_eq!(&b[28..32], &25u32.to_le_bytes());
        assert_eq!(&b[32..48], b"Spec ID Event00\0");
        assert_eq!(&b[48..57], &[1, 0, 0, 0, 2, 1, 1, 0, 0]);
        assert_eq!(&b[57 + 32..57 + 48], b"Spec ID Event02\0");
        assert_eq!(&b[57 + 48..57 + 57], &[1, 0, 0, 0, 2, 1, 2, 2, 0]);
        assert_eq!(log.last_entry(), Some(57));
    }

    #[test]
    fn full_log_is_refused() {
        let mut log = EventLog::sha1(leak(32 + 25 + 40), PlatformClass::Client, false).unwrap();
        let mut digests = Digests::new();
        digests.push(ALG_SHA1, &[1; 20]);
        assert_eq!(log.append(0, 1, &digests, &[0; 8]), Ok(()));
        assert_eq!(log.append(0, 1, &digests, &[]), Err(Full));
    }
}
