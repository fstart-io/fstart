//! Minimal Wireshark `pcapng` writer emitting `LINKTYPE_LINUX_SLL2` (276)
//! frames with `ETH_P_MCTP` (`0x00FA`), matching OpenBMC `libmctp` and
//! Wireshark's built-in MCTP / PLDM dissectors.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use zerocopy::big_endian::{U16 as BeU16, U32 as BeU32};
use zerocopy::little_endian::{I64 as LeI64, U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// `LINKTYPE_LINUX_SLL2` (276): Linux cooked capture v2, which Wireshark uses
/// to dispatch `ETH_P_MCTP` (`0x00FA`) packets to the MCTP dissector.
pub const LINKTYPE_LINUX_SLL2: u16 = 276;
/// Linux `ETH_P_MCTP` protocol number (big-endian in SLL2 header).
pub const ETH_P_MCTP: u16 = 0x00FA;
/// Linux `ARPHRD_MCTP` hardware type.
const ARPHRD_MCTP: u16 = 290;

/// Common 8-byte pcapng block prefix (type + total length).
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PcapNgBlockPrefix {
    pub block_type: LeU32,
    pub block_len: LeU32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ShbBlock {
    block_type: LeU32,
    block_len: LeU32,
    byte_order_magic: LeU32,
    major_version: LeU16,
    minor_version: LeU16,
    section_length: LeI64,
    trailing_len: LeU32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct IdbBlock {
    pub block_type: LeU32,
    pub block_len: LeU32,
    pub link_type: LeU16,
    pub reserved: LeU16,
    pub snap_len: LeU32,
    pub trailing_len: LeU32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct EpbHeader {
    pub block_type: LeU32,
    pub block_len: LeU32,
    pub interface_id: LeU32,
    pub ts_high: LeU32,
    pub ts_low: LeU32,
    pub cap_len: LeU32,
    pub orig_len: LeU32,
}

/// 20-byte Linux cooked capture v2 (`LINKTYPE_LINUX_SLL2`) header in network byte order.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Sll2Header {
    pub protocol_type: BeU16,
    pub reserved: BeU16,
    pub if_index: BeU32,
    pub arphrd_type: BeU16,
    pub pkt_type: u8,
    pub halen: u8,
    pub addr: [u8; 8],
}

pub struct PcapNgWriter {
    out: BufWriter<File>,
}

impl PcapNgWriter {
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut w = Self {
            out: BufWriter::new(file),
        };
        w.write_shb()?;
        w.write_idb()?;
        w.out.flush()?;
        Ok(w)
    }

    fn write_shb(&mut self) -> io::Result<()> {
        let len = core::mem::size_of::<ShbBlock>() as u32;
        let shb = ShbBlock {
            block_type: LeU32::new(0x0A0D_0D0A),
            block_len: LeU32::new(len),
            byte_order_magic: LeU32::new(0x1A2B_3C4D),
            major_version: LeU16::new(1),
            minor_version: LeU16::new(0),
            section_length: LeI64::new(-1),
            trailing_len: LeU32::new(len),
        };
        self.out.write_all(shb.as_bytes())
    }

    fn write_idb(&mut self) -> io::Result<()> {
        let len = core::mem::size_of::<IdbBlock>() as u32;
        let idb = IdbBlock {
            block_type: LeU32::new(0x0000_0001),
            block_len: LeU32::new(len),
            link_type: LeU16::new(LINKTYPE_LINUX_SLL2),
            reserved: LeU16::new(0),
            snap_len: LeU32::new(65535),
            trailing_len: LeU32::new(len),
        };
        self.out.write_all(idb.as_bytes())
    }

    /// Append a raw MCTP packet (`[4-byte MCTP header, payload...]`) wrapped in
    /// a 20-byte `LINKTYPE_LINUX_SLL2` (`ETH_P_MCTP`) header as an Enhanced
    /// Packet Block (type `0x00000006`).
    pub fn write_mctp_packet(&mut self, mctp_packet: &[u8]) -> io::Result<()> {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let sll2 = Sll2Header {
            protocol_type: BeU16::new(ETH_P_MCTP),
            reserved: BeU16::new(0),
            if_index: BeU32::new(1),
            arphrd_type: BeU16::new(ARPHRD_MCTP),
            pkt_type: 0,
            halen: 0,
            addr: [0u8; 8],
        };

        let total_pkt_len = core::mem::size_of::<Sll2Header>() + mctp_packet.len();
        let cap_len = total_pkt_len as u32;
        let padded_len = (total_pkt_len + 3) & !3;
        let block_len = (core::mem::size_of::<EpbHeader>() + padded_len + 4) as u32;

        let epb = EpbHeader {
            block_type: LeU32::new(0x0000_0006),
            block_len: LeU32::new(block_len),
            interface_id: LeU32::new(0),
            ts_high: LeU32::new((now_us >> 32) as u32),
            ts_low: LeU32::new((now_us & 0xFFFF_FFFF) as u32),
            cap_len: LeU32::new(cap_len),
            orig_len: LeU32::new(cap_len),
        };
        self.out.write_all(epb.as_bytes())?;
        self.out.write_all(sll2.as_bytes())?;
        self.out.write_all(mctp_packet)?;
        let pad = padded_len - total_pkt_len;
        if pad > 0 {
            self.out.write_all(&[0u8; 3][..pad])?;
        }
        self.out.write_all(LeU32::new(block_len).as_bytes())?;
        self.out.flush()
    }
}
