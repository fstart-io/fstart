// SPDX-License-Identifier: MIT OR Apache-2.0
//! The performance of CXL memory as AMD's fabric DXE gives it to the HMAT
//! and the SLIT (AmdFabricWhDxe `FabricWhAcpiHmatInstall` `0x9D30`,
//! `FabricHmatGetCxlCdatPerf` `0xC3F0`, PCDs `PcdAmdFabricSlitCxl*`;
//! reports/research/CXL_COMPLETE.md §2.3).
//!
//! A device's latency and bandwidth come from its CDAT's DSLBIS for the
//! DSMAS of its region (or handle `0xFF`), with defaults where it has
//! none. A window's (a CXL domain's) are the worst latency and the
//! smallest bandwidth times the devices, halved; an initiator adds the
//! fabric's latency to it, more from the other socket.

use fstart_pci::cxl::cdat::{Dslbis, data};

/// Latency of a device without CDAT latency, picoseconds.
pub const DEFAULT_LATENCY_PS: u64 = 400_000;
/// Bandwidth of a device without CDAT bandwidth, MB/s.
pub const DEFAULT_BANDWIDTH: u64 = 10_000;
/// What the fabric adds to a CXL target's latency from its own socket.
pub const LOCAL_SOCKET_PS: u64 = 110_000;
/// ... and from the other socket (`0x36EE8`).
pub const REMOTE_SOCKET_PS: u64 = 225_000;
/// The HMAT entries' base unit (the values are scaled into 16 bits by it).
pub const BASE_UNIT: u64 = 10;
/// The SLIT distance of a CXL domain from its socket's CPUs
/// (`PcdAmdFabricSlitCxlLocalDistance` 0x12) and from the other socket's
/// (`...RemoteDistance` 0x1C), Dell's build values.
pub const SLIT_LOCAL: u8 = 18;
pub const SLIT_REMOTE: u8 = 28;
/// The handle a DSLBIS applies to every DSMAS with.
pub const ANY_HANDLE: u8 = 0xff;

/// Latency (ps) and bandwidth (MB/s), read and write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perf {
    pub read_latency: u64,
    pub write_latency: u64,
    pub read_bandwidth: u64,
    pub write_bandwidth: u64,
}

impl Perf {
    /// The defaults of a device without CDAT.
    pub const DEFAULT: Self = Self {
        read_latency: DEFAULT_LATENCY_PS,
        write_latency: DEFAULT_LATENCY_PS,
        read_bandwidth: DEFAULT_BANDWIDTH,
        write_bandwidth: DEFAULT_BANDWIDTH,
    };

    /// Reads and writes perform alike: the HMAT gives access values.
    pub fn symmetric(&self) -> bool {
        self.read_latency == self.write_latency && self.read_bandwidth == self.write_bandwidth
    }
}

/// A device's performance from its CDAT's `dslbis` for DSMAS `handle`.
pub fn device(dslbis: impl Iterator<Item = Dslbis>, handle: u8) -> Perf {
    let (mut access_l, mut read_l, mut write_l) = (None, None, None);
    let (mut access_b, mut read_b, mut write_b) = (None, None, None);
    for d in dslbis.filter(|d| d.handle == handle || d.handle == ANY_HANDLE) {
        let v = Some(d.value());
        match d.data_type {
            data::ACCESS_LATENCY => access_l = v,
            data::READ_LATENCY => read_l = v,
            data::WRITE_LATENCY => write_l = v,
            data::ACCESS_BANDWIDTH => access_b = v,
            data::READ_BANDWIDTH => read_b = v,
            data::WRITE_BANDWIDTH => write_b = v,
            _ => {}
        }
    }
    let latency = access_l.unwrap_or(DEFAULT_LATENCY_PS);
    let bandwidth = access_b.unwrap_or(DEFAULT_BANDWIDTH);
    Perf {
        read_latency: read_l.unwrap_or(latency),
        write_latency: write_l.unwrap_or(latency),
        read_bandwidth: read_b.unwrap_or(bandwidth),
        write_bandwidth: write_b.unwrap_or(bandwidth),
    }
}

/// A CXL domain's performance from its devices': the worst latency, the
/// smallest bandwidth times the device count, halved.
pub fn domain(devices: &[Perf]) -> Perf {
    if devices.is_empty() {
        return Perf::DEFAULT;
    }
    let n = devices.len() as u64;
    let max = |f: fn(&Perf) -> u64| devices.iter().map(f).max().unwrap_or(0);
    let min = |f: fn(&Perf) -> u64| devices.iter().map(f).min().unwrap_or(0);
    Perf {
        read_latency: max(|p| p.read_latency),
        write_latency: max(|p| p.write_latency),
        read_bandwidth: min(|p| p.read_bandwidth).saturating_mul(n) / 2,
        write_bandwidth: min(|p| p.write_bandwidth).saturating_mul(n) / 2,
    }
}

/// What an initiator sees of a CXL domain `perf`: the fabric's latency
/// added (its own socket or the other one) and, across sockets, at most
/// `cross_socket` bandwidth (half the xGMI bandwidth, per link when there
/// are several; `None` when not known).
pub fn from_initiator(perf: &Perf, same_socket: bool, cross_socket: Option<u64>) -> Perf {
    let add = if same_socket {
        LOCAL_SOCKET_PS
    } else {
        REMOTE_SOCKET_PS
    };
    let cap = |bw: u64| match (same_socket, cross_socket) {
        (false, Some(limit)) => bw.min(limit),
        _ => bw,
    };
    Perf {
        read_latency: perf.read_latency.saturating_add(add),
        write_latency: perf.write_latency.saturating_add(add),
        read_bandwidth: cap(perf.read_bandwidth),
        write_bandwidth: cap(perf.write_bandwidth),
    }
}

/// A value as an HMAT entry in [`BASE_UNIT`]s (0 means "not provided", so
/// a value is at least 1; 0xFFFF is "unreachable", so at most 0xFFFE).
pub fn entry(value: u64) -> u16 {
    (value / BASE_UNIT).clamp(1, 0xfffe) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dslbis(handle: u8, data_type: u8, value: u16) -> Dslbis {
        Dslbis {
            handle,
            flags: 0,
            data_type,
            base_unit: 1000,
            entries: [value, 0, 0],
        }
    }

    #[test]
    fn cdat_values_defaults_and_domains() {
        let table = [
            dslbis(0, data::READ_LATENCY, 150),
            dslbis(0, data::ACCESS_BANDWIDTH, 30),
            dslbis(1, data::ACCESS_LATENCY, 999),
            dslbis(ANY_HANDLE, data::WRITE_LATENCY, 200),
        ];
        let p = device(table.iter().copied(), 0);
        assert_eq!(
            p,
            Perf {
                read_latency: 150_000,
                write_latency: 200_000,
                read_bandwidth: 30_000,
                write_bandwidth: 30_000
            }
        );
        assert_eq!(device(core::iter::empty(), 0), Perf::DEFAULT);
        let d = domain(&[p, Perf::DEFAULT]);
        assert_eq!(d.read_latency, 400_000);
        // min(30000, 10000) * 2 / 2.
        assert_eq!(d.read_bandwidth, 10_000);
        let local = from_initiator(&d, true, Some(1000));
        assert_eq!(
            (local.read_latency, local.read_bandwidth),
            (510_000, 10_000)
        );
        let remote = from_initiator(&d, false, Some(1000));
        assert_eq!(
            (remote.read_latency, remote.read_bandwidth),
            (625_000, 1000)
        );
        assert_eq!(entry(625_000), 62_500);
        assert_eq!(entry(0), 1);
        assert_eq!(entry(u64::MAX), 0xfffe);
        assert!(Perf::DEFAULT.symmetric());
    }
}
