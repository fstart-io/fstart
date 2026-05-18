//! GM965 DDR2 raminit scaffolding.
//!
//! The coreboot GM965 raminit is a large cold-boot training flow. This module
//! now shares DDR2 SPD decoding with Pineview through `fstart-spd`, then builds
//! the GM965-specific sysinfo shape: DIMM topology, common frequency/CAS, and
//! derived timing clocks. The actual controller/PHY programming sequence is
//! still guarded behind [`cold_boot_train`].

use fstart_services::{ServiceError, SmBus};
use fstart_spd::{ChipWidth, DimmInfo};

use crate::{hostbridge, mchbar, MchBar};

const TCK_266MHZ_256NS: u32 = 960; // 3.75 ns
const TCK_333MHZ_256NS: u32 = 768; // 3.00 ns
const TRFC_TABLE: [[u8; 4]; 2] = [
    // 256Mb, 512Mb, 1Gb, 2Gb
    [20, 28, 34, 52], // DDR2-533
    [25, 35, 43, 65], // DDR2-667
];
const TFAW_FIXED: [u32; 3] = [0, 14, 17];
const DRT4_ROM_TABLE: [u32; 3] = [0, 0x390c_2850, 0x414e_3064];
const DRT5_ROM_BYTES: [u32; 3] = [0, 0x50, 0x64];
const DRT0_TWTR_LUT: [u8; 3] = [2, 2, 3];
const ODT_TIMING_TABLE: [[u32; 2]; 4] = [
    [0x2000_1010, 0x6091_8788],
    [0x2000_2020, 0x6092_8788],
    [0x2000_3030, 0x6093_8788],
    [0x2000_4040, 0x6094_8788],
];
const IO_CFG2_TABLE: [u32; 3] = [0, 0x0d, 0x0a];
const IO_CFG5_TABLE: [u8; 3] = [0, 0x0b, 0x0a];
const PI_TABLE: [u32; 3] = [0, 0x0000_2121, 0x0000_1111];
const IO_INIT_CFG2_TRAINING: [u32; 3] = [0x05, 0x07, 0x09];

const DCC_INTERLEAVED: u32 = 1 << 1;
const DCC_NO_CHANXOR: u32 = 1 << 10;
const DCC_CMD_MASK: u32 = 7 << 16;
const DCC_CMD_NOP: u32 = 1 << 16;
const DCC_CMD_ABP: u32 = 2 << 16;
const DCC_SET_MREG: u32 = 3 << 16;
const DCC_SET_EREG: u32 = 4 << 16;
const DCC_SET_EREG_MASK: u32 = DCC_CMD_MASK | (3 << 21);
const DCC_CMD_CBR: u32 = 6 << 16;
const CLKCFG_MEMCLK_MASK: u32 = 7 << 4;
const CLKCFG_UPDATE: u32 = 1 << 12;
const PMSTS_SELFREFRESH: u32 = 1 << 0;
const PMSTS_WARM_RESET: u32 = 1 << 1;
const TRAIN_ENABLE_BIT: u32 = 1 << 31;
const LPC_DEV: u8 = 0x1f;
const LPC_FUNC: u8 = 0;
const GEN_PMCON_2: u16 = 0xa2;
const GEN_PMCON_3: u16 = 0xa4;
const GEN_PMCON_2_DRAM_INIT: u8 = 1 << 7;
const GEN_PMCON_2_STATUS_CLR: u8 = 0xe6;
const GEN_PMCON_3_RTC_PWR_STS: u8 = 1 << 1;
const GEN_PMCON_3_SLP_S3_STRETCH: u8 = 1 << 3;
const CX_DRC0_RMS_MASK: u32 = 7 << 8;
const CX_DRC0_RMS_78_US: u32 = 2 << 8;

/// GM965 FSB clock strap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsbClock {
    /// FSB-533.
    Fsb533 = 1,
    /// FSB-800.
    #[default]
    Fsb800 = 2,
    /// FSB-667.
    Fsb667 = 3,
}

/// GM965 DDR2 clock selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum MemClock {
    /// DDR2-533 (266 MHz clock).
    #[default]
    Ddr2_533 = 1,
    /// DDR2-667 (333 MHz clock).
    Ddr2_667 = 2,
}

impl MemClock {
    const fn tck_256ns(self) -> u32 {
        match self {
            Self::Ddr2_533 => TCK_266MHZ_256NS,
            Self::Ddr2_667 => TCK_333MHZ_256NS,
        }
    }

    const fn index(self) -> usize {
        (self as usize) - 1
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Ddr2_533 => "DDR2-533",
            Self::Ddr2_667 => "DDR2-667",
        }
    }
}

/// GM965 channel mode selected from populated channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    /// Only one channel is populated.
    #[default]
    Single = 0,
    /// Two channels are populated but not symmetric enough for interleave.
    DualAsync = 1,
    /// Two symmetric channels can be interleaved.
    DualInterleaved = 2,
}

/// One decoded GM965 DDR2 DIMM slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct DimmSlot {
    /// Whether a valid DDR2 SPD was found.
    pub present: bool,
    /// SMBus SPD EEPROM address.
    pub spd_addr: u8,
    /// Channel number, derived from the GM965 slot index.
    pub channel: u8,
    /// True for dual-rank or better DIMMs.
    pub dual_rank: bool,
    /// True when SDRAM devices are x16.
    pub x16: bool,
    /// Row address bits.
    pub rows: u8,
    /// Column address bits.
    pub cols: u8,
    /// Banks per SDRAM device (4 or 8).
    pub banks: u8,
    /// Page size in bytes.
    pub page_size: u16,
    /// Rank count reported by SPD.
    pub ranks: u8,
    /// Capacity of one rank in MiB.
    pub rank_capacity_mb: u32,
    /// Total DIMM capacity in MiB.
    pub capacity_mb: u32,
    /// Supported CAS-latency bitmask.
    pub cas_supported: u8,
    /// Decoded tCK per CAS level, in units of 1/256 ns.
    pub cycle_time_256ns: [u32; 8],
    /// Decoded tRCD, in units of 1/256 ns.
    pub trcd_256ns: u32,
    /// Decoded tRP, in units of 1/256 ns.
    pub trp_256ns: u32,
    /// Decoded tRAS, in units of 1/256 ns.
    pub tras_256ns: u32,
    /// Decoded tWR, in units of 1/256 ns.
    pub twr_256ns: u32,
    /// Decoded tRRD, in units of 1/256 ns.
    pub trrd_256ns: u32,
    /// Decoded tRTP, in units of 1/256 ns.
    pub trtp_256ns: u32,
}

impl DimmSlot {
    fn from_spd(slot: usize, addr: u8, dimm: &DimmInfo) -> Self {
        let capacity_mb = dimm.rank_capacity_mb.saturating_mul(dimm.ranks as u32);
        Self {
            present: true,
            spd_addr: addr,
            channel: if slot < 2 { 0 } else { 1 },
            dual_rank: dimm.ranks > 1,
            x16: dimm.width == ChipWidth::X16,
            rows: dimm.rows,
            cols: dimm.cols,
            banks: dimm.banks,
            page_size: dimm.page_size as u16,
            ranks: dimm.ranks,
            rank_capacity_mb: dimm.rank_capacity_mb,
            capacity_mb,
            cas_supported: dimm.cas_latencies,
            cycle_time_256ns: dimm.cycle_time_256ns,
            trcd_256ns: dimm.trcd_256ns,
            trp_256ns: dimm.trp_256ns,
            tras_256ns: dimm.tras_256ns,
            twr_256ns: dimm.twr_256ns,
            trrd_256ns: dimm.trrd_256ns,
            trtp_256ns: dimm.trtp_256ns,
        }
    }

    /// Compute the GM965 EPD DRA encoding for this DIMM.
    ///
    /// This mirrors coreboot's `epd_dra_encode()`: the low byte describes
    /// rank 0 and, for dual-rank DIMMs, the high byte repeats the encoding for
    /// rank 1. A zero return means the slot is empty or too small for EPD DRA.
    pub fn epd_dra_encode(&self) -> u16 {
        if !self.present {
            return 0;
        }

        let mut idx =
            self.rows as i16 + self.cols as i16 + i16::from(self.x16) + i16::from(self.banks == 8)
                - 22;
        idx = idx.clamp(0, 4);

        let base = match idx {
            0 => return 0,
            1 => 0,
            2 => {
                if self.banks == 8 {
                    4
                } else {
                    2
                }
            }
            3 => 6,
            4 => 8,
            _ => return 0,
        };

        let mut enc = base + u8::from(self.x16);
        if enc > 3 {
            enc |= 0x80;
        }

        enc as u16 | if self.dual_rank { (enc as u16) << 8 } else { 0 }
    }
}

/// Computed GM965 timing parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    /// Selected CAS latency.
    pub cas: u8,
    /// tRAS in clocks.
    pub tras: u8,
    /// tRP in clocks.
    pub trp: u8,
    /// tRCD in clocks.
    pub trcd: u8,
    /// tRFC in clocks.
    pub trfc: u8,
    /// tWR in clocks.
    pub twr: u8,
    /// tRRD in clocks.
    pub trrd: u8,
    /// tRTP in clocks.
    pub trtp: u8,
    /// FSB strap.
    pub fsb_clock: FsbClock,
    /// DDR2 clock.
    pub mem_clock: MemClock,
    /// Channel mode.
    pub channel_mode: ChannelMode,
}

/// SPD-derived GM965 memory topology and selected timings.
#[derive(Debug, Clone, Copy, Default)]
pub struct RaminitInfo {
    /// Slot 0/1 are channel 0, slot 2/3 are channel 1. X61 uses 0 and 2.
    pub dimms: [DimmSlot; 4],
    /// Number of populated DIMMs.
    pub dimm_count: u8,
    /// Number of populated channels.
    pub channels: u8,
    /// Total installed memory in MiB, before stolen/TSEG reservations.
    pub total_mb: u32,
    /// Top of low usable DRAM in MiB after final map programming.
    pub tolud_mb: u32,
    /// Top of memory in MiB after final map programming.
    pub tom_mb: u32,
    /// Receive-enable coarse delay per channel.
    pub rec_coarse: [u8; 2],
    /// Receive-enable sub-coarse delay per channel.
    pub rec_coarse_low: [u8; 2],
    /// Receive-enable fine delay per channel.
    pub rec_fine: [u8; 2],
    /// Selected frequency/CAS/timing values.
    pub timings: Timings,
}

impl RaminitInfo {
    /// Total installed memory in bytes.
    pub const fn total_bytes(&self) -> u64 {
        (self.total_mb as u64) * 1024 * 1024
    }

    /// Rank-population bitmap in GM965 slot/rank order.
    pub fn rank_bitmap(&self) -> u8 {
        let mut bitmap = 0u8;
        for (slot, dimm) in self.dimms.iter().enumerate() {
            if !dimm.present {
                continue;
            }
            let first_rank = (slot as u8) * 2;
            for rank in 0..dimm.ranks.min(2) {
                bitmap |= 1 << (first_rank + rank);
            }
        }
        bitmap
    }

    /// EPD DRA encodings for all four GM965 DIMM slots.
    pub fn epd_dra_encodings(&self) -> [u16; 4] {
        [
            self.dimms[0].epd_dra_encode(),
            self.dimms[1].epd_dra_encode(),
            self.dimms[2].epd_dra_encode(),
            self.dimms[3].epd_dra_encode(),
        ]
    }
}

fn div_round_up(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

fn populated_dimms(info: &RaminitInfo) -> impl Iterator<Item = &DimmSlot> {
    info.dimms.iter().filter(|d| d.present)
}

fn read_fsb_clock(mch: &MchBar) -> FsbClock {
    match (mch.read32(mchbar::CLKCFG) & 0x7) as u8 {
        1 => FsbClock::Fsb533,
        2 => FsbClock::Fsb800,
        3 => FsbClock::Fsb667,
        _ => FsbClock::Fsb800,
    }
}

fn select_channel_mode(info: &RaminitInfo) -> ChannelMode {
    if info.channels < 2 {
        return ChannelMode::Single;
    }

    let ch0_mb: u32 = info.dimms[0..2].iter().map(|d| d.capacity_mb).sum();
    let ch1_mb: u32 = info.dimms[2..4].iter().map(|d| d.capacity_mb).sum();
    if ch0_mb != 0 && ch0_mb == ch1_mb {
        ChannelMode::DualInterleaved
    } else {
        ChannelMode::DualAsync
    }
}

fn select_frequency_and_cas(
    info: &mut RaminitInfo,
    fsb_clock: FsbClock,
    capid0: u32,
) -> Result<(), ServiceError> {
    let mut cas_mask = 0xffu8;
    let mut tck_min_common = 0u32;
    let mut taa_needed = 0u32;

    for dimm in populated_dimms(info) {
        cas_mask &= dimm.cas_supported;
        let Some(max_cas) = fstart_spd::ddr2::msb_index(dimm.cas_supported) else {
            return Err(ServiceError::HardwareError);
        };
        let tck = dimm.cycle_time_256ns[max_cas as usize];
        if tck == 0 {
            return Err(ServiceError::HardwareError);
        }
        tck_min_common = tck_min_common.max(tck);
        taa_needed = taa_needed.max((max_cas as u32).saturating_mul(tck));
    }

    if cas_mask == 0 {
        fstart_log::error!("gm965 raminit: no common CAS latency");
        return Err(ServiceError::HardwareError);
    }

    let max_mem_clock = if (capid0 & (1 << 30)) != 0 || fsb_clock == FsbClock::Fsb533 {
        MemClock::Ddr2_533
    } else {
        MemClock::Ddr2_667
    };

    for mem_clock in [MemClock::Ddr2_667, MemClock::Ddr2_533] {
        if mem_clock > max_mem_clock {
            continue;
        }

        let tck_clock = mem_clock.tck_256ns();
        if tck_clock < tck_min_common {
            continue;
        }

        let cas_needed = div_round_up(taa_needed, tck_clock).max(3);
        for cas in cas_needed..=6 {
            if (cas_mask & (1 << cas)) != 0 {
                info.timings.fsb_clock = fsb_clock;
                info.timings.mem_clock = mem_clock;
                info.timings.cas = cas as u8;
                info.timings.channel_mode = select_channel_mode(info);
                fstart_log::info!(
                    "gm965 raminit: selected {} CAS{} (mask={:#x}, tCK={} / {})",
                    mem_clock.name(),
                    cas,
                    cas_mask,
                    tck_min_common,
                    tck_clock,
                );
                return Ok(());
            }
        }
    }

    fstart_log::error!("gm965 raminit: no valid frequency/CAS combination");
    Err(ServiceError::HardwareError)
}

fn calculate_timings(info: &mut RaminitInfo) -> Result<(), ServiceError> {
    let tck = info.timings.mem_clock.tck_256ns();
    let mut tras = 0u32;
    let mut trp = 0u32;
    let mut trcd = 0u32;
    let mut twr = 0u32;
    let mut trrd = 0u32;
    let mut trtp = 0u32;
    let mut trfc = 0u32;

    for dimm in populated_dimms(info) {
        tras = tras.max(div_round_up(dimm.tras_256ns, tck));
        trp = trp.max(div_round_up(dimm.trp_256ns, tck));
        trcd = trcd.max(div_round_up(dimm.trcd_256ns, tck));
        twr = twr.max(div_round_up(dimm.twr_256ns, tck));
        trrd = trrd.max(div_round_up(dimm.trrd_256ns, tck).max(2));
        trtp = trtp.max(div_round_up(dimm.trtp_256ns, tck).max(2));

        let width_bits = if dimm.x16 { 4 } else { 3 };
        let bank_bits = if dimm.banks == 8 { 3 } else { 2 };
        let cap_idx = (dimm.rows as i32 + dimm.cols as i32 + width_bits + bank_bits - 28)
            .clamp(0, 3) as usize;
        trfc = trfc.max(TRFC_TABLE[info.timings.mem_clock.index()][cap_idx] as u32);
    }

    if !(4..=31).contains(&tras)
        || !(2..=9).contains(&trp)
        || !(2..=9).contains(&trcd)
        || trfc > 255
    {
        fstart_log::error!(
            "gm965 raminit: derived timings out of range: tRAS={} tRP={} tRCD={} tRFC={}",
            tras,
            trp,
            trcd,
            trfc,
        );
        return Err(ServiceError::HardwareError);
    }

    info.timings.tras = tras as u8;
    info.timings.trp = trp as u8;
    info.timings.trcd = trcd as u8;
    info.timings.twr = twr as u8;
    info.timings.trrd = trrd as u8;
    info.timings.trtp = trtp as u8;
    info.timings.trfc = trfc as u8;

    fstart_log::info!(
        "gm965 raminit: timings CAS{} tRAS={} tRP={} tRCD={} tWR={} tRFC={} tRRD={} tRTP={}",
        info.timings.cas,
        info.timings.tras,
        info.timings.trp,
        info.timings.trcd,
        info.timings.twr,
        info.timings.trfc,
        info.timings.trrd,
        info.timings.trtp,
    );
    Ok(())
}

fn stepping() -> u8 {
    fstart_ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC).read8(0x08)
}

fn lpc() -> fstart_ecam::PciDevBdf {
    fstart_ecam::PciDevBdf::new(0, LPC_DEV, LPC_FUNC)
}

#[cfg(target_arch = "x86_64")]
fn full_reset() -> ! {
    // SAFETY: I/O port 0xcf9 is the standard Intel reset control register.
    unsafe {
        fstart_pio::outb(0xcf9, 0x06);
        fstart_pio::outb(0xcf9, 0x0e);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn full_reset() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn reset_on_stale_rcomp(mch: &MchBar) {
    if stepping() == 0x00 && (mch.read32(mchbar::RCOMP_CTRL) & 2) != 0 {
        fstart_log::error!("gm965 raminit: stale A0 RCOMP state, issuing reset");
        full_reset();
    }
}

fn init_pmcon() {
    let lpc = lpc();
    if (lpc.read8(GEN_PMCON_3) & GEN_PMCON_3_SLP_S3_STRETCH) != 0 {
        lpc.and8(
            GEN_PMCON_3,
            !(GEN_PMCON_3_RTC_PWR_STS | GEN_PMCON_3_SLP_S3_STRETCH),
        );
    }
    lpc.and8(GEN_PMCON_2, GEN_PMCON_2_STATUS_CLR);
    lpc.or8(GEN_PMCON_2, GEN_PMCON_2_DRAM_INIT);
}

fn check_bad_warmboot(mch: &MchBar) {
    if (mch.read32(mchbar::PMSTS) & 3) == PMSTS_WARM_RESET {
        fstart_log::error!("gm965 raminit: bad warm boot state, issuing reset");
        let lpc = lpc();
        lpc.or8(GEN_PMCON_3, GEN_PMCON_3_SLP_S3_STRETCH);
        lpc.and8(GEN_PMCON_2, !GEN_PMCON_2_DRAM_INIT);
        mch.setbits32(mchbar::PMSTS, PMSTS_WARM_RESET);
        full_reset();
    }
}

fn clear_dram_init_in_progress() {
    lpc().and8(GEN_PMCON_2, !GEN_PMCON_2_DRAM_INIT);
}

fn channel_populated(info: &RaminitInfo, ch: usize) -> bool {
    let first = ch * 2;
    info.dimms[first].present || info.dimms[first + 1].present
}

fn channel_dual_rank(info: &RaminitInfo, ch: usize) -> bool {
    let first = ch * 2;
    info.dimms[first].dual_rank || info.dimms[first + 1].dual_rank
}

fn set_pci8(dev: &fstart_ecam::PciDevBdf, reg: u16, clear: u8, set: u8) {
    dev.write8(reg, (dev.read8(reg) & !clear) | set);
}

fn dcc_set_eregx(x: u32) -> u32 {
    (DCC_SET_EREG | ((x - 1) << 21)) & DCC_SET_EREG_MASK
}

fn mem_index(clock: MemClock) -> usize {
    clock as usize
}

fn fsb_index(clock: FsbClock) -> usize {
    clock as usize
}

fn program_clkcfg_lock(info: &RaminitInfo, mch: &MchBar) {
    let want = ((info.timings.mem_clock as u32) + 2) << 4;
    let mut clkcfg = mch.read32(mchbar::CLKCFG) & !(1 << 17);
    if stepping() == 0 && (clkcfg & 0x300) != 0x300 {
        clkcfg |= 0x300;
    }
    clkcfg = (clkcfg & !(CLKCFG_UPDATE | CLKCFG_MEMCLK_MASK)) | want;
    mch.write32(mchbar::CLKCFG, clkcfg);
    mch.clrsetbits32(mchbar::CLKCFG, CLKCFG_MEMCLK_MASK | CLKCFG_UPDATE, want);
    mch.clrsetbits32(mchbar::CLKCFG, CLKCFG_MEMCLK_MASK, want | CLKCFG_UPDATE);
    mch.clrbits32(mchbar::CLKCFG, CLKCFG_UPDATE);
}

fn program_gcfgc(info: &RaminitInfo, mch: &MchBar) {
    let hb = fstart_ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let vco = (hb.read8(0xe5) >> 2) & 7;
    if vco == 7 {
        if stepping() == 0 {
            mch.setbits16(0x1190, 0x4000);
            mch.clrsetbits16(0x119e, 0xe000, 0x9000);
        }
        return;
    }

    mch.setbits32(mchbar::MCHBAR_FFC, 1 << 24);
    let mut render = 5u8;
    if info.timings.fsb_clock == FsbClock::Fsb800 {
        render = match info.timings.mem_clock {
            MemClock::Ddr2_533 => 4,
            MemClock::Ddr2_667 => 3,
        };
    }
    if vco == 3 {
        render = 4;
    }

    let igd = fstart_ecam::PciDevBdf::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC);
    if igd.read16(0) != 0xffff {
        set_pci8(&igd, hostbridge::GCFGC, 0xd0, render);
        set_pci8(&igd, hostbridge::GCFGC + 1, 0xe0, 2);
    }
}

fn set_clkcross_frequencies(info: &RaminitInfo, mch: &MchBar) {
    const T1: [[[u32; 2]; 4]; 3] = [
        [[0; 2]; 4],
        [
            [0, 0],
            [0x0003_00c0, 0x0030_000c],
            [0x0007_0e00, 0x01c0_0038],
            [0x0007_0300, 0x00e0_0018],
        ],
        [
            [0, 0],
            [0, 0],
            [0x0003_0e00, 0x0070_000c],
            [0x0003_00c0, 0x0030_000c],
        ],
    ];
    const T2: [[[u32; 2]; 4]; 3] = [
        [[0; 2]; 4],
        [
            [0, 0],
            [0x0010_0401, 0],
            [0x0002_0108, 0],
            [0x1008_0201, 0x40],
        ],
        [[0, 0], [0, 0], [0x0004_0210, 0], [0x0010_0401, 0]],
    ];

    let mc = mem_index(info.timings.mem_clock);
    let fsb = fsb_index(info.timings.fsb_clock);
    mch.write32(mchbar::CLKCROSS_DATA3, T1[mc][fsb][0]);
    mch.write32(mchbar::CLKCROSS_DATA2, T1[mc][fsb][1]);
    if info.timings.mem_clock == MemClock::Ddr2_667 && info.timings.fsb_clock == FsbClock::Fsb800 {
        mch.write32(mchbar::CLKCROSS_DATA1, 0x180);
    }
    for ch in 0..2 {
        mch.write32(0x1258 + ch as u32 * 0x100, T2[mc][fsb][0]);
        mch.write32(0x125c + ch as u32 * 0x100, T2[mc][fsb][1]);
    }
}

fn program_map(info: &mut RaminitInfo, mch: &MchBar, pre_jedec: bool) {
    let mut global_boundary = 0u8;
    let mut total_mb = 0u32;
    for ch in 0..2 {
        let mut boundary = if pre_jedec { global_boundary } else { 0 };
        for s in 0..2 {
            let slot = ch * 2 + s;
            let dimm = info.dimms[slot];
            let mut rank_size = 0u8;
            let mut dra = 0u8;
            if dimm.present {
                if pre_jedec {
                    rank_size = 4;
                    dra = 0x22;
                } else {
                    rank_size = (dimm.rank_capacity_mb / 32).max(1) as u8;
                    let nibble = dimm.cols.saturating_sub(7);
                    dra = nibble | (nibble << 4);
                    total_mb = total_mb.saturating_add(dimm.rank_capacity_mb);
                }
            }

            boundary = boundary.wrapping_add(rank_size);
            mch.write16(mchbar::cx_drby(ch, s * 2), boundary as u16);
            if dimm.present && dimm.dual_rank {
                mch.write8(mchbar::cx_dra(ch) + s as u32, dra);
                boundary = boundary.wrapping_add(rank_size);
                if !pre_jedec {
                    total_mb = total_mb.saturating_add(dimm.rank_capacity_mb);
                }
            } else {
                dra &= 0x0f;
            }
            mch.write8(mchbar::cx_dra(ch) + s as u32, dra);
            mch.write16(mchbar::cx_drby(ch, s * 2) + 2, boundary as u16);
        }
        if pre_jedec {
            global_boundary = boundary;
        }
    }

    mch.write16(mchbar::DCC2, 0);
    let hb = fstart_ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    if pre_jedec {
        let total = (global_boundary as u32) * 32;
        hb.write16(hostbridge::TOM, ((total >> 7) & 0x1ff) as u16);
        hb.write16(hostbridge::TOLUD, (total.min(3072) << 4) as u16);
        hb.write16(hostbridge::TOUUD, total as u16);
        mch.setbits32(mchbar::DCC, DCC_NO_CHANXOR);
        return;
    }

    info.tom_mb = total_mb;
    info.tolud_mb = total_mb.min(3072);
    let mut touud_mb = total_mb;
    if total_mb.saturating_sub(info.tolud_mb) > 64 {
        let total_aligned = total_mb & !63;
        let remapbase = total_aligned.max(4096);
        let touud_cap = total_aligned.min(4096);
        let remaplimit = remapbase + (touud_cap - info.tolud_mb) - 64;
        let remapbase_reg = ((remapbase >> 6) & 0x03fe) as u16;
        let remaplimit_reg = ((remaplimit >> 6) & 0x03fe) as u16;
        touud_mb = (remaplimit_reg as u32 + 1) * 64;
        hb.write16(hostbridge::REMAPBASE, remapbase_reg);
        hb.write16(hostbridge::REMAPLIMIT, remaplimit_reg);
    }
    hb.write16(hostbridge::TOM, ((total_mb >> 7) & 0x1ff) as u16);
    hb.write16(hostbridge::TOLUD, (info.tolud_mb << 4) as u16);
    hb.write16(hostbridge::TOUUD, touud_mb as u16);
    let ggc = hb.read16(hostbridge::GGC);
    hb.write16(hostbridge::GGC, ggc);
    set_pci8(&hb, hostbridge::ESMRAMC, 0x07, (1 << 1) | 1);
}

fn program_timings(info: &RaminitInfo, mch: &MchBar) {
    let t = info.timings;
    let mut trrd_adj = 0u32;
    if t.mem_clock == MemClock::Ddr2_533 {
        for d in populated_dimms(info) {
            if d.cols > 9 && d.x16 {
                trrd_adj = 1;
            }
        }
    } else {
        trrd_adj = 1;
        for d in populated_dimms(info) {
            if (d.cols > 9 && d.x16) || d.cols > 10 {
                trrd_adj = 2;
            }
        }
    }

    for ch in 0..2 {
        let mut reg = (mch.read32(mchbar::cx_drt0(ch)) & 0xfffc_4318) | 0x0001_0841;
        let btb_wtp = (t.cas - 1) as u32 + 4 + t.twr as u32;
        let btb_wtr = (t.cas - 1) as u32 + 4 + DRT0_TWTR_LUT[mem_index(t.mem_clock)] as u32;
        reg = (reg & !(0xf << 20)) | ((btb_wtr & 0xf) << 20);
        reg = (reg & !(0x1f << 26)) | ((btb_wtp & 0x1f) << 26);
        mch.write32(mchbar::cx_drt0(ch), reg);

        let mut drt1 = ((t.tras as u32) << 21)
            | (((t.trcd - 2) as u32) << 5)
            | ((t.trp - 2) as u32)
            | (trrd_adj << 10);
        if t.mem_clock == MemClock::Ddr2_667 {
            drt1 |= 1 << 28;
        }
        reg = (mch.read32(mchbar::cx_drt1(ch)) & 0xcc1f_e318) | drt1;
        mch.write32(mchbar::cx_drt1(ch), reg);

        let mut drt2 = mch.read32(mchbar::cx_drt2(ch));
        drt2 = (drt2 & !0x1f) | 0x10;
        drt2 = (drt2 & !(0x1f << 17)) | (TFAW_FIXED[mem_index(t.mem_clock)] << 17);
        mch.write32(mchbar::cx_drt2(ch), drt2);

        let wl = (t.cas - 1) as u32;
        let mut drt3 = mch.read32(mchbar::cx_drt3(ch));
        drt3 = (drt3 & !(0x07 << 23)) | (((t.cas - 3) as u32) << 23);
        drt3 = (drt3 & !(0xff << 13)) | ((t.trfc as u32) << 13);
        drt3 = (drt3 & !0x07) | ((wl - 2) & 0x07);
        mch.write32(mchbar::cx_drt3(ch), drt3);
        mch.write32(mchbar::cx_drt4(ch), DRT4_ROM_TABLE[mem_index(t.mem_clock)]);

        let mut drt5 = mch.read32(mchbar::cx_drt5(ch));
        drt5 = (drt5 & !(0x0f << 22)) | ((4 + t.cas as u32 + 2) << 22);
        drt5 = (drt5 & !(0x1ff << 12)) | (DRT5_ROM_BYTES[mem_index(t.mem_clock)] << 12);
        drt5 = (drt5 & !(0x03 << 1)) | (1 << 1);
        mch.write32(mchbar::cx_drt5(ch), drt5);
        mch.clrbits32(mchbar::cx_drt6(ch), 1 << 2);
    }
}

fn program_dram_control(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        let slot = ch * 2;
        let dimm = info.dimms[slot];
        let reg =
            (mch.read32(mchbar::cx_drc0(ch)) & !CX_DRC0_RMS_MASK) | CX_DRC0_RMS_78_US | (1 << 3);
        mch.write32(mchbar::cx_drc0(ch), reg);
        let mut drc1 = mch.read32(mchbar::cx_drc1(ch));
        if !dimm.present {
            drc1 |= (1 << 16) | (1 << 17);
        } else if !dimm.dual_rank {
            drc1 |= 1 << 17;
        }
        drc1 |= 0x000c_0000 | (1 << 12) | (1 << 11);
        mch.write32(mchbar::cx_drc1(ch), drc1);

        let mut drc2 = mch.read32(mchbar::cx_drc2(ch));
        if !dimm.present {
            drc2 |= 0x0300_0000;
        } else if !dimm.dual_rank {
            drc2 |= 0x0200_0000;
        }
        drc2 |= 0x0c00_1000;
        mch.write32(mchbar::cx_drc2(ch), drc2);
    }
}

fn rcomp_init(info: &RaminitInfo, mch: &MchBar) {
    const RCOMP_ROM_TABLE: [[u32; 10]; 9] = [
        [
            0x4c28a249, 0xe38e34d3, 0x3cf3cf38, 0x4c2ca249, 0xe38e34d3, 0x3cf3cf3c, 0x00000055,
            0x55000000, 0, 0,
        ],
        [
            0xc8186145, 0xc30c2cb2, 0x34d34d30, 0x481c71c6, 0xb2ca28a2, 0x30c30c30, 0x00000055,
            0x55000000, 0, 0,
        ],
        [
            0xc8186145, 0xc30c2cb2, 0x34d34d30, 0x481c71c6, 0xb2ca28a2, 0x30c30c30, 0x00000055,
            0x55000000, 0, 0x80000000,
        ],
        [
            0xc8186145, 0xc30c2cb2, 0x34d34d30, 0x481c71c6, 0xb2ca28a2, 0x30c30c30, 0x00000055,
            0x55000000, 0, 0x80000000,
        ],
        [
            0xca28a249, 0x24903cb2, 0x4d34d349, 0xcd34d30c, 0x349140f3, 0x5d759655, 0x00000088,
            0x88000000, 0, 0,
        ],
        [
            0xca28a249, 0x24903cb2, 0x4d34d349, 0xcd34d30c, 0x349140f3, 0x5d759655, 0x00000088,
            0x88000000, 0, 0,
        ],
        [
            0xca28a249, 0x24903cb2, 0x4d34d349, 0xca28a249, 0x140e34b2, 0x4d349245, 0x00000088,
            0x88000000, 0, 0,
        ],
        [
            0x4c28a249, 0xe38e34d3, 0x3cf3cf38, 0x4c2ca249, 0xe38e34d3, 0x3cf3cf3c, 0x00000055,
            0x55000000, 0, 0,
        ],
        [
            0xc8186145, 0xc30c2cb2, 0x34d34d30, 0x481c71c6, 0xb2ca28a2, 0x30c30c30, 0x00000055,
            0x55000000, 0, 0,
        ],
    ];
    mch.setbits32(mchbar::IO_RCOMP_CLK_EN, 1 << 12);
    let mut ctrl = mch.read32(mchbar::RCOMP_CTRL) & 0xfffa_ffee;
    ctrl |= 0x0002_0020;
    if stepping() != 0 {
        ctrl |= 0x0006_0020;
    }
    mch.write32(mchbar::RCOMP_CTRL, ctrl);
    mch.clrsetbits16(mchbar::RCOMP_STATUS, !0x8888u16, 0x1111);
    mch.clrsetbits16(mchbar::RCOMP_CFG, !0xc1ffu16, 0x2e00);
    mch.setbits32(mchbar::RCOMP_CFG3, 1 << 18);
    mch.clrsetbits32(mchbar::RCOMP_CFG4, !0x9999_9999, 0x1111_9999);
    mch.clrsetbits8(mchbar::RCOMP_ODT0, 0x3f, 0x36);
    mch.clrsetbits8(mchbar::RCOMP_ODT1, 0x3f, 0x36);
    for (g, row) in RCOMP_ROM_TABLE.iter().enumerate() {
        let off = mchbar::RCOMP_TABLES + g as u32 * 64;
        for (i, val) in row.iter().take(3).enumerate() {
            mch.write32(off + i as u32 * 4, *val);
        }
        for (i, val) in row.iter().skip(3).take(3).enumerate() {
            mch.write32(off + 0x18 + i as u32 * 4, *val);
        }
        for (i, val) in row.iter().skip(6).take(4).enumerate() {
            mch.write32(off + 0x30 + i as u32 * 4, *val);
        }
    }
    let fsb_codes = [0u8, 0x00, 0x00, 0x01];
    let mem_codes = [0u8, 0x10, 0x50];
    mch.write8(
        mchbar::RCOMP_CFG2,
        fsb_codes[fsb_index(info.timings.fsb_clock)] + mem_codes[mem_index(info.timings.mem_clock)],
    );
    mch.write32(mchbar::RCOMP_CFG3, mch.read32(mchbar::RCOMP_CFG3));
    if stepping() == 1 {
        mch.setbits8(mchbar::RCOMP_CTRL, 3);
    } else {
        mch.setbits8(mchbar::RCOMP_CTRL, 1);
    }
}

fn odt_and_io_setup(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        mch.clrbits16(mchbar::cx_dra_hi(ch), 0x00ff);
        if info.dimms[ch * 2].present && info.dimms[ch * 2].banks == 8 {
            mch.setbits16(mchbar::cx_dra_hi(ch), 0x09);
            mch.setbits32(mchbar::cx_drt1(ch), 0x8000);
        }
    }
    let cas_idx = (info.timings.cas.saturating_sub(3) as usize).min(3);
    for ch in 0..2 {
        mch.clrsetbits32(
            mchbar::cx_odt_low(ch),
            !0x8f3f_8f88,
            ODT_TIMING_TABLE[cas_idx][0],
        );
        mch.clrsetbits32(
            mchbar::cx_odt_high(ch),
            !0x9f48_0000,
            ODT_TIMING_TABLE[cas_idx][1],
        );
        mch.setbits32(mchbar::cx_odt_misc(ch), 0x8000_0000);
        mch.clrsetbits8(mchbar::cx_odt_misc(ch), 0x1f, info.timings.cas + 1);
        mch.clrsetbits8(mchbar::cx_odt_timing(ch), 0x0f, info.timings.cas - 1);
        mch.clrsetbits8(mchbar::cx_odt_ctrl(ch), 0x07, 0x02);
    }
    let mut wr_ctrl = mch.read32(mchbar::WRITE_CTRL) & 0x113f_f3ff;
    wr_ctrl |= 0x8600_0400;
    if stepping() <= 1 {
        wr_ctrl |= 0x10;
    }
    mch.write32(mchbar::WRITE_CTRL, wr_ctrl);
    mch.clrsetbits32(mchbar::MMARB0, !0xfff9_ffff, 0x210000);
    mch.clrsetbits32(mchbar::MMARB1, !0xffff_fbff, 0x300);

    mch.clrsetbits32(mchbar::IO_INIT_CFG, 0x002e_0000, 0x0010_0000);
    mch.setbits8(mchbar::IO_INIT_CFG7, 0x36);
    for ch in 0..2 {
        if channel_populated(info, ch) {
            mch.setbits32(mchbar::DRAM_TYPE_SELECT, 0x8000_0000u32 >> ch);
        }
    }
    mch.setbits32(mchbar::DRAM_TYPE_SELECT, 0x4080);
    mch.clrbits16(mchbar::IO_INIT_CLK_DEP, 0x1e00);
    mch.clrsetbits32(
        mchbar::IO_INIT_CFG2,
        0x001f_0000,
        IO_CFG2_TABLE[mem_index(info.timings.mem_clock)] << 17,
    );
    mch.clrbits32(mchbar::IO_INIT_CFG3, 0x0101_0101);
    mch.clrbits32(mchbar::IO_INIT_CFG4, 0x0101_0101);
    mch.clrsetbits8(
        mchbar::IO_INIT_CFG5,
        0x0f,
        IO_CFG5_TABLE[mem_index(info.timings.mem_clock)],
    );
    mch.clrbits32(mchbar::IO_INIT_CFG6, 0x0030_0000);
    let pi = PI_TABLE[mem_index(info.timings.mem_clock)];
    for i in 0..8u32 {
        for ch in 0..2 {
            mch.write32(mchbar::cx_train_pi(ch) + i * 4, pi);
        }
    }
    for ch in 0..2 {
        if channel_populated(info, ch) {
            mch.clrbits8(mchbar::cx_train_cfg(ch), 1);
            mch.setbits32(mchbar::rw_ptr_ctrl(ch), 0x300);
            mch.setbits32(mchbar::cx_dclkdis(ch), 0x3);
        }
    }
    mch.setbits8(mchbar::DRAM_TYPE_SELECT, 0x40);
}

fn rank_addr(mch: &MchBar, ch: usize, rank: usize) -> usize {
    if ch == 0 && rank == 0 {
        return 0;
    }
    let (prev_ch, prev_rank) = if rank == 0 {
        (ch - 1, 3)
    } else {
        (ch, rank - 1)
    };
    let reg = mch.read32(mchbar::cx_drby(prev_ch, prev_rank));
    let shift = (prev_rank % 2) * 16;
    (((reg >> shift) & 0x1fc) << 25) as usize
}

fn jedec_command(mch: &MchBar, addr: usize, cmd: u32, val: u32) {
    mch.clrsetbits32(mchbar::DCC, DCC_SET_EREG_MASK, cmd);
    // SAFETY: after pre-JEDEC map programming, `addr | val` is a DRAM command
    // address used by the memory controller to latch the selected command.
    unsafe { core::ptr::read_volatile((addr | val as usize) as *const u32) };
}

fn jedec_init_ddr2(info: &RaminitInfo, mch: &MchBar) {
    let wr = (((info.timings.twr - 1) as u32) & 7) << 12;
    let dll_reset = 1 << 11;
    let cas = ((info.timings.cas as u32) & 7) << 7;
    let bt_interleaved = 1 << 6;
    let bl8 = 3 << 3;
    let ocd_default = 7 << 10;
    let odt_150 = 1 << 9;
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let ranks = if channel_dual_rank(info, ch) { 2 } else { 1 };
        for r in 0..ranks {
            let addr = rank_addr(mch, ch, r);
            mch.clrsetbits32(mchbar::DCC, DCC_CMD_MASK, DCC_CMD_NOP);
            mch.setbits32(mchbar::DCC, 0x8000);
            jedec_command(mch, addr, DCC_CMD_ABP, 0);
            jedec_command(mch, addr, dcc_set_eregx(2), 0);
            jedec_command(mch, addr, dcc_set_eregx(3), 0);
            jedec_command(mch, addr, DCC_SET_EREG, odt_150);
            jedec_command(
                mch,
                addr,
                DCC_SET_MREG,
                wr | dll_reset | cas | bt_interleaved | bl8,
            );
            jedec_command(mch, addr, DCC_CMD_ABP, 0);
            jedec_command(mch, addr, DCC_CMD_CBR, 0);
            for _ in 0..100 {
                core::hint::spin_loop();
            }
            // SAFETY: second CBR is triggered by a DRAM read to the rank address.
            unsafe { core::ptr::read_volatile(addr as *const u32) };
            jedec_command(mch, addr, DCC_SET_MREG, wr | cas | bt_interleaved | bl8);
            mch.clrsetbits32(mchbar::DCC, DCC_SET_EREG_MASK, DCC_SET_EREG);
            // SAFETY: EMRS1 OCD calibration and exit are command addresses.
            unsafe {
                core::ptr::read_volatile((addr | (ocd_default | odt_150) as usize) as *const u32);
                core::ptr::read_volatile((addr | odt_150 as usize) as *const u32);
            }
            mch.clrbits32(mchbar::DCC, 0x8000);
        }
    }
}

fn post_jedec_and_final(info: &mut RaminitInfo, mch: &MchBar) {
    mch.clrbits16(mchbar::DRAM_TYPE_SELECT, 0x0200);
    mch.clrbits32(mchbar::DCC, DCC_INTERLEAVED);
    if info.timings.channel_mode != ChannelMode::Single {
        mch.setbits32(mchbar::DCC, DCC_INTERLEAVED);
    }
    mch.setbits32(mchbar::DCC, 0x400);
    program_map(info, mch, false);
    if info.timings.channel_mode == ChannelMode::DualInterleaved {
        mch.clrbits32(mchbar::DCC, 0x600);
    }
    for ch in 0..2 {
        mch.write32(mchbar::cx_ait_lo(ch), 0x0000_06c4);
        mch.write32(mchbar::cx_ait_hi(ch), 0x871a_066d);
    }
    mch.clrbits32(mchbar::FSBPMC3, 1 << 1);
    mch.clrsetbits32(mchbar::SBTEST, 0x0008_0006, 0x8000);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 0x0340_4900, 0x04bd_b600);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 0x03c0_4900, 0x003d_b600);

    let force_dual = stepping() != 0 && stepping() < 4;
    for ch in 0..2 {
        let rank_count = if channel_populated(info, ch) {
            if channel_dual_rank(info, ch) || force_dual {
                2
            } else {
                1
            }
        } else {
            0
        };
        let ssds = [0x00u32, 0x91, 0xb1][rank_count];
        mch.clrsetbits32(mchbar::cx_drc1(ch), 0xff00_0000, ssds << 24);
    }
    mch.setbits32(mchbar::DCC, 0xf_8000);
}

#[derive(Clone, Copy)]
struct RecTiming {
    coarse_high: u8,
    coarse_low: u8,
    fine: u8,
}

fn coarse_add(t: &mut RecTiming, step: i16) -> bool {
    let combined = t.coarse_high as i16 * 4 + t.coarse_low as i16 + step;
    if !(0..=63).contains(&combined) {
        return false;
    }
    t.coarse_high = (combined >> 2) as u8;
    t.coarse_low = (combined & 3) as u8;
    true
}

fn program_rec_timing(mch: &MchBar, ch: usize, t: RecTiming) {
    let mut drt3 = mch.read32(mchbar::cx_drt3(ch));
    drt3 = (drt3 & !(0x0f << 7)) | (((t.coarse_high & 0x0f) as u32) << 7);
    mch.write32(mchbar::cx_drt3(ch), drt3);
    let _ = mch.read32(mchbar::cx_drt3(ch));
    mch.clrsetbits8(mchbar::rec_coarse_low(ch), 0x0c, (t.coarse_low & 3) << 2);
    let _ = mch.read32(mchbar::rec_coarse_low(ch));
}

fn write_fine_delay(mch: &MchBar, ch: usize, fine: u8) {
    mch.clrsetbits8(mchbar::cx_train_cfg(ch), 0xf0, fine << 4);
}

fn test_dqs_level(mch: &MchBar, ch: usize, addr: usize, expect_high: bool) -> bool {
    let mut pass = true;
    for _ in 0..3 {
        for c in 0..2 {
            mch.clrbits16(mchbar::rw_ptr_ctrl(c), 1 << 9);
            mch.setbits16(mchbar::rw_ptr_ctrl(c), 1 << 9);
        }
        // SAFETY: `addr` targets initialized DRAM rank under calibration.
        unsafe { core::ptr::read_volatile(addr as *const u32) };
        // SAFETY: port 0x80/0x61 style IO delay is valid on x86 firmware.
        unsafe { fstart_pio::io_delay() };
        let high = (mch.read32(mchbar::rec_dqs_level(ch)) & (1 << 30)) != 0;
        if high != expect_high {
            pass = false;
        }
    }
    pass
}

fn train_channel(info: &mut RaminitInfo, mch: &MchBar, ch: usize) -> Result<(), ServiceError> {
    let mut t = RecTiming {
        coarse_high: info.timings.cas + 1,
        coarse_low: 0,
        fine: 0,
    };
    let addr = if ch == 0 {
        0
    } else if info.timings.channel_mode != ChannelMode::Single {
        0x40
    } else {
        (mch.read16(0x1206) as usize) << 25
    };
    mch.clrsetbits8(mchbar::cx_train_cfg(ch), 0xfe, 1 << 3);
    program_rec_timing(mch, ch, t);

    if test_dqs_level(mch, ch, addr, true) {
        loop {
            if !coarse_add(&mut t, 1) {
                return Err(ServiceError::HardwareError);
            }
            program_rec_timing(mch, ch, t);
            if !test_dqs_level(mch, ch, addr, true) {
                break;
            }
        }
    }

    if !coarse_add(&mut t, 1) {
        return Err(ServiceError::HardwareError);
    }
    program_rec_timing(mch, ch, t);
    if test_dqs_level(mch, ch, addr, true) {
        if !coarse_add(&mut t, -1) {
            return Err(ServiceError::HardwareError);
        }
        program_rec_timing(mch, ch, t);
    }
    while test_dqs_level(mch, ch, addr, false) {
        t.fine += 1;
        if t.fine > 0x0e {
            t.fine = 0;
            if !coarse_add(&mut t, 1) {
                return Err(ServiceError::HardwareError);
            }
            program_rec_timing(mch, ch, t);
        }
        write_fine_delay(mch, ch, t.fine);
    }

    if !coarse_add(&mut t, -3) {
        return Err(ServiceError::HardwareError);
    }
    program_rec_timing(mch, ch, t);
    while test_dqs_level(mch, ch, addr, true) {
        if !coarse_add(&mut t, -4) {
            return Err(ServiceError::HardwareError);
        }
        program_rec_timing(mch, ch, t);
    }
    if !coarse_add(&mut t, 1) {
        return Err(ServiceError::HardwareError);
    }
    program_rec_timing(mch, ch, t);
    info.rec_coarse[ch] = t.coarse_high;
    info.rec_coarse_low[ch] = t.coarse_low;
    info.rec_fine[ch] = t.fine;
    mch.clrsetbits8(
        mchbar::cx_drt5(ch),
        0xf0,
        (t.coarse_high.saturating_sub(1) & 0x0f) << 4,
    );
    Ok(())
}

fn receive_enable_training(info: &mut RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    for ch in 0..2 {
        mch.setbits32(mchbar::train_enable(ch), TRAIN_ENABLE_BIT);
    }
    mch.clrsetbits32(
        mchbar::IO_INIT_CFG2,
        0x0f80_0000,
        IO_INIT_CFG2_TRAINING[mem_index(info.timings.mem_clock)] << 23,
    );
    for ch in 0..2 {
        mch.clrbits16(mchbar::rw_ptr_ctrl(ch), 0x0c00);
    }
    mch.clrbits16(mchbar::DCC, 0x8000);
    for ch in 0..2 {
        if channel_populated(info, ch) {
            train_channel(info, mch, ch)?;
        }
    }
    for ch in 0..2 {
        mch.setbits16(mchbar::rw_ptr_ctrl(ch), 0x0c00);
        mch.clrbits32(mchbar::train_enable(ch), TRAIN_ENABLE_BIT);
        mch.setbits8(mchbar::cx_drc1(ch), 0x40);
        mch.clrbits8(mchbar::cx_drc1(ch), 0x40);
        mch.clrbits16(mchbar::rw_ptr_ctrl(ch), 1 << 9);
        mch.setbits16(mchbar::rw_ptr_ctrl(ch), 0x0600);
    }
    Ok(())
}

fn wait_rcomp(mch: &MchBar) -> Result<(), ServiceError> {
    if stepping() == 1 {
        return Ok(());
    }
    let mut timeout = 10_000_000u32;
    while (mch.read32(mchbar::RCOMP_CTRL) & 1) != 0 {
        timeout = timeout.saturating_sub(1);
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn program_channel_population(info: &RaminitInfo, mch: &MchBar) {
    let mut pop = 0u8;
    if channel_populated(info, 0) {
        pop |= 1;
    }
    if channel_populated(info, 1) {
        pop |= 2;
    }
    mch.setbits8(0x0a2f, pop);
    mch.setbits32(0x0a30, 1 << 26);
    mch.write16(mchbar::DCC2, (mch.read8(0x0a2e) & 0x1f) as u16);
}

fn dram_power_mgmt(mch: &MchBar) {
    for ch in 0..2u32 {
        let base = 0x1000 + ch * 0x40;
        mch.write8(base + 0x18, mch.read8(base + 0x1d) & 0x7f);
        mch.write8(base + 0x07, 0);
        mch.write32(base + 0x10, if ch == 0 { 0x17 } else { 0 });
        mch.write32(base + 0x14, 0);
        mch.write8(base + 0x1c, 0x98);
    }
    mch.write8(0x1080, 0x0e);
    mch.write8(0x1070, 1);
    mch.write16(0x1001, 0x9200);
    mch.write16(0x1041, 0);
    for ch in 0..2 {
        let base = 0x1000 + ch as u32 * 0x40;
        mch.setbits32(base + 0x10, 1 << 31);
        mch.setbits8(base + 0x18, 0x80);
        mch.setbits8(base + 0x1c, 1);
        mch.setbits32(mchbar::cx_pwr_throttle1(ch), 1 << 31);
    }
}

/// Probe and decode the SPD EEPROMs wired to GM965.
///
/// `spd_addresses` follows coreboot's GM965 slot mapping: index 0/1 are
/// channel 0, index 2/3 are channel 1. A zero address means the slot is not
/// wired. Lenovo X61 uses `[0x50, 0, 0x51, 0]`.
pub fn probe_dimms(
    bus: &mut dyn SmBus,
    spd_addresses: &[u8; 4],
) -> Result<RaminitInfo, ServiceError> {
    let mut info = RaminitInfo::default();
    let mut channel_populated = [false; 2];

    for (slot, addr) in spd_addresses.iter().copied().enumerate() {
        if addr == 0 {
            continue;
        }

        let Some(spd) = fstart_spd::ddr2::read_spd(bus, addr)? else {
            fstart_log::info!("gm965 raminit: no DIMM SPD at {:#x}", addr);
            continue;
        };

        let Some(dimm) = fstart_spd::ddr2::decode_dimm(&spd) else {
            fstart_log::error!("gm965 raminit: invalid/non-DDR2 SPD at {:#x}", addr);
            return Err(ServiceError::HardwareError);
        };

        let channel = if slot < 2 { 0 } else { 1 };
        let slot_info = DimmSlot::from_spd(slot, addr, &dimm);
        info.dimms[slot] = slot_info;
        info.dimm_count += 1;
        info.total_mb = info.total_mb.saturating_add(slot_info.capacity_mb);
        channel_populated[channel] = true;

        fstart_log::info!(
            "gm965 raminit: slot {} ch{} addr {:#x}: {} MiB, {} ranks, {} banks, x{}",
            slot as u32,
            channel as u32,
            addr,
            slot_info.capacity_mb,
            slot_info.ranks as u32,
            slot_info.banks as u32,
            if slot_info.x16 { 16 } else { 8 },
        );
    }

    info.channels = channel_populated.iter().filter(|p| **p).count() as u8;
    if info.dimm_count == 0 {
        fstart_log::error!("gm965 raminit: no DDR2 DIMMs detected");
        return Err(ServiceError::HardwareError);
    }

    Ok(info)
}

/// Run GM965 cold-boot DDR2 initialization.
///
/// The sequence follows coreboot's GM965 `raminit.c`: SPD/timing selection,
/// CLKCFG PLL latch, temporary pre-JEDEC address map, RCOMP, timing/control
/// programming, DDR2 JEDEC commands, final memory map, receive-enable
/// calibration, guarded EPD channel population, and DRAM power-management setup.
pub fn cold_boot_train(info: &mut RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    reset_on_stale_rcomp(mch);
    init_pmcon();

    let hb = fstart_ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let fsb_clock = read_fsb_clock(mch);
    let capid0 = hb.read32(hostbridge::CAPID0);

    select_frequency_and_cas(info, fsb_clock, capid0)?;
    calculate_timings(info)?;

    check_bad_warmboot(mch);
    mch.setbits32(mchbar::PMSTS, PMSTS_SELFREFRESH);
    program_clkcfg_lock(info, mch);
    program_gcfgc(info, mch);
    set_clkcross_frequencies(info, mch);

    mch.setbits32(mchbar::FSBPMC3, 1 << 1);
    mch.setbits32(mchbar::SBTEST, 6);
    mch.setbits32(mchbar::POST_JEDEC_TIM0, 0x0300_0000);
    mch.setbits32(mchbar::POST_JEDEC_TIM1, 0x0300_0000);

    program_map(info, mch, true);
    rcomp_init(info, mch);
    odt_and_io_setup(info, mch);
    program_timings(info, mch);
    program_dram_control(info, mch);
    wait_rcomp(mch)?;
    mch.clrbits32(mchbar::RCOMP_CFG3, 1 << 17);
    mch.clrbits32(mchbar::RCOMP_CTRL, (3 << 16) | (3 << 4));

    jedec_init_ddr2(info, mch);
    post_jedec_and_final(info, mch);
    receive_enable_training(info, mch)?;

    if stepping() != 0 {
        mch.setbits8(mchbar::RCOMP_CTRL, 2);
    }
    mch.clrbits32(mchbar::IO_RCOMP_CLK_EN, 1 << 12);

    // Coreboot/vendor RAMINIT only programs the EPD/channel-population path
    // when EPD_2E[4:0] shows a prior successful initialization. On a first
    // cold boot these registers contain hardware defaults; programming the
    // partial EPD path too early can break otherwise-working DRB/DRA decode.
    if (mch.read8(0x0a2e) & 0x1f) != 0 {
        program_channel_population(info, mch);
    }

    clear_dram_init_in_progress();
    dram_power_mgmt(mch);
    mch.write32(mchbar::SSKPD, 0xcafe);

    fstart_log::info!(
        "gm965 raminit: complete total={} MiB tolud={} MiB rec=({},{}),({},{})",
        info.tom_mb,
        info.tolud_mb,
        info.rec_coarse[0] as u32,
        info.rec_coarse_low[0] as u32,
        info.rec_coarse[1] as u32,
        info.rec_coarse_low[1] as u32,
    );
    Ok(())
}
