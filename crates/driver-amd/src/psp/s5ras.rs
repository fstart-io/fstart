// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH PSP's S5-domain RAS data: what the PSP kept of the last boot's
//! errors across S5, through GetConfig (C2P `0x5C`) with configuration ID
//! `0x19` (Dell AmdPlatformRasWhDxe `ParseS5DomainRasData 0x1e980`, layout
//! [I]; DEVICES.md §2.12).
//!
//! The request names a DRAM buffer of [`DATA_SIZE`] bytes the PSP fills:
//! `{u32 version, u32 count (at most 32), entry[32]}`, each entry
//! `{u16 socket, u16 die, u32 ras_data[8]}`. The vendor turns an entry
//! with a socket above 0 and `ras_data[0] & 0xFFF` into an accelerator's
//! SMN error and makes no CPU record of the others; on a CPU-only system
//! every entry is expected to be empty (§4 Q7).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, little_endian::U32};

use super::Variant;
use super::command::Command;

/// GetConfig's configuration ID of the S5-domain RAS data.
pub const S5_DOMAIN_RAS: u32 = 0x19;
/// The data buffer's size.
pub const DATA_SIZE: u32 = 0x488;
/// Entries the data holds at most.
pub const MAX_ENTRIES: usize = 32;
const ENTRY_SIZE: usize = 0x24;

/// GetConfig (C2P `0x5C`): configuration `config_id` into the DRAM buffer
/// at `address`, `size` bytes (the header's size is `0x1C`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct GetConfig {
    pub config_id: U32,
    pub address_lo: U32,
    pub address_hi: U32,
    pub size: U32,
    reserved: U32,
}

impl GetConfig {
    /// The S5-domain RAS data into `address` ([`DATA_SIZE`] bytes).
    pub fn s5_domain_ras(address: u64) -> Self {
        Self {
            config_id: S5_DOMAIN_RAS.into(),
            address_lo: (address as u32).into(),
            address_hi: ((address >> 32) as u32).into(),
            size: DATA_SIZE.into(),
            reserved: 0.into(),
        }
    }
}

impl Command for GetConfig {
    const ID: u8 = 0x5c;
    const NAME: &'static str = "GetConfig";

    fn supported(generation: Variant) -> bool {
        generation == Variant::Wh
    }
}

/// One entry of the S5-domain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub socket: u16,
    pub die: u16,
    pub ras_data: [u32; 8],
}

impl Entry {
    /// An error the vendor reports: `ras_data[0] & 0xFFF` set.
    pub const fn has_error(&self) -> bool {
        self.ras_data[0] & 0xfff != 0
    }
}

/// The data's version and its entries (at most [`MAX_ENTRIES`], fewer if
/// `data` is short).
pub fn parse(data: &[u8]) -> (u32, heapless::Vec<Entry, MAX_ENTRIES>) {
    let word = |at: usize| {
        data.get(at..at + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let count = (word(4) as usize).min(MAX_ENTRIES);
    let mut entries = heapless::Vec::new();
    for i in 0..count {
        let at = 8 + i * ENTRY_SIZE;
        let Some(raw) = data.get(at..at + ENTRY_SIZE) else {
            break;
        };
        let half = |o: usize| u16::from_le_bytes([raw[o], raw[o + 1]]);
        let _ = entries.push(Entry {
            socket: half(0),
            die: half(2),
            ras_data: core::array::from_fn(|k| word(at + 4 + 4 * k)),
        });
    }
    (word(0), entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psp::command::C2pBuffer;

    #[test]
    fn request_and_data_layout() {
        let buffer = C2pBuffer::new(GetConfig::s5_domain_ras(0x1_2345_6000));
        assert_eq!(buffer.header.size, 0x1c);
        assert_eq!(
            buffer.request.as_bytes(),
            [
                0x19, 0, 0, 0, 0, 0x60, 0x45, 0x23, 1, 0, 0, 0, 0x88, 4, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(8 + MAX_ENTRIES * ENTRY_SIZE, DATA_SIZE as usize);
        let mut data = [0u8; DATA_SIZE as usize];
        data[0] = 1;
        data[4] = 2;
        data[8 + ENTRY_SIZE..8 + ENTRY_SIZE + 2].copy_from_slice(&1u16.to_le_bytes());
        data[8 + ENTRY_SIZE + 4] = 0x21;
        let (version, entries) = parse(&data);
        assert_eq!((version, entries.len()), (1, 2));
        assert!(!entries[0].has_error());
        assert_eq!((entries[1].socket, entries[1].ras_data[0]), (1, 0x21));
        assert!(entries[1].has_error());
        assert_eq!(parse(&data[..10]).1.len(), 0);
    }
}
