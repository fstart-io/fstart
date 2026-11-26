//! Training cache record for Bay Trail native DDR3 S3 resume.
//!
//! Captures the trained per-strobe delays (`rcvn`, `wdqs`, `wdq`, `rdq`) and
//! the exact geometry/memory-map contract so S3 self-refresh exit replays only
//! verified parameters matching the live SPD, fused frequency, stolen memory,
//! and TXE UMA reservation.

use super::delay::HALF_CLK;
use super::{MemoryMap, Params, STROBES, Stolen};
use fstart_core::services::ServiceError;
use sha2::{Digest, Sha256};
use zerocopy::byteorder::little_endian::{I32, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

const MAGIC: [u8; 8] = *b"BYTMRC01";
const VERSION: u32 = 1;

/// Maximum linear delay representable by a 4-bit 2x counter + 6-bit PI code.
const MAX_LINEAR_DELAY: i32 = 15 * HALF_CLK + (HALF_CLK - 1);
/// Read DQS PI is a 6-bit code (`0..64`).
const MAX_RDQS_PI: i32 = 63;

/// Fixed-size binary payload persisted in the platform's MRC cache partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TrainingCache {
    magic: [u8; 8],
    version: U32,
    spd_digest: [u8; 32],
    freq: u8,
    cas_latency: u8,
    write_latency: u8,
    ranks: u8,
    tom_mb: U32,
    bmbound_mb: U32,
    txe_base_mb: U32,
    txe_mb: U32,
    gsm_base_mb: U32,
    gsm_mb: U32,
    gtt_base_mb: U32,
    gtt_mb: U32,
    tseg_base_mb: U32,
    tseg_mb: U32,
    rcvn: [I32; STROBES],
    wdqs: [I32; STROBES],
    wdq: [I32; STROBES],
    rdq: [I32; STROBES],
}

/// Serialized size of [`TrainingCache`] in bytes.
pub const CACHE_BYTES: usize = core::mem::size_of::<TrainingCache>();

fn spd_sha256(spd: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(spd);
    hasher.finalize().into()
}

fn encode_strobes(values: [i32; STROBES]) -> [I32; STROBES] {
    core::array::from_fn(|i| I32::new(values[i]))
}

fn decode_strobes(values: [I32; STROBES], max: i32) -> Result<[i32; STROBES], ServiceError> {
    let mut out = [0i32; STROBES];
    for (dst, src) in out.iter_mut().zip(values) {
        let v = src.get();
        if !(0..=max).contains(&v) {
            return Err(ServiceError::InvalidParam);
        }
        *dst = v;
    }
    Ok(out)
}

impl TrainingCache {
    /// Capture trained delays and memory map from a cold boot.
    pub fn capture(spd: &[u8], p: &Params, stolen: Stolen, map: &MemoryMap) -> Self {
        Self {
            magic: MAGIC,
            version: U32::new(VERSION),
            spd_digest: spd_sha256(spd),
            freq: p.freq as u8,
            cas_latency: p.cas_latency as u8,
            write_latency: p.write_latency as u8,
            ranks: p.ranks as u8,
            tom_mb: U32::new(map.tom_mb),
            bmbound_mb: U32::new(map.bmbound_mb),
            txe_base_mb: U32::new(map.txe_base_mb),
            txe_mb: U32::new(map.txe_mb),
            gsm_base_mb: U32::new(map.gsm_base_mb),
            gsm_mb: U32::new(stolen.gsm_mb),
            gtt_base_mb: U32::new(map.gtt_base_mb),
            gtt_mb: U32::new(stolen.gtt_mb),
            tseg_base_mb: U32::new(map.tseg_base_mb),
            tseg_mb: U32::new(map.tseg_mb),
            rcvn: encode_strobes(p.rcvn),
            wdqs: encode_strobes(p.wdqs),
            wdq: encode_strobes(p.wdq),
            rdq: encode_strobes(p.rdq),
        }
    }

    /// Raw wire representation for capture into the platform MRC cache.
    pub fn as_bytes(&self) -> &[u8] {
        <Self as IntoBytes>::as_bytes(self)
    }

    /// Validate `bytes` against the live SPD, derived timings, and expected
    /// memory map, populating `p`'s trained delays on success.
    pub fn decode(
        bytes: &[u8],
        spd: &[u8],
        stolen: Stolen,
        expected_map: &MemoryMap,
        p: &mut Params,
    ) -> Result<Self, ServiceError> {
        let record = Self::read_from_bytes(bytes).map_err(|_| ServiceError::InvalidParam)?;
        if record.magic != MAGIC || record.version.get() != VERSION {
            fstart_log::error!("baytrail: training cache header mismatch");
            return Err(ServiceError::InvalidParam);
        }
        if record.spd_digest != spd_sha256(spd) {
            fstart_log::error!("baytrail: training cache SPD digest mismatch");
            return Err(ServiceError::InvalidParam);
        }
        if record.freq != p.freq as u8
            || u32::from(record.cas_latency) != p.cas_latency
            || u32::from(record.write_latency) != p.write_latency
            || u32::from(record.ranks) != p.ranks
        {
            fstart_log::error!("baytrail: training cache timing/rank mismatch");
            return Err(ServiceError::InvalidParam);
        }
        let cached_map = MemoryMap {
            tom_mb: record.tom_mb.get(),
            bmbound_mb: record.bmbound_mb.get(),
            txe_base_mb: record.txe_base_mb.get(),
            txe_mb: record.txe_mb.get(),
            gsm_base_mb: record.gsm_base_mb.get(),
            gtt_base_mb: record.gtt_base_mb.get(),
            tseg_base_mb: record.tseg_base_mb.get(),
            tseg_mb: record.tseg_mb.get(),
        };
        if cached_map != *expected_map
            || record.gsm_mb.get() != stolen.gsm_mb
            || record.gtt_mb.get() != stolen.gtt_mb
        {
            fstart_log::error!("baytrail: training cache memory-map mismatch");
            return Err(ServiceError::InvalidParam);
        }
        p.rcvn = decode_strobes(record.rcvn, MAX_LINEAR_DELAY)?;
        p.wdqs = decode_strobes(record.wdqs, MAX_LINEAR_DELAY)?;
        p.wdq = decode_strobes(record.wdq, MAX_LINEAR_DELAY)?;
        p.rdq = decode_strobes(record.rdq, MAX_RDQS_PI)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baytrail::raminit::{DdrFreq, SPD_2GIB, mc};

    fn sample() -> (Params, Stolen, MemoryMap) {
        let mut p = Params::new(&SPD_2GIB, DdrFreq::Ddr1333).expect("SPD decodes");
        p.rcvn = [260, 262, 264, 266, 268, 270, 272, 274];
        p.wdqs = [160, 162, 164, 166, 168, 170, 172, 174];
        p.wdq = [128, 130, 132, 134, 136, 138, 140, 142];
        p.rdq = [28, 29, 30, 31, 32, 33, 34, 35];
        let stolen = Stolen {
            gsm_mb: 64,
            gtt_mb: 2,
        };
        let map = mc::compute_memory_map(p.dimm.size_mb(), stolen, 16, 8);
        (p, stolen, map)
    }

    #[test]
    fn roundtrip_and_reject_mismatches() {
        assert_eq!(CACHE_BYTES, 216);
        let (p, stolen, map) = sample();
        let captured = TrainingCache::capture(&SPD_2GIB, &p, stolen, &map);
        let bytes = captured.as_bytes();
        assert_eq!(bytes.len(), CACHE_BYTES);

        let mut restored = Params::new(&SPD_2GIB, DdrFreq::Ddr1333).unwrap();
        let decoded = TrainingCache::decode(bytes, &SPD_2GIB, stolen, &map, &mut restored).unwrap();
        assert_eq!(decoded, captured);
        assert_eq!(restored.rcvn, p.rcvn);
        assert_eq!(restored.wdqs, p.wdqs);
        assert_eq!(restored.wdq, p.wdq);
        assert_eq!(restored.rdq, p.rdq);

        // Truncated slice rejected.
        assert!(
            TrainingCache::decode(
                &bytes[..CACHE_BYTES - 1],
                &SPD_2GIB,
                stolen,
                &map,
                &mut restored
            )
            .is_err()
        );

        // Bad magic rejected.
        let mut bad_magic = bytes.to_vec();
        bad_magic[0] ^= 0xff;
        assert!(TrainingCache::decode(&bad_magic, &SPD_2GIB, stolen, &map, &mut restored).is_err());

        // Different SPD rejected.
        let mut other_spd = SPD_2GIB;
        other_spd[255] ^= 1;
        assert!(TrainingCache::decode(bytes, &other_spd, stolen, &map, &mut restored).is_err());

        // Different TXE / memory map rejected.
        let other_map = mc::compute_memory_map(p.dimm.size_mb(), stolen, 0, 8);
        assert!(
            TrainingCache::decode(bytes, &SPD_2GIB, stolen, &other_map, &mut restored).is_err()
        );

        // Out-of-range RDQS PI rejected.
        let mut bad_delay = captured;
        bad_delay.rdq[0] = I32::new(64);
        assert!(
            TrainingCache::decode(bad_delay.as_bytes(), &SPD_2GIB, stolen, &map, &mut restored)
                .is_err()
        );
    }
}
