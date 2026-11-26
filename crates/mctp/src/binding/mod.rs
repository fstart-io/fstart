//! Physical transport binding codecs for MCTP.
//!
//! All five bindings are always built:
//! - [`serial`] — MCTP over Serial (DSP0253)
//! - [`smbus`] — MCTP over SMBus / I2C (DSP0237)
//! - [`i3c`] — MCTP over I3C (DSP0233)
//! - [`kcs`] — MCTP over KCS / LPC (DSP0254)
//! - [`pcie_vdm`] — MCTP over PCIe VDM (DSP0238)

pub mod i3c;
pub mod kcs;
pub mod pcie_vdm;
pub mod serial;
pub mod smbus;

/// Identifies which physical transport binding carried a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Serial,
    Smbus,
    I3c,
    Kcs,
    PcieVdm,
}

/// Transport-specific physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysAddr {
    Serial,
    /// 7-bit SMBus / I2C target address.
    Smbus(u8),
    /// 7-bit I3C dynamic address.
    I3c(u8),
    Kcs,
    /// PCIe Bus/Device/Function (16-bit BDF) and routing mode.
    PcieVdm {
        bdf: u16,
        routing: pcie_vdm::PcieRouting,
    },
}

/// SMBus / I3C / KCS Packet Error Code: CRC-8 with polynomial `x^8 + x^2 + x + 1` (`0x07`).
pub fn crc8_smbus(data: &[u8]) -> u8 {
    crc8_smbus_update(0x00, data)
}

/// Incremental CRC-8 (`0x07`) update.
pub fn crc8_smbus_update(mut crc: u8, data: &[u8]) -> u8 {
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if (crc & 0x80) != 0 {
                crc = (crc << 1) ^ 0x07;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// DSP0253 / PPP-style FCS-16 (CRC-16/CCITT-FALSE reflected, poly `0x8408`,
/// init `0xFFFF`, no final inversion), matching Linux `mctp-serial` and
/// `mctp-estack`.
pub fn fcs16_serial(data: &[u8]) -> u16 {
    fcs16_serial_update(0xFFFF, data)
}

/// Incremental FCS-16 update.
pub fn fcs16_serial_update(mut fcs: u16, data: &[u8]) -> u16 {
    for &byte in data {
        fcs ^= u16::from(byte);
        for _ in 0..8 {
            if (fcs & 1) != 0 {
                fcs = (fcs >> 1) ^ 0x8408;
            } else {
                fcs >>= 1;
            }
        }
    }
    fcs
}
