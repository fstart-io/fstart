//! Minimal Wireshark `pcapng` writer emitting `LINKTYPE_LINUX_SLL2` (276)
//! frames with `ETH_P_MCTP` (`0x00FA`), matching OpenBMC `libmctp` and
//! Wireshark's built-in MCTP / PLDM dissectors.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// `LINKTYPE_LINUX_SLL2` (276): Linux cooked capture v2, which Wireshark uses
/// to dispatch `ETH_P_MCTP` (`0x00FA`) packets to the MCTP dissector.
pub const LINKTYPE_LINUX_SLL2: u16 = 276;
/// Linux `ETH_P_MCTP` protocol number (big-endian in SLL2 header).
const ETH_P_MCTP: u16 = 0x00FA;
/// Linux `ARPHRD_MCTP` hardware type.
const ARPHRD_MCTP: u16 = 290;
/// SLL2 header size in bytes.
const SLL2_HEADER_LEN: usize = 20;

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
        // Section Header Block (type 0x0A0D0D0A, 28 bytes total)
        let block_len: u32 = 28;
        self.out.write_all(&0x0A0D_0D0Au32.to_le_bytes())?;
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.write_all(&0x1A2B_3C4Du32.to_le_bytes())?; // Byte-Order Magic
        self.out.write_all(&1u16.to_le_bytes())?; // Major version
        self.out.write_all(&0u16.to_le_bytes())?; // Minor version
        self.out.write_all(&(-1i64).to_le_bytes())?; // Section length unspecified
        self.out.write_all(&block_len.to_le_bytes())?;
        Ok(())
    }

    fn write_idb(&mut self) -> io::Result<()> {
        // Interface Description Block (type 0x00000001, 20 bytes total)
        let block_len: u32 = 20;
        self.out.write_all(&0x0000_0001u32.to_le_bytes())?;
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.write_all(&LINKTYPE_LINUX_SLL2.to_le_bytes())?;
        self.out.write_all(&0u16.to_le_bytes())?; // Reserved
        self.out.write_all(&65535u32.to_le_bytes())?; // SnapLen
        self.out.write_all(&block_len.to_le_bytes())?;
        Ok(())
    }

    /// Append a raw MCTP packet (`[4-byte MCTP header, payload...]`) wrapped in
    /// a 20-byte `LINKTYPE_LINUX_SLL2` (`ETH_P_MCTP`) header as an Enhanced
    /// Packet Block (type `0x00000006`).
    pub fn write_mctp_packet(&mut self, mctp_packet: &[u8]) -> io::Result<()> {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let ts_high = (now_us >> 32) as u32;
        let ts_low = (now_us & 0xFFFF_FFFF) as u32;

        // SLL2 header (20 bytes, network byte order):
        // [protocol_type(2) = 0x00FA, reserved(2), if_index(4)=1, arphrd(2)=290,
        //  pkt_type(1)=0, halen(1)=0, addr(8)=0]
        let mut sll2 = [0u8; SLL2_HEADER_LEN];
        sll2[0..2].copy_from_slice(&ETH_P_MCTP.to_be_bytes());
        sll2[4..8].copy_from_slice(&1u32.to_be_bytes());
        sll2[8..10].copy_from_slice(&ARPHRD_MCTP.to_be_bytes());

        let total_pkt_len = SLL2_HEADER_LEN + mctp_packet.len();
        let cap_len = total_pkt_len as u32;
        let padded_len = (total_pkt_len + 3) & !3;
        let block_len = (32 + padded_len) as u32;

        self.out.write_all(&0x0000_0006u32.to_le_bytes())?;
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.write_all(&0u32.to_le_bytes())?; // Interface ID 0
        self.out.write_all(&ts_high.to_le_bytes())?;
        self.out.write_all(&ts_low.to_le_bytes())?;
        self.out.write_all(&cap_len.to_le_bytes())?;
        self.out.write_all(&cap_len.to_le_bytes())?;
        self.out.write_all(&sll2)?;
        self.out.write_all(mctp_packet)?;
        let pad = padded_len - total_pkt_len;
        if pad > 0 {
            self.out.write_all(&[0u8; 3][..pad])?;
        }
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.flush()
    }
}
