//! GM45 pre-training DDR3 discovery and timing selection.
//!
//! This is only the first phase of coreboot's `collect_dimm_config()` and
//! `find_common_clock_cas()`. It deliberately cannot enable DRAM: the clock,
//! RCOMP, JEDEC, receive-enable and read/write training phases have not been
//! ported. In particular, probing a DIMM must never imply usable memory.

use crate::generic::spd::{DDR3, SPD_MEMORY_TYPE, ddr3};
use fstart_core::services::{ServiceError, SmBus};

/// Coreboot GM45 indexes memory clocks by their physical MHz, not transfer rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemClock {
    Mhz333,
    Mhz400,
    Mhz533,
}

impl MemClock {
    pub(super) const fn mhz(self) -> u32 {
        match self {
            Self::Mhz333 => 333,
            Self::Mhz400 => 400,
            Self::Mhz533 => 533,
        }
    }
}

/// DDR3 selection inputs; both channels must agree on the same clock and CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ddr3Timing {
    pub clock: MemClock,
    pub cas: u8,
    pub tck_256ns: u32,
    pub tras: u32,
    pub trp: u32,
    pub trcd: u32,
    pub twr: u32,
    pub trfc: u32,
    pub trd: u32,
    pub trrd: u32,
    pub tfaw: u32,
    pub twl: u32,
}

/// One DIMM per channel: coreboot's GM45 channel programming cannot represent
/// an additional module in slot 1 without a separate topology implementation.
#[derive(Debug, Clone, Copy)]
pub struct Ddr3Channels {
    pub dimms: [Option<ddr3::Ddr3DimmInfo>; 2],
}

/// Read full SPD only for installed slots; reject unknown types and extra
/// slots rather than accepting the wrong topology and silently omitting RAM.
pub fn probe_ddr3<B: SmBus>(
    bus: &mut B,
    addresses: &[u8; 4],
) -> Result<Ddr3Channels, ServiceError> {
    let mut dimms = [None; 2];
    for (slot, &addr) in addresses.iter().enumerate() {
        if addr == 0 {
            continue;
        }
        let mem_type = match bus.read_byte(addr, SPD_MEMORY_TYPE) {
            Ok(mem_type) => mem_type,
            Err(_) => continue, // An unpopulated optional slot does not respond.
        };
        // A responding but unknown SPD is not an empty slot.
        if mem_type != DDR3 {
            return Err(ServiceError::HardwareError);
        }
        if slot & 1 != 0 {
            return Err(ServiceError::HardwareError);
        }
        let spd = ddr3::read_spd(bus, addr)?.ok_or(ServiceError::HardwareError)?;
        dimms[slot / 2] =
            Some(ddr3::decode_dimm_gm45(&spd).map_err(|_| ServiceError::HardwareError)?);
    }
    if dimms.iter().all(Option::is_none) {
        return Err(ServiceError::HardwareError);
    }
    Ok(Ddr3Channels { dimms })
}

fn ceil_div(a: u32, b: u32) -> u32 {
    a / b + u32::from(!a.is_multiple_of(b))
}

/// Find the common speed and derived cycles from GM45 `raminit.c` lines
/// 541..723. FSB is 667/800/1067 MHz; the host bridge DDR3 limit is 800 or
/// 1067 MT/s. Neither of these inputs may be inferred from SPD alone.
pub fn select_ddr3_timing(
    channels: &Ddr3Channels,
    fsb_mhz: u32,
    max_ddr3_mt: u32,
) -> Result<Ddr3Timing, ServiceError> {
    if !matches!(fsb_mhz, 667 | 800 | 1067) || !matches!(max_ddr3_mt, 800 | 1067) {
        return Err(ServiceError::HardwareError);
    }
    let mut mask = !0u32;
    let mut tck_min = 0;
    let mut taa_min = 0;
    let mut tras = 0;
    let mut trp = 0;
    let mut trcd = 0;
    let mut twr = 0;
    let mut density = 0;
    let mut page_size = 0;
    let mut populated = false;
    for dimm in channels.dimms.iter().flatten() {
        ddr3::validate_gm45(dimm).map_err(|_| ServiceError::HardwareError)?;
        // Reject unsupported page-size combinations before indexing the GM45
        // tFAW table; only 1K/2K chip pages are defined in coreboot.
        if !matches!(dimm.page_size, 1024 | 2048) || dimm.tck_min_256ns == 0 {
            return Err(ServiceError::HardwareError);
        }
        populated = true;
        mask &= dimm.cas_latencies;
        tck_min = tck_min.max(dimm.tck_min_256ns);
        taa_min = taa_min.max(dimm.taa_min_256ns);
        tras = tras.max(dimm.tras_256ns);
        trp = trp.max(dimm.trp_256ns);
        trcd = trcd.max(dimm.trcd_256ns);
        twr = twr.max(dimm.twr_256ns);
        density = density.max(dimm.density_code as usize);
        page_size = page_size.max(dimm.page_size);
    }
    if !populated || tck_min == 0 {
        return Err(ServiceError::HardwareError);
    }
    let max_mhz = (max_ddr3_mt / 2).min(fsb_mhz / 2);
    let supported = 256_000 / tck_min;
    for clock in [MemClock::Mhz533, MemClock::Mhz400, MemClock::Mhz333] {
        let mhz = clock.mhz();
        if mhz > max_mhz || mhz > supported {
            continue;
        }
        let tck = 256_000 / mhz;
        let first_cas = ceil_div(taa_min, tck).max(4);
        for cas in first_cas..=18 {
            if mask & (1 << cas) == 0 || cas * tck >= 160 * 32 {
                continue;
            }
            let index = match clock {
                MemClock::Mhz533 => 0,
                MemClock::Mhz400 => 1,
                MemClock::Mhz333 => 2,
            };
            // Coreboot GM45 tRFC/tFAW tables indexed by chip density and
            // chip page size respectively; do not substitute SPD tRFC.
            const TRFC: [[u32; 4]; 3] = [[40, 56, 68, 104], [30, 42, 51, 78], [25, 35, 43, 65]];
            const TFAW: [[u32; 3]; 2] = [[20, 15, 13], [27, 20, 17]];
            let trd = cas
                + match fsb_mhz {
                    667 => 1,
                    800 => 2,
                    1067 => 3 + u32::from(clock == MemClock::Mhz533),
                    _ => unreachable!(),
                };
            let page_kib = page_size / 1024;
            return Ok(Ddr3Timing {
                clock,
                cas: cas as u8,
                tck_256ns: tck,
                tras: ceil_div(tras, tck),
                trp: ceil_div(trp, tck),
                trcd: ceil_div(trcd, tck),
                twr: ceil_div(twr, tck),
                trfc: TRFC[index][density],
                trd,
                trrd: 2 + page_kib * (1 + u32::from(clock == MemClock::Mhz533)),
                tfaw: TFAW[(page_kib - 1) as usize][index],
                twl: if clock == MemClock::Mhz533 { 6 } else { 5 },
            });
        }
    }
    Err(ServiceError::HardwareError)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBus {
        spd: [u8; 256],
        extra_slot: bool,
    }
    impl SmBus for FakeBus {
        fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            if addr != 0x50 && !(addr == 0x51 && self.extra_slot) {
                return Err(ServiceError::HardwareError);
            }
            Ok(self.spd[cmd as usize])
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Ok(())
        }
    }
    fn spd() -> [u8; 256] {
        let mut spd = [0; 256];
        spd[2] = DDR3;
        spd[3] = 3;
        spd[4] = 2;
        spd[5] = (2 << 3) | 1;
        spd[7] = 1;
        spd[8] = 3;
        spd[10] = 1;
        spd[11] = 8;
        spd[12] = 12; // DDR3-667
        spd[14] = 0b0011_1100; // CL6..9
        spd[16] = 100;
        spd[17] = 120;
        spd[18] = 110;
        spd[20] = 110;
        spd[21] = 1;
        spd[22] = 0x18;
        spd[24] = 0xa0;
        spd[25] = 4;
        spd
    }
    #[test]
    fn gm45_spd_discovery_rejects_extra_slot_and_mixed_type() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        assert!(probe_ddr3(&mut bus, &[0x50, 0x51, 0, 0]).is_ok());
        bus.extra_slot = true;
        assert!(probe_ddr3(&mut bus, &[0x50, 0x51, 0, 0]).is_err());
        bus.spd[2] = crate::generic::spd::ddr2::DDR2;
        assert!(probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).is_err());
    }
    #[test]
    fn gm45_clock_cas_and_derived_cycles_follow_coreboot_tables() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: false,
        };
        let channels = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        let t = select_ddr3_timing(&channels, 800, 800).unwrap();
        assert_eq!(
            (t.clock, t.cas, t.trfc, t.trd, t.trrd, t.tfaw, t.twl),
            (MemClock::Mhz400, 6, 51, 8, 3, 15, 5)
        );
        assert_eq!((t.tras, t.trp, t.trcd, t.twr), (14, 6, 6, 6));
        assert!(select_ddr3_timing(&channels, 533, 800).is_err());
        bus.spd[14] = 0;
        let channels = probe_ddr3(&mut bus, &[0x50, 0, 0, 0]).unwrap();
        assert!(select_ddr3_timing(&channels, 800, 800).is_err());
    }

    #[test]
    fn gm45_dual_channel_uses_slowest_spd_and_rejects_unsupported_straps() {
        let mut bus = FakeBus {
            spd: spd(),
            extra_slot: true,
        };
        let mut channels = probe_ddr3(&mut bus, &[0x50, 0, 0x51, 0]).unwrap();
        channels.dimms[1].as_mut().unwrap().trp_256ns += 640;
        channels.dimms[1].as_mut().unwrap().cas_latencies &= !(1 << 6);
        let t = select_ddr3_timing(&channels, 800, 800).unwrap();
        assert_eq!((t.clock, t.cas, t.trp), (MemClock::Mhz400, 7, 7));
        assert!(select_ddr3_timing(&channels, 800, 0).is_err());
        assert!(select_ddr3_timing(&channels, 533, 800).is_err());
        channels.dimms[0].as_mut().unwrap().page_size = 4096;
        assert!(select_ddr3_timing(&channels, 800, 800).is_err());
    }
}
