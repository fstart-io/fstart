//! Sandy Bridge / Ivy Bridge native DDR3 initialization.
//!
//! A port of coreboot `northbridge/intel/sandybridge/raminit.c`,
//! `raminit_native.c`, `raminit_common.c`, `raminit_iosav.c` and
//! `raminit_tables.c`: SPD discovery (including XMP), frequency and CAS
//! selection through the PCU, controller timing, JEDEC reset and MRS, the
//! complete training sequence (receive enable, read MPR, JEDEC write
//! leveling, write flyby, command rate/phase, aggressive read and write),
//! the memory map, ECC scrubbing and the final power-management registers.
//!
//! Training results are captured in [`TrainingRecord`]; a later boot whose
//! modules and CPU are unchanged replays it (coreboot's fast boot, and the
//! only way to bring DRAM out of self-refresh on S3 resume).
// Index-based loops follow coreboot's per-channel/per-lane C structure.
#![allow(clippy::needless_range_loop)]

mod iosav;
mod record;
mod tables;
mod train;

use super::regs::{
    BROADCAST, CAPID0_A, CAPID0_B, CMD_PI_CODING, COMP_OFST_1, GDCR_RX, GDCR_TX, GGC,
    HostBridgeConfig, MC_BIOS_REQ, MC_INIT_STATE_G, MESEG_MASK, Mchbar, RCOMP_TIMER, SCHED_CBIT,
    TC_DBP, TC_DTP, TC_OTHP, TC_RAP, TC_RFP, TC_RFTP, TC_RWP, TC_SRFTP,
};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_spd::ddr3::{self, Ddr3Attr, XmpProfile};
use iosav::Iosav;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub use record::TrainingRecord;

/// The platform's CAR workspace for a cached and a captured
/// [`TrainingRecord`].
pub const TRAINING_RECORD_SIZE: usize = 768;

pub const NUM_CHANNELS: usize = 2;
pub const NUM_SLOTRANKS: usize = 4;
pub const NUM_SLOTS: usize = 2;
pub const NUM_LANES: usize = 9;

/// One quadrature clock is 64 PI ticks.
const QCLK_PI: i32 = 64;
/// Maximum delay for command, control and clock.
const CCC_MAX_PI: i32 = 2 * QCLK_PI - 1;
const T_DLLK: u32 = 512;
const MIN_CAS: u32 = 4;
const MAX_CAS: u32 = 18;

/// tCK in 1/256 ns of the standard DDR3 clocks (coreboot `TCK_*MHZ`).
pub mod tck {
    pub const MHZ1333: u32 = 192;
    pub const MHZ1200: u32 = 212;
    pub const MHZ1100: u32 = 232;
    pub const MHZ1066: u32 = 240;
    pub const MHZ1000: u32 = 256;
    pub const MHZ933: u32 = 274;
    pub const MHZ900: u32 = 284;
    pub const MHZ800: u32 = 320;
    pub const MHZ700: u32 = 365;
    pub const MHZ666: u32 = 384;
    pub const MHZ533: u32 = 480;
    pub const MHZ400: u32 = 640;
}

/// `NS2MHZ_DIV256`.
const NS2MHZ_DIV256: u32 = 1000 << 8;

/// CPU generation and stepping facts raminit depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuId(pub u32);

impl CpuId {
    pub const fn is_sandy(self) -> bool {
        self.0 & 0xffff0 == 0x206a0
    }
    pub const fn is_ivy(self) -> bool {
        self.0 & 0xffff0 == 0x306a0
    }
    pub const fn stepping(self) -> u32 {
        self.0 & 0xf
    }
}

/// Inputs from the board and the chipset.
#[derive(Debug, Clone, Copy)]
pub struct RaminitConfig {
    /// SPD SMBus addresses: channel 0 slot 0/1, channel 1 slot 0/1.
    pub spd_addresses: [u8; 4],
    /// Board DDR3 clock cap in MHz (devicetree `max_mem_clock_mhz`); zero
    /// leaves the fused limit.
    pub max_mem_clock_mhz: u16,
    /// Mobile platform (allows DLL-off power down).
    pub mobile: bool,
    /// ME UMA in MiB, carved below TOM.
    pub me_uma_mib: u32,
    /// TSEG size in MiB.
    pub tseg_mib: u32,
    /// Accept XMP profiles beyond their declared DIMMs per channel.
    pub ignore_xmp_max_dimms: bool,
    /// Accept XMP profiles requesting a voltage other than 1.5 V.
    pub ignore_xmp_voltage: bool,
    /// Enable ECC when every module supports it.
    pub enable_ecc: bool,
}

/// Why training stopped; the channel is disabled for the fallback attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainError {
    pub channel: usize,
}

type TrainResult = Result<(), TrainError>;

/// Per-lane trained delays (coreboot `struct ram_lane_timings`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneTimings {
    pub rcven: i32,
    pub rx_dqs_p: i32,
    pub rx_dqs_n: i32,
    pub tx_dq: i32,
    pub tx_dqs: i32,
}

/// Per-rank trained delays (coreboot `struct ram_rank_timings`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RankTimings {
    pub roundtrip_latency: i32,
    pub io_latency: i32,
    pub pi_coding: i32,
    pub lanes: [LaneTimings; NUM_LANES],
}

/// DIMM facts raminit uses after the SPD decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DimmFacts {
    pub size_mb: u32,
    pub ranks: u8,
    pub width: u8,
    pub row_bits: u8,
    pub dll_off_mode: bool,
}

/// The controller state coreboot calls `ramctr_timing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctrl {
    pub cpu: u32,
    pub spd_crc: [[u16; NUM_SLOTS]; NUM_CHANNELS],
    pub base_freq: u32,
    pub frq: u32,
    pub cas_supported: u32,
    pub t_ck: u32,
    pub t_aa: u32,
    pub t_wr: u32,
    pub t_rcd: u32,
    pub t_rrd: u32,
    pub t_rp: u32,
    pub t_ras: u32,
    pub t_rfc: u32,
    pub t_wtr: u32,
    pub t_rtp: u32,
    pub t_faw: u32,
    pub t_cwl: u32,
    pub t_cmd: u32,
    pub cas: u32,
    pub cwl: u32,
    pub t_refi: u32,
    pub t_mod: u32,
    pub t_xs_offset: u32,
    pub t_wlo: u32,
    pub t_cke: u32,
    pub t_xpdll: u32,
    pub t_xp: u32,
    pub t_aonpd: u32,
    pub mdll_wake_delay: u32,
    pub rankmap: [u8; NUM_CHANNELS],
    pub ref_card_offset: [u32; NUM_CHANNELS],
    pub mad_dimm: [u32; NUM_CHANNELS],
    pub channel_size_mb: [u32; NUM_CHANNELS],
    pub cmd_stretch: [u32; NUM_CHANNELS],
    pub pi_code_offset: i32,
    pub pi_coding_threshold: i32,
    pub ecc_supported: bool,
    pub ecc_forced: bool,
    pub ecc_enabled: bool,
    pub lanes: usize,
    pub edge_offset: [i32; 3],
    pub tx_dq_offset: [i32; 3],
    pub extended_temperature_range: bool,
    pub auto_self_refresh: bool,
    pub rank_mirror: [[bool; NUM_SLOTRANKS]; NUM_CHANNELS],
    pub timings: [[RankTimings; NUM_SLOTRANKS]; NUM_CHANNELS],
    pub dimms: [[DimmFacts; NUM_SLOTS]; NUM_CHANNELS],
}

impl Ctrl {
    fn cpu(&self) -> CpuId {
        CpuId(self.cpu)
    }

    fn populated_channels(&self) -> impl Iterator<Item = usize> + use<> {
        let map = self.rankmap;
        (0..NUM_CHANNELS).filter(move |&ch| map[ch] != 0)
    }

    fn populated_ranks(&self, channel: usize) -> impl Iterator<Item = usize> + use<> {
        let map = self.rankmap[channel];
        (0..NUM_SLOTRANKS).filter(move |&rank| map & (1 << rank) != 0)
    }

    /// Every populated (channel, rank) in coreboot's `FOR_ALL_CHANNELS
    /// FOR_ALL_POPULATED_RANKS` order.
    fn all_ranks(&self) -> impl Iterator<Item = (usize, usize)> + use<> {
        let map = self.rankmap;
        (0..NUM_CHANNELS).flat_map(move |ch| {
            (0..NUM_SLOTRANKS)
                .filter(move |&rank| map[ch] & (1 << rank) != 0)
                .map(move |rank| (ch, rank))
        })
    }

    fn rank(&mut self, channel: usize, rank: usize) -> &mut RankTimings {
        &mut self.timings[channel][rank]
    }

    fn disable_channel(&mut self, channel: usize) {
        self.rankmap[channel] = 0;
        self.rank_mirror[channel] = [false; NUM_SLOTRANKS];
        self.channel_size_mb[channel] = 0;
        self.cmd_stretch[channel] = 0;
        self.mad_dimm[channel] = 0;
        self.timings[channel] = Default::default();
        self.dimms[channel] = Default::default();
    }
}

/// Raw SPD of every slot; zero when unpopulated.
pub type Spds = [[u8; 256]; 4];

/// Read every configured SPD completely (coreboot `mainboard_get_spd`).
/// A slot that does not answer stays zero, i.e. unpopulated.
pub fn read_spds(smbus: &mut dyn SmBus, addresses: &[u8; 4]) -> Result<Spds, ServiceError> {
    let mut spds = [[0u8; 256]; 4];
    for (slot, &address) in addresses.iter().enumerate() {
        if address == 0 {
            continue;
        }
        if let Some(spd) = crate::generic::spd::ddr3::read_spd(smbus, address)? {
            spds[slot] = spd;
        }
    }
    Ok(spds)
}

/// The unique module CRC of every slot (coreboot `spd_crc` / fast-boot key).
pub fn spd_crcs(spds: &Spds) -> [[u16; NUM_SLOTS]; NUM_CHANNELS] {
    core::array::from_fn(|ch| core::array::from_fn(|slot| ddr3::unique_crc(&spds[2 * ch + slot])))
}

/// The SNB/IVB memory controller during and after training.
pub struct Raminit {
    mch: &'static Mchbar,
    hb: &'static HostBridgeConfig,
    io: Iosav<'static>,
    pub c: Ctrl,
    config: RaminitConfig,
    /// Decoded module attributes, for SMBIOS.
    pub attrs: [[Option<Ddr3Attr>; NUM_SLOTS]; NUM_CHANNELS],
}

impl Raminit {
    pub fn new(mch: &'static Mchbar, hb: &'static HostBridgeConfig, config: RaminitConfig) -> Self {
        Self {
            mch,
            hb,
            io: Iosav::new(mch),
            c: Ctrl::default(),
            config,
            attrs: [[None; NUM_SLOTS]; NUM_CHANNELS],
        }
    }

    /// coreboot `reinit_ctrl`.
    fn reinit(&mut self, cpu: u32) {
        let capid = self.hb.capid0_a.extract();
        self.c = Ctrl {
            cpu,
            ecc_forced: capid.is_set(CAPID0_A::ECC_FORCED),
            ..Ctrl::default()
        };
        self.c.ecc_supported = self.c.ecc_forced || !capid.is_set(CAPID0_A::ECCDIS);
        fstart_log::info!(
            "sandybridge: ECC supported {} forced {}",
            self.c.ecc_supported as u8,
            self.c.ecc_forced as u8
        );
    }

    /// coreboot `dram_find_spds_ddr3`: decode every SPD (preferring XMP
    /// profile 1), build the rank map, mirroring, ECC and reference-card
    /// offsets.
    fn find_spds(&mut self, spds: &Spds) -> Result<(), ServiceError> {
        let c = &mut self.c;
        c.rankmap = [0; NUM_CHANNELS];
        c.extended_temperature_range = true;
        c.auto_self_refresh = true;
        let mut can_use_ecc = c.ecc_supported;
        let mut dimms = 0;
        for ch in 0..NUM_CHANNELS {
            c.channel_size_mb[ch] = 0;
            let ch_dimms = (0..NUM_SLOTS)
                .filter(|slot| spds[2 * ch + slot][2] == ddr3::DDR3)
                .count() as u8;
            let mut reference = [u8::MAX; NUM_SLOTS];
            for slot in 0..NUM_SLOTS {
                let spd = &spds[2 * ch + slot];
                c.spd_crc[ch][slot] = ddr3::unique_crc(spd);
                let attr = match ddr3::decode_xmp(spd, XmpProfile::One) {
                    Ok(xmp)
                        if ch_dimms > xmp.dimms_per_channel
                            && !self.config.ignore_xmp_max_dimms =>
                    {
                        fstart_log::info!(
                            "sandybridge: XMP supports {} DIMMs, {} installed",
                            xmp.dimms_per_channel,
                            ch_dimms
                        );
                        ddr3::decode_attr(spd).ok().map(|(attr, _)| attr)
                    }
                    Ok(xmp) if xmp.voltage != 1500 && !self.config.ignore_xmp_voltage => {
                        fstart_log::info!(
                            "sandybridge: XMP requests unsupported {} mV",
                            xmp.voltage
                        );
                        ddr3::decode_attr(spd).ok().map(|(attr, _)| attr)
                    }
                    Ok(xmp) => Some(xmp),
                    Err(_) => ddr3::decode_attr(spd).ok().map(|(attr, status)| {
                        if status.is_some() {
                            fstart_log::error!(
                                "sandybridge: SPD ch{} slot{} CRC or field error",
                                ch,
                                slot
                            );
                        }
                        attr
                    }),
                };
                self.attrs[ch][slot] = attr;
                let Some(dimm) = attr else {
                    c.dimms[ch][slot] = DimmFacts::default();
                    continue;
                };
                fstart_log::info!(
                    "sandybridge: ch{} slot{}: {} MiB, {} rank(s), x{}, tCK {}",
                    ch,
                    slot,
                    dimm.size_mb,
                    dimm.ranks,
                    dimm.width,
                    dimm.t_ck
                );
                if dimm.ranks > 2 || dimm.ranks == 0 {
                    return Err(ServiceError::HardwareError);
                }
                dimms += 1;
                c.dimms[ch][slot] = DimmFacts {
                    size_mb: dimm.size_mb,
                    ranks: dimm.ranks,
                    width: dimm.width,
                    row_bits: dimm.row_bits,
                    dll_off_mode: dimm.flags.dll_off_mode,
                };
                c.rank_mirror[ch][slot * 2] = false;
                c.rank_mirror[ch][slot * 2 + 1] = dimm.flags.pins_mirrored;
                c.channel_size_mb[ch] += dimm.size_mb;
                can_use_ecc &= dimm.flags.is_ecc;
                c.auto_self_refresh &= dimm.flags.asr;
                c.extended_temperature_range &= dimm.flags.ext_temp_refresh;
                c.rankmap[ch] |= ((1u8 << dimm.ranks) - 1) << (2 * slot);
                reference[slot] = dimm.reference_card;
            }
            c.ref_card_offset[ch] = match (ch_dimms, reference) {
                (2, [rc0 @ 0..6, rc1 @ 0..6]) => {
                    const OFFSET: [[u32; 6]; 6] = [
                        [0, 0, 0, 0, 2, 2],
                        [0, 0, 0, 0, 2, 2],
                        [0, 0, 0, 0, 2, 2],
                        [0, 0, 0, 0, 1, 1],
                        [2, 2, 2, 1, 0, 0],
                        [2, 2, 2, 1, 0, 0],
                    ];
                    OFFSET[usize::from(rc0)][usize::from(rc1)]
                }
                _ => 0,
            };
            fstart_log::info!("sandybridge: channel {} rankmap {:#x}", ch, c.rankmap[ch]);
        }
        if c.ecc_forced || self.config.enable_ecc {
            c.ecc_enabled = can_use_ecc;
        }
        if c.ecc_forced && !c.ecc_enabled {
            fstart_log::error!("sandybridge: ECC forced but a non-ECC DIMM is installed");
            return Err(ServiceError::HardwareError);
        }
        c.lanes = if c.ecc_enabled { 9 } else { 8 };
        if dimms == 0 {
            fstart_log::error!("sandybridge: no DIMMs found");
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    /// coreboot `dram_find_common_params`.
    fn find_common_params(&mut self) -> Result<(), ServiceError> {
        let c = &mut self.c;
        c.cas_supported = (1 << (MAX_CAS - MIN_CAS + 1)) - 1;
        let mut valid = 0;
        for dimm in self.attrs.iter().flatten().flatten() {
            valid += 1;
            c.cas_supported &= u32::from(dimm.cas_supported);
            c.t_ck = c.t_ck.max(dimm.t_ck);
            c.t_aa = c.t_aa.max(dimm.t_aa);
            c.t_wr = c.t_wr.max(dimm.t_wr);
            c.t_rcd = c.t_rcd.max(dimm.t_rcd);
            c.t_rrd = c.t_rrd.max(dimm.t_rrd);
            c.t_rp = c.t_rp.max(dimm.t_rp);
            c.t_ras = c.t_ras.max(dimm.t_ras);
            c.t_rfc = c.t_rfc.max(dimm.t_rfc);
            c.t_wtr = c.t_wtr.max(dimm.t_wtr);
            c.t_rtp = c.t_rtp.max(dimm.t_rtp);
            c.t_faw = c.t_faw.max(dimm.t_faw);
            c.t_cwl = c.t_cwl.max(dimm.t_cwl);
            c.t_cmd = c.t_cmd.max(dimm.t_cmd);
        }
        if c.cas_supported == 0 {
            fstart_log::error!("sandybridge: DIMMs share no CAS latency");
            return Err(ServiceError::HardwareError);
        }
        if valid == 0 {
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    /// coreboot `get_mem_min_tck`.
    fn mem_min_tck(&self) -> u32 {
        let max = self.config.max_mem_clock_mhz;
        if max != 0 {
            return match max {
                1066.. => tck::MHZ1066,
                933.. => tck::MHZ933,
                800.. => tck::MHZ800,
                666.. => tck::MHZ666,
                533.. => tck::MHZ533,
                _ => tck::MHZ400,
            };
        }
        if self.hb.device_id.get() & 0x50 == 0 {
            match self.hb.capid0_a.read(CAPID0_A::DMFC) {
                7 => tck::MHZ533,
                6 => tck::MHZ666,
                5 => tck::MHZ800,
                _ => tck::MHZ800,
            }
        } else {
            match self.hb.capid0_b.read(CAPID0_B::DMFC) {
                7 => tck::MHZ533,
                6 => tck::MHZ666,
                5 => tck::MHZ800,
                4 => tck::MHZ933,
                3 => tck::MHZ1066,
                2 => tck::MHZ1200,
                1 => tck::MHZ1333,
                _ => tck::MHZ800,
            }
        }
    }

    /// coreboot `normalize_tclk`: round tCK up to the next supported clock.
    fn normalize_tclk(&mut self, ref_100mhz: bool) {
        loop {
            const STEPS: [(u32, u32); 11] = [
                (tck::MHZ1200, 133),
                (tck::MHZ1100, 100),
                (tck::MHZ1066, 133),
                (tck::MHZ1000, 100),
                (tck::MHZ933, 133),
                (tck::MHZ900, 100),
                (tck::MHZ800, 133),
                (tck::MHZ700, 100),
                (tck::MHZ666, 133),
                (tck::MHZ533, 133),
                (tck::MHZ400, 133),
            ];
            let Some(&(t_ck, base)) = STEPS.iter().find(|(t_ck, _)| self.c.t_ck <= *t_ck) else {
                self.c.t_ck = 0;
                return;
            };
            self.c.t_ck = t_ck;
            self.c.base_freq = base;
            if ref_100mhz || base != 100 {
                return;
            }
            // Skip the unsupported 100 MHz reference clock.
            self.c.t_ck += 1;
        }
    }

    /// coreboot `get_FRQ`.
    fn frq(&self) -> Result<u32, ServiceError> {
        let c = &self.c;
        let frq = 256_000 / (c.t_ck * c.base_freq);
        match (c.cpu().is_ivy(), c.cpu().is_sandy(), c.base_freq) {
            (true, _, 100) => Ok(frq.clamp(7, 12)),
            (true, _, 133) => Ok(frq.clamp(3, 10)),
            (_, true, 133) => Ok(frq.clamp(3, 8)),
            _ => Err(ServiceError::NotSupported),
        }
    }

    /// coreboot `find_cas_tck`, starting from the slowest of the board/fuse
    /// limit and `floor`. Coreboot starts from the platform limit alone,
    /// which runs a slow DIMM above its SPD tCKmin and resets the lower clock
    /// `dram_freq` asks for after an MPLL lock failure; `floor` carries both.
    fn find_cas_tck(&mut self, floor: u32) -> Result<(), ServiceError> {
        let ref_100mhz = self.hb.capid0_b.read(CAPID0_B::PLL_REF100) != 0;
        self.c.t_ck = self.mem_min_tck().max(floor);
        let cas = loop {
            self.normalize_tclk(ref_100mhz);
            if self.c.t_ck == 0 {
                fstart_log::error!("sandybridge: no compatible clock / CAS");
                return Err(ServiceError::HardwareError);
            }
            let first = self.c.t_aa.div_ceil(self.c.t_ck);
            match (first.max(MIN_CAS)..=MAX_CAS)
                .find(|cas| (self.c.cas_supported >> (cas - MIN_CAS)) & 1 != 0)
            {
                Some(cas) => break cas,
                None => self.c.t_ck += 1,
            }
        };
        self.c.frq = self.frq()?;
        self.c.cas = cas;
        fstart_log::info!(
            "sandybridge: DRAM {} MHz, CAS {}",
            NS2MHZ_DIV256 / self.c.t_ck,
            cas
        );
        Ok(())
    }

    /// coreboot `dram_freq`: lock the MPLL through the PCU, lowering the
    /// clock until it locks.
    fn dram_freq(&mut self) -> Result<(), ServiceError> {
        if self.c.t_ck > tck::MHZ400 {
            fstart_log::error!("sandybridge: DRAM below 400 MHz; using 400 MHz");
            self.c.t_ck = tck::MHZ400;
        }
        // The DIMMs' common tCKmin from the SPDs.
        let mut floor = self.c.t_ck;
        loop {
            self.find_cas_tck(floor)?;
            // The MPLL never locks if the frequency is already set.
            if self.mch.mc_bios_data.get() & 0xff != 0 {
                return Ok(());
            }
            self.mch.mc_bios_req.write(
                MC_BIOS_REQ::FREQ.val(self.c.frq)
                    + MC_BIOS_REQ::REF_100MHZ.val(u32::from(self.c.base_freq == 100))
                    + MC_BIOS_REQ::RUN_BUSY::SET,
            );
            let mut waited = 0u32;
            while self.mch.mc_bios_req.is_set(MC_BIOS_REQ::RUN_BUSY) {
                fstart_arch::udelay(10);
                waited += 10;
            }
            fstart_log::info!("sandybridge: MPLL settled in {} us", waited);
            if self.mch.mc_bios_data.get() & 0xff >= self.c.frq {
                return Ok(());
            }
            fstart_log::info!("sandybridge: MPLL did not lock, retrying lower");
            floor = self.c.t_ck + 1;
        }
    }

    /// coreboot `dram_timing`: derive cycle counts from the selected clock.
    fn dram_timing(&mut self) -> Result<(), ServiceError> {
        let c = &mut self.c;
        // (edge offsets, tx_dq offsets, pi coding threshold) per clock.
        let (edge, tx_dq, threshold) = match c.t_ck {
            tck::MHZ1200 => ([18, 8, 8], [20, 8, 8], 10),
            tck::MHZ1100 => ([17, 7, 7], [19, 7, 7], 13),
            tck::MHZ1066 => ([16, 7, 7], [18, 7, 7], 13),
            tck::MHZ1000 => ([15, 6, 6], [17, 6, 6], 13),
            tck::MHZ933 => ([14, 6, 6], [15, 6, 6], 15),
            tck::MHZ900 => ([14, 6, 6], [15, 6, 6], 12),
            tck::MHZ800 => ([13, 5, 5], [14, 5, 5], 15),
            tck::MHZ700 => ([13, 5, 5], [14, 5, 5], 16),
            tck::MHZ666 => ([10, 4, 4], [11, 4, 4], 16),
            tck::MHZ533 => ([8, 3, 3], [9, 3, 3], 17),
            _ => ([6, 2, 2], [6, 2, 2], 17),
        };
        c.edge_offset = edge;
        c.tx_dq_offset = tx_dq;
        c.pi_coding_threshold = threshold;
        c.pi_code_offset = (256_000 / c.t_ck as i32) / 66;
        c.mdll_wake_delay = 128_000 / c.t_ck + 3;
        c.cwl = if c.t_cwl != 0 {
            c.t_cwl.div_ceil(c.t_ck)
        } else {
            match c.t_ck {
                tck::MHZ1333 => 12,
                tck::MHZ1200 | tck::MHZ1100 => 11,
                tck::MHZ1066 | tck::MHZ1000 => 10,
                tck::MHZ933 | tck::MHZ900 => 9,
                tck::MHZ800 | tck::MHZ700 => 8,
                tck::MHZ666 => 7,
                tck::MHZ533 => 6,
                _ => 5,
            }
        };
        let cycles = |time: u32| time.div_ceil(c.t_ck);
        c.t_rcd = cycles(c.t_rcd);
        c.t_rp = cycles(c.t_rp);
        c.t_ras = cycles(c.t_ras);
        c.t_wr = cycles(c.t_wr);
        c.t_faw = cycles(c.t_faw);
        c.t_rrd = cycles(c.t_rrd);
        c.t_rtp = cycles(c.t_rtp);
        c.t_wtr = cycles(c.t_wtr);
        c.t_rfc = cycles(c.t_rfc);
        let (row, col) = tables::index(c.frq, c.base_freq).ok_or(ServiceError::NotSupported)?;
        c.t_refi = tables::FRQ_REFI[row][col];
        c.t_mod = u32::from(tables::FRQ_MOD[row][col]);
        c.t_xs_offset = u32::from(tables::FRQ_XS[row][col]);
        c.t_wlo = u32::from(tables::FRQ_WLO[row][col]);
        c.t_cke = u32::from(tables::FRQ_CKE[row][col]);
        c.t_xpdll = u32::from(tables::FRQ_XPDLL[row][col]);
        c.t_xp = u32::from(tables::FRQ_XP[row][col]);
        c.t_aonpd = u32::from(tables::FRQ_AONPD[row][col]);
        fstart_log::info!(
            "sandybridge: CWL {} tRCD {} tRP {} tRAS {} tWR {} tFAW {} tRRD {} tRTP {} tWTR {} tRFC {}",
            c.cwl,
            c.t_rcd,
            c.t_rp,
            c.t_ras,
            c.t_wr,
            c.t_faw,
            c.t_rrd,
            c.t_rtp,
            c.t_wtr,
            c.t_rfc
        );
        Ok(())
    }

    /// coreboot `dram_dimm_mapping`.
    fn dimm_mapping(&mut self) {
        let c = &mut self.c;
        for ch in 0..NUM_CHANNELS {
            let [d0, d1] = c.dimms[ch];
            let (a, b, select) = if d0.size_mb >= d1.size_mb {
                (d0, d1, 0)
            } else {
                (d1, d0, 1 << 16)
            };
            let mut reg = select;
            if a.ranks > 0 {
                reg |= a.size_mb / 256;
                reg |= u32::from(a.ranks - 1) << 17;
                reg |= u32::from(a.width / 8 - 1) << 19;
            }
            if b.ranks > 0 {
                reg |= (b.size_mb / 256) << 8;
                reg |= u32::from(b.ranks - 1) << 18;
                reg |= u32::from(b.width / 8 - 1) << 20;
            }
            // Rank interleave and enhanced interleave.
            reg |= 1 << 21 | 1 << 22;
            c.mad_dimm[ch] = if a.ranks > 0 || b.ranks > 0 { reg } else { 0 };
        }
    }

    /// coreboot `dram_dimm_set_mapping`.
    fn dimm_set_mapping(&self, training: bool) {
        let ecc = match (self.c.ecc_enabled, training) {
            (false, _) => 0,
            (true, true) => 1 << 24,
            (true, false) => 3 << 24,
        };
        for ch in 0..NUM_CHANNELS {
            self.mch.mad_dimm[ch].set(self.c.mad_dimm[ch] | ecc);
        }
        if self.c.ecc_enabled {
            fstart_arch::udelay(10);
        }
    }

    /// coreboot `dram_zones`.
    pub fn zones(&self, training: bool) {
        let size = |ch: usize| {
            let mb = self.c.channel_size_mb[ch];
            if training {
                if mb != 0 { 256 } else { 0 }
            } else {
                mb
            }
        };
        let (ch0, ch1) = (size(0), size(1));
        let (val, chnl) = if ch0 >= ch1 {
            (ch1 / 256, 0x24)
        } else {
            (ch0 / 256, 0x21)
        };
        let reg =
            (self.mch.mad_zr.get() & !0xffff_0000) | (val & 0xff) << 24 | ((2 * val) & 0xff) << 16;
        self.mch.mad_zr.set(reg);
        self.mch.mad_chnl.set(chnl);
    }

    /// coreboot `dram_memorymap`: TOM, TOLUD, TOUUD, remap, TSEG, stolen
    /// graphics and the ME segment, all in MiB.
    fn memory_map(&self) {
        let hb = self.hb;
        const MMIO_SIZE: u32 = 2048;
        let me_uma = self.config.me_uma_mib;
        let ggc = hb.ggc.extract();
        let (gfx, gtt) = if ggc.is_set(GGC::IVD) {
            (0, 0)
        } else {
            (
                u32::from(ggc.read(GGC::GMS)) * 32,
                u32::from(ggc.read(GGC::GGMS)),
            )
        };
        let tseg = self.config.tseg_mib;
        let tom = self.c.channel_size_mb[0] + self.c.channel_size_mb[1];
        let mestolen = tom - me_uma;
        let mut tolud = (4096 - MMIO_SIZE + gfx + gtt + tseg).min(tom - me_uma);
        let mut gfxbase = tolud - gfx;
        let mut gttbase = gfxbase - gtt;
        let mut tsegbase = gttbase - tseg;
        let delta = tsegbase & (tseg - 1);
        tsegbase &= !(tseg - 1);
        gttbase -= delta;
        gfxbase -= delta;
        tolud -= delta;
        let (reclaim, remapbase, remaplimit, touud) = if tom - me_uma > tolud {
            let base = 4096u32.max(tom - me_uma);
            let limit = base + 4096u32.min(tom - me_uma) - tolud - 1;
            (true, base, limit, limit + 1)
        } else {
            (false, 0, 0, tom - me_uma)
        };
        let set_lo = |current: u32, mib: u32| (current & !0xfff0_0000) | (mib & 0xfff) << 20;
        let set_hi = |current: u32, mib: u32| (current & !0x000f_ffff) | (mib & 0xffff_f000) >> 12;
        hb.tom.set(set_lo(hb.tom.get(), tom));
        hb.tom_hi.set(set_hi(hb.tom_hi.get(), tom));
        hb.tolud.set(set_lo(hb.tolud.get(), tolud));
        hb.touud.set(set_lo(hb.touud.get(), touud));
        hb.touud_hi.set(set_hi(hb.touud_hi.get(), touud));
        if reclaim {
            hb.remapbase.set(remapbase << 20);
            hb.remapbase_hi.set(remapbase >> 12);
            hb.remaplimit.set(remaplimit << 20);
            hb.remaplimit_hi.set(remaplimit >> 12);
        }
        hb.tsegmb.set(set_lo(hb.tsegmb.get(), tsegbase));
        hb.bdsm.set(set_lo(hb.bdsm.get(), gfxbase));
        hb.bgsm.set(set_lo(hb.bgsm.get(), gttbase));
        if me_uma != 0 {
            let mask = 0x80000 - me_uma;
            hb.meseg_mask_hi.set(set_hi(hb.meseg_mask_hi.get(), mask));
            hb.meseg_base.set(set_lo(hb.meseg_base.get(), mestolen));
            hb.meseg_base_hi
                .set(set_hi(hb.meseg_base_hi.get(), mestolen));
            hb.meseg_mask.set(set_lo(hb.meseg_mask.get(), mask));
            hb.meseg_mask
                .modify(MESEG_MASK::STOLEN_EN::SET + MESEG_MASK::LOCK::SET);
        }
        fstart_log::info!(
            "sandybridge: TOM {} TOLUD {} TOUUD {} TSEG {} MiB, ME UMA {} MiB",
            tom,
            tolud,
            touud,
            tsegbase,
            me_uma
        );
    }

    /// coreboot `toggle_io_reset`.
    fn toggle_io_reset(&self) {
        let reg = self.mch.mc_init_state_g.get();
        self.mch
            .mc_init_state_g
            .set(reg | MC_INIT_STATE_G::IO_RESET::SET.value);
        fstart_arch::udelay(1);
        self.mch
            .mc_init_state_g
            .set(reg & !MC_INIT_STATE_G::IO_RESET::SET.value);
        fstart_arch::udelay(1);
    }

    /// coreboot `dram_xover`.
    fn xover(&self) {
        for ch in 0..NUM_CHANNELS {
            self.mch.clk[ch].pi_code.set(xover_clk(self.c.rankmap[ch]));
            self.mch.cmd[ch]
                .pi_coding
                .set(xover_cmd(self.c.rankmap[ch]));
        }
    }

    /// coreboot `dram_odt_stretch`.
    fn odt_stretch(&self, ch: usize) {
        let mut stretch = self.c.ref_card_offset[ch];
        let cpu = self.c.cpu();
        if cpu.is_sandy() && cpu.stepping() == 4 {
            if stretch == 2 {
                stretch = 3;
            }
            let reg = &self.mch.mc[ch].sched_second_cbit;
            reg.set((reg.get() & !(0xf << 10)) | stretch << 12 | stretch << 10);
        } else {
            self.mch.mc[ch]
                .tc_othp
                .modify(TC_OTHP::ODT_DELAY_D0.val(stretch) + TC_OTHP::ODT_DELAY_D1.val(stretch));
        }
    }

    fn tc_rap(&self, t_cmd: u32) -> u32 {
        let c = &self.c;
        (TC_RAP::T_RRD.val(c.t_rrd)
            + TC_RAP::T_RTP.val(c.t_rtp)
            + TC_RAP::T_CKE.val(c.t_cke)
            + TC_RAP::T_WTR.val(c.t_wtr)
            + TC_RAP::T_FAW.val(c.t_faw)
            + TC_RAP::T_WR.val(c.t_wr)
            + TC_RAP::T_CMD.val(t_cmd))
        .value
    }

    /// coreboot `dram_timing_regs`.
    fn timing_regs(&self) {
        let c = &self.c;
        let dbp = TC_DBP::T_RCD.val(c.t_rcd)
            + TC_DBP::T_RP.val(c.t_rp)
            + TC_DBP::T_AA.val(c.cas)
            + TC_DBP::T_CWL.val(c.cwl)
            + TC_DBP::T_RAS.val(c.t_ras);
        let othp = TC_OTHP::T_XPDLL.val(c.t_xpdll.min(31))
            + TC_OTHP::T_XP.val(c.t_xp.min(7))
            + TC_OTHP::T_AONPD.val(c.t_aonpd)
            + TC_OTHP::T_CPDED.val(1)
            + TC_OTHP::T_PRPDEN.val(1);
        let dtp = TC_DTP::OVERCLOCK_T_XP.val(u32::from(c.t_xp >= 8))
            + TC_DTP::OVERCLOCK_T_XPDLL.val(u32::from(c.t_xpdll >= 32));
        // tREFIx9: min(8.9 * tREFI, tRASmax = 70 us) / 1024.
        let refix9 = ((c.t_refi * 89) / 10).min((70_000 << 8) / c.t_ck);
        let rftp = TC_RFTP::T_REFI.val(c.t_refi)
            + TC_RFTP::T_RFC.val(c.t_rfc)
            + TC_RFTP::T_REFIX9.val(refix9 / 1024);
        let srftp = TC_SRFTP::T_XSDLL.val(T_DLLK)
            + TC_SRFTP::T_XS_OFFSET.val(c.t_xs_offset)
            + TC_SRFTP::T_ZQOPER.val(T_DLLK - c.t_xs_offset)
            + TC_SRFTP::T_MOD.val(c.t_mod - 8);
        for ch in 0..NUM_CHANNELS {
            let mc = &self.mch.mc[ch];
            mc.tc_dbp.write(dbp);
            mc.tc_rap.set(self.tc_rap(3));
            mc.tc_othp.write(othp);
            if c.cpu().is_ivy() {
                mc.tc_dtp.write(dtp);
            }
            self.odt_stretch(ch);
            mc.tc_rftp.write(rftp);
            mc.tc_rfp.modify(TC_RFP::OREF_RI.val(0xff));
            mc.tc_srftp.write(srftp);
        }
    }

    /// coreboot `program_timings`.
    fn program_timings(&self, ch: usize) {
        let c = &self.c;
        let map = c.rankmap[ch];
        let mut clk_pi_coding = xover_clk(map);
        let mut clk_logic_delay = 0;
        let mut cmd_delay = c
            .populated_ranks(ch)
            .map(|rank| -c.timings[ch][rank].pi_coding)
            .fold(0, i32::max);
        if cmd_delay > CCC_MAX_PI {
            fstart_log::error!("sandybridge: C{} command delay overflow {}", ch, cmd_delay);
            cmd_delay = CCC_MAX_PI;
        }
        let mut ctl_delay = [0i32; NUM_SLOTS];
        for (slot, ctl) in ctl_delay.iter_mut().enumerate() {
            let slot_map = (map >> (2 * slot)) & 3;
            if slot_map & 1 != 0 {
                *ctl += c.timings[ch][2 * slot].pi_coding + cmd_delay;
            }
            if slot_map & 2 != 0 {
                *ctl += c.timings[ch][2 * slot + 1].pi_coding + cmd_delay;
            }
            if slot_map == 3 {
                *ctl /= 2;
            }
            // coreboot keeps ctl_delay unsigned: only the overflow is clamped.
            if *ctl as u32 > CCC_MAX_PI as u32 {
                fstart_log::error!("sandybridge: C{}S{} control delay overflow", ch, slot);
                *ctl = CCC_MAX_PI;
            }
        }
        for rank in c.populated_ranks(ch) {
            let mut clk_delay = c.timings[ch][rank].pi_coding + cmd_delay + c.pi_code_offset;
            if clk_delay < 0 {
                fstart_log::error!("sandybridge: C{}R{} clock delay underflow", ch, rank);
                clk_delay = 0;
            }
            // The clock is periodic and may wrap.
            clk_delay %= CCC_MAX_PI + 1;
            clk_pi_coding |= ((clk_delay % QCLK_PI) as u32) << (6 * rank);
            clk_logic_delay |= ((clk_delay / QCLK_PI) as u32) << rank;
        }
        let mut cmd =
            tock_registers::LocalRegisterCopy::<u32, CMD_PI_CODING::Register>::new(xover_cmd(map));
        cmd.modify(
            CMD_PI_CODING::CMD_PI_CODE.val((cmd_delay % QCLK_PI) as u32)
                + CMD_PI_CODING::CMD_LOGIC_DELAY.val((cmd_delay / QCLK_PI) as u32)
                + CMD_PI_CODING::CTL_PI_CODE_D0.val((ctl_delay[0] % QCLK_PI) as u32)
                + CMD_PI_CODING::CTL_PI_CODE_D1.val((ctl_delay[1] % QCLK_PI) as u32)
                + CMD_PI_CODING::CTL_LOGIC_DELAY_D0.val((ctl_delay[0] / QCLK_PI) as u32)
                + CMD_PI_CODING::CTL_LOGIC_DELAY_D1.val((ctl_delay[1] / QCLK_PI) as u32),
        );
        self.mch.cmd[ch].pi_coding.set(cmd.get());
        self.mch.clk[ch].pi_code.set(clk_pi_coding);
        self.mch.clk[ch].logic_delay.set(clk_logic_delay);

        let mc = &self.mch.mc[ch];
        let mut io_latency = mc.io_latency.get() & !0xffff;
        let mut roundtrip = 0u32;
        for rank in c.populated_ranks(ch) {
            let t = &c.timings[ch][rank];
            io_latency |= (t.io_latency as u32 & 0xf) << (4 * rank);
            roundtrip |= (t.roundtrip_latency as u32 & 0xff) << (8 * rank);
            for lane in 0..c.lanes {
                let l = &t.lanes[lane];
                let regs = self.mch.lane(ch, lane);
                regs.rx[rank].write(
                    GDCR_RX::RCVEN_PI_CODE.val((l.rcven % QCLK_PI) as u32)
                        + GDCR_RX::RX_DQS_P_PI_CODE.val(l.rx_dqs_p as u32)
                        + GDCR_RX::RCVEN_LOGIC_DELAY.val((l.rcven / QCLK_PI) as u32)
                        + GDCR_RX::RX_DQS_N_PI_CODE.val(l.rx_dqs_n as u32),
                );
                regs.tx[rank].write(
                    GDCR_TX::TX_DQ_PI_CODE.val((l.tx_dq % QCLK_PI) as u32)
                        + GDCR_TX::TX_DQS_PI_CODE.val((l.tx_dqs % QCLK_PI) as u32)
                        + GDCR_TX::TX_DQS_LOGIC_DELAY.val((l.tx_dqs / QCLK_PI) as u32)
                        + GDCR_TX::TX_DQ_LOGIC_DELAY.val((l.tx_dq / QCLK_PI) as u32),
                );
            }
        }
        mc.roundtrip_latency.set(roundtrip);
        mc.io_latency.set(io_latency);
    }

    fn wait_rcomp(&self) {
        while !self.mch.rcomp_timer.is_set(RCOMP_TIMER::FIRST_RCOMP_DONE) {
            core::hint::spin_loop();
        }
    }

    /// coreboot `get_COMP2`.
    fn comp2(&self) -> u32 {
        let ivy = self.c.cpu().is_ivy();
        let pick = |ivb, snb| if ivy { ivb } else { snb };
        match self.c.t_ck {
            t if t <= tck::MHZ1066 => pick(0x0C23_5924, 0x0C21_410C),
            t if t <= tck::MHZ933 => pick(0x0C44_6964, 0x0C42_514C),
            t if t <= tck::MHZ800 => pick(0x0C66_71E4, 0x0C63_69CC),
            t if t <= tck::MHZ666 => pick(0x0CA8_C264, 0x0CA5_7A4C),
            t if t <= tck::MHZ533 => pick(0x0CEB_DB64, 0x0CE7_C34C),
            _ => pick(0x0D6F_F5E4, 0x0D6B_EDCC),
        }
    }

    /// coreboot `get_COMP1`.
    fn comp1(&self, ch: usize) -> u32 {
        let orig = self.mch.comp[ch].ofst1.extract();
        let cpu = self.c.cpu();
        if cpu.is_sandy() && cpu.stepping() != 7 {
            let mut comp = orig;
            comp.modify(
                COMP_OFST_1::CLK_ODT_UP.val(1)
                    + COMP_OFST_1::CLK_DRV_UP.val(1)
                    + COMP_OFST_1::CTL_DRV_UP.val(1),
            );
            return comp.get();
        }
        // Fix the PCODE COMP offset bug: revert to defaults.
        let dq_drv_up = if cpu.is_ivy() {
            2
        } else {
            orig.read(COMP_OFST_1::DQ_DRV_UP)
        };
        (COMP_OFST_1::DQ_ODT_DOWN.val(4)
            + COMP_OFST_1::DQ_ODT_UP.val(4)
            + COMP_OFST_1::CLK_ODT_DOWN.val(4)
            + COMP_OFST_1::CLK_ODT_UP.val(orig.read(COMP_OFST_1::CLK_ODT_UP))
            + COMP_OFST_1::DQ_DRV_DOWN.val(4)
            + COMP_OFST_1::DQ_DRV_UP.val(dq_drv_up)
            + COMP_OFST_1::CLK_DRV_DOWN.val(4)
            + COMP_OFST_1::CLK_DRV_UP.val(orig.read(COMP_OFST_1::CLK_DRV_UP))
            + COMP_OFST_1::CTL_DRV_DOWN.val(4)
            + COMP_OFST_1::CTL_DRV_UP.val(orig.read(COMP_OFST_1::CTL_DRV_UP)))
        .value
    }

    /// coreboot `dram_ioregs`.
    fn ioregs(&self) {
        for ch in 0..NUM_CHANNELS {
            self.mch.clk[ch]
                .ranks_used
                .set(u32::from(self.c.rankmap[ch]));
        }
        for ch in 0..NUM_CHANNELS {
            self.mch.cmd[ch]
                .ctl_ranks_used
                .set(u32::from(self.c.rankmap[ch]));
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        self.wait_rcomp();
        self.mch.comp_ofst2.set(self.comp2());
        for ch in self.c.populated_channels() {
            self.mch.comp[ch].ofst1.set(self.comp1(ch));
        }
        // Force RCOMP and wait 20 us.
        self.mch.m_comp.set(self.mch.m_comp.get() | 1 << 8);
        fstart_arch::udelay(20);
    }

    fn wait_iosav_idle(&self) {
        while self.mch.mc[0].status.get() & 0x14 == 0 {
            core::hint::spin_loop();
        }
    }

    /// coreboot `write_reset`: a ZQCS used as a timer on a populated rank.
    fn write_reset(&mut self) {
        let ch = if self.c.rankmap[0] != 0 { 0 } else { 1 };
        self.io.wait(ch);
        let rank = if self.c.rankmap[ch] & 1 != 0 { 0 } else { 2 };
        self.io.zqcs(ch, rank, 3, 8, 0);
        self.io.run(ch, 1, true);
        self.io.wait(ch);
    }

    /// coreboot `dram_jedecreset`.
    fn jedec_reset(&mut self) {
        self.wait_rcomp();
        self.wait_iosav_idle();
        let reg = 0x112;
        self.mch.mc_init_state_g.set(reg);
        self.mch.mc[BROADCAST].mc_init_state.set(0);
        self.mch.mc_init_state_g.set(reg | 2);
        // Assert DIMM reset for 200 us, wait 500 us after deassertion.
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::DDR_RESET::CLEAR);
        fstart_arch::udelay(200);
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::DDR_RESET::SET);
        fstart_arch::udelay(500);
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::DCLK_ENABLE::SET);
        fstart_arch::udelay(1);
        self.cke_and_reset();
    }

    /// Valid-rank CKE, then a NOP reset, per channel.
    fn cke_and_reset(&mut self) {
        for ch in 0..NUM_CHANNELS {
            let map = u32::from(self.c.rankmap[ch]);
            let mc = &self.mch.mc[ch];
            mc.mc_init_state.set(map);
            mc.mc_init_state.set((map & !0xf0) | map << 4);
            self.write_reset();
        }
    }

    /// coreboot `write_mrreg`.
    fn write_mrreg(&mut self, ch: usize, rank: usize, reg: u32, val: u32) {
        self.io.wait(ch);
        let (bank, val) = if self.c.rank_mirror[ch][rank] {
            mirror_mrreg(reg, val)
        } else {
            (reg, val)
        };
        let mrs = |ranksel_ap, post| iosav::Ssq {
            ranksel_ap,
            gap: 4,
            post,
            address: val,
            bank,
            ..iosav::Ssq::new(iosav::MRS, rank as u32)
        };
        let t_mod = self.c.t_mod;
        self.io.write(ch, &[mrs(0, 4), mrs(1, 4), mrs(0, t_mod)]);
        self.io.run_once_and_wait(ch);
    }

    /// coreboot `get_power_down_mode`.
    fn power_down_mode(&self, ch: usize) -> u32 {
        const APD_PPD: u32 = 3;
        const PPD: u32 = 2;
        const NONE: u32 = 0;
        const DLL_OFF: u32 = 6;
        let c = &self.c;
        if c.t_xp > 8 {
            return NONE;
        }
        if c.t_xpdll > 32 {
            return PPD;
        }
        if c.populated_ranks(ch)
            .any(|rank| !c.dimms[ch][rank >> 1].dll_off_mode)
        {
            return APD_PPD;
        }
        if self.config.mobile { DLL_OFF } else { APD_PPD }
    }

    fn make_mr0(&self, ch: usize) -> u32 {
        const MCH_WR: [u32; 12] = [1, 2, 3, 4, 0, 5, 0, 6, 0, 7, 0, 0];
        let c = &self.c;
        let slow_exit = matches!(self.power_down_mode(ch), 6 | 7);
        let mch_cas = if c.cas < 12 {
            (c.cas - 4) << 1
        } else {
            ((c.cas - 12) << 1) | 1
        };
        let mch_wr = MCH_WR[(c.t_wr - 5) as usize];
        // DLL reset (self clearing) after the clock change.
        (1 << 8)
            | (mch_cas & 1) << 2
            | (mch_cas & 0xe) << 3
            | mch_wr << 9
            | u32::from(!slow_exit) << 12
    }

    /// (Rtt_WR, Rtt_Nom) in ohms from the DIMM count of the channel.
    fn odt(&self, ch: usize) -> (u32, u32) {
        let map = self.c.rankmap[ch];
        let dimms = (map & 1) + ((map >> 2) & 1);
        if dimms == 1 { (60, 60) } else { (120, 30) }
    }

    fn make_mr1(&self, ch: usize) -> u32 {
        let (_, rtt_nom) = self.odt(ch);
        let odt = match rtt_nom {
            30 => (1 << 9) | (1 << 2),
            60 => 1 << 2,
            120 => 1 << 6,
            _ => 0,
        };
        2 | odt
    }

    fn mr2(&mut self, ch: usize, rank: usize) {
        let c = &self.c;
        let (rtt_wr, _) = self.odt(ch);
        let srt = c.cpu().is_ivy()
            && c.t_ck >= tck::MHZ1066
            && c.extended_temperature_range
            && !c.auto_self_refresh;
        let mr2 = (c.cwl - 5) << 3
            | u32::from(c.auto_self_refresh) << 6
            | u32::from(srt) << 7
            | (rtt_wr / 60) << 9;
        self.write_mrreg(ch, rank, 2, mr2);
        let shadow = &self.mch.mc[ch].tc_mr2_shadow;
        let mut reg = shadow.get() & (3 << 14 | 3 << 6);
        reg |= mr2 & !(3 << 6);
        if srt {
            reg |= 1 << (rank / 2 + 6);
        }
        if self.c.rank_mirror[ch][rank] {
            reg |= 1 << (rank / 2 + 14);
        }
        shadow.set(reg);
    }

    /// coreboot `dram_mrscommands`.
    fn mrs_commands(&mut self) {
        for ch in self.c.populated_channels() {
            for rank in self.c.populated_ranks(ch) {
                self.mr2(ch, rank);
                self.write_mrreg(ch, rank, 3, 0);
                let mr1 = self.make_mr1(ch);
                self.write_mrreg(ch, rank, 1, mr1);
                let mr0 = self.make_mr0(ch);
                self.write_mrreg(ch, rank, 0, mr0);
            }
        }
        // NOP without ODT or chip selects, then ZQCL on every rank.
        let zqcl = [
            iosav::Ssq {
                command: iosav::NOP & !(0xff << 8),
                ranksel_ap: 0,
                gap: 4,
                post: 15,
                address: 2,
                ..iosav::Ssq::new(iosav::NOP, 0)
            },
            iosav::Ssq {
                gap: 4,
                post: 400,
                address: 1 << 10,
                update: iosav::Update {
                    inc_rank: 1,
                    ..iosav::Update::wrap(20)
                },
                ..iosav::Ssq::new(iosav::ZQCS, 0)
            },
        ];
        self.io.write(BROADCAST, &zqcl);
        self.io.run(BROADCAST, 4, false);
        for ch in 0..NUM_CHANNELS {
            self.io.wait(ch);
        }
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::REFRESH_ENABLE::SET);
        for ch in self.c.populated_channels() {
            self.mch.mc[ch]
                .sched_cbit
                .modify(SCHED_CBIT::DIS_REFRESH::CLEAR);
            self.io.wait(ch);
            let rank = if self.c.rankmap[ch] & 1 != 0 { 0 } else { 2 };
            self.io.wait(ch);
            self.io.zqcs(ch, rank, 4, 101, 31);
            self.io.run_once_and_wait(ch);
        }
    }

    /// coreboot `set_wmm_behavior`.
    fn wmm_behavior(&self) {
        let cpu = self.c.cpu();
        let snb_d = cpu.is_sandy() && matches!(cpu.stepping(), 5 | 6);
        self.mch.mc[BROADCAST]
            .wdbwm
            .set(if snb_d { 0x141d_1519 } else { 0x551d_1519 });
    }

    /// coreboot `prepare_training`.
    fn prepare_training(&self) {
        for ch in self.c.populated_channels() {
            self.mch.mc[ch].tc_rap.modify(TC_RAP::DIS_3ST::SET);
        }
        fstart_arch::udelay(1);
        for ch in self.c.populated_channels() {
            self.io.wait(ch);
        }
    }

    /// coreboot `set_read_write_timings`.
    fn read_write_timings(&self) {
        let c = &self.c;
        let rwdrdd_inc = if c.t_ck <= tck::MHZ1066 { 4 } else { 2 };
        for ch in c.populated_channels() {
            let (min_pi, max_pi) = c
                .populated_ranks(ch)
                .map(|rank| c.timings[ch][rank].pi_coding)
                .fold((10_000, -10_000), |(lo, hi), pi| (lo.min(pi), hi.max(pi)));
            let wrdrdd = if max_pi - min_pi > 51 {
                0
            } else {
                c.ref_card_offset[ch]
            };
            let val = if c.pi_coding_threshold < max_pi - min_pi {
                3
            } else {
                2
            };
            self.odt_stretch(ch);
            self.mch.mc[ch].tc_rwp.write(
                TC_RWP::T_RRDR.val(0)
                    + TC_RWP::T_RRDD.val(val)
                    + TC_RWP::T_WWDR.val(val)
                    + TC_RWP::T_WWDD.val(val)
                    + TC_RWP::T_RWDRDD.val(c.ref_card_offset[ch] + rwdrdd_inc)
                    + TC_RWP::T_WRDRDD.val(wrdrdd)
                    + TC_RWP::T_RWSR.val(2)
                    + TC_RWP::DEC_WRD::SET,
            );
        }
    }

    /// coreboot `set_normal_operation`.
    pub fn normal_operation(&self) {
        for ch in self.c.populated_channels() {
            self.mch.mc[ch]
                .mc_init_state
                .set(1 << 12 | u32::from(self.c.rankmap[ch]));
            self.mch.mc[ch].tc_rap.modify(TC_RAP::DIS_3ST::CLEAR);
        }
    }

    /// coreboot `set_scrambling_seed`.
    pub fn scrambling_seed(&self) {
        const SEEDS: [[u32; 3]; NUM_CHANNELS] = [
            [0x0000_9a36, 0xbafc_fdcf, 0x46d1_ab68],
            [0x0002_8bfa, 0x53fe_4b49, 0x19ed_5483],
        ];
        for ch in self.c.populated_channels() {
            let mc = &self.mch.mc[ch];
            mc.sched_cbit.modify(SCHED_CBIT::DIS_SCRAMBLE::CLEAR);
            mc.scrambling_seed_1.set(SEEDS[ch][0]);
            mc.scrambling_seed_2_hi.set(SEEDS[ch][1]);
            mc.scrambling_seed_2_lo.set(SEEDS[ch][2]);
        }
    }

    /// coreboot `final_registers`.
    pub fn final_registers(&self) {
        let c = &self.c;
        let mch = self.mch;
        if c.cpu().is_ivy() {
            mch.mc[BROADCAST].wmm_read_config.set(0x46);
        }
        for ch in 0..NUM_CHANNELS {
            let mc = &mch.mc[ch];
            let cpded = if c.cpu().is_sandy() && c.cpu().stepping() < 5 {
                2
            } else {
                1
            };
            mc.tc_othp.modify(TC_OTHP::T_CPDED.val(cpded));
            // 64 DCLKs until idle, decision per rank.
            mc.pm_pdwn_config.set(self.power_down_mode(ch) << 8 | 64);
            mc.pm_trml_m_config.set(0x0000_0aaa);
        }
        mch.mc[BROADCAST].pm_bw_limit_config.set(0x5f70_03ff);
        mch.pm_dll_config.set(if c.cpu().is_sandy() {
            0x0003_30f0
        } else {
            0x0007_3000 | c.mdll_wake_delay
        });
        for ch in 0..NUM_CHANNELS {
            mch.mc[ch].pm_cmd_pwr.set(match c.rankmap[ch] {
                0 => 0,
                1 | 4 | 5 => 0x0037_3131,
                _ => 0x009b_6ea1,
            });
        }
        mch.mem_trml_estimation_config.set(0xca91_71e5);
        mch.mem_trml_thresholds_config
            .set((mch.mem_trml_thresholds_config.get() & !0x00ff_ffff) | 0x00e4_d5d0);
        mch.mem_trml_interrupt
            .set(mch.mem_trml_interrupt.get() & !0x1f);
        for ch in 0..NUM_CHANNELS {
            mch.mc[ch].tc_rfp.modify(TC_RFP::REFRESH_2X_CONTROL.val(1));
        }
        mch.mc_init_state_g.modify(MC_INIT_STATE_G::PURE_SRX::SET);
        mch.mc_init_state_g.modify(MC_INIT_STATE_G::SAFE_SR::SET);

        // Watermark latencies for the graphics driver, from the first
        // populated channel (coreboot leaves `channel` at the break).
        let ch = c.populated_channels().next().unwrap_or(0);
        let mut t1_cycles = (mch.mc[ch].tc_zqcal.get() >> 8) & 0xff;
        let dll = mch.pm_dll_config.get();
        if dll & (1 << 17) != 0 {
            t1_cycles += dll & 0xfff;
        }
        t1_cycles += mch.mc[ch].tc_srftp.get() & 0xfff;
        let mut t1_ns = t1_cycles * c.t_ck / 256 + 544;
        if dll & (1 << 17) == 0 {
            t1_ns += 500;
        }
        let t2_ns = 10 * ((mch.sapmtimers.get() >> 8) & 0xfff);
        let t3_ns = if mch.sapmctl.get() & 8 != 0 {
            10 * ((mch.bandtimers_ivb.get() >> 8) & 0xfff)
                + 10 * (mch.sapmtimers2_bandtimers_snb.get() & 0xff)
        } else {
            500
        };
        let wm = |ns: u32| ns.div_ceil(500);
        fstart_log::info!("sandybridge: t123 {} {} {}", t1_ns, t2_ns, t3_ns);
        mch.sskpd.set(
            (mch.sskpd.get() & !0x3f3f_3f3f)
                | ((wm(t1_ns) + wm(t2_ns)) << 16)
                | (wm(t1_ns) << 8)
                | ((wm(t3_ns) + wm(t2_ns) + wm(t1_ns)) << 24)
                | 0x0c,
        );
    }

    /// coreboot `restore_timings`: replay a cached training without the
    /// JEDEC reset, so DRAM in self-refresh keeps its content.
    fn restore_timings(&mut self) {
        for ch in self.c.populated_channels() {
            self.mch.mc[ch]
                .tc_rap
                .set(self.tc_rap(self.c.cmd_stretch[ch]));
        }
        fstart_arch::udelay(1);
        for ch in self.c.populated_channels() {
            self.io.wait(ch);
        }
        for ch in self.c.populated_channels() {
            self.mch.mc[ch].tc_rwp.modify(TC_RWP::DEC_WRD::SET);
        }
        for ch in self.c.populated_channels() {
            fstart_arch::udelay(1);
            self.mch.mc[ch]
                .sched_cbit
                .modify(SCHED_CBIT::DIS_REFRESH::SET);
        }
        self.mch.training_mod.set(0);
        self.mch.mc[BROADCAST].iosav_dc_mask.set(0);
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        self.wait_rcomp();
        self.wait_iosav_idle();
        self.mch.mc_init_state_g.set(0x116);
        self.mch.mc[BROADCAST].mc_init_state.set(0);
        fstart_arch::udelay(500);
        self.cke_and_reset();
        self.mrs_commands();
        self.mch.train[0].training_mod.set(0);
        for ch in 0..NUM_CHANNELS {
            let mux = &self.mch.cmd_mux[ch].debug_mux;
            mux.set(mux.get() & !(0x3f << 24));
            fstart_arch::udelay(2);
        }
    }

    /// coreboot `try_init_dram_ddr3`.
    fn try_init(&mut self, fast_boot: bool, s3resume: bool) -> Result<TrainResult, ServiceError> {
        fstart_log::info!(
            "sandybridge: starting {} Bridge RAM training ({})",
            if self.c.cpu().is_sandy() {
                "Sandy"
            } else {
                "Ivy"
            },
            if fast_boot {
                "fast boot"
            } else {
                "full initialization"
            }
        );
        if !fast_boot {
            self.find_common_params()?;
            self.dimm_mapping();
        }
        self.dram_freq()?;
        if !fast_boot {
            self.dram_timing()?;
        }
        self.mch.mrc_revision.set(0xc04e_b002);
        self.xover();
        self.timing_regs();
        self.mch.mc[BROADCAST].pm_thml_stat.set(0x5500);
        self.mch.mc[BROADCAST].sched_cbit.set(0x1010_0005);
        self.wmm_behavior();
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::IO_RESET::CLEAR);
        self.dimm_set_mapping(true);
        self.zones(true);
        self.memory_map();
        self.ioregs();
        fstart_arch::udelay(1);
        if fast_boot {
            self.restore_timings();
        } else {
            self.jedec_reset();
            self.mrs_commands();
            self.prepare_training();
            let trained = (|| {
                self.receive_enable_calibration()?;
                self.read_mpr_training()?;
                self.write_training()?;
                self.command_training()?;
                self.aggressive_read_training()?;
                self.aggressive_write_training()?;
                self.normalize_training();
                Ok(())
            })();
            if let Err(error) = trained {
                return Ok(Err(error));
            }
        }
        self.read_write_timings();
        if !s3resume && let Err(error) = self.channel_test() {
            return Ok(Err(error));
        }
        self.dimm_set_mapping(false);
        Ok(Ok(()))
    }

    /// Train, or replay `cached`, with coreboot's one-channel fallback.
    /// Returns whether the cached training was used.
    pub fn init(
        &mut self,
        cpu: u32,
        spds: &Spds,
        cached: Option<&Ctrl>,
        s3resume: bool,
    ) -> Result<bool, ServiceError> {
        let mut fast_boot = false;
        if let Some(cached) = cached {
            fstart_log::info!("sandybridge: trying stored timings");
            self.c = *cached;
            match self.try_init(true, s3resume)? {
                Ok(()) => fast_boot = true,
                Err(_) if s3resume => return Err(ServiceError::HardwareError),
                Err(_) => fstart_log::error!("sandybridge: stored timings are invalid"),
            }
        }
        if fast_boot {
            return Ok(true);
        }
        self.reinit(cpu);
        self.find_spds(spds)?;
        let failed = match self.try_init(false, s3resume)? {
            Ok(()) => return Ok(false),
            Err(error) => error,
        };
        fstart_log::error!(
            "sandybridge: RAM training failed, disabling channel {}",
            failed.channel
        );
        self.reinit(cpu);
        self.find_spds(spds)?;
        self.c.disable_channel(failed.channel);
        self.attrs[failed.channel] = [None; NUM_SLOTS];
        match self.try_init(false, s3resume)? {
            Ok(()) => Ok(false),
            Err(_) => Err(ServiceError::HardwareError),
        }
    }

    /// Finish after training (coreboot `init_dram_ddr3` after the loop).
    pub fn finish(&mut self, s3resume: bool) {
        // Only exists on Ivy Bridge, written unconditionally like coreboot.
        self.mch.channel_hash.set(0x00a0_30ce);
        self.scrambling_seed();
        if !s3resume && self.c.ecc_enabled {
            self.channel_scrub();
        }
        self.normal_operation();
        self.final_registers();
        self.zones(false);
    }

    /// coreboot `report_memory_config`.
    pub fn report(&self) {
        let refclk = if self.mch.mc_bios_req.is_set(MC_BIOS_REQ::REF_100MHZ) {
            100
        } else {
            133
        };
        let clock = (self.mch.mc_bios_data.get() * refclk * 100 * 2 + 50) / 100;
        fstart_log::info!("sandybridge: DDR3 ref {} MHz, clock {} MHz", refclk, clock);
        for ch in 0..NUM_CHANNELS {
            let conf = self.mch.mad_dimm[ch].get();
            fstart_log::info!(
                "sandybridge: channel {} MAD_DIMM {:#x}: A {} MiB, B {} MiB",
                ch,
                conf,
                (conf & 0xff) * 256,
                ((conf >> 8) & 0xff) * 256
            );
        }
    }

    /// The ME segment base in MiB as programmed (MESEG_BASE bits 35:20).
    pub fn me_uma_base_mib(&self) -> u32 {
        (self.hb.meseg_base.get() >> 20) | ((self.hb.meseg_base_hi.get() & 0xf) << 12)
    }
}

fn xover_clk(rankmap: u8) -> u32 {
    u32::from(rankmap) << 24
}

fn xover_cmd(rankmap: u8) -> u32 {
    let mut reg = CMD_PI_CODING::CMD_XOVER_ENABLE::SET.value;
    if rankmap & 0x03 != 0 {
        reg |= CMD_PI_CODING::CTL_XOVER_ENABLE_D0::SET.value;
    }
    if rankmap & 0x0c != 0 {
        reg |= CMD_PI_CODING::CTL_XOVER_ENABLE_D1::SET.value;
    }
    reg
}

/// DDR3 rank 1 address mirroring: A3<->A4, A5<->A6, A7<->A8, BA0<->BA1.
const fn mirror_mrreg(bank: u32, addr: u32) -> (u32, u32) {
    (
        ((bank >> 1) & 1) | ((bank << 1) & 2),
        (addr & !0x1f8) | ((addr >> 1) & 0xa8) | ((addr & 0xa8) << 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_mrs_swaps_address_and_bank_pairs() {
        // BA0 <-> BA1, A3 <-> A4.
        assert_eq!(mirror_mrreg(1, 1 << 3), (2, 1 << 4));
        assert_eq!(mirror_mrreg(2, 1 << 8), (1, 1 << 7));
        assert_eq!(mirror_mrreg(3, 0x7), (3, 0x7));
    }

    #[test]
    fn xover_encodings_match_coreboot() {
        assert_eq!(xover_clk(0x5), 0x0500_0000);
        assert_eq!(xover_cmd(0x1), 1 << 14 | 1 << 17);
        assert_eq!(xover_cmd(0x4), 1 << 14 | 1 << 26);
        assert_eq!(xover_cmd(0xf), 1 << 14 | 1 << 17 | 1 << 26);
    }

    #[test]
    fn frequency_tables_cover_the_clamped_multipliers() {
        for frq in 3..=10 {
            assert!(tables::index(frq, 133).is_some());
        }
        for frq in 7..=12 {
            assert!(tables::index(frq, 100).is_some());
        }
        assert!(tables::index(11, 133).is_none());
        assert_eq!(tables::FRQ_REFI[0][5], 8320);
    }
}
