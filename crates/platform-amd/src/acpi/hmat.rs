// SPDX-License-Identifier: MIT OR Apache-2.0
//! The HMAT as AMD's fabric DXE builds it for DRAM (`FabricWhAcpiHmatInstall`,
//! Dell AmdFabricWhDxe `0x9d30`, GIGABYTE `0x5e74`; 2P_COMPLETE §2.13): the
//! domains with CPUs are the initiators, the domains with DRAM (and, for
//! the SoC that adds them, CXL ones) the targets; from an initiator a
//! target's latency grows by the fabric's, more from the other socket, and
//! its bandwidth is capped by half the xGMI bandwidth across sockets. The
//! structures and their scaling are `fstart_acpi::hmat`'s.

extern crate alloc;

use alloc::vec::Vec;

pub use fstart_acpi::hmat::Performance;

/// What the fabric adds to a DRAM target's latency from its own socket, ps
/// (both donors).
pub const LOCAL_LATENCY_PS: u64 = 110_000;
/// ... and from the other socket (`0x36EE8`).
pub const REMOTE_LATENCY_PS: u64 = 225_000;

/// The DRAM bandwidth of a domain, MB/s: three quarters of `channels`
/// channels of 8 bytes at `mts` MT/s, the slowest DIMM's rate of the
/// domain's channels (AmdFabricWhDxe `0xc060`/`0xc1f0`).
pub const fn dram_bandwidth(mts: u32, channels: u32) -> u64 {
    mts as u64 * channels as u64 * 8 * 3 / 4
}

/// Half the xGMI bandwidth, MB/s, of links running at `links` Mbps each
/// (`HalfxGMIBandwidth`, `0xc2f0`: 16 lanes, bits to bytes, halved).
pub fn half_xgmi(links: impl IntoIterator<Item = u32>) -> u64 {
    links.into_iter().map(u64::from).sum::<u64>() * 16 / 8 / 2
}

/// A target: a domain with memory on `socket`, its own latency (0 for
/// DRAM) and bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub domain: u32,
    pub socket: u8,
    pub perf: Performance,
}

/// A DRAM target of `domain` on `socket` with `channels` populated
/// channels whose slowest DIMM runs at `mts` MT/s.
pub const fn dram(domain: u32, socket: u8, mts: u32, channels: u32) -> Target {
    let bandwidth = dram_bandwidth(mts, channels);
    Target {
        domain,
        socket,
        perf: Performance {
            read_latency: 0,
            write_latency: 0,
            read_bandwidth: bandwidth,
            write_bandwidth: bandwidth,
        },
    }
}

/// What an initiator on `socket` sees of `target`: the fabric's latency
/// added, across sockets the bandwidth at most `half_xgmi` (when known).
pub fn seen(socket: u8, target: &Target, half_xgmi: Option<u64>) -> Performance {
    let (add, cap) = if socket == target.socket {
        (LOCAL_LATENCY_PS, u64::MAX)
    } else {
        (REMOTE_LATENCY_PS, half_xgmi.unwrap_or(u64::MAX))
    };
    let p = target.perf;
    Performance {
        read_latency: p.read_latency + add,
        write_latency: p.write_latency + add,
        read_bandwidth: p.read_bandwidth.min(cap),
        write_bandwidth: p.write_bandwidth.min(cap),
    }
}

/// The HMAT of `initiators` (`(domain, socket)` of the domains with CPUs)
/// and `targets`, or `None` without either.
pub fn hmat(
    initiators: &[(u32, u8)],
    targets: &[Target],
    half_xgmi: Option<u64>,
) -> Option<Vec<u8>> {
    if initiators.is_empty() || targets.is_empty() {
        return None;
    }
    let ids: Vec<u32> = initiators.iter().map(|i| i.0).collect();
    let target_ids: Vec<u32> = targets.iter().map(|t| t.domain).collect();
    let entries = fstart_acpi::hmat::locality(&ids, &target_ids, |i, t| {
        let socket = initiators.iter().find(|x| x.0 == i)?.1;
        let target = targets.iter().find(|x| x.domain == t)?;
        Some(seen(socket, target, half_xgmi))
    });
    fstart_acpi::hmat::build_hmat(&entries).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2P NPS1 DDR5-6400, 16 channels each, 4 xGMI links at 32 Gbps.
    #[test]
    fn two_sockets_of_dram() {
        let bw = dram_bandwidth(6400, 16);
        assert_eq!(bw, 614_400);
        let half = half_xgmi([32_000; 4]);
        assert_eq!(half, 128_000);
        let targets = [dram(0, 0, 6400, 16), dram(1, 1, 6400, 16)];
        let local = seen(0, &targets[0], Some(half));
        let remote = seen(0, &targets[1], Some(half));
        assert_eq!((local.read_latency, local.read_bandwidth), (110_000, bw));
        assert_eq!(
            (remote.read_latency, remote.read_bandwidth),
            (225_000, half)
        );
        let table = hmat(&[(0, 0), (1, 1)], &targets, Some(half)).unwrap();
        assert_eq!(&table[..4], b"HMAT");
        // Two MPDA structures (40 bytes each), then two 2x2 matrices (32 +
        // 4 domains + 4 entries, padded: 56 bytes each).
        assert_eq!(table.len(), 40 + 2 * 40 + 2 * 56);
        let latency = &table[120..176];
        // Access latency, in units of 10 ps: 11000 local, 22500 remote.
        assert_eq!(latency[9], 0);
        assert_eq!(&latency[24..32], &10u64.to_le_bytes());
        let entry = |m: &[u8], k: usize| u16::from_le_bytes([m[48 + 2 * k], m[49 + 2 * k]]);
        assert_eq!(
            (0..4).map(|k| entry(latency, k)).collect::<Vec<_>>(),
            [11_000, 22_500, 22_500, 11_000]
        );
        // Bandwidth in MB/s / 10: the remote one capped at half xGMI.
        let bandwidth = &table[176..232];
        assert_eq!(bandwidth[9], 3);
        assert_eq!(
            (0..4).map(|k| entry(bandwidth, k)).collect::<Vec<_>>(),
            [61_440, 12_800, 12_800, 61_440]
        );
        assert!(hmat(&[], &targets, None).is_none());
    }
}
