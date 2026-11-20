//! The persisted training (coreboot stores `ramctr_timing` in the MRC cache
//! as `MRC_CACHE_VERSION` 6). fstart keeps only what the fast-boot path
//! reads, compactly, so it fits the shared memory-cache journal.

use super::{
    Ctrl, DimmFacts, LaneTimings, NUM_CHANNELS, NUM_LANES, NUM_SLOTRANKS, NUM_SLOTS, RankTimings,
};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

const VERSION: u32 = u32::from_le_bytes(*b"SNB1");

#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct LaneRecord {
    rcven: u16,
    rx_dqs_p: u8,
    rx_dqs_n: u8,
    tx_dq: u16,
    tx_dqs: u16,
}

#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct RankRecord {
    roundtrip_latency: u8,
    io_latency: u8,
    pi_coding: i16,
    lanes: [LaneRecord; NUM_LANES],
}

/// Fast-boot training state, keyed to the CPU and the modules' unique CRCs.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TrainingRecord {
    version: u32,
    cpu: u32,
    spd_crc: [[u16; NUM_SLOTS]; NUM_CHANNELS],
    mad_dimm: [u32; NUM_CHANNELS],
    t_ck: u16,
    t_aa: u16,
    cas_supported: u16,
    t_cmd: u16,
    t_refi: u16,
    t_rfc: u16,
    channel_size_mb: [u16; NUM_CHANNELS],
    pi_code_offset: i16,
    pi_coding_threshold: i16,
    mdll_wake_delay: u16,
    base_freq: u8,
    frq: u8,
    cas: u8,
    cwl: u8,
    t_rcd: u8,
    t_rp: u8,
    t_ras: u8,
    t_wr: u8,
    t_faw: u8,
    t_rrd: u8,
    t_rtp: u8,
    t_wtr: u8,
    t_mod: u8,
    t_xs_offset: u8,
    t_wlo: u8,
    t_cke: u8,
    t_xpdll: u8,
    t_xp: u8,
    t_aonpd: u8,
    lanes: u8,
    ecc_enabled: u8,
    extended_temperature_range: u8,
    auto_self_refresh: u8,
    rankmap: [u8; NUM_CHANNELS],
    ref_card_offset: [u8; NUM_CHANNELS],
    cmd_stretch: [u8; NUM_CHANNELS],
    rank_mirror: [u8; NUM_CHANNELS],
    /// Per DIMM: bit 7 DLL-off capable, bits 4:0 row address bits.
    dimm_flags: [[u8; NUM_SLOTS]; NUM_CHANNELS],
    dimm_size_256mb: [[u8; NUM_SLOTS]; NUM_CHANNELS],
    dimm_ranks_width: [[u8; NUM_SLOTS]; NUM_CHANNELS],
    _reserved: [u8; 3],
    timings: [[RankRecord; NUM_SLOTRANKS]; NUM_CHANNELS],
}

const _: () = assert!(core::mem::size_of::<TrainingRecord>() <= super::TRAINING_RECORD_SIZE);

fn fit<T: TryFrom<i64>>(value: impl Into<i64>) -> Option<T> {
    T::try_from(value.into()).ok()
}

impl TrainingRecord {
    /// Capture `c`; `None` when a value does not fit (never on valid training).
    pub fn capture(c: &Ctrl) -> Option<Self> {
        let rank = |t: &RankTimings| -> Option<RankRecord> {
            let mut lanes = [LaneRecord::default(); NUM_LANES];
            for (out, l) in lanes.iter_mut().zip(t.lanes.iter()) {
                *out = LaneRecord {
                    rcven: fit(l.rcven)?,
                    rx_dqs_p: fit(l.rx_dqs_p)?,
                    rx_dqs_n: fit(l.rx_dqs_n)?,
                    tx_dq: fit(l.tx_dq)?,
                    tx_dqs: fit(l.tx_dqs)?,
                };
            }
            Some(RankRecord {
                roundtrip_latency: fit(t.roundtrip_latency)?,
                io_latency: fit(t.io_latency)?,
                pi_coding: fit(t.pi_coding)?,
                lanes,
            })
        };
        let mut timings = [[RankRecord::default(); NUM_SLOTRANKS]; NUM_CHANNELS];
        for (ch, ranks) in timings.iter_mut().enumerate() {
            for (r, out) in ranks.iter_mut().enumerate() {
                *out = rank(&c.timings[ch][r])?;
            }
        }
        let per_dimm = |f: &dyn Fn(&DimmFacts) -> u8| -> [[u8; NUM_SLOTS]; NUM_CHANNELS] {
            core::array::from_fn(|ch| core::array::from_fn(|slot| f(&c.dimms[ch][slot])))
        };
        Some(Self {
            version: VERSION,
            cpu: c.cpu,
            spd_crc: c.spd_crc,
            mad_dimm: c.mad_dimm,
            t_ck: fit(c.t_ck)?,
            t_aa: fit(c.t_aa)?,
            cas_supported: fit(c.cas_supported)?,
            t_cmd: fit(c.t_cmd)?,
            t_refi: fit(c.t_refi)?,
            t_rfc: fit(c.t_rfc)?,
            channel_size_mb: [fit(c.channel_size_mb[0])?, fit(c.channel_size_mb[1])?],
            pi_code_offset: fit(c.pi_code_offset)?,
            pi_coding_threshold: fit(c.pi_coding_threshold)?,
            mdll_wake_delay: fit(c.mdll_wake_delay)?,
            base_freq: fit(c.base_freq)?,
            frq: fit(c.frq)?,
            cas: fit(c.cas)?,
            cwl: fit(c.cwl)?,
            t_rcd: fit(c.t_rcd)?,
            t_rp: fit(c.t_rp)?,
            t_ras: fit(c.t_ras)?,
            t_wr: fit(c.t_wr)?,
            t_faw: fit(c.t_faw)?,
            t_rrd: fit(c.t_rrd)?,
            t_rtp: fit(c.t_rtp)?,
            t_wtr: fit(c.t_wtr)?,
            t_mod: fit(c.t_mod)?,
            t_xs_offset: fit(c.t_xs_offset)?,
            t_wlo: fit(c.t_wlo)?,
            t_cke: fit(c.t_cke)?,
            t_xpdll: fit(c.t_xpdll)?,
            t_xp: fit(c.t_xp)?,
            t_aonpd: fit(c.t_aonpd)?,
            lanes: fit(c.lanes as i64)?,
            ecc_enabled: c.ecc_enabled.into(),
            extended_temperature_range: c.extended_temperature_range.into(),
            auto_self_refresh: c.auto_self_refresh.into(),
            rankmap: c.rankmap,
            ref_card_offset: [fit(c.ref_card_offset[0])?, fit(c.ref_card_offset[1])?],
            cmd_stretch: [fit(c.cmd_stretch[0])?, fit(c.cmd_stretch[1])?],
            rank_mirror: core::array::from_fn(|ch| {
                c.rank_mirror[ch]
                    .iter()
                    .enumerate()
                    .fold(0, |bits, (rank, m)| bits | u8::from(*m) << rank)
            }),
            dimm_flags: per_dimm(&|d| u8::from(d.dll_off_mode) << 7 | (d.row_bits & 0x1f)),
            dimm_size_256mb: per_dimm(&|d| (d.size_mb / 256).min(255) as u8),
            dimm_ranks_width: per_dimm(&|d| d.ranks << 4 | (d.width / 4)),
            _reserved: [0; 3],
            timings,
        })
    }

    /// Parse a journaled record for `cpu` and the current module CRCs.
    pub fn parse(
        bytes: &[u8],
        cpu: u32,
        spd_crc: &[[u16; NUM_SLOTS]; NUM_CHANNELS],
    ) -> Option<Ctrl> {
        let (record, _) = Self::read_from_prefix(bytes).ok()?;
        if record.version != VERSION {
            return None;
        }
        if record.cpu != cpu {
            fstart_log::info!(
                "sandybridge: CPUID {:#x} differs from stored {:#x}",
                cpu,
                record.cpu
            );
            return None;
        }
        if record.spd_crc != *spd_crc {
            fstart_log::info!("sandybridge: stored timings CRC16 mismatch");
            return None;
        }
        Some(record.restore())
    }

    fn restore(&self) -> Ctrl {
        let mut c = Ctrl {
            cpu: self.cpu,
            spd_crc: self.spd_crc,
            mad_dimm: self.mad_dimm,
            t_ck: self.t_ck.into(),
            t_aa: self.t_aa.into(),
            cas_supported: self.cas_supported.into(),
            t_cmd: self.t_cmd.into(),
            t_refi: self.t_refi.into(),
            t_rfc: self.t_rfc.into(),
            channel_size_mb: self.channel_size_mb.map(u32::from),
            pi_code_offset: self.pi_code_offset.into(),
            pi_coding_threshold: self.pi_coding_threshold.into(),
            mdll_wake_delay: self.mdll_wake_delay.into(),
            base_freq: self.base_freq.into(),
            frq: self.frq.into(),
            cas: self.cas.into(),
            cwl: self.cwl.into(),
            t_rcd: self.t_rcd.into(),
            t_rp: self.t_rp.into(),
            t_ras: self.t_ras.into(),
            t_wr: self.t_wr.into(),
            t_faw: self.t_faw.into(),
            t_rrd: self.t_rrd.into(),
            t_rtp: self.t_rtp.into(),
            t_wtr: self.t_wtr.into(),
            t_mod: self.t_mod.into(),
            t_xs_offset: self.t_xs_offset.into(),
            t_wlo: self.t_wlo.into(),
            t_cke: self.t_cke.into(),
            t_xpdll: self.t_xpdll.into(),
            t_xp: self.t_xp.into(),
            t_aonpd: self.t_aonpd.into(),
            lanes: self.lanes.into(),
            ecc_enabled: self.ecc_enabled != 0,
            extended_temperature_range: self.extended_temperature_range != 0,
            auto_self_refresh: self.auto_self_refresh != 0,
            rankmap: self.rankmap,
            ref_card_offset: self.ref_card_offset.map(u32::from),
            cmd_stretch: self.cmd_stretch.map(u32::from),
            ..Ctrl::default()
        };
        for ch in 0..NUM_CHANNELS {
            for rank in 0..NUM_SLOTRANKS {
                c.rank_mirror[ch][rank] = self.rank_mirror[ch] & (1 << rank) != 0;
                let r = &self.timings[ch][rank];
                let t = &mut c.timings[ch][rank];
                t.roundtrip_latency = r.roundtrip_latency.into();
                t.io_latency = r.io_latency.into();
                t.pi_coding = r.pi_coding.into();
                for (out, l) in t.lanes.iter_mut().zip(r.lanes.iter()) {
                    *out = LaneTimings {
                        rcven: l.rcven.into(),
                        rx_dqs_p: l.rx_dqs_p.into(),
                        rx_dqs_n: l.rx_dqs_n.into(),
                        tx_dq: l.tx_dq.into(),
                        tx_dqs: l.tx_dqs.into(),
                    };
                }
            }
            for slot in 0..NUM_SLOTS {
                let flags = self.dimm_flags[ch][slot];
                let rw = self.dimm_ranks_width[ch][slot];
                c.dimms[ch][slot] = DimmFacts {
                    size_mb: u32::from(self.dimm_size_256mb[ch][slot]) * 256,
                    ranks: rw >> 4,
                    width: (rw & 0xf) * 4,
                    row_bits: flags & 0x1f,
                    dll_off_mode: flags & 0x80 != 0,
                };
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips_the_fast_boot_state() {
        let mut c = Ctrl {
            cpu: 0x206a7,
            t_ck: 384,
            t_aa: 3328,
            cas: 9,
            cwl: 7,
            frq: 5,
            base_freq: 133,
            lanes: 8,
            rankmap: [0x3, 0x1],
            mad_dimm: [0x0066_0010, 0x0060_0008],
            channel_size_mb: [4096, 2048],
            pi_code_offset: 10,
            ..Ctrl::default()
        };
        c.rank_mirror[0][1] = true;
        c.timings[0][1].pi_coding = -12;
        c.timings[0][1].lanes[3] = LaneTimings {
            rcven: 100,
            rx_dqs_p: 30,
            rx_dqs_n: 31,
            tx_dq: 90,
            tx_dqs: 200,
        };
        c.dimms[0][0] = DimmFacts {
            size_mb: 4096,
            ranks: 2,
            width: 8,
            row_bits: 15,
            dll_off_mode: true,
        };
        c.spd_crc = [[0x1234, 0], [0x5678, 0]];
        let record = TrainingRecord::capture(&c).unwrap();
        let bytes = record.as_bytes();
        assert!(TrainingRecord::parse(bytes, 0x206a7, &[[0x1234, 0], [0x5678, 1]]).is_none());
        assert!(TrainingRecord::parse(bytes, 0x306a9, &c.spd_crc).is_none());
        let back = TrainingRecord::parse(bytes, 0x206a7, &c.spd_crc).unwrap();
        assert_eq!(back.timings, c.timings);
        assert_eq!(back.dimms, c.dimms);
        assert_eq!(back.rank_mirror, c.rank_mirror);
        assert_eq!(back.mad_dimm, c.mad_dimm);
        assert_eq!(back.t_aa, c.t_aa);
    }
}
