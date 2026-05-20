//! Pure DDR3 timing selection helpers for Sandy Bridge native raminit.
//!
//! Values follow coreboot `raminit_native.c`: tCK is represented in units of
//! 1/256 ns (`TCK_800MHZ = 320`, etc.), CAS support bits are indexed from CL4,
//! and X220 first-pass support is Sandy Bridge 133 MHz reference only.

use fstart_services::ServiceError;
use fstart_spd::ddr3::Ddr3DimmInfo;

const MIN_CAS: u8 = 4;
const MAX_CAS: u8 = 18;
const BASE_FREQ_MHZ: u16 = 133;

pub const TCK_1066MHZ: u32 = 240;
pub const TCK_933MHZ: u32 = 274;
pub const TCK_800MHZ: u32 = 320;
pub const TCK_666MHZ: u32 = 384;
pub const TCK_533MHZ: u32 = 480;
pub const TCK_400MHZ: u32 = 640;

/// Selected Sandy Bridge DDR3 timing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimingParams {
    pub tck_256ns: u32,
    pub base_freq_mhz: u16,
    pub frq: u32,
    pub cas: u8,
    pub cwl: u8,
    pub trcd: u32,
    pub trp: u32,
    pub tras: u32,
    pub twr: u32,
    pub tfaw: u32,
    pub trrd: u32,
    pub trtp: u32,
    pub twtr: u32,
    pub trfc: u32,
    pub trefi: u32,
    pub tmod: u32,
    pub txs_offset: u32,
    pub twlo: u32,
    pub tcke: u32,
    pub txpdll: u32,
    pub txp: u32,
    pub taonpd: u32,
    pub mdll_wake_delay: u16,
}

impl TimingParams {
    pub fn mem_clock_mhz(&self) -> u16 {
        ((1000u32 << 8) / self.tck_256ns) as u16
    }
}

/// Compute common X220/Sandy DDR3 timings from all populated DIMMs.
pub fn select_timings(
    dimms: impl Iterator<Item = Ddr3DimmInfo>,
    max_mem_clock_mhz: u16,
) -> Result<TimingParams, ServiceError> {
    let mut any = false;
    let mut tck = max_mem_clock_to_tck(max_mem_clock_mhz);
    let mut taa = 0u32;
    let mut trcd = 0u32;
    let mut trp = 0u32;
    let mut tras = 0u32;
    let mut twr = 0u32;
    let mut tfaw = 0u32;
    let mut trrd = 0u32;
    let mut trtp = 0u32;
    let mut twtr = 0u32;
    let mut trfc = 0u32;
    let mut cas_supported = 0xffffu16;

    for dimm in dimms {
        any = true;
        tck = tck.max(ps_to_tck256(dimm.tck_min_ps));
        taa = taa.max(ps_to_tck256(dimm.taa_min_ps));
        trcd = trcd.max(ps_to_tck256(dimm.trcd_min_ps));
        trp = trp.max(ps_to_tck256(dimm.trp_min_ps));
        tras = tras.max(ps_to_tck256(dimm.tras_min_ps));
        twr = twr.max(ps_to_tck256(dimm.twr_min_ps));
        tfaw = tfaw.max(ps_to_tck256(dimm.tfaw_min_ps));
        trrd = trrd.max(ps_to_tck256(dimm.trrd_min_ps));
        trtp = trtp.max(ps_to_tck256(dimm.trtp_min_ps));
        twtr = twtr.max(ps_to_tck256(dimm.twtr_min_ps));
        trfc = trfc.max(ps_to_tck256(dimm.trfc_min_ps));
        cas_supported &= dimm.cas_latencies;
    }

    if !any || cas_supported == 0 {
        return Err(ServiceError::HardwareError);
    }

    let (tck, cas) = find_cas_tck(tck, taa, cas_supported)?;
    let frq = (256_000 / (tck * u32::from(BASE_FREQ_MHZ))).clamp(3, 8);
    let table_idx = (frq - 3) as usize;
    Ok(TimingParams {
        tck_256ns: tck,
        base_freq_mhz: BASE_FREQ_MHZ,
        frq,
        cas,
        cwl: cwl_for_tck(tck),
        trcd: div_round_up(trcd, tck),
        trp: div_round_up(trp, tck),
        tras: div_round_up(tras, tck),
        twr: div_round_up(twr, tck),
        tfaw: div_round_up(tfaw, tck),
        trrd: div_round_up(trrd, tck),
        trtp: div_round_up(trtp, tck),
        twtr: div_round_up(twtr, tck),
        trfc: div_round_up(trfc, tck),
        trefi: FRQ_REFI_133[table_idx],
        tmod: u32::from(FRQ_MOD_133[table_idx]),
        txs_offset: u32::from(FRQ_XS_133[table_idx]),
        twlo: u32::from(FRQ_WLO_133[table_idx]),
        tcke: u32::from(FRQ_CKE_133[table_idx]),
        txpdll: u32::from(FRQ_XPDLL_133[table_idx]),
        txp: u32::from(FRQ_XP_133[table_idx]),
        taonpd: u32::from(FRQ_AONPD_133[table_idx]),
        mdll_wake_delay: ((128_000 / tck) + 3) as u16,
    })
}

fn max_mem_clock_to_tck(max_mem_clock_mhz: u16) -> u32 {
    if max_mem_clock_mhz >= 1066 {
        TCK_1066MHZ
    } else if max_mem_clock_mhz >= 933 {
        TCK_933MHZ
    } else if max_mem_clock_mhz >= 800 {
        TCK_800MHZ
    } else if max_mem_clock_mhz >= 666 {
        TCK_666MHZ
    } else if max_mem_clock_mhz >= 533 {
        TCK_533MHZ
    } else {
        TCK_400MHZ
    }
}

fn normalize_tck_133mhz(tck: u32) -> Option<u32> {
    if tck <= TCK_1066MHZ {
        Some(TCK_1066MHZ)
    } else if tck <= TCK_933MHZ {
        Some(TCK_933MHZ)
    } else if tck <= TCK_800MHZ {
        Some(TCK_800MHZ)
    } else if tck <= TCK_666MHZ {
        Some(TCK_666MHZ)
    } else if tck <= TCK_533MHZ {
        Some(TCK_533MHZ)
    } else if tck <= TCK_400MHZ {
        Some(TCK_400MHZ)
    } else {
        None
    }
}

fn find_cas_tck(mut tck: u32, taa: u32, cas_supported: u16) -> Result<(u32, u8), ServiceError> {
    loop {
        let Some(norm_tck) = normalize_tck_133mhz(tck) else {
            return Err(ServiceError::NotSupported);
        };
        tck = norm_tck;
        let start_cas = div_round_up(taa, tck).max(u32::from(MIN_CAS)) as u8;
        for cas in start_cas..=MAX_CAS {
            if (cas_supported & (1 << (cas - MIN_CAS))) != 0 {
                return Ok((tck, cas));
            }
        }
        tck += 1;
    }
}

const FRQ_REFI_133: [u32; 8] = [3120, 4160, 5200, 6240, 7280, 8320, 9360, 10400];
const FRQ_XS_133: [u8; 8] = [4, 6, 7, 8, 10, 11, 12, 14];
const FRQ_MOD_133: [u8; 8] = [12, 12, 12, 12, 15, 16, 18, 20];
const FRQ_WLO_133: [u8; 8] = [4, 5, 6, 6, 8, 8, 9, 10];
const FRQ_CKE_133: [u8; 8] = [3, 3, 4, 4, 5, 6, 6, 7];
const FRQ_XPDLL_133: [u8; 8] = [10, 13, 16, 20, 23, 26, 29, 32];
const FRQ_XP_133: [u8; 8] = [3, 4, 4, 5, 6, 7, 8, 8];
const FRQ_AONPD_133: [u8; 8] = [4, 5, 6, 8, 8, 10, 11, 12];

fn cwl_for_tck(tck: u32) -> u8 {
    match tck {
        TCK_1066MHZ => 10,
        TCK_933MHZ => 9,
        TCK_800MHZ => 8,
        TCK_666MHZ => 7,
        TCK_533MHZ => 6,
        _ => 5,
    }
}

#[inline]
fn ps_to_tck256(ps: u32) -> u32 {
    (ps * 256).div_ceil(1000)
}

#[inline]
const fn div_round_up(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x220_cap_selects_ddr3_1333_cl9() {
        let dimm = Ddr3DimmInfo {
            ranks: 2,
            rows: 15,
            cols: 10,
            banks: 8,
            device_width_bits: 8,
            bus_width_bits: 64,
            rank_capacity_mb: 2048,
            module_capacity_mb: 4096,
            cas_latencies: 0x01fe, // CL5..CL12
            tck_min_ps: 1250,
            taa_min_ps: 13_125,
            twr_min_ps: 15_000,
            trcd_min_ps: 13_125,
            trrd_min_ps: 6_000,
            trp_min_ps: 13_125,
            tras_min_ps: 36_000,
            trc_min_ps: 49_125,
            trfc_min_ps: 260_000,
            twtr_min_ps: 7_500,
            trtp_min_ps: 7_500,
            tfaw_min_ps: 30_000,
            supports_1_5v: true,
            supports_auto_self_refresh: true,
            rank1_mirrored: false,
        };
        let timing = select_timings([dimm].into_iter(), 666).unwrap();
        assert_eq!(timing.tck_256ns, TCK_666MHZ);
        assert_eq!(timing.mem_clock_mhz(), 666);
        assert_eq!(timing.cas, 9);
        assert_eq!(timing.cwl, 7);
    }
}
