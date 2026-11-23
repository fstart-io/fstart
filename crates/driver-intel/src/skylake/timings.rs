// SPDX-License-Identifier: MIT OR Apache-2.0
//! Standard-profile DDR4 SPD timings and PLL selection, translated from
//! coreboot native/spd_timings.c. All times ending in `_fs` are femtoseconds;
//! `clock_khz` is DCLK, half the DDR data rate. No XMP or SA-GV is selected.

use super::{
    Error,
    cpu::{Cpu, Family, Package},
    spd::Dimms,
};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    CapidA [ DDR_OVERCLOCK OFFSET(3) NUMBITS(1) [] ],
    CapidC [ DMFC_DDR4 OFFSET(17) NUMBITS(3) [] ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceClock {
    Mhz133,
    Mhz100,
}
impl ReferenceClock {
    const fn index(self) -> u8 {
        match self {
            Self::Mhz133 => 0,
            Self::Mhz100 => 1,
        }
    }
}

/// Fused standard-profile limit, including the pre-N0 KBL DT/Halo 2DPC
/// restriction. CAPID0_A/0_C are PCI 00:00.0 offsets e4/ec.
pub fn fused_tck_min(cpu: Cpu, capid_a: u32, capid_c: u32, two_dimms_per_channel: bool) -> u32 {
    const TCK: [u32; 8] = [
        2500000, 1875000, 1500000, 1250000, 1071429, 938000, 833333, 750000,
    ];
    let a = LocalRegisterCopy::<u32, CapidA::Register>::new(capid_a);
    let c = LocalRegisterCopy::<u32, CapidC::Register>::new(capid_c);
    let dmfc = if a.is_set(CapidA::DDR_OVERCLOCK) {
        0
    } else {
        c.read(CapidC::DMFC_DDR4)
    };
    let mut ratio = if dmfc == 0 { 10 } else { 11 - dmfc };
    if cpu.family == Family::KabyLake
        && cpu.package == Package::DesktopHalo
        && cpu.model() == 0x906e0
        && cpu.stepping_id() < 10
        && dmfc != 0
        && ratio == 10
        && two_dimms_per_channel
    {
        ratio -= 1;
    }
    TCK[(ratio - 3) as usize]
}

// Flags: 133 normal, 100 normal, 133 odd, 100 odd, in bits 0..3.
const FREQUENCIES: &[(u32, u8)] = &[
    (2500000, 3),
    (2000000, 2),
    (1875000, 1),
    (1818182, 8),
    (1666667, 6),
    (1538462, 8),
    (1500000, 1),
    (1428571, 2),
    (1363636, 4),
    (1333333, 8),
    (1250000, 3),
    (1176471, 8),
    (1153846, 4),
    (1111111, 2),
    (1071429, 1),
    (1052632, 8),
    (1000000, 6),
    (952381, 8),
    (938000, 1),
    (909091, 2),
    (882353, 4),
    (869565, 8),
    (833333, 3),
    (800000, 8),
    (789474, 4),
    (769231, 2),
    (750000, 1),
    (740741, 8),
    (714286, 6),
    (689655, 8),
    (681818, 1),
    (666667, 2),
    (652174, 4),
    (645161, 8),
    (625000, 3),
    (606061, 8),
    (600000, 4),
    (588235, 2),
    (576923, 1),
    (571429, 8),
    (555556, 6),
    (540541, 8),
    (535714, 1),
    (526316, 2),
    (517241, 4),
    (512821, 8),
    (500000, 3),
    (487805, 8),
    (483871, 4),
    (476190, 2),
    (468750, 1),
    (465116, 8),
    (454545, 6),
    (444444, 8),
    (441176, 1),
    (434783, 2),
    (428571, 4),
    (425532, 8),
    (416667, 3),
    (408163, 8),
    (405405, 4),
    (400000, 2),
    (394737, 1),
    (392157, 8),
    (384615, 6),
    (377358, 8),
    (375000, 1),
    (370370, 2),
    (365854, 4),
    (363636, 8),
    (357143, 3),
    (350877, 8),
    (348837, 4),
    (344828, 2),
    (340909, 1),
    (338983, 8),
    (333333, 6),
    (327869, 8),
    (326087, 1),
    (322581, 2),
    (319149, 4),
    (317460, 8),
    (312500, 1),
    (306122, 4),
    (300000, 1),
    (294118, 4),
    (288462, 1),
    (283019, 4),
    (277778, 1),
    (272727, 4),
    (267857, 1),
    (263158, 4),
    (258621, 1),
    (254237, 4),
    (250000, 1),
    (245902, 4),
    (241935, 1),
    (238095, 4),
];

/// Source `spd_timings.c:tck_to_freq_enum` training-policy bins. These are
/// neither the full PLL table index nor a rounded measurement in MT/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum FrequencyBin {
    Mts800 = 800,
    Mts1067 = 1067,
    Mts1333 = 1333,
    Mts1600 = 1600,
    Mts1867 = 1867,
    Mts2133 = 2133,
    Mts2400 = 2400,
    Mts2667 = 2667,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub tck_fs: u32,
    pub reference: ReferenceClock,
    pub odd_ratio: bool,
    index: usize,
}
impl Clock {
    /// Source policy uses thresholds in tCK, not `data_rate_mts()`. In
    /// particular the supported 2000-MT/s PLL point remains in FREQ_1867.
    pub const fn frequency_bin(self) -> FrequencyBin {
        match self.tck_fs {
            ..=750_000 => FrequencyBin::Mts2667,
            750_001..=833_333 => FrequencyBin::Mts2400,
            833_334..=938_000 => FrequencyBin::Mts2133,
            938_001..=1_071_429 => FrequencyBin::Mts1867,
            1_071_430..=1_250_000 => FrequencyBin::Mts1600,
            1_250_001..=1_500_000 => FrequencyBin::Mts1333,
            1_500_001..=1_875_000 => FrequencyBin::Mts1067,
            _ => FrequencyBin::Mts800,
        }
    }

    pub const fn clock_khz(self) -> u32 {
        (1_000_000_000_000u64 / self.tck_fs as u64) as u32
    }
    /// For inventory and policy. Do not publish until the PLL has accepted it.
    pub const fn data_rate_mts(self) -> u32 {
        self.clock_khz() / 500
    }
}

/// Source PLL band selection uses nearest-ps comparisons, not exact-fs
/// comparisons (notably DDR4-2400). Reject slower-than-table requests instead
/// of inheriting the source's fall-through to its fastest entry.
pub fn select_clock(tck_min_fs: u32, reference: ReferenceClock) -> Result<Clock, Error> {
    let requested_ps = (u64::from(tck_min_fs) + 500) / 1000;
    if tck_min_fs == 0 || requested_ps > 2500 {
        return Err(Error::ClockTooSlow);
    }
    let start = FREQUENCIES
        .iter()
        .rposition(|(tck, _)| (u64::from(*tck) + 500) / 1000 >= requested_ps)
        .ok_or(Error::ClockTooSlow)?;
    for index in (0..=start).rev() {
        let (tck_fs, flags) = FREQUENCIES[index];
        let normal = flags & (1 << reference.index()) != 0;
        let odd = flags & (4 << reference.index()) != 0;
        if normal || odd {
            return Ok(Clock {
                tck_fs,
                reference,
                odd_ratio: !normal,
                index,
            });
        }
    }
    Err(Error::ClockTooSlow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    pub clock: Clock,
    pub cas: u16,
    pub cwl: u16,
    pub trcd: u16,
    pub trp: u16,
    pub tras: u16,
    pub trc: u16,
    pub tfaw: u16,
    pub trefi: u16,
    pub trfc: u16,
    pub trfc2: u16,
    pub trfc4: u16,
    pub trrd_s: u16,
    pub trrd_l: u16,
    pub twr: u16,
    pub trtp: u16,
    pub twtr_l: u16,
    pub twtr_s: u16,
}

fn time_fs(mtb: u32, fine: u8) -> Result<u32, Error> {
    let fs = i64::from(mtb) * 125000 + i64::from(fine as i8) * 1000;
    if fs <= 0 {
        return Err(Error::InvalidTiming);
    }
    u32::try_from(fs).map_err(|_| Error::InvalidTiming)
}
fn clocks(fs: u32, tck: u32, rounding_per_mille: u32) -> Result<u16, Error> {
    let adjusted = fs
        .checked_sub(tck * rounding_per_mille / 1000)
        .ok_or(Error::InvalidTiming)?;
    let value = adjusted.div_ceil(tck);
    if value == 0 {
        return Err(Error::InvalidTiming);
    }
    u16::try_from(value).map_err(|_| Error::InvalidTiming)
}
fn cwl(tck: u32, cas: u16) -> u16 {
    let cwl2400 = if cas >= 16 { 16 } else { 14 };
    let cwl1600 = if cas >= 11 { 11 } else { 9 };
    match tck {
        ..=769231 => 18,
        769232..=833333 => cwl2400,
        833334..=938000 => 14,
        938001..=1071429 => 12,
        1071430..=1250000 => cwl1600,
        _ => 9,
    }
}
fn merge(target: &mut u16, value: u16) {
    *target = (*target).max(value);
}

/// Compute worst-case timing over present DIMMs and intersect CAS masks.
/// The caller supplies the fused clock floor; it is never inferred from SPD.
/// An empty topology is an error, not successful memory initialization.
pub fn decode(dimms: &Dimms<'_>, fused_tck_min_fs: u32) -> Result<Timings, Error> {
    let mut tck_min = fused_tck_min_fs;
    let mut tck_max = u32::MAX;
    let mut taa_max = 0;
    let mut mask = 0x3ffff;
    let mut count = 0;
    for dimm in dimms.iter().flatten().flatten() {
        count += 1;
        let dimm_min = time_fs(dimm.raw[18].into(), dimm.raw[125])?;
        let dimm_max = time_fs(dimm.raw[19].into(), dimm.raw[124])?;
        if dimm_min > dimm_max {
            return Err(Error::InvalidTiming);
        }
        tck_min = tck_min.max(dimm_min);
        tck_max = tck_max.min(dimm_max);
        taa_max = taa_max.max(time_fs(dimm.raw[24].into(), dimm.raw[123])?);
        mask &= u32::from_le_bytes(dimm.raw[20..24].try_into().unwrap()) & 0x3ffff;
    }
    if count == 0 {
        return Err(Error::NoMemory);
    }
    if mask == 0 {
        return Err(Error::NoCasLatency);
    }
    let mut clock = select_clock(tck_min, ReferenceClock::Mhz133)?;
    let initial_odd = clock.odd_ratio;
    let mut rounding = 25;
    let cas = loop {
        // DDR4 SPD bytes 19/124 bound the slowest legal clock too. Check
        // both initial PLL quantization and each CAS-driven downgrade.
        if clock.tck_fs > tck_max {
            return Err(Error::ClockOutsideDimmRange);
        }
        let minimum = clocks(taa_max, clock.tck_fs, rounding)?.max(7);
        if let Some(cas) = (minimum..=24).find(|cas| {
            *cas != 23
                && u64::from(*cas) * u64::from(clock.tck_fs) <= 20_000_000
                && mask & (1 << (*cas - 7)) != 0
        }) {
            break cas;
        }
        let slower = (0..clock.index)
            .rev()
            .find(|i| {
                let flags = FREQUENCIES[*i].1;
                flags & 1 != 0 || (initial_odd && flags & 4 != 0)
            })
            .ok_or(Error::NoCasLatency)?;
        clock = select_clock(FREQUENCIES[slower].0, ReferenceClock::Mhz133)?;
        rounding = 10;
    };
    let tck = clock.tck_fs;
    let cwl = cwl(tck, cas);
    // The source timing register expressions subtract CWL from CL unsigned.
    // Reject an invalid combination rather than programming wrapped values.
    if cas < cwl {
        return Err(Error::InvalidTiming);
    }
    let mut twr = clocks(15_000_000, tck, 10)?;
    twr = (twr + (twr & 1)).clamp(10, 24);
    if twr == 22 {
        twr = 24;
    }
    let mut trtp = clocks(7_500_000, tck, 10)?.clamp(5, 12);
    if trtp == 11 {
        trtp = 12;
    }
    let mut result = Timings {
        clock,
        cas,
        cwl,
        trcd: 0,
        trp: 0,
        tras: 0,
        trc: 0,
        tfaw: 0,
        trefi: u16::try_from(7_800_000u32.div_ceil(tck / 1000))
            .map_err(|_| Error::InvalidTiming)?,
        trfc: 0,
        trfc2: 0,
        trfc4: 0,
        trrd_s: 0,
        trrd_l: 0,
        twr,
        trtp,
        twtr_l: clocks(7_500_000, tck, 10)?,
        twtr_s: clocks(2_500_000, tck, 10)?,
    };
    for dimm in dimms.iter().flatten().flatten() {
        let spd = dimm.raw;
        let timing = |mtb: u32, fine: u8| clocks(time_fs(mtb, fine)?, tck, 10);
        merge(&mut result.trcd, timing(spd[25].into(), spd[122])?);
        merge(&mut result.trp, timing(spd[26].into(), spd[121])?);
        merge(
            &mut result.tras,
            timing((u32::from(spd[27] & 0xf) << 8) | u32::from(spd[28]), 0)?,
        );
        merge(
            &mut result.trc,
            timing(
                (u32::from(spd[27] >> 4) << 8) | u32::from(spd[29]),
                spd[120],
            )?,
        );
        merge(
            &mut result.tfaw,
            timing((u32::from(spd[36] & 0xf) << 8) | u32::from(spd[37]), 0)?,
        );
        // Unlike packed tRAS/tRC, tRFC1/2/4 are full 16-bit MTB fields
        // (JEDEC DDR4 SPD bytes 30..35).
        let rfc = |lo: usize| u32::from(u16::from_le_bytes([spd[lo], spd[lo + 1]]));
        merge(&mut result.trfc, timing(rfc(30), 0)?);
        if rfc(32) != 0 {
            merge(&mut result.trfc2, timing(rfc(32), 0)?);
        }
        if rfc(34) != 0 {
            merge(&mut result.trfc4, timing(rfc(34), 0)?);
        }
        merge(&mut result.trrd_s, timing(spd[38].into(), spd[119])?);
        merge(&mut result.trrd_l, timing(spd[39].into(), spd[118])?);
    }
    result.trp = result.trp.max(result.trcd);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::spd::{
        Dimm,
        testing::{seal, sodimm},
    };
    use super::*;
    #[test]
    fn spd_timings_2133_and_half_rate_units() {
        let raw = sodimm();
        let dimm = Dimm::decode(&raw).unwrap();
        let t = decode(&[[Some(dimm), None], [Some(dimm), None]], 938000).unwrap();
        assert_eq!(
            (t.clock.tck_fs, t.clock.clock_khz(), t.clock.data_rate_mts()),
            (938000, 1066098, 2132)
        );
        assert_eq!(
            (t.cas, t.cwl, t.trcd, t.trp, t.tras, t.trfc),
            (15, 14, 15, 15, 36, 374)
        );
        assert_eq!((t.twr, t.trtp, t.trrd_s, t.trrd_l), (16, 8, 5, 7));
        assert_eq!(
            (t.trfc2, t.trfc4, t.twtr_s, t.twtr_l, t.trefi),
            (278, 171, 3, 8, 8316)
        );
    }
    #[test]
    fn training_frequency_bins_follow_source_tck_thresholds_not_raw_mts() {
        use FrequencyBin::*;
        let mut clock = select_clock(2_500_000, ReferenceClock::Mhz133).unwrap();
        for (tck_fs, expected) in [
            (750_000, Mts2667),
            (750_001, Mts2400),
            (833_333, Mts2400),
            (833_334, Mts2133),
            (938_000, Mts2133),
            (938_001, Mts1867),
            (1_000_000, Mts1867),
            (1_071_429, Mts1867),
            (1_071_430, Mts1600),
            (1_250_000, Mts1600),
            (1_250_001, Mts1333),
            (1_500_000, Mts1333),
            (1_500_001, Mts1067),
            (1_875_000, Mts1067),
            (1_875_001, Mts800),
            (2_500_000, Mts800),
        ] {
            clock.tck_fs = tck_fs;
            assert_eq!(clock.frequency_bin(), expected, "source tCK={tck_fs}");
        }
        let intermediate = select_clock(1_000_000, ReferenceClock::Mhz100).unwrap();
        assert_eq!(intermediate.data_rate_mts(), 2000);
        assert_eq!(intermediate.frequency_bin(), Mts1867);
    }

    #[test]
    fn pll_table_reference_compatibility_and_boundaries() {
        assert_eq!(
            select_clock(833000, ReferenceClock::Mhz133).unwrap().tck_fs,
            833333
        );
        assert!(
            select_clock(1_000_000, ReferenceClock::Mhz133)
                .unwrap()
                .odd_ratio
        );
        assert!(
            !select_clock(1_000_000, ReferenceClock::Mhz100)
                .unwrap()
                .odd_ratio
        );
        assert_eq!(
            select_clock(3_000_000, ReferenceClock::Mhz133),
            Err(Error::ClockTooSlow)
        );
        for reference in [ReferenceClock::Mhz133, ReferenceClock::Mhz100] {
            for &(tck, _) in FREQUENCIES {
                let selected = select_clock(tck, reference).unwrap();
                let flags = FREQUENCIES[selected.index].1;
                let bit = if selected.odd_ratio { 4 } else { 1 };
                assert_ne!(flags & (bit << reference.index()), 0);
                assert!(selected.tck_fs >= tck);
            }
        }
    }
    #[test]
    fn capid_fuse_and_kbl_pre_n0_2dpc_restriction() {
        let b0 = Cpu::from_signature(0x906e9).unwrap();
        let n0 = Cpu::from_signature(0x906ea).unwrap();
        assert_eq!(fused_tck_min(b0, 0, 1 << 17, false), 750000);
        assert_eq!(fused_tck_min(b0, 0, 1 << 17, true), 833333);
        assert_eq!(fused_tck_min(n0, 0, 1 << 17, true), 750000);
        assert_eq!(fused_tck_min(b0, 8, 7 << 17, true), 750000);
        for dmfc in 0..8 {
            assert!(fused_tck_min(n0, 0, dmfc << 17, false) >= 750000);
        }
    }
    #[test]
    fn cas_intersection_and_frequency_downgrade() {
        let raw = sodimm();
        let mut slow = raw;
        slow[20..24].copy_from_slice(&(1u32 << (14 - 7)).to_le_bytes());
        seal(&mut slow);
        let fast = Dimm::decode(&raw).unwrap();
        let slow = Dimm::decode(&slow).unwrap();
        let t = decode(&[[Some(fast), None], [Some(slow), None]], 938000).unwrap();
        assert_eq!((t.clock.tck_fs, t.cas), (1071429, 14));
        let mut disjoint = raw;
        disjoint[20..24].copy_from_slice(&1u32.to_le_bytes());
        seal(&mut disjoint);
        assert_eq!(
            decode(
                &[
                    [Some(fast), None],
                    [Some(Dimm::decode(&disjoint).unwrap()), None]
                ],
                938000
            ),
            Err(Error::NoCasLatency)
        );
        assert_eq!(decode(&[[None; 2]; 2], 938000), Err(Error::NoMemory));
    }
    #[test]
    fn spd_tck_max_bounds_initial_selection_and_cas_fallback() {
        let mut raw = sodimm();
        raw[19] = 8;
        raw[124] = (-62i8) as u8; // 938000 fs, exactly the first PLL bin
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        assert!(decode(&[[Some(d), None], [None; 2]], 938000).is_ok());
        assert_eq!(
            decode(&[[Some(d), None], [None; 2]], 1_000_000),
            Err(Error::ClockOutsideDimmRange)
        );
        // Positive fine offset permits the exact fallback boundary.
        raw[19] = 8;
        raw[124] = 72; // 1072000 fs
        raw[20..24].copy_from_slice(&(1u32 << (14 - 7)).to_le_bytes());
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        assert_eq!(
            decode(&[[Some(d), None], [None; 2]], 938000)
                .unwrap()
                .clock
                .tck_fs,
            1071429
        );
        raw[124] = 71; // 1071000 fs: fallback must not exceed this
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        assert_eq!(
            decode(&[[Some(d), None], [None; 2]], 938000),
            Err(Error::ClockOutsideDimmRange)
        );
        raw[124] = 0; // review reproduction: max 1 ns, CL14 only
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        assert_eq!(
            decode(&[[Some(d), None], [None; 2]], 938000),
            Err(Error::ClockOutsideDimmRange)
        );
    }
    #[test]
    fn spd_tck_max_uses_smallest_populated_dimm_and_rejects_invalid_range() {
        let raw = sodimm();
        let mut limited = raw;
        limited[19] = 8;
        limited[124] = (-63i8) as u8; // below this DIMM's tCKmin
        seal(&mut limited);
        assert_eq!(
            decode(
                &[[Some(Dimm::decode(&limited).unwrap()), None], [None; 2]],
                938000
            ),
            Err(Error::InvalidTiming)
        );
        limited[124] = 0;
        seal(&mut limited);
        assert_eq!(
            decode(
                &[
                    [Some(Dimm::decode(&raw).unwrap()), None],
                    [None, Some(Dimm::decode(&limited).unwrap())]
                ],
                1071429
            ),
            Err(Error::ClockOutsideDimmRange)
        );
    }
    #[test]
    fn worst_case_timings_signed_fine_offsets_and_invalid_timing() {
        let raw = sodimm();
        let mut second = raw;
        second[25] = 120;
        second[122] = (-1i8) as u8;
        seal(&mut second);
        let a = Dimm::decode(&raw).unwrap();
        let b = Dimm::decode(&second).unwrap();
        let t = decode(&[[Some(a), Some(b)], [None, None]], 938000).unwrap();
        assert_eq!((t.trcd, t.trp), (16, 16));
        second[25] = 0;
        second[122] = 255;
        seal(&mut second);
        assert_eq!(
            decode(
                &[[Some(Dimm::decode(&second).unwrap()), None], [None, None]],
                938000
            ),
            Err(Error::InvalidTiming)
        );
    }
}
