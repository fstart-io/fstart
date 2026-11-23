//! SPD collection and common parameters (`spd_bitmunching.c`).

use super::{Error, PCI_CAPID0_A};
use crate::generic::spd::ddr3::read_spd;
use fstart_core::services::SmBus;
use fstart_driver_spd::ddr3::{self, Ddr3DimmInfo};

/// Each slot is independent; the board resolves any population straps before
/// calling the driver. SMBus addresses use the 7-bit fstart convention.
#[derive(Clone, Copy, Debug)]
pub enum Slot<'a> {
    Empty,
    MemoryDown(&'a [u8; 256]),
    SmBus(u8),
}

#[derive(Debug)]
pub struct Topology {
    pub dimms: [[Option<Ddr3DimmInfo>; 2]; 2],
    /// Physical slots wired on the board, including SMBus sockets that NAK.
    pub wired: [[bool; 2]; 2],
    pub rankmap: [u8; 2],
    pub mirrored: [u8; 2],
    pub dpc: [u8; 2],
    pub channel_size_mib: [u32; 2],
    pub lanes: u8,
    /// Intersected SPD byte 31 (thermal and refresh options).
    pub thermal: u8,
    /// Bit n indicates support for CAS n (shared SPD decoder convention).
    pub cas: u32,
}

impl Topology {
    pub fn channels(&self) -> impl Iterator<Item = usize> + '_ {
        (0..2).filter(|&ch| self.dpc[ch] != 0)
    }
    pub fn populated_ranks(&self, ch: usize) -> impl Iterator<Item = usize> + '_ {
        (0..4).filter(move |&rank| self.rankmap[ch] & (1 << rank) != 0)
    }
    pub fn dimms(&self) -> impl Iterator<Item = &Ddr3DimmInfo> {
        self.dimms.iter().flatten().flatten()
    }
}

pub(super) fn channel_capacity_mib(capid_a: u32) -> u32 {
    let two_dpc = [16384, 8192, 2048, 512][PCI_CAPID0_A::DDRSZ.read(capid_a) as usize];
    if PCI_CAPID0_A::DDPCD.is_set(capid_a) {
        two_dpc / 2
    } else {
        two_dpc
    }
}

pub(super) fn crc(buf: &[u8; 256]) -> u16 {
    fstart_driver_spd::crc16(&buf[..if buf[0] & 0x80 == 0 { 126 } else { 117 }])
}

/// `ult` selects the board's 1.35 V (ULT) or 1.5 V (traditional) rail.
/// `capid_a` is the host bridge CAPID0_A dword at 0xe4. An all-blob board
/// need not initialize SMBus. Keep the raw payloads in caller-owned
/// CAR for the MRC cache and SMBIOS; empty slots are always zeroed.
pub fn collect(
    slots: [[Slot<'_>; 2]; 2],
    mut bus: Option<&mut dyn SmBus>,
    capid_a: u32,
    ult: bool,
    raw: &mut [[[u8; 256]; 2]; 2],
) -> Result<Topology, Error> {
    let mut t = Topology {
        dimms: [[None; 2]; 2],
        wired: slots.map(|ch| ch.map(|s| !matches!(s, Slot::Empty))),
        rankmap: [0; 2],
        mirrored: [0; 2],
        dpc: [0; 2],
        channel_size_mib: [0; 2],
        lanes: 8,
        thermal: 0xff,
        cas: u32::MAX,
    };
    let mut all_ecc = true;
    for (ch, channel) in slots.iter().enumerate() {
        for (slot, src) in channel.iter().enumerate() {
            let buf = &mut raw[ch][slot];
            buf.fill(0);
            match src {
                Slot::Empty => continue,
                Slot::MemoryDown(blob) => buf.copy_from_slice(&blob[..]),
                Slot::SmBus(addr) => {
                    if *addr == 0 || *addr >= 128 {
                        return Err(Error::UnsupportedMemory);
                    }
                    let bus = bus
                        .as_deref_mut()
                        .ok_or(Error::Bus(fstart_core::services::ServiceError::NoDevice))?;
                    let Some(payload) = read_spd(bus, *addr).map_err(Error::Bus)? else {
                        continue;
                    };
                    *buf = payload;
                }
            }
            if crc(buf) != u16::from_le_bytes([buf[126], buf[127]]) {
                fstart_log::error!("Haswell: SPD CRC mismatch on CH{} slot {}", ch, slot);
                return Err(Error::SpdCrc);
            }
            let d = ddr3::decode_dimm(buf).map_err(Error::Spd)?;
            // Byte 6 bit 0 is inverted: 0 means operable at 1.5 V.
            let volts = buf[ddr3::SPD_NOMINAL_VOLTAGE as usize];
            if (ult && volts & 2 == 0) || (!ult && volts & 1 != 0) {
                return Err(Error::UnsupportedMemory);
            }
            // Each slot has exactly two CS pins, the scrub visits eight
            // banks and REUT has eight column bits in cache-line units.
            // Never silently truncate an unsupported geometry or train an
            // RDIMM as an unbuffered module.
            use ddr3::Ddr3ModuleType as Module;
            if !(1..=2).contains(&d.ranks)
                || d.primary_bus_width_bits != 64
                || d.banks != 8
                || d.cols > 11
                || d.size_mb() / 256 > 255
                || !d.size_mb().is_multiple_of(256)
                || !matches!(
                    d.module_type,
                    Module::Udimm
                        | Module::Sodimm
                        | Module::SoUdimm72b
                        | Module::MiniUdimm
                        | Module::MicroDimm
                )
                || !matches!(
                    d.width,
                    fstart_driver_spd::ChipWidth::X8 | fstart_driver_spd::ChipWidth::X16
                )
            {
                return Err(Error::UnsupportedMemory);
            }
            // The shared decoder sizes the rank from row/column addressing.
            // The native MRC sizes it from component density and bus width;
            // an inconsistent pair cannot safely drive both MAD and scrub.
            let rank_mib = (32u32 << d.density_code) * u32::from(d.primary_bus_width_bits)
                / u32::from(d.width.bits());
            if d.rank_capacity_mb != rank_mib {
                return Err(Error::UnsupportedMemory);
            }
            t.dpc[ch] += 1;
            t.channel_size_mib[ch] += d.size_mb();
            t.rankmap[ch] |= ((1 << d.ranks) - 1) << (slot * 2);
            t.mirrored[ch] |= u8::from(d.mirrored) << (slot * 2 + 1);
            t.cas &= d.cas_latencies;
            t.thermal &= buf[31];
            all_ecc &= d.bus_width_bits == 72;
            t.dimms[ch][slot] = Some(d);
        }
    }
    if t.dpc == [0; 2] {
        return Err(Error::NoMemory);
    }
    if (PCI_CAPID0_A::PDCD.is_set(capid_a) && t.dpc[1] != 0)
        || (PCI_CAPID0_A::DDPCD.is_set(capid_a) && t.dpc.contains(&2))
        || t.channel_size_mib
            .iter()
            .any(|&size| size > channel_capacity_mib(capid_a))
    {
        return Err(Error::UnsupportedMemory);
    }
    let forced = PCI_CAPID0_A::FDEE.is_set(capid_a);
    if t.cas == 0 || (forced && !all_ecc) {
        return Err(Error::UnsupportedMemory);
    }
    if all_ecc && (forced || !PCI_CAPID0_A::ECCDIS.is_set(capid_a)) {
        t.lanes = 9;
    }
    Ok(t)
}
