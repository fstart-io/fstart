//! Minimal Wireshark `pcapng` writer emitting `LINKTYPE_MCTP` (291) packets
//! so captured MCTP, MCTP Control, PLDM, and SPDM traffic can be inspected
//! directly in Wireshark / `tshark`.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wireshark / libpcap `LINKTYPE_MCTP` (official assignment: 291).
pub const LINKTYPE_MCTP: u16 = 291;

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
        self.out.write_all(&LINKTYPE_MCTP.to_le_bytes())?;
        self.out.write_all(&0u16.to_le_bytes())?; // Reserved
        self.out.write_all(&65535u32.to_le_bytes())?; // SnapLen
        self.out.write_all(&block_len.to_le_bytes())?;
        Ok(())
    }

    /// Append a raw MCTP packet (`[4-byte MCTP header, payload...]`) as an
    /// Enhanced Packet Block (type `0x00000006`).
    pub fn write_mctp_packet(&mut self, mctp_packet: &[u8]) -> io::Result<()> {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let ts_high = (now_us >> 32) as u32;
        let ts_low = (now_us & 0xFFFF_FFFF) as u32;

        let cap_len = mctp_packet.len() as u32;
        let padded_len = (mctp_packet.len() + 3) & !3;
        let block_len = (32 + padded_len) as u32;

        self.out.write_all(&0x0000_0006u32.to_le_bytes())?;
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.write_all(&0u32.to_le_bytes())?; // Interface ID 0
        self.out.write_all(&ts_high.to_le_bytes())?;
        self.out.write_all(&ts_low.to_le_bytes())?;
        self.out.write_all(&cap_len.to_le_bytes())?;
        self.out.write_all(&cap_len.to_le_bytes())?;
        self.out.write_all(mctp_packet)?;
        let pad = padded_len - mctp_packet.len();
        if pad > 0 {
            self.out.write_all(&[0u8; 3][..pad])?;
        }
        self.out.write_all(&block_len.to_le_bytes())?;
        self.out.flush()
    }
}
