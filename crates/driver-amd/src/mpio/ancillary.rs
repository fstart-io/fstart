// SPDX-License-Identifier: MIT OR Apache-2.0
//! Retained Siena/NFX type1/2/3/7 ancillary records. These are protocol
//! facts, not Genoa host structures: sp5 `mpio.extattr.layout`; NFX
//! AmdNbioPei ffa45b1d, called by ffa45311. No vendor virtual pointers.

use super::rs::{Kind, Port};
use heapless::Vec;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{IntoBytes, little_endian::U32};

register_bitfields![u32,
    Header [
        TYPE OFFSET(0) NUMBITS(8) [], ID OFFSET(8) NUMBITS(8) [],
        DWORDS OFFSET(16) NUMBITS(8) [], PARAMETER OFFSET(24) NUMBITS(8) []
    ],
    Offsets [FIRST OFFSET(0) NUMBITS(16) [], SECOND OFFSET(16) NUMBITS(16) []]
];

/// The stream header carries an eight-bit dword count, not a byte count.
pub const MAX_DWORDS: usize = 255;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Capacity,
}

/// One controller instance's endian-defined stream. Empty ports contribute
/// neither records nor link IDs. A port with attributes gets a type2 link
/// record whose dword index becomes its ASK ancillaryDataIdx.
pub struct Ancillary {
    words: Vec<U32, MAX_DWORDS>,
    instance: u8,
    links: u8,
    phy_blocks: u8,
}

impl Ancillary {
    pub const fn new(instance: u8) -> Self {
        Self {
            words: Vec::new(),
            instance,
            links: 0,
            phy_blocks: 0,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        self.words.as_slice().as_bytes()
    }
    pub fn dwords(&self) -> u32 {
        self.words.len() as u32
    }

    /// Only explicit type3 PHY and type7 speed-override values. Their
    /// electrical semantics are board policy, not guessed by this encoder.
    /// Capacity failure is transactional: no partial stream is published.
    pub fn add(&mut self, port: &Port, phy: &[u32], speed: &[u32]) -> Result<u16, Error> {
        if phy.is_empty() && speed.is_empty() {
            return Ok(0);
        }
        let overhead = usize::from(self.words.is_empty())
            + usize::from(!phy.is_empty())
            + usize::from(!speed.is_empty())
            + 2;
        let total = phy
            .len()
            .checked_add(speed.len())
            .and_then(|n| n.checked_add(overhead))
            .and_then(|n| n.checked_add(self.words.len()))
            .filter(|&n| n <= MAX_DWORDS)
            .ok_or(Error::Capacity)?;
        // All pushes below fit by the complete preflight; do not silently
        // truncate an attribute, link, count, or firmware buffer offset.
        if self.words.is_empty() {
            self.push(header(1, self.instance, 0, 0));
        }
        let mut first = 0;
        let mut second = 0;
        if !phy.is_empty() {
            first = self.words.len() as u16;
            self.push(header(3, self.phy_blocks, (phy.len() + 1) as u8, 0));
            for &value in phy {
                self.push(value);
            }
            self.phy_blocks += 1;
        }
        if !speed.is_empty() {
            let offset = self.words.len() as u16;
            if first == 0 {
                first = offset;
            } else {
                second = offset;
            }
            self.push(header(
                7,
                u8::from(port.kind != Kind::Pcie),
                (speed.len() + 1) as u8,
                port.start.min(port.end),
            ));
            for &value in speed {
                self.push(value);
            }
        }
        let index = self.words.len() as u16;
        self.push(header(2, self.links, 2, 0));
        let mut offsets = LocalRegisterCopy::<u32, Offsets::Register>::new(0);
        offsets.write(Offsets::FIRST.val(first.into()) + Offsets::SECOND.val(second.into()));
        self.push(offsets.get());
        self.links += 1;
        let stream_header = Header::DWORDS.val(total as u32).modify(self.words[0].get());
        self.words[0].set(stream_header);
        Ok(index)
    }
    fn push(&mut self, value: u32) {
        self.words.push(value.into()).unwrap();
    }
}

fn header(kind: u8, id: u8, dwords: u8, parameter: u8) -> u32 {
    let mut header = LocalRegisterCopy::<u32, Header::Register>::new(0);
    header.write(
        Header::TYPE.val(kind.into())
            + Header::ID.val(id.into())
            + Header::DWORDS.val(dwords.into())
            + Header::PARAMETER.val(parameter.into()),
    );
    header.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_nfx_sata_stream_and_ask_dword_index() {
        let mut stream = Ancillary::new(0);
        assert_eq!(stream.add(&Port::pcie(0, 3), &[], &[]), Ok(0));
        assert!(stream.bytes().is_empty());
        let mut sata = Port::pcie(92, 95);
        sata.kind = Kind::Sata;
        assert_eq!(stream.add(&sata, &[], &[0x1c07, 0x2107, 0x08002807]), Ok(5));
        assert_eq!(
            stream.bytes(),
            &[
                1, 0, 7, 0, 7, 1, 4, 92, 7, 28, 0, 0, 7, 33, 0, 0, 7, 40, 0, 8, 2, 0, 2, 0, 1, 0,
                0, 0,
            ]
        );
        assert_eq!(stream.dwords(), 7);
    }
    #[test]
    fn phy_and_speed_blocks_link_by_offsets_not_host_addresses() {
        let mut stream = Ancillary::new(1);
        assert_eq!(
            stream.add(&Port::pcie(15, 12), &[0x12345678], &[0xabcdef01]),
            Ok(5)
        );
        assert_eq!(
            stream
                .words
                .iter()
                .map(|v| v.get())
                .collect::<std::vec::Vec<_>>(),
            [
                0x00070101, 0x00020003, 0x12345678, 0x0c020007, 0xabcdef01, 0x00020002, 0x00030001
            ]
        );
        assert_eq!(stream.add(&Port::pcie(0, 3), &[42], &[]), Ok(9));
        assert_eq!(stream.words[7].get(), 0x00020103); // next PHY block
        assert_eq!(stream.words[9].get(), 0x00020102); // next attributed link
    }
    #[test]
    fn capacity_failure_preserves_the_complete_previous_stream() {
        let mut stream = Ancillary::new(0);
        stream.add(&Port::pcie(0, 3), &[1], &[]).unwrap();
        let before = stream.words.clone();
        assert_eq!(
            stream.add(&Port::pcie(4, 7), &[0; MAX_DWORDS], &[]),
            Err(Error::Capacity)
        );
        assert_eq!(stream.words, before);
    }
}
