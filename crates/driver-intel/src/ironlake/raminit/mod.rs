//! Ironlake (Arrandale/Clarkdale uncore) DDR3 initialization.
//!
//! A port of coreboot `northbridge/intel/ironlake/raminit.c`,
//! `quickpath.c` and `raminit_tables.c`. That code is a reverse-engineered
//! replay of the vendor MRC: most register writes have no documented
//! meaning, so the port keeps coreboot's order, values and arithmetic
//! (including its C integer semantics) and names only what is known.
//!
//! Memory I/O timings live in scan chains behind MCHBAR register groups:
//! one chain per channel (`0x500 + (channel << 10)`) and a global one
//! (`0x1d0`). Each section is N ≤ 32 bits: N − 2 data bits, a mask bit and a
//! halt bit.

// The DRAM training loops index several per-channel and per-rank arrays in
// lockstep, as coreboot's raminit does; the shared index keeps them comparable.
#![allow(clippy::needless_range_loop)]

mod quickpath;
mod tables;
mod timing;
mod training;

use super::{DmiBar, EpBar, MchBar};
use crate::MmioBar;
use fstart_arch::x86::cpu::intel::model_2065x::{TEMPERATURE_TARGET, TURBO_POWER_CURRENT_LIMIT};
use fstart_arch::x86::mtrr::{IA32_MTRR_PHYSBASE0, IA32_MTRR_PHYSMASK0};
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam::EcamDevice;
use tables::*;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub(super) const NUM_CHANNELS: usize = 2;
pub(super) const NUM_SLOTS: usize = 2;
pub(super) const NUM_RANKS: usize = 2;
const NUM_LANES: usize = 9;

/// Northbridge PCI registers raminit programs directly.
const GGC: u16 = 0x52;
const DEVEN: u16 = 0x54;
const REMAPBASE: u16 = 0x98;
const REMAPLIMIT: u16 = 0x9a;
const TOM: u16 = 0xa0;
const TOUUD: u16 = 0xa2;
const IGD_BASE: u16 = 0xa4;
const GTT_BASE: u16 = 0xa8;
const TSEG: u16 = 0xac;
const TOLUD: u16 = 0xb0;
const CAPID0: u16 = 0xe0;
/// GMA multi-size aperture control.
const MSAC: u16 = 0x62;
/// PCH LPC GEN_PMCON_2: bit 7 marks an interrupted DRAM init.
const GEN_PMCON_2: u16 = 0xa2;

/// QuickPath uncore registers (bus `QUICKPATH_BUS`).
const MAX_RTIDS: u16 = 0x60;
const MIRROR_PORT_CTL: u16 = 0xd0;
const fn sad_dram_rule(index: u16) -> u16 {
    0x80 + 4 * index
}
const fn sad_interleave_list(index: u16) -> u16 {
    0xc0 + 4 * index
}

/// Ironlake IOMMU units (coreboot `memmap.h`).
pub(super) const IOMMU_BASE1: u32 = 0xfed9_0000;
pub(super) const IOMMU_BASE2: u32 = 0xfed9_1000;
pub(super) const IOMMU_BASE4: u32 = 0xfed9_3000;

/// coreboot `CONFIG_SMM_TSEG_SIZE` for model 2065x: 8 MiB.
pub(super) const TSEG_SIZE_MIB: u32 = 8;
/// coreboot `DEFAULT_PCI_MMIO_SIZE`.
const DEFAULT_PCI_MMIO_SIZE_MIB: u32 = 2048;

/// SPD byte offsets raminit reads (coreboot's anonymous enum).
mod spd {
    pub const DEVICE_TYPE: usize = 2;
    pub const MODULE_TYPE: usize = 3;
    pub const DENSITY: usize = 4;
    pub const RANKS_AND_DQ: usize = 7;
    pub const MEMORY_BUS_WIDTH: usize = 8;
    pub const TIMEBASE_DIVIDEND: usize = 10;
    pub const TIMEBASE_DIVISOR: usize = 11;
    pub const CYCLETIME: usize = 12;
    pub const CAS_LATENCIES_LSB: usize = 14;
    pub const CAS_LATENCIES_MSB: usize = 15;
    pub const CAS_LATENCY_TIME: usize = 16;
    pub const THERMAL_AND_REFRESH: usize = 31;
    pub const REFERENCE_RAW_CARD_USED: usize = 62;
    pub const RANK1_ADDRESS_MAPPING: usize = 63;
}

/// SPD bytes raminit consumes (coreboot `useful_addresses`).
const USEFUL_SPD_ADDRESSES: [u8; 64] = [
    2, 3, 4, 7, 8, 10, 11, 12, 14, 15, 16, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
    0x1a, 0x1b, 0x1c, 0x1d, 31, 0x20, 62, 63, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d,
    0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
    0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0, 0, 0,
];
const USEFUL_SPD_COUNT: usize = 61;

/// The smallest and largest passing value of one lane timing.
#[derive(
    Clone, Copy, Default, Debug, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct Bound {
    pub smallest: u8,
    pub largest: u8,
}

/// `timing_bounds_t`: `[CHANNEL][SLOT][RANK][LANE]`.
pub type TimingBounds = [[[[Bound; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS];

/// Lane-indexed values: `[CHANNEL][SLOT][RANK][LANE]`.
pub type LaneValues = [[[[u16; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS];

/// coreboot `struct ram_training`: everything an S3 resume (and a cold
/// boot's cache check) replays verbatim. Its byte layout is the cache
/// record payload; explicit padding keeps it free of implicit holes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct RamTraining {
    /// `[TM][CHANNEL][SLOT][RANK][LANE]`.
    pub lane_timings: [LaneValues; 4],
    pub reg_178: u16,
    pub reg_10b: u16,
    pub reg178_center: u8,
    pub reg178_smallest: u8,
    pub reg178_largest: u8,
    _pad0: u8,
    pub timing_bounds: [TimingBounds; 2],
    pub timing_offset: LaneValues,
    pub timing2_offset: LaneValues,
    pub timing2_bounds: [[[[[u16; 2]; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
    /// `[CHANNEL][REGISTER]`.
    pub reg274265: [[u8; 3]; NUM_CHANNELS],
    pub reg2ca9_bit0: u8,
    _pad1: u8,
    pub reg_6dc: u32,
    pub reg_6e8: u32,
}

impl RamTraining {
    const fn zeroed() -> Self {
        Self {
            lane_timings: [[[[[0; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS]; 4],
            reg_178: 0,
            reg_10b: 0,
            reg178_center: 0,
            reg178_smallest: 0,
            reg178_largest: 0,
            _pad0: 0,
            timing_bounds: [[[[[Bound {
                smallest: 0,
                largest: 0,
            }; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS]; 2],
            timing_offset: [[[[0; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
            timing2_offset: [[[[0; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
            timing2_bounds: [[[[[0; 2]; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
            reg274265: [[0; 3]; NUM_CHANNELS],
            reg2ca9_bit0: 0,
            _pad1: 0,
            reg_6dc: 0,
            reg_6e8: 0,
        }
    }
}

/// The training cache record: the training plus the identity of the
/// modules it belongs to. coreboot keeps the training alone in its MRC
/// cache; the SPD digest additionally refuses training for other DIMMs.
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TrainingRecord {
    /// coreboot `MRC_CACHE_VERSION`.
    version: u32,
    spd_digest: [u8; 32],
    pub training: RamTraining,
}

/// coreboot `MRC_CACHE_VERSION`.
const RECORD_VERSION: u32 = 3;

impl TrainingRecord {
    /// The record if `bytes` hold training for these modules.
    fn parse(bytes: &[u8], spd_digest: &[u8; 32]) -> Option<RamTraining> {
        let record = Self::read_from_bytes(bytes).ok()?;
        (record.version == RECORD_VERSION && record.spd_digest == *spd_digest)
            .then_some(record.training)
    }
}

/// coreboot `struct raminfo`.
pub(super) struct RamInfo {
    /// Real (not DDR) clock / 133.(3) − 3.
    pub clock_speed_index: u16,
    /// In 1.(1)/2 MHz.
    pub fsb_frequency: u16,
    pub is_x16_module: [[u8; NUM_SLOTS]; NUM_CHANNELS],
    pub density: [[u8; NUM_SLOTS]; NUM_CHANNELS],
    pub populated_ranks: [[[u8; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
    pub cas_latency: u8,
    pub board_lane_delay: [u8; NUM_LANES],
    pub use_ecc: u8,
    pub revision: u8,
    pub max_supported_clock_speed_index: u8,
    pub uma_enabled: u8,
    pub spd: [[[u8; 151]; NUM_SLOTS]; NUM_CHANNELS],
    pub silicon_revision: u8,
    pub populated_ranks_mask: [u8; NUM_CHANNELS],
    pub max_slots_used_in_channel: u8,
    pub mode4030: [u8; NUM_CHANNELS],
    pub avg4044: [u16; NUM_CHANNELS],
    pub max4048: [u16; NUM_CHANNELS],
    pub total_memory_mb: u32,
    pub interleaved_part_mb: u32,
    pub non_interleaved_part_mb: u32,
    pub memory_reserved_for_heci_mb: u32,
    pub training: RamTraining,
    pub last_500_command: [u32; NUM_CHANNELS],
    pub delay46_ps: [u32; NUM_CHANNELS],
    pub delay54_ps: [u32; NUM_CHANNELS],
    pub revision_flag_1: u8,
    pub some_delay_1_cycle_floor: u8,
    pub some_delay_2_halfcycles_ceil: u8,
    pub some_delay_3_ps_rounded: u8,
    pub cached_training: Option<RamTraining>,
}

impl RamInfo {
    /// coreboot's `memset(&info, 0x5a, sizeof(info))`, then its explicit
    /// initializers.
    const fn new() -> Self {
        Self {
            clock_speed_index: 0x5a5a,
            fsb_frequency: 0x5a5a,
            is_x16_module: [[0x5a; NUM_SLOTS]; NUM_CHANNELS],
            density: [[0x5a; NUM_SLOTS]; NUM_CHANNELS],
            populated_ranks: [[[0; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS],
            cas_latency: 0x5a,
            board_lane_delay: [0x14, 0x07, 0x07, 0x08, 0x56, 0x04, 0x04, 0x05, 0x10],
            use_ecc: 0x5a,
            revision: 0x5a,
            max_supported_clock_speed_index: 0x5a,
            uma_enabled: 0x5a,
            spd: [[[0x5a; 151]; NUM_SLOTS]; NUM_CHANNELS],
            silicon_revision: 0x5a,
            populated_ranks_mask: [0x5a; NUM_CHANNELS],
            max_slots_used_in_channel: 0x5a,
            mode4030: [0x5a; NUM_CHANNELS],
            avg4044: [0x5a5a; NUM_CHANNELS],
            max4048: [0x5a5a; NUM_CHANNELS],
            total_memory_mb: 0x5a5a_5a5a,
            interleaved_part_mb: 0x5a5a_5a5a,
            non_interleaved_part_mb: 0x5a5a_5a5a,
            memory_reserved_for_heci_mb: 0,
            training: RamTraining::zeroed(),
            last_500_command: [0; NUM_CHANNELS],
            delay46_ps: [0x5a5a_5a5a; NUM_CHANNELS],
            delay54_ps: [0x5a5a_5a5a; NUM_CHANNELS],
            revision_flag_1: 0x5a,
            some_delay_1_cycle_floor: 0x5a,
            some_delay_2_halfcycles_ceil: 0x5a,
            some_delay_3_ps_rounded: 0x5a,
            cached_training: None,
        }
    }

    /// The time of a DDR transfer in ps.
    fn halfcycle_ps(&self) -> u32 {
        3750 / (u32::from(self.clock_speed_index) + 3)
    }

    /// The clock cycle in ps.
    fn cycle_ps(&self) -> u32 {
        2 * self.halfcycle_ps()
    }

    /// The FSB cycle in ps.
    fn fsbcycle_ps(&self) -> u32 {
        900_000 / u32::from(self.fsb_frequency)
    }

    /// Frequency in 1.(1) = 10/9 MHz units.
    fn frequency_11(&self) -> u32 {
        (u32::from(self.clock_speed_index) + 3) * 120
    }

    /// Frequency in 0.1 MHz units.
    fn frequency_01(&self) -> u32 {
        100 * self.frequency_11() / 9
    }

    fn ps_to_halfcycles(&self, ps: u32) -> u32 {
        (self.frequency_11() * 2) * ps / 900_000
    }

    fn ns_to_cycles(&self, ns: u32) -> u32 {
        self.frequency_11() * ns / 900
    }

    fn populated(&self, channel: usize, slot: usize, rank: usize) -> bool {
        self.populated_ranks[channel][slot][rank] != 0
    }

    /// coreboot `count_ranks_in_channel()`, including its use of
    /// `NUM_SLOTS` as the rank bound (both are 2).
    fn count_ranks_in_channel(&self, channel: usize) -> u32 {
        let mut count = 0;
        for slot in 0..NUM_SLOTS {
            for rank in 0..NUM_SLOTS {
                count += u32::from(self.populated_ranks[channel][slot][rank]);
            }
        }
        count
    }

    /// coreboot `extended_silicon_revision`: revision 0 with registered
    /// (SO-UDIMM type 3) modules is treated as revision 4.
    fn extended_silicon_revision(&self) -> usize {
        let mut revision = usize::from(self.silicon_revision);
        if self.silicon_revision == 0 {
            for channel in 0..NUM_CHANNELS {
                for slot in 0..NUM_SLOTS {
                    if self.spd[channel][slot][spd::MODULE_TYPE] & 0xf == 3 {
                        revision = 4;
                    }
                }
            }
        }
        revision
    }
}

/// `FOR_POPULATED_RANKS`: channel, slot, rank in ascending order.
fn populated_ranks(info: &RamInfo) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    (0..NUM_CHANNELS).flat_map(move |channel| {
        (0..NUM_SLOTS).flat_map(move |slot| {
            (0..NUM_RANKS)
                .filter(move |&rank| info.populated(channel, slot, rank))
                .map(move |rank| (channel, slot, rank))
        })
    })
}

/// `FOR_POPULATED_RANKS_BACKWARDS`: channels descending.
fn populated_ranks_backwards(info: &RamInfo) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    (0..NUM_CHANNELS).rev().flat_map(move |channel| {
        (0..NUM_SLOTS).flat_map(move |slot| {
            (0..NUM_RANKS)
                .filter(move |&rank| info.populated(channel, slot, rank))
                .map(move |rank| (channel, slot, rank))
        })
    })
}

/// coreboot `get_lane_offset()`.
const fn get_lane_offset(slot: usize, rank: usize, lane: usize) -> u16 {
    (0x124 * lane as i32
        + if lane & 4 != 0 { 0x23e } else { 0 }
        + 11 * rank as i32
        + 22 * slot as i32
        - 0x452 * (lane == 8) as i32) as u16
}

/// coreboot `get_timing_register_addr()`.
const fn get_timing_register_addr(lane: usize, tm: usize, slot: usize, rank: usize) -> u16 {
    const OFFS: [u16; 4] = [0x1d, 0xa8, 0xe6, 0x5c];
    get_lane_offset(slot, rank, lane) + OFFS[(tm + 3) % 4]
}

/// The physical memory raminit tests through.
fn read32p(address: u32) -> u32 {
    // SAFETY: the bootstrap identity map covers the low 4 GiB; raminit only
    // touches ranks it has just mapped.
    unsafe { fstart_core::mmio::read32(address as usize as *const u32) }
}

fn write32p(address: u32, value: u32) {
    // SAFETY: as for read32p.
    unsafe { fstart_core::mmio::write32(address as usize as *mut u32, value) }
}

fn sfence() {
    // SAFETY: a store fence.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

/// Stop the boot: coreboot `die()`.
fn die(message: &str) -> ! {
    fstart_log::error!("ironlake raminit: {}", message);
    fstart_log::flush();
    fstart_arch::x86::boot::halt()
}

/// One raminit run: the uncore windows and the information gathered.
pub(super) struct Raminit<'a> {
    pub info: &'a mut RamInfo,
    mch: MchBar,
    dmi: DmiBar,
    ep: EpBar,
    rcba: usize,
    gpiobase: u16,
    pmbase: u16,
    hecibar: usize,
    qpi_bus: u8,
}

/// Inputs from the chipset bindings.
pub(super) struct RaminitConfig {
    pub mchbar: usize,
    pub dmibar: usize,
    pub epbar: usize,
    pub rcba: usize,
    pub gpiobase: u16,
    pub pmbase: u16,
    pub hecibar: usize,
    pub qpi_bus: u8,
    /// IGD UMA size selector (coreboot CMOS `gfx_uma_size`, 0 = 32 MiB).
    pub gfx_uma_size: u8,
}

/// What raminit reports to the northbridge.
pub(super) struct Trained {
    pub record: Option<TrainingRecord>,
    pub total_memory_mb: u32,
    pub fsb_frequency: u16,
    pub clock_speed_index: u16,
}

/// The raminit working set, in CAR: coreboot keeps `timings_car` there too.
pub(super) struct RamState {
    info: RamInfo,
    timings: [TimingBounds; 64],
}

impl RamState {
    pub(super) const fn new() -> Self {
        Self {
            info: RamInfo::new(),
            timings: [[[[[Bound {
                smallest: 0,
                largest: 0,
            }; NUM_LANES]; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS]; 64],
        }
    }
}

fn northbridge() -> EcamDevice {
    EcamDevice::new(0, 0, 0)
}

fn southbridge() -> EcamDevice {
    EcamDevice::new(0, 0x1f, 0)
}

fn gma() -> EcamDevice {
    EcamDevice::new(0, 2, 0)
}

impl<'a> Raminit<'a> {
    pub(super) fn new(info: &'a mut RamInfo, config: &RaminitConfig) -> Self {
        Self {
            info,
            mch: MchBar(config.mchbar),
            dmi: DmiBar(config.dmibar),
            ep: EpBar(config.epbar),
            rcba: config.rcba,
            gpiobase: config.gpiobase,
            pmbase: config.pmbase,
            hecibar: config.hecibar,
            qpi_bus: config.qpi_bus,
        }
    }

    fn qpi_non_core(&self) -> EcamDevice {
        EcamDevice::new(self.qpi_bus, 0, 0)
    }
    fn qpi_sad(&self) -> EcamDevice {
        EcamDevice::new(self.qpi_bus, 0, 1)
    }
    fn qpi_link0(&self) -> EcamDevice {
        EcamDevice::new(self.qpi_bus, 2, 0)
    }
    fn qpi_phy0(&self) -> EcamDevice {
        EcamDevice::new(self.qpi_bus, 2, 1)
    }

    /// Spin until `bit` of an MCHBAR dword clears.
    fn wait_clear(&self, reg: u32, bit: u32) {
        if !fstart_arch::wait_us(1_000_000, || self.mch.read32(reg) & (1 << bit) == 0) {
            die("MCHBAR busy bit stuck");
        }
    }

    /// Write the global scan chain (coreboot `write_1d0()`).
    fn write_1d0(&self, val: u32, addr: u16, bits: u32, flag: u32) {
        self.mch.write32(0x1d0, 0);
        self.wait_clear(0x1d0, 23);
        self.mch.write32(
            0x1d4,
            (val & ((1 << bits) - 1)) | (2 << bits) | (flag << bits),
        );
        self.mch.write32(0x1d0, (1 << 30) | u32::from(addr));
        self.wait_clear(0x1d0, 23);
    }

    /// Read the global scan chain (coreboot `read_1d0()`).
    fn read_1d0(&self, addr: u16, split: u32) -> u16 {
        self.mch.write32(0x1d0, 0);
        self.wait_clear(0x1d0, 23);
        let select = (u32::from(self.mch.read8(0x246) >> 2) & 3) + 0x361 - u32::from(addr);
        self.mch.write32(0x1d0, (1 << 31) | select);
        self.wait_clear(0x1d0, 23);
        let val = self.mch.read32(0x1d8);
        self.write_1d0(0, 0x33d, 0, 0);
        self.write_1d0(0, 0x33d, 0, 0);
        (val & ((1 << split) - 1)) as u16
    }

    fn rmw_1d0(&self, addr: u16, and: u32, or: u32, split: u32) {
        let value = u32::from(self.read_1d0(addr, split));
        self.write_1d0((value & and) | or, addr, split, 1);
    }

    /// Read a channel's scan chain (coreboot `read_500()`).
    fn read_500(&mut self, channel: usize, addr: u16, split: u32) -> u16 {
        let base = 0x500 + ((channel as u32) << 10);
        self.info.last_500_command[channel] = 1 << 31;
        self.mch.write32(base, 0);
        self.wait_clear(base, 23);
        let select = (u32::from(self.mch.read8(0x246 + ((channel as u32) << 10)) >> 2) & 3) + 0xb88
            - u32::from(addr);
        self.mch.write32(base, (1 << 31) | select);
        self.wait_clear(base, 23);
        let val = self.mch.read32(0x508 + ((channel as u32) << 10));
        (val & ((1 << split) - 1)) as u16
    }

    /// Write a channel's scan chain (coreboot `write_500()`).
    fn write_500(&mut self, channel: usize, val: u32, addr: u16, bits: u32, flag: u32) {
        if self.info.last_500_command[channel] == 1 << 31 {
            self.info.last_500_command[channel] = 1 << 30;
            self.write_500(channel, 0, 0xb61, 0, 0);
        }
        let base = 0x500 + ((channel as u32) << 10);
        self.mch.write32(base, 0);
        self.wait_clear(base, 23);
        self.mch.write32(
            0x504 + ((channel as u32) << 10),
            (val & ((1 << bits) - 1)) | (2 << bits) | (flag << bits),
        );
        self.mch.write32(base, (1 << 30) | u32::from(addr));
        self.wait_clear(base, 23);
    }

    fn rmw_500(&mut self, channel: usize, addr: u16, bits: u32, and: u32, or: u32) {
        let val = u32::from(self.read_500(channel, addr, bits)) & and;
        self.write_500(channel, val | or, addr, bits, 1);
    }

    /// coreboot `rw_test()`: a quick pattern test of one rank.
    fn rw_test(&self, rank: u32) -> u32 {
        const MASK: u32 = 0xf00f_c33c;
        let base = rank << 28;
        let mut ok = 0xff;
        for i in 0..64 {
            write32p(base | (i << 2), 0);
        }
        sfence();
        for i in 0..64 {
            read32p(base | (i << 2));
        }
        sfence();
        for i in 0..32 {
            let pattern = if (MASK >> i) & 1 != 0 { 0xffff_ffff } else { 0 };
            write32p(base | (i << 3), pattern);
            write32p(base | (i << 3) | 4, pattern);
        }
        sfence();
        for i in 0..32 {
            let pattern = if (MASK >> i) & 1 != 0 { 0xff } else { 0 };
            let val = read32p(base | (i << 3));
            for j in 0..4 {
                if (val >> (j * 8)) & 0xff != pattern {
                    ok &= !(1 << j);
                }
            }
            let val = read32p(base | (i << 3) | 4);
            for j in 0..4 {
                if (val >> (j * 8)) & 0xff != pattern {
                    ok &= !(16 << j);
                }
            }
        }
        sfence();
        for i in 0..64 {
            write32p(base | (i << 2), 0);
        }
        sfence();
        for i in 0..64 {
            read32p(base | (i << 2));
        }
        ok
    }

    fn program_timings(&mut self, base: u16, channel: usize, slot: usize, rank: usize) {
        for lane in 0..8 {
            let tm2 = self.info.training.lane_timings[2][channel][slot][rank][lane];
            let tm3 = self.info.training.lane_timings[3][channel][slot][rank][lane];
            self.write_500(
                channel,
                u32::from(base.wrapping_add(tm2)),
                get_timing_register_addr(lane, 2, slot, rank),
                9,
                0,
            );
            self.write_500(
                channel,
                u32::from(base.wrapping_add(tm3)),
                get_timing_register_addr(lane, 3, slot, rank),
                9,
                0,
            );
        }
    }

    fn write_26c(&self, channel: u32, si: u16) {
        self.mch.write32(0x26c + (channel << 10), 0x0324_3f35);
        self.mch
            .write32(0x268 + (channel << 10), 0xcfc0_0000 | (u32::from(si) << 9));
        self.mch.write16(0x2b9 + (channel << 10), si);
    }

    fn toggle_1d0_142_5ff(&self) {
        let reg32 = u32::from(self.read_1d0(0x142, 3));
        if reg32 & (1 << 1) != 0 {
            self.write_1d0(0, 0x142, 3, 1);
        }
        self.mch.write8(0x5ff, 0);
        self.mch.write8(0x5ff, 1 << 7);
        if reg32 & (1 << 1) != 0 {
            self.write_1d0(0x2, 0x142, 3, 1);
        }
    }

    fn get_580(&self, channel: u32, addr: u8) -> u32 {
        self.toggle_1d0_142_5ff();
        let reg = 0x580 + (channel << 10);
        self.mch.write32(reg, 0x8493_c012 | u32::from(addr));
        self.mch.setbits8(reg, 1 << 0);
        let mut ret = 0;
        if !fstart_arch::wait_us(1_000_000, || {
            ret = self.mch.read32(reg);
            ret & (1 << 16) != 0
        }) {
            die("MCHBAR 0x580 completion stuck");
        }
        self.mch.clrbits8(reg, 1 << 0);
        ret
    }

    fn seq9(&mut self, channel: usize, slot: usize, rank: usize) {
        for i in 0..2 {
            for lane in 0..8 {
                let value = self.info.training.lane_timings[i + 1][channel][slot][rank][lane];
                self.write_500(
                    channel,
                    u32::from(value),
                    get_timing_register_addr(lane, i + 1, slot, rank),
                    9,
                    0,
                );
            }
        }
        self.write_1d0(1, 0x103, 6, 1);
        for lane in 0..8 {
            let value = self.info.training.lane_timings[0][channel][slot][rank][lane];
            self.write_500(
                channel,
                u32::from(value),
                get_timing_register_addr(lane, 0, slot, rank),
                9,
                0,
            );
        }
        for i in 0..2 {
            for lane in 0..8 {
                let value = self.info.training.lane_timings[i + 1][channel][slot][rank][lane];
                self.write_500(
                    channel,
                    u32::from(value),
                    get_timing_register_addr(lane, i + 1, slot, rank),
                    9,
                    0,
                );
            }
            self.get_580(channel as u32, (((i + 1) << 2) | (rank << 5)) as u8);
        }
        self.toggle_1d0_142_5ff();
        self.write_1d0(0x2, 0x142, 3, 1);
        for lane in 0..8 {
            let value = self.read_500(channel, get_timing_register_addr(lane, 2, slot, rank), 9);
            self.info.training.lane_timings[2][channel][slot][rank][lane] = value;
            self.info.training.lane_timings[3][channel][slot][rank][lane] = value + 0x20;
        }
    }

    fn config_rank(&mut self, s3resume: bool, channel: usize, slot: usize, rank: usize) {
        self.write_1d0(0, 0x178, 7, 1);
        self.seq9(channel, slot, rank);
        self.program_timings(0x80, channel, slot, rank);
        let add = if channel == 0 {
            self.info.count_ranks_in_channel(1)
        } else {
            0
        };
        let total = rank as u32 + add;
        if !s3resume {
            self.rw_test(total);
        }
        self.program_timings(0x00, channel, slot, rank);
        if !s3resume {
            self.rw_test(total);
            self.rw_test(total);
        }
        self.write_1d0(0, 0x142, 3, 1);
        self.write_1d0(0, 0x103, 6, 1);
        self.get_580(channel as u32, (0xc | (rank << 5)) as u8);
        self.read_1d0(0x142, 3);
        self.mch.write8(0x5ff, 0);
        self.mch.write8(0x5ff, 1 << 7);
    }

    fn set_4cf(&mut self, channel: usize, bit: u32, val: u32) {
        const REGTABLE: [u16; 3] = [0x4cf, 0x659, 0x697];
        let val = val & 1;
        for reg in REGTABLE {
            self.rmw_500(channel, reg, 4, !(1 << bit), val << bit);
        }
    }

    fn set_334(&self, zero: bool) {
        const VAL3: [u32; 4] = [0x2a2b_2a2b, 0x2627_2627, 0x2e2f_2e2f, 0x2a2b];
        for channel in 0..NUM_CHANNELS as u32 {
            for j in 0..4u32 {
                let a = if j == 1 { 0x2929_2929 } else { 0x3131_3131 };
                let lmask = if j == 3 { 0xffff } else { 0xffff_ffff };
                let c: u16 = if (j == 0 || j == 3) && zero {
                    0
                } else if j == 3 {
                    0x5f
                } else {
                    0x5f5f
                };
                for k in 0..2u32 {
                    self.mch.write32(0x138 + 8 * k, (channel << 26) | (j << 24));
                    self.mch.read32(0x138 + 8 * k);
                    self.mch.read32(0x13c + 8 * k);
                }
                let group = (channel << 10) + j * 0x44;
                self.mch
                    .write32(0x334 + group, if zero { 0 } else { VAL3[j as usize] });
                self.mch
                    .write32(0x32c + group, if zero { 0 } else { 0x1819_1819 & lmask });
                self.mch.write16(0x34a + group, c);
                self.mch
                    .write32(0x33c + group, if zero { 0 } else { a & lmask });
                self.mch
                    .write32(0x344 + group, if zero { 0 } else { a & lmask });
            }
        }
        self.mch.setbits32(0x130, 1 << 0);
        if !fstart_arch::wait_us(1_000_000, || self.mch.read8(0x130) & 1 == 0) {
            die("MCHBAR 0x130 busy");
        }
    }

    /// Spin until bit 0 of MCHBAR 0x130 clears.
    fn wait_130(&self) {
        if !fstart_arch::wait_us(1_000_000, || self.mch.read32(0x130) & 1 == 0) {
            die("MCHBAR 0x130 busy");
        }
    }
}

/// coreboot `find_highest_bit_set()`.
fn find_highest_bit_set(val: u16) -> i32 {
    (0..16)
        .rev()
        .find(|bit| val & (1 << bit) != 0)
        .unwrap_or(-1)
}

/// coreboot `find_lowest_bit_set32()`.
fn find_lowest_bit_set32(val: u32) -> i32 {
    (0..32).find(|bit| val & (1 << bit) != 0).unwrap_or(-1)
}

/// coreboot `get_bits_420()`.
fn get_bits_420(reg32: u32) -> u32 {
    ((reg32 >> 4) & 1) | ((reg32 >> 2) & 2) | (reg32 & 4)
}

/// A digest of every SPD byte raminit read, binding cached training to
/// the installed modules.
fn spd_digest(info: &RamInfo) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for channel in 0..NUM_CHANNELS {
        hasher.update([info.populated_ranks[channel][0][0]]);
        for &addr in &USEFUL_SPD_ADDRESSES[..USEFUL_SPD_COUNT] {
            hasher.update([info.spd[channel][0][usize::from(addr)]]);
        }
    }
    hasher.finalize().into()
}

impl Raminit<'_> {
    /// Read the SPDs (coreboot `raminit()` SPD loop). Like coreboot, every
    /// module's bytes go to the channel's slot 0 entry: one DIMM per channel.
    fn read_spds(&mut self, smbus: &mut dyn SmBus, spd_map: &[u8; 4]) -> Result<(), ServiceError> {
        let info = &mut *self.info;
        info.populated_ranks = [[[0; NUM_RANKS]; NUM_SLOTS]; NUM_CHANNELS];
        info.use_ecc = 1;
        for channel in 0..NUM_CHANNELS {
            for slot in 0..NUM_SLOTS {
                let address = spd_map[2 * channel + slot];
                if address == 0 {
                    continue;
                }
                let present =
                    (0..5).any(|_| smbus.read_byte(address, spd::DEVICE_TYPE as u8).is_ok());
                if !present {
                    continue;
                }
                for &byte in &USEFUL_SPD_ADDRESSES[..USEFUL_SPD_COUNT] {
                    info.spd[channel][0][usize::from(byte)] =
                        smbus.read_byte(address, byte).unwrap_or(0xff);
                }
                if info.spd[channel][0][spd::DEVICE_TYPE] != 11 {
                    fstart_log::error!("ironlake: only DDR3 is supported");
                    return Err(ServiceError::NotSupported);
                }
                let v = info.spd[channel][0][spd::RANKS_AND_DQ];
                info.populated_ranks[channel][0][0] = 1;
                info.populated_ranks[channel][0][1] = (v >> 3) & 7;
                if (v >> 3) & 7 > 1 {
                    fstart_log::error!("ironlake: at most 2 ranks are supported");
                    return Err(ServiceError::NotSupported);
                }
                if v & 7 == 0 || v & 7 > 2 {
                    fstart_log::error!("ironlake: only x8 and x16 modules are supported");
                    return Err(ServiceError::NotSupported);
                }
                let module_type = info.spd[channel][slot][spd::MODULE_TYPE] & 0xf;
                if module_type != 2 && module_type != 3 {
                    fstart_log::error!("ironlake: registered memory is not supported");
                    return Err(ServiceError::NotSupported);
                }
                info.is_x16_module[channel][0] = (v & 7) - 1;
                info.density[channel][slot] = info.spd[channel][slot][spd::DENSITY] & 0xf;
                if info.spd[channel][slot][spd::MEMORY_BUS_WIDTH] & 0x18 == 0 {
                    info.use_ecc = 0;
                }
            }
        }
        for channel in 0..NUM_CHANNELS {
            let mut mask = 0;
            for slot in 0..NUM_SLOTS {
                for rank in 0..NUM_RANKS {
                    mask |= info.populated_ranks[channel][slot][rank] << (2 * slot + rank);
                }
            }
            info.populated_ranks_mask[channel] = mask;
        }
        if info.populated_ranks_mask == [0, 0] {
            fstart_log::error!("ironlake: no DIMM found");
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    /// coreboot `dmi_setup()`.
    fn dmi_setup(&self) {
        self.dmi.read8(0x254);
        self.dmi.write8(0x254, 1 << 0);
        self.dmi.write16(0x1b8, 0x18f2);
        self.mch.clrsetbits16(0x48, !0, 1 << 1);
        self.dmi.setbits32(0xd68, 1 << 27);
        // SAFETY: GPIOBASE is decoded since the southbridge's early init.
        unsafe {
            let level2 = fstart_core::pio::inl(self.gpiobase | 0x38);
            fstart_core::pio::outl(self.gpiobase | 0x38, (level2 & !0x14_0000) | 0x40_0000);
            fstart_core::pio::inb(self.gpiobase | 0xe);
        }
    }

    /// coreboot `chipset_init()`: soft-reset detection, DMI, IGD stolen
    /// memory and the graphics clock setup.
    fn chipset_init(&self, s3resume: bool, gfx_uma_size: u8) {
        let x2ca8 = self.mch.read8(0x2ca8);
        if x2ca8 & 1 != 0 || (x2ca8 == 8 && !s3resume) {
            fstart_log::info!("ironlake: soft reset detected, rebooting properly");
            self.mch.write8(0x2ca8, 0);
            fstart_arch::x86::boot::system_reset(true);
        }
        self.dmi_setup();
        self.mch.write16(0x1170, 0xa880);
        self.mch.write8(0x11c1, 1 << 0);
        self.mch.write16(0x1170, 0xb880);
        self.mch.clrsetbits8(0x1210, !0, 0x84);
        let ggc = 0xb00 | ((u16::from(gfx_uma_size) + 5) << 4);
        let nb = northbridge();
        nb.write16(GGC, ggc | 2);
        let deven = nb.read16(DEVEN);
        if deven & 8 != 0 {
            self.mch.write8(0x2c30, 1 << 5);
            nb.read8(0x8);
            self.mch.setbits16(0x2c30, 1 << 9);
            self.mch.write16(0x2c32, 0x434);
            self.mch.clrsetbits32(0x2c44, !0, 0x0105_3687);
            gma().read8(MSAC);
            gma().write8(MSAC, 0x2);
            let rcba = crate::ibexpeak::RcbaWindow(self.rcba);
            rcba.read8(0x2318);
            rcba.write8(0x2318, 0x47);
            rcba.read8(0x2320);
            rcba.write8(0x2320, 0xfc);
        }
        self.mch.clrsetbits32(0x30, !0, 0x40);
        nb.write16(GGC, ggc);
        let rcba = crate::ibexpeak::RcbaWindow(self.rcba);
        rcba.read32(0x3428);
        rcba.write32(0x3428, 0x1d);
    }
}

/// coreboot `raminit()`. Never returns on the cold path's first pass: the
/// QuickPath reconfiguration needs a CPU-only reset, after which the
/// bootblock runs again and this resumes with `MCHBAR 0x2ca8 != 0`.
pub(super) fn run(
    state: &mut RamState,
    config: &RaminitConfig,
    smbus: &mut dyn SmBus,
    spd_map: &[u8; 4],
    s3resume: bool,
    cached: Option<&[u8]>,
) -> Result<Trained, ServiceError> {
    state.info = RamInfo::new();
    let RamState { info, timings } = state;
    let mut ram = Raminit::new(info, config);
    // coreboot romstage: chipset_init() right before raminit(); the board's
    // pre-raminit clock programming ran before this phase.
    ram.chipset_init(s3resume, config.gfx_uma_size);
    ram.raminit(smbus, spd_map, s3resume, cached, timings)
}

impl Raminit<'_> {
    #[allow(clippy::too_many_lines)]
    fn raminit(
        &mut self,
        smbus: &mut dyn SmBus,
        spd_map: &[u8; 4],
        s3resume: bool,
        cached: Option<&[u8]>,
        timings: &mut [TimingBounds; 64],
    ) -> Result<Trained, ServiceError> {
        let x2ca8 = self.mch.read8(0x2ca8);
        fstart_log::info!(
            "ironlake: scratchpad MCHBAR 0x2ca8 = {:#x}",
            u32::from(x2ca8)
        );

        // Wait for some bit, maybe TXT clear.
        // SAFETY: fixed TPM locality-0 access register, read only.
        let txt =
            || unsafe { fstart_core::mmio::read8(0xfed4_0000usize as *const u8) } & (1 << 7) != 0;
        if !fstart_arch::wait_us(1_000_000, txt) {
            fstart_log::error!("ironlake: TXT/TPM access bit never set");
            return Err(ServiceError::Timeout);
        }

        // Wait for the ME.
        if crate::ibexpeak::me::early_init().is_err() {
            fstart_log::error!("ironlake: continuing without a ready ME");
        }
        self.info.memory_reserved_for_heci_mb = crate::ibexpeak::me::early_uma_size();

        self.read_spds(smbus, spd_map)?;
        let digest = spd_digest(self.info);
        northbridge().read32(CAPID0 + 4);

        self.mch.clrbits8(0x2ca8, (1 << 1) | (1 << 0));
        self.collect_system_info();
        self.calculate_timings()?;

        if !s3resume {
            let pmcon2 = southbridge().read8(GEN_PMCON_2);
            if x2ca8 == 0 && pmcon2 & 0x80 != 0 {
                // Don't enable S4-assertion stretch; clear bit 7.
                southbridge().write8(GEN_PMCON_2, pmcon2 & !(1 << 7));
                fstart_log::info!("ironlake: interrupted RAM init, reset required");
                fstart_arch::x86::boot::system_reset(true);
            }
        }
        if !s3resume && x2ca8 == 0 {
            let sb = southbridge();
            sb.write8(GEN_PMCON_2, sb.read8(GEN_PMCON_2) | 0x80);
        }

        self.compute_derived_timings();
        self.early_quickpath_init(x2ca8);

        self.info.cached_training = cached.and_then(|bytes| TrainingRecord::parse(bytes, &digest));

        if x2ca8 == 0 {
            self.late_quickpath_init(s3resume);
        }

        self.mch.setbits32(0x2c80, 1 << 24);
        self.mch
            .write32(0x1804, self.mch.read32(0x1c04) & !(1 << 27));
        self.mch.read8(0x2ca8);

        if x2ca8 == 0 {
            self.mch.clrbits8(0x2ca8, 3);
            self.mch
                .write8(0x2ca8, self.mch.read8(0x2ca8).wrapping_add(4));
            // A CPU reset without resetting the platform.
            fstart_log::info!("ironlake: issuing a CPU reset");
            // The reset CPU must take the S3 path again.
            if s3resume {
                let pm = crate::southbridge::pmio_ich::PmIo::new(self.pmbase);
                pm.write32(
                    crate::southbridge::pmio_ich::PM1_CNT,
                    pm.read32(crate::southbridge::pmio_ich::PM1_CNT) | (5 << 10),
                );
            }
            fstart_log::flush();
            self.mch.setbits32(0x1af0, 1 << 4);
            fstart_arch::x86::boot::halt();
        }

        self.mch.clrbits8(0x2ca8, 0);
        self.mch.clrbits32(0x2c80, 1 << 24);
        self.qpi_non_core().write32(MAX_RTIDS, 0x20220);
        {
            let x2c20 = (self.mch.read16(0x2c20) >> 8) & 3;
            let x2c10 = self.mch.read16(0x2c10);
            let mut value = self.mch.read16(0x2c00);
            if x2c20 == 0 && x2c10 & 0x300 == 0 {
                value |= 1 << 7;
            } else {
                value &= !(1 << 0);
            }
            self.mch.write16(0x2c00, value);
        }
        fstart_arch::x86::udelay(1000);

        self.write_1d0(0, 0x33d, 0, 0);
        self.write_500(0, 0, 0xb61, 0, 0);
        self.write_500(1, 0, 0xb61, 0, 0);
        self.mch.write32(0x1a30, 0);
        self.mch.write32(0x1a34, 0);
        self.mch.write16(
            0x614,
            0xb5b
                | (u16::from(self.info.populated_ranks[1][0][0]) * 0x404)
                | (u16::from(self.info.populated_ranks[0][0][0]) * 0xa0),
        );
        self.mch.write16(0x616, 0x26a);
        self.mch.write32(0x134, 0x85_6000);
        self.mch.write32(0x160, 0x5ff_ffff);
        self.mch.clrsetbits32(0x114, !0, 0xc202_4440);
        self.mch.clrsetbits32(0x118, !0, 0x4);
        for channel in 0..NUM_CHANNELS {
            self.mch.write32(
                0x260 + ((channel as u32) << 10),
                0x30809ff | (u32::from(self.info.populated_ranks_mask[channel] & 3) << 20),
            );
        }
        for channel in 0..NUM_CHANNELS as u32 {
            let base = channel << 10;
            self.mch.write16(0x31c + base, 0x101);
            self.mch.write16(0x360 + base, 0x909);
            self.mch.write16(0x3a4 + base, 0x101);
            self.mch.write16(0x3e8 + base, 0x101);
            self.mch.write32(0x320 + base, 0x2900_2900);
            self.mch.write32(0x324 + base, 0);
            self.mch.write32(0x368 + base, 0x3200_3200);
            self.mch.write16(0x352 + base, 0x505);
            self.mch.write16(0x354 + base, 0x3c3c);
            self.mch.write16(0x356 + base, 0x1040);
            self.mch.write16(0x39a + base, 0x73e4);
            self.mch.write16(0x3de + base, 0x77ed);
            self.mch.write16(0x422 + base, 0x1040);
        }

        self.write_1d0(0x4, 0x151, 4, 1);
        self.write_1d0(0, 0x142, 3, 1);
        // SAFETY: model 2065x implements MSR_TURBO_POWER_CURRENT_LIMIT.
        unsafe { fstart_arch::x86::msr::rdmsr(TURBO_POWER_CURRENT_LIMIT.index()) };
        self.write_500(1, 1, 0x6b3, 4, 1);
        self.write_500(1, 1, 0x6cf, 4, 1);

        self.rmw_1d0(0x21c, 0x38, 0, 6);

        self.write_1d0(
            (u32::from(self.info.populated_ranks[1][0][0] == 0) << 1)
                | u32::from(self.info.populated_ranks[0][0][0] == 0),
            0x1d1,
            3,
            1,
        );
        for channel in 0..NUM_CHANNELS as u32 {
            self.mch.write16(0x38e + (channel << 10), 0x5f5f);
            self.mch.write16(0x3d2 + (channel << 10), 0x5f5f);
        }

        self.set_334(false);
        self.program_base_timings();
        self.mch.setbits8(0x5ff, 1 << 7);

        self.write_1d0(0x2, 0x1d5, 2, 1);
        self.write_1d0(0x20, 0x166, 7, 1);
        self.write_1d0(0x0, 0xeb, 3, 1);
        self.write_1d0(0x0, 0xf3, 6, 1);

        for channel in 0..NUM_CHANNELS {
            let mut a = 0;
            if self.info.populated_ranks[channel][0][1] != 0 && self.info.clock_speed_index > 1 {
                a = 3;
            }
            if self.info.silicon_revision == 0 || self.info.silicon_revision == 1 {
                a = 3;
            }
            for lane in 0..9 {
                let addr = 0x125 + get_lane_offset(0, 0, lane);
                self.rmw_500(channel, addr, 6, 0xf, a);
            }
        }

        if s3resume {
            let Some(training) = self.info.cached_training else {
                fstart_log::error!("ironlake: no training data for S3 resume, rebooting");
                let pm = crate::southbridge::pmio_ich::PmIo::new(self.pmbase);
                let pm1 = pm.read32(crate::southbridge::pmio_ich::PM1_CNT);
                pm.write32(crate::southbridge::pmio_ich::PM1_CNT, pm1 & !(7 << 10));
                fstart_log::flush();
                // SAFETY: the reset-control port.
                unsafe { fstart_core::pio::outb(0xcf9, 0x0e) };
                fstart_arch::x86::boot::halt();
            };
            self.info.training = training;
            for tm in 0..4 {
                for channel in 0..NUM_CHANNELS {
                    for slot in 0..NUM_SLOTS {
                        for rank in 0..NUM_RANKS {
                            for lane in 0..9 {
                                let value =
                                    self.info.training.lane_timings[tm][channel][slot][rank][lane];
                                self.write_500(
                                    channel,
                                    u32::from(value),
                                    get_timing_register_addr(lane, tm, slot, rank),
                                    9,
                                    0,
                                );
                            }
                        }
                    }
                }
            }
            self.write_1d0(u32::from(training.reg_178), 0x178, 7, 1);
            self.write_1d0(u32::from(training.reg_10b), 0x10b, 6, 1);
        }

        self.mch.clrsetbits32(0x1f4, !0, 1 << 17);
        self.mch.write32(0x1f0, 0x1d00_0200);
        self.mch.setbits8(0x1f0, 1 << 0);
        if !fstart_arch::wait_us(1_000_000, || self.mch.read8(0x1f0) & 1 == 0) {
            die("MCHBAR 0x1f0 busy");
        }

        self.program_board_delay();

        self.mch.write8(0x5ff, 0);
        self.mch.write8(0x5ff, 1 << 7);
        self.mch.write8(0x5f4, 1 << 0);

        self.mch.clrbits32(0x130, 1 << 1);
        self.wait_130();

        self.rmw_1d0(0x14b, 0x47, 0x30, 7);
        self.rmw_1d0(0xd6, 0x38, 7, 6);
        self.rmw_1d0(0x328, 0x38, 7, 6);

        for channel in 0..NUM_CHANNELS {
            self.set_4cf(channel, 1, 0);
        }

        self.rmw_1d0(0x116, 0xe, 0, 4);
        self.rmw_1d0(0xae, 0x3e, 0, 6);
        self.rmw_1d0(0x300, 0x3e, 0, 6);
        self.mch.clrbits16(0x356, 1 << 15);
        self.mch.clrbits16(0x756, 1 << 15);
        self.mch.clrbits32(0x140, 7 << 24);
        self.mch.clrbits32(0x138, 7 << 24);
        self.mch.write32(0x130, 0x3111_1301);
        self.wait_130();

        let mut value_a1;
        {
            let val_xa1 = get_bits_420(u32::from(self.read_1d0(0xa1, 6)));
            let val_2f3 = get_bits_420(u32::from(self.read_1d0(0x2f3, 6)));
            value_a1 = val_xa1;
            self.rmw_1d0(0x320, 0x38, val_2f3, 6);
            self.rmw_1d0(0x14b, 0x78, val_xa1, 7);
            self.rmw_1d0(0xce, 0x38, val_xa1, 6);
        }

        for channel in 0..NUM_CHANNELS {
            self.set_4cf(channel, 1, 1);
        }

        self.rmw_1d0(0x116, 0xe, 1, 4);
        {
            if self.mch.read32(0x144) & 0x1f < 0x13 {
                value_a1 += 2;
            } else {
                value_a1 += 1;
            }
            if value_a1 > 7 {
                value_a1 = 7;
            }
            self.write_1d0(2, 0xae, 6, 1);
            self.write_1d0(2, 0x300, 6, 1);
            self.write_1d0(value_a1, 0x121, 3, 1);
            self.rmw_1d0(0xd6, 0x38, 4, 6);
            self.rmw_1d0(0x328, 0x38, 4, 6);
        }

        for channel in 0..NUM_CHANNELS {
            self.set_4cf(channel, 2, 0);
        }

        self.mch.write32(
            0x130,
            0x1111_1301
                | (u32::from(self.info.populated_ranks[1][0][0]) << 30)
                | (u32::from(self.info.populated_ranks[0][0][0]) << 29),
        );
        if !fstart_arch::wait_us(1_000_000, || self.mch.read8(0x130) & 1 == 0) {
            die("MCHBAR 0x130 busy");
        }

        {
            let val_xa1 = get_bits_420(u32::from(self.read_1d0(0xa1, 6)));
            self.read_1d0(0x2f3, 6);
            self.rmw_1d0(0x21c, 0x38, 0, 6);
            self.rmw_1d0(0x14b, 0x78, val_xa1, 7);
        }

        for channel in 0..NUM_CHANNELS {
            self.set_4cf(channel, 2, 1);
        }

        self.set_334(true);

        self.mch.write8(0x1e8, 1 << 2);

        for channel in 0..NUM_CHANNELS {
            let mask = u32::from(self.info.populated_ranks_mask[channel]);
            self.write_500(channel, 0x3 & !mask, 0x6b7, 2, 1);
            self.write_500(channel, 0x3, 0x69b, 2, 1);
        }
        self.mch.clrsetbits32(0x2d0, !0xff0c_01ff, 0x20_0000);
        self.mch.write16(0x6c0, 0x14a0);
        self.mch.clrsetbits32(0x6d0, !0xff00_00ff, 0x8000);
        self.mch.write16(0x232, 1 << 3);
        // 0x40004 or 0 depending on ?
        self.mch.clrsetbits32(0x234, 0x40004, 0x40004);
        self.mch.clrsetbits32(0x34, 0x7, 5);
        self.mch.write32(0x128, 0x215_0d05);
        self.mch.write8(0x12c, 0x1f);
        self.mch.write8(0x12d, 0x56);
        self.mch.write8(0x12e, 0x31);
        self.mch.write8(0x12f, 0);
        self.mch.write8(0x271, 1 << 1);
        self.mch.write8(0x671, 1 << 1);
        self.mch.write8(0x1e8, 1 << 2);
        for channel in 0..NUM_CHANNELS {
            self.mch.write32(
                0x294 + ((channel as u32) << 10),
                u32::from(self.info.populated_ranks_mask[channel] & 3) << 16,
            );
        }
        self.mch.clrsetbits32(0x134, !0xfc01_ffff, 0x10000);
        self.mch.clrsetbits32(0x134, !0xfc85_ffff, 0x85_0000);
        for channel in 0..NUM_CHANNELS {
            self.mch.clrsetbits32(
                0x260 + ((channel as u32) << 10),
                0xf << 20,
                (1 << 27) | (u32::from(self.info.populated_ranks_mask[channel] & 3) << 20),
            );
        }

        if !s3resume {
            self.jedec_init();
        }

        for (totalrank, (channel, slot, rank)) in
            (0..).zip(populated_ranks(self.info).collect::<heapless::Vec<_, 8>>())
        {
            self.jedec_read(channel, slot, rank, totalrank, 0xa, 0x400);
        }

        self.mch.write8(0x12c, 0x9f);
        self.mch.clrsetbits8(0x271, 0x3e, 0x0e);
        self.mch.clrsetbits8(0x671, 0x3e, 0x0e);

        if !s3resume {
            self.program_ranks_after_jedec();
            let (a, b) = (self.mch.read8(0x243), self.mch.read8(0x643));
            self.mch.write8(0x243, a | 2);
            self.mch.write8(0x643, b | 2);

            self.write_1d0(7, 0x19b, 3, 1);
            self.write_1d0(7, 0x1c0, 3, 1);
            self.write_1d0(4, 0x1c6, 4, 1);
            self.write_1d0(4, 0x1cc, 4, 1);
            self.rmw_1d0(0x151, 0xf, 0x4, 4);
            self.mch.write32(0x584, 0xfffff);
            self.mch.write32(0x984, 0xfffff);

            for (channel, slot, rank) in populated_ranks(self.info).collect::<heapless::Vec<_, 8>>()
            {
                self.config_rank(s3resume, channel, slot, rank);
            }

            self.mch.write8(0x243, 1);
            self.mch.write8(0x643, 1);
        }

        // Was "== 1" but is common.
        northbridge().write16(0xc8, 3);
        self.write_26c(0, 0x820);
        self.write_26c(1, 0x820);
        self.mch.setbits32(0x130, 1 << 1);

        if s3resume {
            self.program_ranks_after_jedec();
        }

        self.mch.clrbits32(0xfa4, (1 << 24) | (1 << 1));
        self.mch.write32(0xfb0, 0x2000_e019);

        if !s3resume {
            self.ram_training(timings)?;
        }

        self.program_modules_memory_map(false);
        self.program_total_memory_map();

        if self.info.non_interleaved_part_mb != 0 && self.info.interleaved_part_mb != 0 {
            self.mch.write8(0x111, (1 << 5) | (1 << 6));
        } else if self.have_match_ranks(0, 4) && self.have_match_ranks(1, 4) {
            self.mch.write8(0x111, (3 << 2) | (1 << 5) | (1 << 7));
        } else if self.have_match_ranks(0, 2) && self.have_match_ranks(1, 2) {
            self.mch.write8(0x111, (3 << 2) | (1 << 5));
        } else {
            self.mch.write8(0x111, (3 << 2) | (1 << 5) | (1 << 6));
        }

        self.post_training();

        let heci_uma_addr = ((u64::from(northbridge().read16(TOM)) << 6)
            - u64::from(self.info.memory_reserved_for_heci_mb))
            << 20;
        crate::ibexpeak::me::setup_uma(
            &crate::ibexpeak::me::UmaChannels {
                dmibar: self.dmi.0,
                rcba: self.rcba,
                mchbar: self.mch.0,
            },
            self.hecibar,
            heci_uma_addr,
            self.info.memory_reserved_for_heci_mb,
        )?;

        if self.info.uma_enabled != 0 {
            self.mch.setbits32(0x11b0, 1 << 14);
            self.mch.setbits32(0x11b4, 1 << 14);
            self.mch.setbits16(0x1190, 1 << 14);
            let ax = self.mch.read16(0x1190) & 0xf00;
            self.mch
                .write16(0x1170, ax | (self.mch.read16(0x1170) & 0x107f) | 0x4080);
            self.mch.setbits16(0x1170, 1 << 12);
            fstart_arch::x86::udelay(1000);
            let mut ecx = 0xffffu32;
            while ecx != 0 && self.mch.read16(0x1170) & (1 << 12) != 0 {
                ecx -= 1;
            }
            self.mch.clrbits16(0x1190, 1 << 14);
        }

        let sb = southbridge();
        sb.write8(GEN_PMCON_2, sb.read8(GEN_PMCON_2) & !0x80);
        fstart_arch::x86::udelay(10000);
        self.mch.write16(0x2ca8, 1 << 3);
        fstart_arch::x86::udelay(1000);

        crate::ibexpeak::me::early_status();

        let record = (!s3resume).then(|| TrainingRecord {
            version: RECORD_VERSION,
            spd_digest: digest,
            training: self.save_timings(),
        });
        Ok(Trained {
            record,
            total_memory_mb: self.info.total_memory_mb,
            fsb_frequency: self.info.fsb_frequency,
            clock_speed_index: self.info.clock_speed_index,
        })
    }

    /// The per-channel rank controls coreboot programs after JEDEC init on
    /// a cold boot and after the scan-chain setup on resume.
    fn program_ranks_after_jedec(&self) {
        for channel in 0..NUM_CHANNELS {
            let base = (channel as u32) << 10;
            self.mch.write32(
                0x294 + base,
                u32::from(self.info.populated_ranks_mask[channel] & 3) << 16,
            );
            self.mch.write16(
                0x298 + base,
                u16::from(self.info.populated_ranks[channel][0][0])
                    | (u16::from(self.info.populated_ranks[channel][0][1]) << 5),
            );
            self.mch.write32(0x29c + base, 0x77a);
        }
        self.mch.clrsetbits32(0x2c0, !0, 0x6009_cc00);
    }

    /// The fixed controller, IOMMU, power and thermal programming after the
    /// memory map (coreboot `raminit()` tail before the HECI UMA setup).
    fn post_training(&self) {
        let mch = self.mch;
        mch.clrbits32(0xfac, 1 << 31);
        mch.write32(0xfb4, 0x4800);
        mch.write32(0xfb8, if self.info.revision < 8 { 0x20 } else { 0x0 });
        mch.write32(0xe94, 0x7ffff);
        mch.write32(0xfc0, 0x8000_2040);
        mch.write32(0xfc4, 0x70_1246);
        mch.clrbits8(0xfc8, 0x70);
        mch.setbits32(0xe5c, 1 << 24);
        mch.clrsetbits32(0x1a70, 3 << 20, 2 << 20);
        mch.write32(0x50, 0x700b0);
        mch.write32(0x3c, 0x10);
        mch.clrsetbits8(0x1aa8, 0x3f, 0xa);
        mch.setbits8(0xff4, 1 << 1);
        mch.clrsetbits32(0xff8, 0xe008, 0x1020);

        mch.write32(0xd00, IOMMU_BASE2 | 1);
        mch.write32(0xd40, IOMMU_BASE1 | 1);
        mch.write32(0xdc0, IOMMU_BASE4 | 1);
        write32p(IOMMU_BASE1 | 0xffc, 0x8000_0000);
        write32p(IOMMU_BASE2 | 0xffc, 0xc000_0000);
        write32p(IOMMU_BASE4 | 0xffc, 0x8000_0000);

        {
            let eax = u32::from(self.info.fsb_frequency) / 9;
            mch.clrsetbits32(
                0xfcc,
                0x3ffff,
                (eax * 0x280) | (eax * 0x5000) | eax | 0x40000,
            );
            mch.write32(0x20, 0x33001);
        }

        for channel in 0..NUM_CHANNELS as u32 {
            let base = channel << 10;
            mch.clrbits32(0x220 + base, 0x7770);
            if self.info.max_slots_used_in_channel == 1 {
                mch.setbits16(0x237 + base, 0x0201);
            } else {
                mch.clrbits16(0x237 + base, 0x0201);
            }
            mch.setbits8(0x241 + base, 1 << 0);
            if self.info.clock_speed_index <= 1
                && (self.info.silicon_revision == 2 || self.info.silicon_revision == 3)
            {
                mch.setbits32(0x248 + base, 0x0010_2000);
            } else {
                mch.clrbits32(0x248 + base, 0x0010_2000);
            }
        }

        mch.setbits32(0x115, 1 << 24);
        {
            let mut al: u32 = 0xd;
            if !(self.info.silicon_revision == 0 || self.info.silicon_revision == 1) {
                al += 2;
            }
            al |= ((1 << (self.info.max_slots_used_in_channel - 1)) - 1) << 4;
            mch.write32(0x210, (al << 16) | 0x20);
        }

        for channel in 0..NUM_CHANNELS as u32 {
            let base = channel << 10;
            mch.write32(0x288 + base, 0x7060_5040);
            mch.write32(0x28c + base, 0xfffe_c080);
            mch.write32(
                0x290 + base,
                0x282_091c | ((u32::from(self.info.max_slots_used_in_channel) - 1) << 0x16),
            );
        }
        let nb = northbridge();
        nb.read32(0x40);
        let reg1c = self.ep.read32(0x004);
        nb.read32(0x40);
        self.ep.write32(0x004, reg1c);
        mch.read8(0xe08);
        nb.read32(0xe4);
        mch.setbits8(0x1210, 1 << 1);
        mch.write32(0x1200, 0x880_0440);
        mch.write32(0x1204, 0x53ff_0453);
        mch.write32(0x1208, 0x1900_2043);
        mch.write16(0x1214, 0x320);
        if self.info.revision == 0x10 || self.info.revision == 0x11 {
            mch.write16(0x1214, 0x220);
            mch.setbits8(0x1210, 1 << 6);
        }
        mch.setbits8(0x1214, 1 << 2);
        mch.write8(0x120c, 1);
        mch.write8(0x1218, 3);
        mch.write8(0x121a, 3);
        mch.write8(0x121c, 3);
        mch.write16(0xc14, 0);
        mch.write16(0xc20, 0);
        mch.write32(0x1c, 0);

        // Revision dependent here.
        mch.setbits16(0x1230, 0x1f07);
        if self.info.uma_enabled != 0 {
            mch.setbits32(0x11f4, 1 << 28);
        }
        mch.setbits16(0x1230, 1 << 15);
        mch.setbits8(0x1214, 1 << 0);

        let reg_1020 = mch.read32(0x1020) as u16;
        mch.write8(0x1070, 1);
        mch.write32(0x1000, 0x100);
        mch.write8(0x1007, 0);
        let (bl, ebpb): (u32, u32) = if reg_1020 != 0 {
            mch.write16(0x1018, 0);
            (u32::from(reg_1020 >> 8), u32::from(reg_1020 & 0xff))
        } else {
            (8, 0)
        };
        // SAFETY: model 2065x implements MSR_TEMPERATURE_TARGET.
        unsafe { fstart_arch::x86::msr::rdmsr(TEMPERATURE_TARGET.index()) };
        mch.write32(0x1014, 0xffff_ffff);
        mch.write32(
            0x1010,
            ((((ebpb + 0x7d) << 7) / bl) & 0xff) * u32::from(reg_1020 != 0),
        );
        mch.write8(0x101c, 0xb8);
        mch.clrsetbits8(0x123e, 0xf0, 0x60);
        if reg_1020 != 0 {
            mch.clrsetbits32(0x123c, 0xf << 20, 0x6 << 20);
            mch.write8(0x101c, 0xb8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_and_timing_addresses_match_coreboot() {
        assert_eq!(get_lane_offset(0, 0, 0), 0);
        assert_eq!(get_lane_offset(1, 1, 4), 0x124 * 4 + 0x23e + 11 + 22);
        assert_eq!(get_lane_offset(0, 0, 8), 0x124 * 8 - 0x452);
        assert_eq!(get_timing_register_addr(0, 0, 0, 0), 0x5c);
        assert_eq!(get_timing_register_addr(0, 1, 0, 0), 0x1d);
        assert_eq!(get_timing_register_addr(0, 3, 0, 0), 0xe6);
    }

    #[test]
    fn training_record_has_no_implicit_padding() {
        assert_eq!(core::mem::size_of::<RamTraining>(), 1464);
        assert!(core::mem::size_of::<TrainingRecord>() <= 1536);
    }

    #[test]
    fn bit_helpers_match_coreboot() {
        assert_eq!(find_highest_bit_set(0), -1);
        assert_eq!(find_highest_bit_set(0x3fe), 9);
        assert_eq!(find_lowest_bit_set32(0x800), 11);
        assert_eq!(get_bits_420(0x10 | 0x8 | 0x4), 7);
    }
}
