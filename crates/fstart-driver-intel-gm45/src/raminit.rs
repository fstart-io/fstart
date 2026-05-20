//! GM45 DDR2/DDR3 raminit scaffolding.
//!
//! The coreboot GM45 raminit is a large cold-boot training flow. This module
//! shares SPD decoding with `fstart-spd`, then builds the GM45-specific sysinfo
//! shape: DIMM topology, common frequency/CAS, and derived timing clocks.
//! Controller programming is being ported incrementally from coreboot.

use fstart_mmio::RawMmioBar;
use fstart_services::{ServiceError, SmBus};
use fstart_spd::{ChipCapacity, ChipWidth, DimmInfo, DDR3, SPD_MEMORY_TYPE};

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

use crate::{
    EpBar, Gm45HostBridgePciConfig, MchBar, ESMRAMC_REG, GCFGC_HI_REG, GCFGC_LO_REG,
    HOST_BRIDGE_UNDOC_F0_REG,
};

pub mod hostbridge {
    /// Host bridge: bus 0, device 0, function 0.
    pub const HOST_DEV: u8 = 0;
    pub const HOST_FUNC: u8 = 0;

    pub const EPBAR_LO: u16 = 0x40;
    pub const EPBAR_HI: u16 = 0x44;
    pub const GGC: u16 = 0x52;
    pub const MCHBAR_LO: u16 = 0x48;
    pub const MCHBAR_HI: u16 = 0x4c;
    pub const DEVEN: u16 = 0x54;
    pub const PCIEXBAR_LO: u8 = 0x60;
    pub const PCIEXBAR_HI: u8 = 0x64;
    pub const DMIBAR_LO: u16 = 0x68;
    pub const DMIBAR_HI: u16 = 0x6c;
    /// GM45 PMBASE host-bridge register; coreboot programs this to 0x500 | 1
    /// in `northbridge/intel/gm45/early_init.c::gm45_early_init()`.
    pub const PMBASE: u16 = 0x78;
    pub const PAM0: u16 = 0x90;
    pub const REMAPBASE: u16 = 0x98;
    pub const REMAPLIMIT: u16 = 0x9a;
    pub const SMRAM: u16 = 0x9d;
    pub const ESMRAMC: u16 = 0x9e;
    pub const TOM: u16 = 0xa0;
    pub const TOUUD: u16 = 0xa2;
    pub const TOLUD: u16 = 0xb0;
    pub const CAPID0: u16 = 0xe0;
    pub const SKPD: u16 = 0xdc;

    pub const PEG_DEV: u8 = 1;
    pub const PEG_FUNC: u8 = 0;
    pub const IGD_DEV: u8 = 2;
    pub const IGD_FUNC: u8 = 0;
    pub const IGD_ALT_FUNC: u8 = 1;
    pub const IGD_BAR0_GTTMMADR: u16 = 0x10;
    pub const IGD_BSM: u16 = 0x5c;
    pub const IGD_MSAC: u16 = 0x62;
    pub const IGD_SWSCI: u16 = 0xe8;
    pub const IGD_GDRST: u16 = 0xc0;
    pub const IGD_DISPLAY_CLOCK: u16 = 0xcc;
    pub const IGD_ASLS: u16 = 0xfc;
    pub const GCFGC: u16 = 0xf0;

    pub const DEFAULT_ECAM_BASE: usize = 0xe000_0000;
}

pub mod mchbar {
    pub const FSBPMC3: u32 = 0x0040;
    pub const PM_CTRL0: u32 = 0x0040;
    pub const PM_CTRL1: u32 = 0x0044;
    pub const PM_NOCARB: u32 = 0x0090;
    pub const FSBPMC5: u32 = 0x0094;
    pub const PM_NOCARB_HI: u32 = 0x0094;
    pub const DCC: u32 = 0x0200;
    pub const DCC2: u32 = 0x0204;
    pub const CLKCROSS_DATA3: u32 = 0x0208;
    pub const CLKCROSS_DATA2: u32 = 0x020c;
    pub const CLKCROSS_DATA1: u32 = 0x0210;
    pub const WRITE_CTRL: u32 = 0x0218;
    pub const MMARB0: u32 = 0x0220;
    pub const MMARB1: u32 = 0x0224;
    pub const SBTEST: u32 = 0x0230;
    pub const POST_JEDEC_TIM0: u32 = 0x0238;
    pub const POST_JEDEC_TIM1: u32 = 0x023c;
    pub const RCOMP_CTRL: u32 = 0x0400;
    pub const RCOMP_STATUS: u32 = 0x0404;
    pub const RCOMP_CFG: u32 = 0x040c;
    pub const RCOMP_CFG2: u32 = 0x0414;
    pub const RCOMP_CFG3: u32 = 0x0418;
    pub const RCOMP_CFG4: u32 = 0x041c;
    pub const RCOMP_ODT0: u32 = 0x04d0;
    pub const RCOMP_ODT1: u32 = 0x04d4;
    pub const RCOMP_TABLES: u32 = 0x0680;
    pub const PM_SCHED: u32 = 0x0b00;
    pub const PM_SCHED_B90: u32 = 0x0b90;
    pub const IGD_HSYNC_VSYNC: u32 = 0x0bd0;
    pub const PM_BD8: u32 = 0x0bd8;
    pub const CLKCFG: u32 = 0x0c00;
    pub const CLKCFG_C14: u32 = 0x0c14;
    pub const CLKCFG_C16: u32 = 0x0c16;
    pub const CLKCFG_C20: u32 = 0x0c20;
    pub const HGIPMC2_LO: u32 = 0x0c38;
    pub const HGIPMC2_HI: u32 = 0x0c3a;
    pub const MCHBAR_FFC: u32 = 0x0ffc;
    pub const C2C3TT: u32 = 0x0f00;
    pub const C3C4TT: u32 = 0x0f04;
    pub const PM_F08: u32 = 0x0f08;
    pub const PM_F10: u32 = 0x0f10;
    pub const PMSTS: u32 = 0x0f14;
    pub const PM_F60: u32 = 0x0f60;
    pub const PM_F80: u32 = 0x0f80;
    pub const GIPMC1: u32 = 0x0fb0;
    pub const FSBPMC1: u32 = 0x0fb8;
    pub const UPMC3: u32 = 0x0fc0;
    pub const IO_INIT_CFG: u32 = 0x1400;
    pub const IO_INIT_CLK_DEP: u32 = 0x140c;
    pub const IO_INIT_CFG2: u32 = 0x1414;
    pub const IO_INIT_CFG3: u32 = 0x1418;
    pub const IO_INIT_CFG4: u32 = 0x141c;
    pub const IO_INIT_CFG5: u32 = 0x142c;
    pub const DRAM_TYPE_SELECT: u32 = 0x1434;
    pub const IO_INIT_CFG6: u32 = 0x1438;
    pub const IO_INIT_CFG7: u32 = 0x1440;
    pub const IO_RCOMP_CLK_EN: u32 = 0x1444;
    pub const THERMAL_ENABLE: u32 = 0x10ef;
    pub const SSKPD: u32 = 0x0c1c;

    pub const fn cx_drby(ch: usize, rank: usize) -> u32 {
        0x1200 + (ch as u32) * 0x100 + ((rank as u32) / 2) * 4
    }
    pub const fn cx_dra(ch: usize) -> u32 {
        0x1208 + (ch as u32) * 0x100
    }
    pub const fn cx_dra_hi(ch: usize) -> u32 {
        0x120a + (ch as u32) * 0x100
    }
    pub const fn cx_dclkdis(ch: usize) -> u32 {
        0x120c + (ch as u32) * 0x100
    }
    pub const fn cx_drt0(ch: usize) -> u32 {
        0x1210 + (ch as u32) * 0x100
    }
    pub const fn cx_drt1(ch: usize) -> u32 {
        0x1214 + (ch as u32) * 0x100
    }
    pub const fn cx_drt2(ch: usize) -> u32 {
        0x1218 + (ch as u32) * 0x100
    }
    pub const fn cx_drt3(ch: usize) -> u32 {
        0x121c + (ch as u32) * 0x100
    }
    pub const fn cx_drt4(ch: usize) -> u32 {
        0x1220 + (ch as u32) * 0x100
    }
    pub const fn cx_drt5(ch: usize) -> u32 {
        0x1224 + (ch as u32) * 0x100
    }
    pub const fn cx_drt6(ch: usize) -> u32 {
        0x1228 + (ch as u32) * 0x100
    }
    pub const fn cx_drc0(ch: usize) -> u32 {
        0x1230 + (ch as u32) * 0x100
    }
    pub const fn cx_drc1(ch: usize) -> u32 {
        0x1234 + (ch as u32) * 0x100
    }
    pub const fn cx_drc2(ch: usize) -> u32 {
        0x1238 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_low(ch: usize) -> u32 {
        0x1248 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_high(ch: usize) -> u32 {
        0x124c + (ch as u32) * 0x100
    }
    pub const fn cx_ait_lo(ch: usize) -> u32 {
        0x1250 + (ch as u32) * 0x100
    }
    pub const fn cx_ait_hi(ch: usize) -> u32 {
        0x1254 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_misc(ch: usize) -> u32 {
        0x1260 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_timing(ch: usize) -> u32 {
        0x1268 + (ch as u32) * 0x100
    }
    pub const fn cx_pwr_throttle1(ch: usize) -> u32 {
        0x1274 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_ctrl(ch: usize) -> u32 {
        0x12a0 + (ch as u32) * 0x100
    }
    pub const fn train_enable(ch: usize) -> u32 {
        0x12a4 + (ch as u32) * 0x100
    }
    pub const fn cx_train_cfg(ch: usize) -> u32 {
        0x1484 + (ch as u32) * 0x100
    }
    pub const fn cx_wrty(ch: usize, group: usize) -> u32 {
        0x1470 + (ch as u32) * 0x100 + (3 - group as u32) * 4
    }
    pub const fn cx_recy(ch: usize, group: usize) -> u32 {
        0x14a0 + (ch as u32) * 0x100 + (3 - group as u32) * 4
    }
    pub const fn cx_rdty(ch: usize, lane: usize) -> u32 {
        0x14b0 + (ch as u32) * 0x100 + (7 - lane as u32) * 4
    }
    pub const fn cx_train_pi(ch: usize) -> u32 {
        0x1490 + (ch as u32) * 0x100
    }
    pub const fn rec_dqs_level(ch: usize) -> u32 {
        0x14ac + (ch as u32) * 0x100
    }
    pub const fn rec_coarse_low(ch: usize) -> u32 {
        0x14b0 + (ch as u32) * 0x100
    }
    pub const fn rw_ptr_ctrl(ch: usize) -> u32 {
        0x14f0 + (ch as u32) * 0x100
    }
}

pub mod epbar {
    pub const EPPVCCAP1: u32 = 0x004;
    pub const EPVC0RCTL: u32 = 0x014;
    pub const EPVC1RCAP: u32 = 0x01c;
    pub const EPVC1RCTL: u32 = 0x020;
    pub const EPVC1RSTS: u32 = 0x026;
    pub const EPVC1MTS: u32 = 0x028;
    pub const EPVC1ITC: u32 = 0x02c;
    pub const EPVC1IST: u32 = 0x038;
    pub const EPESD: u32 = 0x044;
    pub const EPLE1D: u32 = 0x050;
    pub const EPLE1A: u32 = 0x058;
    pub const EPLE2D: u32 = 0x060;
    pub const EPLE2A: u32 = 0x068;

    pub const fn portarb(idx: u32) -> u32 {
        0x100 + 4 * idx
    }
}

register_bitfields! [u32,
    /// DCC — DRAM Controller Control.
    DCC_REG [
        INTERLEAVED OFFSET(1) NUMBITS(1) [],
        NO_CHANXOR OFFSET(10) NUMBITS(1) [],
        CMD OFFSET(16) NUMBITS(3) [],
        INIT_COMPLETE OFFSET(19) NUMBITS(1) [],
        SET_EREG_RANK OFFSET(21) NUMBITS(2) []
    ],
    /// CLKCFG — GM45 clock configuration.
    CLKCFG_REG [
        MEMCLK OFFSET(4) NUMBITS(3) [],
        UPDATE OFFSET(12) NUMBITS(1) []
    ],
    /// PMSTS — GM45 power-management status.
    PMSTS_REG [
        SELFREFRESH OFFSET(0) NUMBITS(1) [],
        WARM_RESET OFFSET(1) NUMBITS(1) []
    ],
    /// TRAIN_ENABLE — per-channel training enable register.
    TRAIN_ENABLE_REG [
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    /// CxDRC0 — channel DRAM control 0.
    CX_DRC0_REG [
        RMS OFFSET(8) NUMBITS(3) [],
        RANKEN OFFSET(24) NUMBITS(4) []
    ],
    /// CxDRC1 — channel DRAM control 1.
    CX_DRC1_REG [
        MUSTWR OFFSET(11) NUMBITS(2) [],
        NOTPOP OFFSET(16) NUMBITS(4) []
    ],
    /// CxDRC2 — channel DRAM control 2.
    CX_DRC2_REG [
        CLK1067MT OFFSET(0) NUMBITS(1) [],
        MUSTWR OFFSET(12) NUMBITS(1) [],
        NOTPOP OFFSET(24) NUMBITS(4) []
    ],
    /// CxDRA — channel DRAM rank attributes.
    CX_DRA_REG [
        BANKS OFFSET(16) NUMBITS(11) []
    ]
];

register_bitfields! [u8,
    /// ICH9 GEN_PMCON_2 bits touched around GM45 reset handling.
    GEN_PMCON_2_REG [
        DRAM_INIT OFFSET(7) NUMBITS(1) []
    ],
    /// ICH9 GEN_PMCON_3 bits touched around GM45 reset handling.
    GEN_PMCON_3_REG [
        RTC_PWR_STS OFFSET(1) NUMBITS(1) [],
        SLP_S3_STRETCH OFFSET(3) NUMBITS(1) []
    ]
];

const TCK_266MHZ_256NS: u32 = 960; // 3.75 ns / DDR2-533 / DDR3-533MT
const TCK_333MHZ_256NS: u32 = 768; // 3.00 ns / DDR2-667 / DDR3-667MT
const TCK_400MHZ_256NS: u32 = 640; // 2.50 ns / DDR3-800MT
const TCK_533MHZ_256NS: u32 = 480; // 1.875 ns / DDR3-1067MT
const TRFC_TABLE: [[u8; 4]; 3] = [
    // 256Mb, 512Mb, 1Gb, 2Gb
    [20, 28, 34, 52], // DDR2-533 fallback (below coreboot's normal GM45 range)
    [25, 35, 43, 65], // DDR2-667
    [30, 42, 51, 78], // DDR2-800
];
const DDR3_TRFC_TABLE: [[u8; 4]; 3] = [
    // 256Mb, 512Mb, 1Gb, 2Gb; coreboot tRFC_from_clock_and_cap.
    [40, 56, 68, 104], // DDR3-1067MT
    [30, 42, 51, 78],  // DDR3-800MT
    [25, 35, 43, 65],  // DDR3-667MT
];
const DRT4_ROM_TABLE: [u32; 3] = [0, 0x508c_3828, 0x3874_3021];
const DRT5_ROM_BYTES: [u32; 3] = [0, 0x28, 0x21];
const DRT0_TWTR_LUT: [u8; 3] = [3, 3, 3];
const DCC_INTERLEAVED: u32 = DCC_REG::INTERLEAVED::SET.value;
const DCC_NO_CHANXOR: u32 = DCC_REG::NO_CHANXOR::SET.value;
const DCC_CMD_MASK: u32 = DCC_REG::CMD.mask << DCC_REG::CMD.shift;
const DCC_CMD_NOP: u32 = DCC_REG::CMD.val(1).value;
const DCC_CMD_ABP: u32 = DCC_REG::CMD.val(2).value;
const DCC_SET_MREG: u32 = DCC_REG::CMD.val(3).value;
const DCC_SET_EREG: u32 = DCC_REG::CMD.val(4).value;
const DCC_SET_EREG_MASK: u32 = (DCC_REG::CMD.mask << DCC_REG::CMD.shift)
    | (DCC_REG::SET_EREG_RANK.mask << DCC_REG::SET_EREG_RANK.shift);
const DCC_CMD_CBR: u32 = DCC_REG::CMD.val(6).value;
const DCC_CMD_NORMAL_OPERATION: u32 = DCC_REG::CMD.val(7).value;
const DCC_INIT_COMPLETE: u32 = DCC_REG::INIT_COMPLETE::SET.value;
const CLKCFG_MEMCLK_MASK: u32 = CLKCFG_REG::MEMCLK.mask << CLKCFG_REG::MEMCLK.shift;
const CLKCFG_UPDATE: u32 = CLKCFG_REG::UPDATE::SET.value;
const PMSTS_SELFREFRESH: u32 = PMSTS_REG::SELFREFRESH::SET.value;
const PMSTS_WARM_RESET: u32 = PMSTS_REG::WARM_RESET::SET.value;
const TRAIN_ENABLE_BIT: u32 = TRAIN_ENABLE_REG::ENABLE::SET.value;
const LPC_DEV: u8 = 0x1f;
const LPC_FUNC: u8 = 0;
const GEN_PMCON_2: u16 = 0xa2;
const GEN_PMCON_3: u16 = 0xa4;
const GEN_PMCON_2_DRAM_INIT: u8 = GEN_PMCON_2_REG::DRAM_INIT::SET.value;
const GEN_PMCON_2_STATUS_CLR: u8 = 0xe6;
const GEN_PMCON_3_RTC_PWR_STS: u8 = GEN_PMCON_3_REG::RTC_PWR_STS::SET.value;
const GEN_PMCON_3_SLP_S3_STRETCH: u8 = GEN_PMCON_3_REG::SLP_S3_STRETCH::SET.value;
const CMOS_READ_TRAINING: u8 = 0x80;
const CMOS_WRITE_TRAINING: u8 = 0x90;
const CX_DRC0_RANKEN_MASK: u32 = CX_DRC0_REG::RANKEN.mask << CX_DRC0_REG::RANKEN.shift;
const CX_DRC0_RMS_MASK: u32 = CX_DRC0_REG::RMS.mask << CX_DRC0_REG::RMS.shift;
const CX_DRC0_RMS_78_US: u32 = CX_DRC0_REG::RMS.val(2).value;
const CX_DRC1_NOTPOP_MASK: u32 = CX_DRC1_REG::NOTPOP.mask << CX_DRC1_REG::NOTPOP.shift;
const CX_DRC1_MUSTWR: u32 = CX_DRC1_REG::MUSTWR.val(3).value;
const CX_DRC2_NOTPOP_MASK: u32 = CX_DRC2_REG::NOTPOP.mask << CX_DRC2_REG::NOTPOP.shift;
const CX_DRC2_MUSTWR: u32 = CX_DRC2_REG::MUSTWR::SET.value;
const CX_DRC2_CLK1067MT: u32 = CX_DRC2_REG::CLK1067MT::SET.value;
const CX_DRA_BANKS_MASK: u32 = CX_DRA_REG::BANKS.mask << CX_DRA_REG::BANKS.shift;
const STEPPING_CONVERSION_A1: u8 = 9;

/// GM45 FSB clock strap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsbClock {
    /// FSB-1067.
    Fsb1067 = 1,
    /// FSB-800.
    #[default]
    Fsb800 = 2,
    /// FSB-667.
    Fsb667 = 3,
}

/// DRAM generation selected from SPD byte 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DdrType {
    /// DDR2 SDRAM.
    #[default]
    Ddr2,
    /// DDR3 SDRAM.
    Ddr3,
}

/// GM45 memory clock selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemClock {
    /// DDR2-533 (266 MHz clock).
    #[default]
    Ddr2_533,
    /// DDR2-667 (333 MHz clock).
    Ddr2_667,
    /// DDR2-800 (400 MHz clock).
    Ddr2_800,
    /// DDR3-667MT (333 MHz clock).
    Ddr3_667,
    /// DDR3-800MT (400 MHz clock).
    Ddr3_800,
    /// DDR3-1067MT (533 MHz clock).
    Ddr3_1067,
}

impl FsbClock {
    const fn data_rate_mt(self) -> u32 {
        match self {
            Self::Fsb1067 => 1067,
            Self::Fsb800 => 800,
            Self::Fsb667 => 667,
        }
    }
}

impl MemClock {
    const fn tck_256ns(self) -> u32 {
        match self {
            Self::Ddr2_533 => TCK_266MHZ_256NS,
            Self::Ddr2_667 | Self::Ddr3_667 => TCK_333MHZ_256NS,
            Self::Ddr2_800 | Self::Ddr3_800 => TCK_400MHZ_256NS,
            Self::Ddr3_1067 => TCK_533MHZ_256NS,
        }
    }

    const fn ddr2_index(self) -> usize {
        match self {
            Self::Ddr2_533 => 0,
            Self::Ddr2_667 => 1,
            Self::Ddr2_800 => 2,
            _ => 0,
        }
    }

    const fn ddr3_index(self) -> usize {
        match self {
            Self::Ddr3_1067 => 0,
            Self::Ddr3_800 => 1,
            Self::Ddr3_667 => 2,
            _ => 2,
        }
    }

    const fn coreboot_mem_index(self) -> usize {
        match self {
            Self::Ddr2_533 => 2,
            Self::Ddr2_667 => 2,
            Self::Ddr2_800 => 1,
            Self::Ddr3_1067 => 0,
            Self::Ddr3_800 => 1,
            Self::Ddr3_667 => 2,
        }
    }

    const fn data_rate_mt(self) -> u32 {
        match self {
            Self::Ddr2_533 => 533,
            Self::Ddr2_667 | Self::Ddr3_667 => 667,
            Self::Ddr2_800 | Self::Ddr3_800 => 800,
            Self::Ddr3_1067 => 1067,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Ddr2_533 => "DDR2-533",
            Self::Ddr2_667 => "DDR2-667",
            Self::Ddr2_800 => "DDR2-800",
            Self::Ddr3_667 => "DDR3-667",
            Self::Ddr3_800 => "DDR3-800",
            Self::Ddr3_1067 => "DDR3-1067",
        }
    }
}

/// GM45 channel mode selected from populated channels.
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

/// One decoded GM45 DIMM slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct DimmSlot {
    /// Whether a valid SPD was found.
    pub present: bool,
    /// DRAM generation for this DIMM.
    pub ddr_type: DdrType,
    /// SMBus SPD EEPROM address.
    pub spd_addr: u8,
    /// Channel number, derived from the GM45 slot index.
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
    /// Supported CAS-latency bitmask; bit N means CAS N.
    pub cas_supported: u32,
    /// Decoded tCK per CAS level, in units of 1/256 ns. DDR3 uses `tck_min_256ns`.
    pub cycle_time_256ns: [u32; 8],
    /// Minimum cycle time from DDR3 SPD, in units of 1/256 ns.
    pub tck_min_256ns: u32,
    /// Minimum CAS access time, in units of 1/256 ns.
    pub taa_min_256ns: u32,
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
    /// Decoded tFAW, in units of 1/256 ns.
    pub tfaw_256ns: u32,
    /// SPD density/chip-capacity code used by GM45 timing tables.
    pub chip_capacity_idx: u8,
    /// GM45 raw card type: 0xa = A, ... 0xf = F.
    pub raw_card_type: u8,
}

impl DimmSlot {
    fn from_spd(slot: usize, addr: u8, dimm: &DimmInfo) -> Self {
        let capacity_mb = dimm.rank_capacity_mb.saturating_mul(dimm.ranks as u32);
        Self {
            present: true,
            ddr_type: DdrType::Ddr2,
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
            cas_supported: dimm.cas_latencies as u32,
            cycle_time_256ns: dimm.cycle_time_256ns,
            tck_min_256ns: 0,
            taa_min_256ns: dimm.access_time_256ns.iter().copied().max().unwrap_or(0),
            trcd_256ns: dimm.trcd_256ns,
            trp_256ns: dimm.trp_256ns,
            tras_256ns: dimm.tras_256ns,
            twr_256ns: dimm.twr_256ns,
            trrd_256ns: dimm.trrd_256ns,
            trtp_256ns: dimm.trtp_256ns,
            tfaw_256ns: 0,
            chip_capacity_idx: chip_capacity_index(dimm.chip_capacity),
            raw_card_type: 0x0a,
        }
    }

    fn from_ddr3_spd(slot: usize, addr: u8, dimm: &fstart_spd::ddr3::Ddr3DimmInfo) -> Self {
        let capacity_mb = dimm.rank_capacity_mb.saturating_mul(dimm.ranks as u32);
        Self {
            present: true,
            ddr_type: DdrType::Ddr3,
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
            cycle_time_256ns: [0; 8],
            tck_min_256ns: dimm.tck_min_256ns,
            taa_min_256ns: dimm.taa_min_256ns,
            trcd_256ns: dimm.trcd_256ns,
            trp_256ns: dimm.trp_256ns,
            tras_256ns: dimm.tras_256ns,
            twr_256ns: dimm.twr_256ns,
            trrd_256ns: dimm.trrd_256ns,
            trtp_256ns: dimm.trtp_256ns,
            tfaw_256ns: dimm.tfaw_256ns,
            chip_capacity_idx: dimm.density_code,
            raw_card_type: 0x0a + dimm.raw_card,
        }
    }

    /// Compute the vendor/GM965-derived GM45 EPD DRA encoding for this DIMM.
    ///
    /// This mirrors the vendor/GM965 EPD encoding path: the low byte describes
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

/// Computed GM45 timing parameters.
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
    /// tRD in clocks.
    pub trd: u8,
    /// tFAW in clocks.
    pub tfaw: u8,
    /// tWL in clocks.
    pub twl: u8,
    /// FSB strap.
    pub fsb_clock: FsbClock,
    /// Selected DDR2/DDR3 memory data rate.
    pub mem_clock: MemClock,
    /// Channel mode.
    pub channel_mode: ChannelMode,
}

/// SPD-derived GM45 memory topology and selected timings.
#[derive(Debug, Clone, Copy, Default)]
pub struct RaminitInfo {
    /// Slot 0/1 are channel 0, slot 2/3 are channel 1. X200 uses 0 and 2.
    pub dimms: [DimmSlot; 4],
    /// Number of populated DIMMs.
    pub dimm_count: u8,
    /// Number of populated channels.
    pub channels: u8,
    /// DRAM generation common to all populated DIMMs.
    pub ddr_type: DdrType,
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

    /// Rank-population bitmap in GM45 slot/rank order.
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

    /// EPD DRA encodings for all four GM45 DIMM slots.
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

fn chip_capacity_index(capacity: ChipCapacity) -> u8 {
    match capacity {
        ChipCapacity::Cap256M => 0,
        ChipCapacity::Cap512M => 1,
        ChipCapacity::Cap1G => 2,
        ChipCapacity::Cap2G => 3,
        ChipCapacity::Cap4G => 4,
        ChipCapacity::Cap8G => 5,
        ChipCapacity::Cap16G => 6,
    }
}

fn msb_index_u32(value: u32) -> Option<u8> {
    if value == 0 {
        None
    } else {
        Some(31 - value.leading_zeros() as u8)
    }
}

fn populated_dimms(info: &RaminitInfo) -> impl Iterator<Item = &DimmSlot> {
    info.dimms.iter().filter(|d| d.present)
}

fn read_fsb_clock(mch: &MchBar) -> Result<FsbClock, ServiceError> {
    match (mch.read32(mchbar::CLKCFG) & 0x7) as u8 {
        // Coreboot GM45 maps CLKCFG[2:0] 6/2/3 to FSB 1067/800/667.
        6 => Ok(FsbClock::Fsb1067),
        2 => Ok(FsbClock::Fsb800),
        3 => Ok(FsbClock::Fsb667),
        _ => Err(ServiceError::HardwareError),
    }
}

fn select_channel_mode(info: &RaminitInfo) -> ChannelMode {
    if info.channels < 2 {
        ChannelMode::Single
    } else {
        // Coreboot selects interleaving whenever both channels are populated,
        // even for mismatched DIMM capacity.
        ChannelMode::DualInterleaved
    }
}

fn select_frequency_and_cas(
    info: &mut RaminitInfo,
    fsb_clock: FsbClock,
    capid0: u32,
) -> Result<(), ServiceError> {
    let mut cas_mask = u32::MAX;
    let mut tck_min_common = 0u32;
    let mut taa_needed = 0u32;

    for dimm in populated_dimms(info) {
        cas_mask &= dimm.cas_supported;
        let Some(max_cas) = msb_index_u32(dimm.cas_supported) else {
            return Err(ServiceError::HardwareError);
        };
        let (tck, taa) = if info.ddr_type == DdrType::Ddr3 {
            (dimm.tck_min_256ns, dimm.taa_min_256ns)
        } else {
            let tck = dimm.cycle_time_256ns[max_cas as usize];
            (tck, (max_cas as u32).saturating_mul(tck))
        };
        if tck == 0 || taa == 0 {
            return Err(ServiceError::HardwareError);
        }
        tck_min_common = tck_min_common.max(tck);
        taa_needed = taa_needed.max(taa);
    }

    if cas_mask == 0 {
        fstart_log::error!("gm45 raminit: no common CAS latency");
        return Err(ServiceError::HardwareError);
    }

    let candidates: &[MemClock] = if info.ddr_type == DdrType::Ddr3 {
        let ddr_cap = (capid0 >> 30) & 0x03;
        match ddr_cap {
            0 => &[MemClock::Ddr3_1067, MemClock::Ddr3_800, MemClock::Ddr3_667],
            1 => &[MemClock::Ddr3_800, MemClock::Ddr3_667],
            _ => &[],
        }
    } else {
        let capid_hi = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC)
            .read32(hostbridge::CAPID0 + 4);
        let max_ddr2_mt = if (capid_hi & (1 << (53 - 32))) != 0 {
            667
        } else {
            800
        };
        if max_ddr2_mt >= 800 && fsb_clock != FsbClock::Fsb667 {
            &[MemClock::Ddr2_800, MemClock::Ddr2_667]
        } else {
            &[MemClock::Ddr2_667]
        }
    };

    for &mem_clock in candidates {
        if mem_clock.data_rate_mt() > fsb_clock.data_rate_mt() {
            continue;
        }

        let tck_clock = mem_clock.tck_256ns();
        if tck_clock < tck_min_common {
            continue;
        }

        let min_cas = if info.ddr_type == DdrType::Ddr3 { 4 } else { 3 };
        let max_cas = if info.ddr_type == DdrType::Ddr3 {
            18
        } else {
            6
        };
        let cas_needed = div_round_up(taa_needed, tck_clock).max(min_cas);
        for cas in cas_needed..=max_cas {
            if (cas_mask & (1 << cas)) != 0 && cas.saturating_mul(tck_clock) < 32 * 160 {
                info.timings.fsb_clock = fsb_clock;
                info.timings.mem_clock = mem_clock;
                info.timings.cas = cas as u8;
                info.timings.channel_mode = select_channel_mode(info);
                fstart_log::info!(
                    "gm45 raminit: selected {} CAS{} (mask={:#x}, tCK={} / {})",
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

    fstart_log::error!("gm45 raminit: no valid frequency/CAS combination");
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
        let mut trrd_dram = 2 + (dimm.page_size as u32 / 1024);
        if info.timings.mem_clock == MemClock::Ddr3_1067 {
            trrd_dram += dimm.page_size as u32 / 1024;
        }
        trrd = trrd.max(trrd_dram);
        trtp = trtp.max(div_round_up(dimm.trtp_256ns, tck).max(2));

        if info.ddr_type == DdrType::Ddr3 {
            let cap_idx = (dimm.chip_capacity_idx as usize).min(3);
            trfc = trfc.max(DDR3_TRFC_TABLE[info.timings.mem_clock.ddr3_index()][cap_idx] as u32);
        } else {
            let width_bits = if dimm.x16 { 4 } else { 3 };
            let bank_bits = if dimm.banks == 8 { 3 } else { 2 };
            let cap_idx = (dimm.rows as i32 + dimm.cols as i32 + width_bits + bank_bits - 28)
                .clamp(0, 3) as usize;
            trfc = trfc.max(TRFC_TABLE[info.timings.mem_clock.ddr2_index()][cap_idx] as u32);
        }
    }

    let mut trd = info.timings.cas as u32;
    match info.timings.fsb_clock {
        FsbClock::Fsb667 => trd += 1,
        FsbClock::Fsb800 => trd += 2,
        FsbClock::Fsb1067 => trd += 3,
    }
    if info.timings.fsb_clock == FsbClock::Fsb1067 && info.timings.mem_clock == MemClock::Ddr3_1067
    {
        trd += 1;
    }

    let mut tfaw = 0u32;
    const TFAW_BY_PAGE_AND_CLOCK: [[u8; 3]; 2] = [[20, 15, 13], [27, 20, 17]];
    let tfaw_clock_idx = match info.timings.mem_clock {
        MemClock::Ddr3_1067 => 0,
        MemClock::Ddr2_800 | MemClock::Ddr3_800 => 1,
        MemClock::Ddr2_667 | MemClock::Ddr3_667 => 2,
        MemClock::Ddr2_533 => 2,
    };
    for dimm in populated_dimms(info) {
        let page_idx = (dimm.page_size / 1024).saturating_sub(1).min(1) as usize;
        tfaw = tfaw.max(TFAW_BY_PAGE_AND_CLOCK[page_idx][tfaw_clock_idx] as u32);
    }

    let twl = if info.ddr_type == DdrType::Ddr3 {
        if info.timings.mem_clock == MemClock::Ddr3_1067 {
            6
        } else {
            5
        }
    } else {
        info.timings.cas.saturating_sub(1) as u32
    };

    if !(4..=31).contains(&tras)
        || !(2..=9).contains(&trp)
        || !(2..=9).contains(&trcd)
        || trfc > 255
    {
        fstart_log::error!(
            "gm45 raminit: derived timings out of range: tRAS={} tRP={} tRCD={} tRFC={}",
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
    info.timings.trd = trd as u8;
    info.timings.tfaw = tfaw as u8;
    info.timings.twl = twl as u8;

    fstart_log::info!(
        "gm45 raminit: timings CAS{} tRAS={} tRP={} tRCD={} tWR={} tRFC={} tRRD={} tRTP={} tRD={} tFAW={} tWL={}",
        info.timings.cas,
        info.timings.tras,
        info.timings.trp,
        info.timings.trcd,
        info.timings.twr,
        info.timings.trfc,
        info.timings.trrd,
        info.timings.trtp,
        info.timings.trd,
        info.timings.tfaw,
        info.timings.twl,
    );
    Ok(())
}

fn stepping() -> u8 {
    fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC).read8(0x08)
}

fn lpc() -> fstart_ecam::EcamDevice {
    fstart_ecam::EcamDevice::new(0, LPC_DEV, LPC_FUNC)
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
        fstart_log::error!("gm45 raminit: stale A0 RCOMP state, issuing reset");
        full_reset();
    }
}

fn gm45_early_reset_prepare(mch: &MchBar) {
    mch.clrsetbits32(mchbar::CLKCFG, 3 << 21, 1 << 3);
    for ch in 0..2 {
        let channel = mch.dram_channel(ch);
        channel
            .drc0
            .set((channel.drc0.get() & !CX_DRC0_RANKEN_MASK) | if ch == 0 { 1 << 24 } else { 0 });
        let drc1 =
            (channel.drc1.get() | CX_DRC1_NOTPOP_MASK) & !(if ch == 0 { 1 << 16 } else { 0 });
        channel.drc1.set(drc1);
        let drc2 =
            (channel.drc2.get() | CX_DRC2_NOTPOP_MASK) & !(if ch == 0 { 1 << 24 } else { 0 });
        channel.drc2.set(drc2);
        for drby in channel.drby.iter() {
            drby.set(4 | (4 << 16));
        }
    }
    mch.clrsetbits32(mchbar::DCC, DCC_CMD_MASK, DCC_CMD_NOP);
    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    hb.write8(0xf0, hb.read8(0xf0) & !(1 << 2));
    hb.write8(0xf0, hb.read8(0xf0) | (1 << 2));
    mch.setbits32(mchbar::DCC, 1 << 19);
}

fn init_pmcon(mch: &MchBar) {
    let lpc = lpc();
    let pmcon2 = lpc.read8(GEN_PMCON_2);
    if (pmcon2 & GEN_PMCON_2_DRAM_INIT) != 0 {
        fstart_log::error!("gm45 raminit: interrupted previous RAM init, issuing reset");
        lpc.write8(GEN_PMCON_2, pmcon2 & !GEN_PMCON_2_DRAM_INIT);
        gm45_early_reset_prepare(mch);
        full_reset();
    }
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
        fstart_log::error!("gm45 raminit: bad warm boot state, issuing reset");
        let lpc = lpc();
        lpc.or8(GEN_PMCON_3, GEN_PMCON_3_SLP_S3_STRETCH);
        lpc.and8(GEN_PMCON_2, !GEN_PMCON_2_DRAM_INIT);
        mch.setbits32(mchbar::PMSTS, PMSTS_WARM_RESET);
        gm45_early_reset_prepare(mch);
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

fn channel_rank_count(info: &RaminitInfo, ch: usize) -> usize {
    let first = ch * 2;
    info.dimms[first..first + 2]
        .iter()
        .filter(|d| d.present)
        .map(|d| d.ranks as usize)
        .sum::<usize>()
        .min(4)
}

fn first_channel_dimm(info: &RaminitInfo, ch: usize) -> Option<DimmSlot> {
    let first = ch * 2;
    info.dimms[first..first + 2]
        .iter()
        .copied()
        .find(|d| d.present)
}

fn set_pci8(dev: &fstart_ecam::EcamDevice, reg: u16, clear: u8, set: u8) {
    dev.write8(reg, (dev.read8(reg) & !clear) | set);
}

fn dcc_set_eregx(x: u32) -> u32 {
    (DCC_SET_EREG | ((x - 1) << 21)) & DCC_SET_EREG_MASK
}

fn mem_index(clock: MemClock) -> usize {
    clock.coreboot_mem_index()
}

fn fsb_index(clock: FsbClock) -> usize {
    match clock {
        FsbClock::Fsb1067 => 0,
        FsbClock::Fsb800 => 1,
        FsbClock::Fsb667 => 2,
    }
}

fn vc1_program_timings(fsb: FsbClock) {
    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let epbar_base = (hb.read32(hostbridge::EPBAR_LO) & 0xffff_f000) as usize;
    let ep = EpBar::new(epbar_base);
    let (itc, ist) = match fsb {
        FsbClock::Fsb1067 => (0x1a, 0x0138_0138),
        FsbClock::Fsb800 => (0x14, 0x00f0_00f0),
        FsbClock::Fsb667 => (0x10, 0x00c0_00c0),
    };
    ep.clrsetbits8(epbar::EPVC1ITC, 0xff, itc);
    ep.write32(epbar::EPVC1IST, ist);
    ep.write32(epbar::EPVC1IST + 4, ist);
}

fn program_dram_type(info: &RaminitInfo, mch: &MchBar) {
    match info.ddr_type {
        // Coreboot programs bit 7 for DDR2 and bits [1:0] for DDR3 before
        // the controller frequency and timing sequence.
        DdrType::Ddr2 => mch.setbits8(mchbar::DRAM_TYPE_SELECT, 1 << 7),
        DdrType::Ddr3 => mch.setbits8(mchbar::DRAM_TYPE_SELECT, 3),
    }
}

fn gm45_clkcfg_memclk_field(clock: MemClock) -> u32 {
    // Coreboot's GM45 `mem_clock_t` encodes 1067/800/667 MT/s as 0/1/2
    // (also 533/400/333 MHz aliases). CLKCFG wants `6 - mem_clock_t` in
    // bits [6:4], so do not depend on this crate's Rust enum discriminants.
    6 - clock.coreboot_mem_index() as u32
}

fn program_clkcfg_lock(info: &RaminitInfo, mch: &MchBar) {
    // Coreboot clears these side-band CLKCFG bits before changing memory
    // frequency, then restores the upper control field at the end of the
    // sequence (`set_system_memory_frequency()`).
    mch.clrbits16(mchbar::CLKCFG + 0x60, 1 << 15);
    mch.clrbits16(mchbar::CLKCFG + 0x48, 1 << 15);

    let want = gm45_clkcfg_memclk_field(info.timings.mem_clock) << 4;
    let mut clkcfg = mch.read32(mchbar::CLKCFG) & !(1 << 17);
    if stepping() == 0 && (clkcfg & 0x300) != 0x300 {
        clkcfg |= 0x300;
    }
    clkcfg = (clkcfg & !(CLKCFG_UPDATE | CLKCFG_MEMCLK_MASK)) | want;
    mch.write32(mchbar::CLKCFG, clkcfg);
    mch.clrsetbits32(mchbar::CLKCFG, CLKCFG_MEMCLK_MASK | CLKCFG_UPDATE, want);
    mch.clrsetbits32(mchbar::CLKCFG, CLKCFG_MEMCLK_MASK, want | CLKCFG_UPDATE);
    mch.clrbits32(mchbar::CLKCFG, CLKCFG_UPDATE);

    if info.timings.fsb_clock == FsbClock::Fsb1067
        && matches!(
            info.timings.mem_clock,
            MemClock::Ddr2_667 | MemClock::Ddr3_667
        )
    {
        // Coreboot `set_system_memory_frequency()` special programming for
        // FSB1067 + MEM667. Offsets are relative to CLKCFG_MCHBAR (0x0c00).
        // Coreboot uses a 32-bit write at +0x16; split it to avoid an
        // unaligned volatile u32 access in Rust while preserving bytes.
        mch.write16(mchbar::CLKCFG + 0x16, 0x30f0);
        mch.write16(mchbar::CLKCFG + 0x18, 0x0000);
        mch.write32(mchbar::CLKCFG + 0x64, 0x0000_50c1);
        mch.clrsetbits32(mchbar::CLKCFG, 1 << 12, 1 << 17);
        mch.setbits32(mchbar::CLKCFG, (1 << 17) | (1 << 12));
        mch.clrbits32(mchbar::CLKCFG, 1 << 12);
        mch.write32(mchbar::CLKCFG + 0x04, 0x9bad_1f1f);
        mch.write8(mchbar::CLKCFG + 0x08, 0xf4);
        mch.write8(mchbar::CLKCFG + 0x0a, 0x43);
        mch.write8(mchbar::CLKCFG + 0x0c, 0x10);
        mch.write8(mchbar::CLKCFG + 0x0d, 0x80);
        mch.write32(mchbar::CLKCFG + 0x50, 0x0b0e_151b);
        mch.write8(mchbar::CLKCFG + 0x54, 0xb4);
        mch.write8(mchbar::CLKCFG + 0x55, 0x10);
        mch.write8(mchbar::CLKCFG + 0x56, 0x08);
        mch.setbits32(mchbar::CLKCFG, 1 << 10);
        mch.setbits32(mchbar::CLKCFG, 1 << 11);
        mch.clrbits32(mchbar::CLKCFG, 1 << 10);
        mch.clrbits32(mchbar::CLKCFG, 1 << 11);
    }

    mch.setbits32(mchbar::CLKCFG + 0x48, 0x3f << 24);
}

fn program_gcfgc(info: &RaminitInfo, mch: &MchBar) {
    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
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
            MemClock::Ddr2_667 | MemClock::Ddr3_667 => 4,
            MemClock::Ddr2_533 | MemClock::Ddr2_800 | MemClock::Ddr3_800 | MemClock::Ddr3_1067 => 3,
        };
    }
    if vco == 3 {
        render = 4;
    }

    let igd = fstart_ecam::EcamDevice::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC);
    if igd.read16(0) != 0xffff {
        set_pci8(
            &igd,
            hostbridge::GCFGC,
            (GCFGC_LO_REG::GMADR_RENDER.mask << GCFGC_LO_REG::GMADR_RENDER.shift)
                | (GCFGC_LO_REG::GMADR_FORMAT.mask << GCFGC_LO_REG::GMADR_FORMAT.shift),
            GCFGC_LO_REG::GMADR_RENDER.val(render).value,
        );
        set_pci8(
            &igd,
            hostbridge::GCFGC + 1,
            GCFGC_HI_REG::STOLEN_MEMORY.mask << GCFGC_HI_REG::STOLEN_MEMORY.shift,
            GCFGC_HI_REG::STOLEN_MEMORY.val(2).value,
        );
    }
}

fn ddr3_fsb_index(clock: FsbClock) -> usize {
    match clock {
        FsbClock::Fsb1067 => 0,
        FsbClock::Fsb800 => 1,
        FsbClock::Fsb667 => 2,
    }
}

fn channel_is_card_f(info: &RaminitInfo, ch: usize) -> bool {
    info.dimms[ch * 2..ch * 2 + 2]
        .iter()
        .any(|d| d.present && d.raw_card_type == 0x0f)
}

fn set_clkcross_frequencies(info: &RaminitInfo, mch: &MchBar) {
    if info.ddr_type == DdrType::Ddr3 {
        // Coreboot `clock_crossing_setup()` tables for GM45 DDR3.
        const DDR3_CROSS: [[[u32; 4]; 3]; 3] = [
            [
                [0x0000_0000, 0x0000_0000, 0x0018_0006, 0x0081_0060],
                [0x0000_0000, 0x0000_0000, 0x0000_001c, 0x0003_00e0],
                [0x0000_0000, 0x0000_1c00, 0x03c0_0038, 0x0007_e000],
            ],
            [
                [0, 0, 0, 0],
                [0x0000_0000, 0x0000_0000, 0x0030_000c, 0x0003_00c0],
                [0x0000_0000, 0x0000_0380, 0x0060_001c, 0x0003_0c00],
            ],
            [
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0x0000_0000, 0x0000_0000, 0x0030_000c, 0x0003_00c0],
            ],
        ];
        const DDR3_CHANNEL: [[u32; 3]; 3] = [
            [0x4010_0401, 0x1004_0220, 0x0804_0110],
            [0x0000_0000, 0x4010_0401, 0x0008_0201],
            [0x0000_0000, 0x0000_0000, 0x4010_0401],
        ];

        let fsb = ddr3_fsb_index(info.timings.fsb_clock);
        let mem = info.timings.mem_clock.ddr3_index();
        let data = DDR3_CROSS[fsb][mem];
        mch.write32(mchbar::CLKCROSS_DATA3, data[3]);
        mch.write32(mchbar::CLKCROSS_DATA2, data[2]);
        if (info.timings.fsb_clock == FsbClock::Fsb1067
            || info.timings.fsb_clock == FsbClock::Fsb800)
            && info.timings.mem_clock == MemClock::Ddr3_667
        {
            mch.write32(mchbar::CLKCROSS_DATA1, data[1]);
        }
        for ch in 0..2 {
            let value = if info.timings.fsb_clock == FsbClock::Fsb1067
                && info.timings.mem_clock == MemClock::Ddr3_800
                && channel_is_card_f(info, ch)
            {
                0x0804_0120
            } else {
                DDR3_CHANNEL[fsb][mem]
            };
            mch.write32(0x1258 + ch as u32 * 0x100, value);
            mch.write32(0x125c + ch as u32 * 0x100, 0);
        }
        return;
    }

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

fn decode_igd_memory_size_kib(gms: u16) -> u32 {
    match gms & 0x0f {
        0x1 => 1024,
        0x2 => 4 * 1024,
        0x3 => 8 * 1024,
        0x4 => 16 * 1024,
        0x5 => 32 * 1024,
        0x6 => 48 * 1024,
        0x7 => 64 * 1024,
        0x8 => 128 * 1024,
        0x9 => 256 * 1024,
        0xa => 96 * 1024,
        0xb => 160 * 1024,
        0xc => 224 * 1024,
        0xd => 352 * 1024,
        _ => 0,
    }
}

fn decode_igd_gtt_size_kib(ggms: u16) -> u32 {
    match ggms & 0x0f {
        0x1 => 1024,
        0x3 | 0x9 => 2 * 1024,
        0xa => 3 * 1024,
        0xb => 4 * 1024,
        _ => 0,
    }
}

fn rank_page_log2(dimm: DimmSlot, pre_jedec: bool) -> u8 {
    if pre_jedec {
        12
    } else {
        // Coreboot derives timing from the SDRAM-device page size, then
        // stores whole-rank page size for CxDRA by multiplying by chips/rank.
        let chips_per_rank = if dimm.x16 { 4 } else { 8 };
        match (dimm.page_size as u32).saturating_mul(chips_per_rank) {
            1024 => 10,
            2048 => 11,
            4096 => 12,
            8192 => 13,
            _ => 12,
        }
    }
}

fn set_dra_page_size(mut reg: u32, rank: usize, log2_page_size: u8) -> u32 {
    // Coreboot's CxDRA_PAGESIZE(rank, log2(page_size)) stores
    // `log2(page_size) - 10` in a 3-bit field.  The temporary pre-JEDEC
    // 4 KiB page therefore encodes as 2; final map uses whole-rank page size.
    let shift = (rank * 4) as u32;
    let encoded = log2_page_size.saturating_sub(10) as u32;
    reg &= !(0x07 << shift);
    reg | ((encoded & 0x07) << shift)
}

fn igd_compute_ggc() {
    // Coreboot `igd_compute_ggc()`: keep IGD enabled unless CAPID disables it,
    // use default gfx_uma_size=4 (32 MiB), 2 MiB GTT, and add shadow GTT when
    // VT-d is available.
    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let capid_hi = hb.read32(hostbridge::CAPID0 + 4);
    let ggc = if (capid_hi & (1 << (33 - 32))) != 0 {
        0x0002
    } else {
        let mut value = 0x0300 | (5 << 4);
        if (capid_hi & (1 << (48 - 32))) == 0 {
            value |= 0x0800;
        }
        value
    };
    hb.write16(hostbridge::GGC, ggc);
}

fn program_map(info: &mut RaminitInfo, mch: &MchBar, pre_jedec: bool) {
    let mut total_mb_by_channel = [0u32; 2];
    let mut base_mb = 0u32;
    let map_mode = if pre_jedec && info.timings.channel_mode == ChannelMode::DualInterleaved {
        // Coreboot never uses interleaving for the temporary pre-JEDEC map.
        ChannelMode::DualAsync
    } else {
        info.timings.channel_mode
    };

    for (ch, channel_total_mb) in total_mb_by_channel.iter_mut().enumerate() {
        if map_mode == ChannelMode::DualInterleaved {
            base_mb = 0;
        }

        let mut dra = mch.read32(mchbar::cx_dra(ch)) & !0xffff;
        for s in 0..2 {
            let slot = ch * 2 + s;
            let dimm = info.dimms[slot];
            let rank_capacity_mb = if pre_jedec {
                128
            } else {
                dimm.rank_capacity_mb
            };
            let first_rank = s * 2;

            if dimm.present {
                base_mb = base_mb.saturating_add(rank_capacity_mb);
                *channel_total_mb = channel_total_mb.saturating_add(rank_capacity_mb);
                dra = set_dra_page_size(dra, first_rank, rank_page_log2(dimm, pre_jedec));
            }
            let low_bound = (base_mb / 32) & 0xffff;

            if dimm.present && dimm.ranks > 1 {
                base_mb = base_mb.saturating_add(rank_capacity_mb);
                *channel_total_mb = channel_total_mb.saturating_add(rank_capacity_mb);
                dra = set_dra_page_size(dra, first_rank + 1, rank_page_log2(dimm, pre_jedec));
            }
            let high_bound = (base_mb / 32) & 0xffff;
            // Coreboot/gm45.h notes that the two 16-bit CxDRBy boundaries in
            // each 32-bit register must be programmed at the same time.
            mch.write32(
                mchbar::cx_drby(ch, first_rank),
                low_bound | (high_bound << 16),
            );
        }
        mch.write32(mchbar::cx_dra(ch), dra);
    }

    mch.write16(mchbar::DCC2, 0);
    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let ggc = if pre_jedec {
        0
    } else {
        hb.read16(hostbridge::GGC)
    };

    let mut uma_size_mb = 0u32;
    if !pre_jedec {
        if (ggc & 2) == 0 {
            uma_size_mb = uma_size_mb
                .saturating_add(decode_igd_memory_size_kib((ggc >> 4) & 0x0f) / 1024)
                .saturating_add(decode_igd_gtt_size_kib((ggc >> 8) & 0x0f) / 1024);
        }
        // Coreboot reserves and enables a 2 MiB TSEG through ESMRAMC here.
        set_pci8(
            &hb,
            hostbridge::ESMRAMC,
            (ESMRAMC_REG::T_EN.mask << ESMRAMC_REG::T_EN.shift)
                | (ESMRAMC_REG::TSEG_SIZE.mask << ESMRAMC_REG::TSEG_SIZE.shift),
            ESMRAMC_REG::T_EN::SET.value | ESMRAMC_REG::TSEG_SIZE.val(1).value,
        );
        uma_size_mb = uma_size_mb.saturating_add(2);
    }

    // X200 devicetree sets pci_mmio_size=2048 MiB; keep that coreboot board
    // policy as the GM45 default until this becomes a serde config field.
    const PCI_MMIO_SIZE_MB: u32 = 2048;
    let mmio_start_mb = 4096u32
        .saturating_sub(PCI_MMIO_SIZE_MB)
        .saturating_add(uma_size_mb);
    let me = fstart_ecam::EcamDevice::new(0, 3, 0);
    let me_active = !pre_jedec && me.read8(0x08) != 0xff;
    let me_size_mb = if me_active { 32 } else { 0 };
    let used_me_size_mb = if total_mb_by_channel[0] != total_mb_by_channel[1] {
        me_size_mb
    } else {
        2 * me_size_mb
    };

    let tom_mb = total_mb_by_channel[0].saturating_add(total_mb_by_channel[1]);
    let mut tom_minus_me_mb = tom_mb.saturating_sub(used_me_size_mb);
    let mut tolud_mb = tom_minus_me_mb.min(mmio_start_mb);
    let mut touud_mb = tom_minus_me_mb;
    let mut remapbase_mb = 0xffffu32;
    let mut remaplimit_mb = 0u32;
    let capid0_hi = hb.read32(hostbridge::CAPID0 + 4);
    let reclaim_capable = (capid0_hi & (1 << (47 - 32))) == 0;

    if reclaim_capable && tom_minus_me_mb >= mmio_start_mb.saturating_add(64) {
        tom_minus_me_mb &= !(64 - 1);
        tolud_mb &= !(64 - 1);
        if tom_minus_me_mb > 4096 {
            remapbase_mb = tom_minus_me_mb;
            remaplimit_mb = remapbase_mb.saturating_add(4096 - tolud_mb);
        } else {
            remapbase_mb = 4096;
            remaplimit_mb = remapbase_mb.saturating_add(tom_minus_me_mb.saturating_sub(tolud_mb));
        }
        touud_mb = remaplimit_mb;
        remaplimit_mb = remaplimit_mb.saturating_sub(64);
    }

    hb.write16(hostbridge::TOM, ((tom_mb >> 7) & 0x01ff) as u16);
    hb.write16(hostbridge::TOLUD, (tolud_mb << 4) as u16);
    hb.write16(hostbridge::TOUUD, touud_mb as u16);
    hb.write16(hostbridge::REMAPBASE, ((remapbase_mb >> 6) & 0x03ff) as u16);
    hb.write16(
        hostbridge::REMAPLIMIT,
        ((remaplimit_mb >> 6) & 0x03ff) as u16,
    );

    if pre_jedec {
        // Coreboot's `prejedec_memory_map()` forces the temporary map to
        // non-interleaved/no-channel-XOR operation so JEDEC command addresses
        // are rank-local and easy to derive.
        mch.clrbits32(mchbar::DCC, DCC_INTERLEAVED);
        mch.setbits32(mchbar::DCC, DCC_NO_CHANXOR);
    } else {
        match map_mode {
            ChannelMode::Single | ChannelMode::DualAsync => {
                mch.clrbits32(mchbar::DCC, DCC_INTERLEAVED)
            }
            ChannelMode::DualInterleaved => {
                mch.clrbits32(mchbar::DCC, DCC_NO_CHANXOR | (1 << 9));
                mch.setbits32(mchbar::DCC, DCC_INTERLEAVED);
            }
        }
    }

    if !pre_jedec {
        info.tom_mb = tom_mb;
        info.tolud_mb = tolud_mb;
        fstart_log::info!(
            "gm45 raminit: map TOM={}M TOLUD={}M TOUUD={}M remap={:#x}/{:#x} ME={}M UMA+TSEG={}M",
            tom_mb,
            tolud_mb,
            touud_mb,
            remapbase_mb,
            remaplimit_mb,
            used_me_size_mb,
            uma_size_mb
        );
    }
}

fn program_timings_ddr3(info: &RaminitInfo, mch: &MchBar) {
    let t = info.timings;
    const DDR3_DRT4_BY_CLOCK: [[u8; 3]; 4] = [
        [0x07, 0x0a, 0x0d],
        [0x3a, 0x46, 0x5d],
        [0x0c, 0x0e, 0x18],
        [0x21, 0x28, 0x35],
    ];
    let clk_idx = 2 - t.mem_clock.ddr3_index();

    for ch in 0..2 {
        let btb_wtp = t.twl as u32 + 4 + t.twr as u32;
        let btb_wtr = t.twl as u32 + 4 + 4;
        let mut reg = mch.read32(mchbar::cx_drt0(ch));
        reg = (reg & !(0x0f << 20)) | ((btb_wtr & 0x0f) << 20);
        reg = (reg & !(0x1f << 26)) | ((btb_wtp & 0x1f) << 26);
        if t.mem_clock != MemClock::Ddr3_1067 {
            reg = (reg & !(0x07 << 15)) | (((9 - t.cas) as u32 & 0x07) << 15);
            reg = (reg & !(0x0f << 10)) | (((t.cas - 3) as u32 & 0x0f) << 10);
        } else {
            reg = (reg & !(0x07 << 15)) | (((10 - t.cas) as u32 & 0x07) << 15);
            reg = (reg & !(0x0f << 10)) | (((t.cas - 4) as u32 & 0x0f) << 10);
        }
        reg = (reg & !(0x07 << 5)) | (3 << 5);
        reg = (reg & !0x07) | 1;
        mch.write32(mchbar::cx_drt0(ch), reg);

        reg = mch.read32(mchbar::cx_drt1(ch));
        reg = (reg & !(0x03 << 28)) | (1 << 28);
        reg = (reg & !(0x1f << 21)) | ((t.tras as u32 & 0x1f) << 21);
        reg = (reg & !(0x07 << 10)) | (((t.trrd - 2) as u32 & 0x07) << 10);
        reg = (reg & !(0x07 << 5)) | (((t.trcd - 2) as u32 & 0x07) << 5);
        reg = (reg & !0x07) | ((t.trp - 2) as u32 & 0x07);
        mch.write32(mchbar::cx_drt1(ch), reg);

        reg = mch.read32(mchbar::cx_drt2(ch));
        reg = (reg & !(0x1f << 17)) | ((t.tfaw as u32 & 0x1f) << 17);
        if t.mem_clock != MemClock::Ddr3_1067 {
            reg = (reg & !(0x07 << 12)) | (2 << 12);
            reg = (reg & !(0x0f << 6)) | (9 << 6);
        } else {
            reg = (reg & !(0x07 << 12)) | (3 << 12);
            reg = (reg & !(0x0f << 6)) | (0x0c << 6);
        }
        reg = (reg & !0x1f) | 0x13;
        mch.write32(mchbar::cx_drt2(ch), reg);

        reg = mch.read32(mchbar::cx_drt3(ch)) | (0x03 << 28);
        reg &= !(0x03 << 26);
        reg = (reg & !(0x07 << 23)) | (((t.cas - 3) as u32 & 0x07) << 23);
        reg = (reg & !(0xff << 13)) | ((t.trfc as u32) << 13);
        reg = (reg & !0x07) | ((t.twl.saturating_sub(2) as u32) & 0x07);
        mch.write32(mchbar::cx_drt3(ch), reg);

        reg = mch.read32(mchbar::cx_drt4(ch));
        reg = (reg & !(0x01f << 27)) | ((DDR3_DRT4_BY_CLOCK[0][clk_idx] as u32) << 27);
        reg = (reg & !(0x3ff << 17)) | ((DDR3_DRT4_BY_CLOCK[1][clk_idx] as u32) << 17);
        reg = (reg & !(0x03f << 10)) | ((DDR3_DRT4_BY_CLOCK[2][clk_idx] as u32) << 10);
        reg = (reg & !0x1ff) | DDR3_DRT4_BY_CLOCK[3][clk_idx] as u32;
        mch.write32(mchbar::cx_drt4(ch), reg);

        reg = mch.read32(mchbar::cx_drt5(ch));
        if t.mem_clock == MemClock::Ddr3_1067 {
            reg = (reg & !(0x0f << 28)) | (0x08 << 28);
        }
        reg = (reg & !(0x0f << 22)) | ((4 + t.cas as u32 + 2) << 22);
        reg = (reg & !(0x1ff << 12)) | (0x190 << 12);
        reg = (reg & !(0x0f << 4)) | (((t.cas - 2) as u32) << 4);
        reg = (reg & !(0x03 << 2)) | (1 << 2);
        reg &= !0x03;
        mch.write32(mchbar::cx_drt5(ch), reg);

        reg = mch.read32(mchbar::cx_drt6(ch));
        reg = (reg & !(0xffff << 16)) | (0x066a << 16);
        reg |= 1 << 2;
        mch.write32(mchbar::cx_drt6(ch), reg);
    }
}

fn program_timings(info: &RaminitInfo, mch: &MchBar) {
    if info.ddr_type == DdrType::Ddr3 {
        program_timings_ddr3(info, mch);
        return;
    }

    let t = info.timings;

    for ch in 0..2 {
        let mut reg = mch.read32(mchbar::cx_drt0(ch));
        let btb_wtp = (t.cas - 1) as u32 + 4 + t.twr as u32;
        let btb_wtr = (t.cas - 1) as u32 + 4 + DRT0_TWTR_LUT[mem_index(t.mem_clock)] as u32;
        reg = (reg & !(0xf << 20)) | ((btb_wtr & 0xf) << 20);
        reg = (reg & !(0x1f << 26)) | ((btb_wtp & 0x1f) << 26);
        reg = (reg & !(0x07 << 15)) | (2 << 15);
        reg = (reg & !(0x0f << 10))
            | ((if t.mem_clock == MemClock::Ddr2_667 {
                2
            } else {
                3
            }) << 10);
        reg = (reg & !(0x07 << 5)) | (3 << 5);
        reg = (reg & !0x07) | 1;
        mch.write32(mchbar::cx_drt0(ch), reg);

        let mut drt1 = ((t.tras as u32) << 21)
            | (((t.trcd - 2) as u32) << 5)
            | ((t.trp - 2) as u32)
            | (((t.trrd - 2) as u32 & 0x07) << 10);
        drt1 |= 1 << 28;
        reg = (mch.read32(mchbar::cx_drt1(ch)) & 0xcc1f_e318) | drt1;
        mch.write32(mchbar::cx_drt1(ch), reg);

        let mut drt2 = mch.read32(mchbar::cx_drt2(ch));
        drt2 = (drt2 & !0x1f) | 0x13;
        drt2 = (drt2 & !(0x1f << 17)) | ((t.tfaw as u32 & 0x1f) << 17);
        drt2 = (drt2 & !(0x07 << 12)) | (0x01 << 12);
        drt2 = (drt2 & !(0x0f << 6)) | (0x01 << 6);
        mch.write32(mchbar::cx_drt2(ch), drt2);

        let wl = (t.cas - 1) as u32;
        let mut drt3 = mch.read32(mchbar::cx_drt3(ch)) & !(0x03 << 28);
        drt3 &= !(0x03 << 26);
        drt3 = (drt3 & !(0x07 << 23)) | (((t.cas - 3) as u32) << 23);
        drt3 = (drt3 & !(0xff << 13)) | ((t.trfc as u32) << 13);
        drt3 = (drt3 & !0x07) | ((wl - 2) & 0x07);
        mch.write32(mchbar::cx_drt3(ch), drt3);
        mch.write32(mchbar::cx_drt4(ch), DRT4_ROM_TABLE[mem_index(t.mem_clock)]);

        let mut drt5 = mch.read32(mchbar::cx_drt5(ch));
        drt5 = (drt5 & !(0x0f << 22)) | ((4 + t.cas as u32 + 2) << 22);
        drt5 = (drt5 & !(0x1ff << 12)) | (DRT5_ROM_BYTES[mem_index(t.mem_clock)] << 12);
        drt5 = (drt5 & !(0x0f << 4)) | (((t.cas - 2) as u32) << 4);
        drt5 = (drt5 & !(0x03 << 2)) | (1 << 2);
        drt5 &= !0x03;
        mch.write32(mchbar::cx_drt5(ch), drt5);
        mch.clrbits32(mchbar::cx_drt6(ch), 1 << 2);
    }
}

fn program_dram_control(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        let channel = mch.dram_channel(ch);
        let ranks = channel_rank_count(info, ch);
        let mut drc0 = channel.drc0.get() & !CX_DRC0_RANKEN_MASK;
        for r in 0..ranks {
            drc0 |= 1 << (24 + r);
        }
        drc0 = (drc0 & !CX_DRC0_RMS_MASK) | CX_DRC0_RMS_78_US;
        channel.drc0.set(drc0);

        let mut drc1 = channel.drc1.get() | CX_DRC1_NOTPOP_MASK;
        for r in 0..ranks {
            drc1 &= !(1 << (16 + r));
        }
        drc1 |= CX_DRC1_MUSTWR;
        channel.drc1.set(drc1);

        let mut drc2 = channel.drc2.get() | CX_DRC2_NOTPOP_MASK;
        for r in 0..ranks {
            drc2 &= !(1 << (24 + r));
        }
        drc2 |= CX_DRC2_MUSTWR;
        if info.timings.mem_clock == MemClock::Ddr3_1067 {
            drc2 |= CX_DRC2_CLK1067MT;
        }
        channel.drc2.set(drc2);
    }
}

fn dra_banks(rank: usize, banks: u8) -> u32 {
    let shift = rank * 3 + 16;
    ((banks as u32) << (shift - 3)) & (0x03 << shift)
}

fn program_dram_banks(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        let channel = mch.dram_channel(ch);
        let first = first_channel_dimm(info, ch);
        let trpall = first.is_some_and(|d| d.banks == 8);
        let mut drt1 = channel.drt[1].get() & !(1 << 15);
        if trpall {
            drt1 |= 1 << 15;
        }
        channel.drt[1].set(drt1);

        let mut dra = channel.dra.get() & !CX_DRA_BANKS_MASK;
        let ranks = channel_rank_count(info, ch);
        let banks = first.map_or(0, |d| d.banks);
        for r in 0..ranks {
            dra |= dra_banks(r, banks);
        }
        channel.dra.set(dra);
    }
}

fn dram_powerup(info: &RaminitInfo, mch: &MchBar) {
    fstart_arch_x86::udelay(200);
    let mut clkcfg = mch.read32(mchbar::CLKCFG) & !((3 << 21) | (1 << 3));
    if info.ddr_type == DdrType::Ddr2 && stepping() < 4 {
        clkcfg |= (2 << 21) | (1 << 3);
    } else {
        clkcfg |= 3 << 21;
    }
    mch.write32(mchbar::CLKCFG, clkcfg);

    if info.ddr_type == DdrType::Ddr3 {
        mch.setbits32(mchbar::DRAM_TYPE_SELECT, 1 << 10);
        fstart_arch_x86::udelay(1);
    }
    mch.setbits32(mchbar::DRAM_TYPE_SELECT, 1 << 6);
    if info.ddr_type == DdrType::Ddr3 {
        fstart_arch_x86::udelay(1);
        mch.setbits32(mchbar::DRAM_TYPE_SELECT, 1 << 9);
        mch.clrbits32(mchbar::DRAM_TYPE_SELECT, 1 << 10);
        fstart_arch_x86::udelay(500);
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
    if info.ddr_type == DdrType::Ddr3 {
        // Coreboot's DDR3 RCOMP code seeds. TODO: port
        // `raminit_rcomp_calibration()` dynamic LUT calibration; these
        // per-group ODT/code values match the upstream setup.
        for (off, code) in [
            (0x6acu32, 0x55u8),
            (0x6ec, 0x66),
            (0x72c, 0x66),
            (0x76c, 0x66),
            (0x7ac, 0x66),
            (0x7ec, 0x66),
            (0x86c, 0x55),
            (0x8ac, 0x66),
        ] {
            mch.clrbits8(off, 0x0f);
            mch.write8(off + 4, code);
        }
        mch.clrsetbits32(mchbar::RCOMP_ODT0, (7 << 3) | 7, (2 << 3) | 2);
    } else {
        for off in (0..=0x200u32).step_by(0x40) {
            mch.clrsetbits8(0x6ac + off, 0x0f, 0x0a);
            mch.write8(0x6b0 + off, 0x55);
        }
        mch.clrsetbits32(mchbar::RCOMP_ODT0, (7 << 3) | 7, (1 << 3) | 1);
    }

    let fsb_codes = [0u8, 0x00, 0x00, 0x01];
    let mem_codes = [0u8, 0x10, 0x50];
    mch.write8(
        mchbar::RCOMP_CFG2,
        fsb_codes[fsb_index(info.timings.fsb_clock)] + mem_codes[mem_index(info.timings.mem_clock)],
    );
    mch.write32(mchbar::RCOMP_CFG3, mch.read32(mchbar::RCOMP_CFG3));
}

const DDR3_RCOMP_LUT: [[[u8; 8]; 64]; 2] = [
    [
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [8, 8, 3, 3, 3, 3, 5, 7],
        [9, 9, 3, 3, 3, 3, 6, 7],
        [9, 9, 3, 3, 3, 3, 6, 7],
        [10, 10, 3, 3, 3, 3, 7, 8],
        [11, 10, 3, 3, 3, 3, 7, 8],
        [12, 11, 3, 3, 3, 3, 8, 9],
        [13, 11, 3, 3, 3, 3, 9, 9],
        [14, 12, 3, 3, 3, 3, 9, 10],
        [15, 13, 3, 3, 3, 3, 9, 10],
        [16, 14, 3, 3, 3, 3, 9, 11],
        [18, 16, 3, 3, 3, 3, 10, 12],
        [20, 18, 4, 3, 4, 4, 10, 12],
        [22, 22, 4, 4, 4, 4, 11, 12],
        [24, 24, 4, 4, 4, 4, 11, 12],
        [28, 26, 4, 4, 4, 4, 12, 12],
        [32, 28, 5, 4, 5, 5, 12, 12],
        [36, 32, 5, 5, 5, 5, 13, 13],
        [40, 36, 5, 5, 5, 5, 14, 13],
        [43, 40, 5, 5, 5, 5, 15, 14],
        [43, 43, 5, 5, 6, 5, 15, 14],
        [43, 43, 6, 5, 6, 5, 15, 15],
        [43, 43, 6, 5, 6, 6, 15, 15],
        [43, 43, 6, 6, 6, 6, 15, 15],
        [43, 43, 6, 6, 7, 6, 15, 15],
        [43, 43, 7, 6, 7, 6, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 8, 7, 8, 7, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
    ],
    [
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [8, 8, 3, 3, 3, 3, 5, 5],
        [9, 9, 3, 3, 3, 3, 6, 6],
        [9, 9, 3, 3, 3, 3, 6, 6],
        [10, 10, 3, 3, 3, 3, 7, 7],
        [10, 10, 3, 3, 3, 3, 7, 7],
        [12, 11, 3, 3, 3, 3, 8, 8],
        [13, 11, 3, 3, 3, 3, 9, 9],
        [14, 12, 3, 3, 3, 3, 9, 9],
        [15, 13, 3, 3, 3, 3, 9, 9],
        [16, 14, 3, 3, 3, 3, 9, 9],
        [18, 16, 3, 3, 3, 3, 10, 10],
        [20, 18, 4, 3, 4, 4, 10, 10],
        [22, 22, 4, 4, 4, 4, 11, 11],
        [24, 24, 4, 4, 4, 4, 11, 11],
        [28, 26, 4, 4, 4, 4, 12, 12],
        [32, 28, 5, 4, 5, 5, 12, 12],
        [36, 32, 5, 5, 5, 5, 13, 13],
        [40, 36, 5, 5, 5, 5, 14, 14],
        [43, 40, 5, 5, 5, 5, 15, 15],
        [43, 43, 5, 5, 6, 5, 15, 15],
        [43, 43, 6, 5, 6, 5, 15, 15],
        [43, 43, 6, 5, 6, 6, 15, 15],
        [43, 43, 6, 6, 6, 6, 15, 15],
        [43, 43, 6, 6, 7, 6, 15, 15],
        [43, 43, 7, 6, 7, 6, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 7, 7, 7, 7, 15, 15],
        [43, 43, 8, 7, 8, 7, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
        [43, 43, 8, 8, 8, 8, 15, 15],
    ],
];

const DDR3_RCOMP_LOOKUP_SCHEDULE: [[usize; 2]; 6] =
    [[0, 1], [2, 3], [4, 5], [4, 5], [6, 7], [6, 7]];

fn ddr3_rcomp_lookup_and_write(mch: &MchBar, a1_step: usize, row: usize, col: usize, mut off: u32) {
    for i in (row..row + 16).step_by(4) {
        let val = (DDR3_RCOMP_LUT[a1_step][i][col] as u32 & 0x3f)
            | ((DDR3_RCOMP_LUT[a1_step][i + 1][col] as u32 & 0x3f) << 8)
            | ((DDR3_RCOMP_LUT[a1_step][i + 2][col] as u32 & 0x3f) << 16)
            | ((DDR3_RCOMP_LUT[a1_step][i + 3][col] as u32 & 0x3f) << 24);
        mch.write32(off, val);
        off += 4;
    }
}

fn raminit_rcomp_calibration(mch: &MchBar) -> Result<(), ServiceError> {
    // Coreboot `raminit_rcomp_calibration()`: sample DDR3 RCOMP pull-up/down
    // lookup indices, validate them, then rewrite the dynamic RCOMP LUTs.
    let a1_step = usize::from(stepping() >= STEPPING_CONVERSION_A1);
    let mut lut_idx = [[[-1i8; 2]; 6]; 2];

    mch.setbits32(mchbar::RCOMP_CTRL, 1 << 2);
    mch.setbits32(mchbar::RCOMP_CFG3, 1 << 17);
    mch.clrbits32(mchbar::RCOMP_CFG, 1 << 23);
    mch.clrbits32(mchbar::RCOMP_CFG4, (1 << 7) | (1 << 3));
    mch.setbits32(mchbar::RCOMP_CTRL, 1);

    for _ in 0..12 {
        let mut guard = 10_000u32;
        loop {
            mch.setbits32(mchbar::RCOMP_CTRL, 1 << 3);
            fstart_arch_x86::udelay(10);
            mch.clrbits32(mchbar::RCOMP_CTRL, 1 << 3);
            if (mch.read32(0x530) & 0x7) == 0x4 {
                break;
            }
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
        let reg = mch.read32(mchbar::RCOMP_CTRL);
        let group = ((reg >> 13) & 0x7) as usize;
        let channel = ((reg >> 12) & 0x1) as usize;
        if group > 5 {
            break;
        }
        let sample = mch.read32(0x518);
        lut_idx[channel][group][0] = ((sample >> 24) & 0x7f) as i8;
        lut_idx[channel][group][1] = ((sample >> 16) & 0x7f) as i8;
    }

    mch.setbits32(mchbar::RCOMP_CTRL, 1 << 3);
    fstart_arch_x86::udelay(10);
    mch.clrbits32(mchbar::RCOMP_CTRL, 1 << 3);
    mch.clrbits32(mchbar::RCOMP_CTRL, 1 << 2);

    for (channel, channel_idx) in lut_idx.iter().enumerate() {
        for (group, group_idx) in channel_idx.iter().enumerate() {
            for (pu_pd, idx) in group_idx.iter().copied().enumerate() {
                if !(7..=55).contains(&idx) {
                    fstart_log::error!(
                        "gm45 raminit: bad DDR3 RCOMP LUT index ch{} group{} dir{} = {}",
                        channel as u32,
                        group as u32,
                        pu_pd as u32,
                        idx as i32,
                    );
                    return Err(ServiceError::HardwareError);
                }
            }
        }
    }

    let mut off = mchbar::RCOMP_TABLES;
    for (channel, channel_idx) in lut_idx.iter().enumerate() {
        for (group, group_idx) in channel_idx.iter().enumerate() {
            for pu_pd in (0..2).rev() {
                ddr3_rcomp_lookup_and_write(
                    mch,
                    a1_step,
                    (group_idx[pu_pd] as usize) - 7,
                    DDR3_RCOMP_LOOKUP_SCHEDULE[group][pu_pd],
                    off,
                );
                off += 0x18;
            }
            off += 0x10;
            // Channel B has only the first two groups.
            if channel == 1 && group == 1 {
                break;
            }
        }
        off += 0x40;
    }
    Ok(())
}

fn run_rcomp_handshake(mch: &MchBar) -> Result<(), ServiceError> {
    // Coreboot `rcomp_initialization()`: run initial RCOMP, run a second pass,
    // then restore normal periodic RCOMP state. Dynamic DDR3 LUT calibration
    // remains a separate deferred step.
    mch.setbits32(mchbar::RCOMP_CFG3, 1 << 17);
    mch.clrbits32(mchbar::RCOMP_CFG, 1 << 23);
    mch.clrbits32(mchbar::RCOMP_CFG4, (1 << 7) | (1 << 3));
    mch.setbits32(mchbar::RCOMP_CTRL, 1);
    wait_rcomp(mch)?;

    mch.setbits32(mchbar::RCOMP_CFG, 1 << 19);
    mch.setbits32(mchbar::RCOMP_CTRL, 1);
    wait_rcomp(mch)?;

    mch.clrbits32(mchbar::RCOMP_CFG, 1 << 19);
    mch.setbits32(mchbar::RCOMP_CFG, 1 << 23);
    mch.clrbits32(mchbar::RCOMP_CFG3, 1 << 17);
    mch.setbits32(mchbar::RCOMP_CFG4, (1 << 7) | (1 << 3));
    mch.setbits32(mchbar::RCOMP_CTRL, 1 << 1);
    Ok(())
}

fn odt_misc_setup(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        mch.clrbits16(mchbar::cx_dra_hi(ch), 0x00ff);
        if info.dimms[ch * 2].present && info.dimms[ch * 2].banks == 8 {
            mch.setbits16(mchbar::cx_dra_hi(ch), 0x09);
            mch.setbits32(mchbar::cx_drt1(ch), 0x8000);
        }
    }
    let t = info.timings;
    for ch in 0..2 {
        if info.ddr_type == DdrType::Ddr3 {
            let mut high = mch.read32(mchbar::cx_odt_high(ch));
            high |= 0x03 << 29;
            high = (high & !(0x03 << 20)) | (0x02 << 20);
            high = (high & !(0x07 << 16)) | (((t.cas.saturating_sub(3) as u32) & 0x07) << 16);
            high = (high & !(0x0f << 12)) | (0x07 << 12);
            if t.mem_clock != MemClock::Ddr3_1067 {
                high = (high & !(0x0f << 8)) | (((12 - t.cas) as u32 & 0x0f) << 8);
                high = (high & !(0x0f << 4)) | (((2 + t.cas) as u32 & 0x0f) << 4);
            } else {
                high = (high & !(0x0f << 8)) | (((13 - t.cas) as u32 & 0x0f) << 8);
                high = (high & !(0x0f << 4)) | (((1 + t.cas) as u32 & 0x0f) << 4);
            }
            high = (high & !0x0f) | 0x07;
            mch.write32(mchbar::cx_odt_high(ch), high);

            let odt_low_freq = match t.mem_clock {
                MemClock::Ddr3_667 => 0,
                MemClock::Ddr3_800 => 2,
                MemClock::Ddr3_1067 => 5,
                _ => 0,
            };
            let mut low = mch.read32(mchbar::cx_odt_low(ch));
            low = (low & !(0x07 << 28)) | (0x02 << 28);
            low = (low & !(0x03 << 22)) | (0x02 << 22);
            low = (low & !(0x07 << 12)) | (0x02 << 12);
            low = (low & !(0x07 << 4)) | (0x02 << 4);
            low = (low & !0x07) | odt_low_freq;
            mch.write32(mchbar::cx_odt_low(ch), low);
        } else {
            let ddr2_667 = t.mem_clock == MemClock::Ddr2_667;
            let mut high = mch.read32(mchbar::cx_odt_high(ch));
            high |= 0x03 << 29;
            high = (high & !(0x03 << 20)) | (0x01 << 20);
            high = (high & !(0x07 << 16)) | (((t.cas.saturating_sub(2) as u32) & 0x07) << 16);
            high = (high & !(0x0f << 12)) | (0x08 << 12);
            high = (high & !(0x0f << 8)) | (0x07 << 8);
            let odt_delay = if ddr2_667 { 4 } else { 5 };
            high = (high & !(0x0f << 4)) | (odt_delay << 4);
            high = (high & !0x0f) | odt_delay;
            mch.write32(mchbar::cx_odt_high(ch), high);

            let mut low = mch.read32(mchbar::cx_odt_low(ch));
            low = (low & !(0x07 << 28)) | ((if ddr2_667 { 2 } else { 3 }) << 28);
            low = (low & !(0x03 << 22)) | (0x01 << 22);
            let twl12 = if ddr2_667 {
                t.twl.saturating_sub(1)
            } else {
                t.twl.saturating_sub(2)
            };
            low = (low & !(0x07 << 12)) | (((twl12 as u32) & 0x07) << 12);
            low = (low & !(0x07 << 4)) | (((t.twl.saturating_sub(1) as u32) & 0x07) << 4);
            low &= !0x07;
            mch.write32(mchbar::cx_odt_low(ch), low);
        }
        mch.clrsetbits32(mchbar::cx_odt_misc(ch), (1 << 24) | 0x1f, t.trd as u32);
        mch.clrsetbits8(mchbar::cx_odt_timing(ch), 0x0f, t.twl);
        mch.clrsetbits8(mchbar::cx_odt_ctrl(ch), 0x0f, 0x0a);
    }
    let mut wr_ctrl = mch.read32(mchbar::WRITE_CTRL) & 0x113f_f3ff;
    wr_ctrl |= 0x8600_0400;
    mch.write32(mchbar::WRITE_CTRL, wr_ctrl);
    mch.clrsetbits32(mchbar::MMARB0, !0xfff9_ffff, 0x210000);
    mch.clrsetbits32(mchbar::MMARB1, !0xffff_fbff, 0x300);
    if stepping() >= 4 {
        mch.setbits8(0x0234, 1 << 3);
    }
}

fn channel_card_f(info: &RaminitInfo, ch: usize) -> bool {
    first_channel_dimm(info, ch).is_some_and(|d| d.raw_card_type == 0x0f)
}

fn ddr3_select_clock_mux(info: &RaminitInfo, mch: &MchBar) {
    let clk1067 = info.timings.mem_clock == MemClock::Ddr3_1067;
    let card_f = [channel_card_f(info, 0), channel_card_f(info, 1)];
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let mixed = if ch == 1 && (!channel_populated(info, 0) || card_f[0] != card_f[1]) {
            4 << 11
        } else {
            0
        };
        let base = 0x14b0 + ch as u32 * 0x100;
        let vals = [
            if clk1067 && !card_f[ch] { 3 } else { 2 },
            if !clk1067 && !card_f[ch] { 2 } else { 3 },
            2,
            if card_f[ch] { 3 } else { 2 },
            if clk1067 && !card_f[ch] { 1 } else { 0 },
            if !clk1067 && !card_f[ch] { 0 } else { 1 },
            0,
            if card_f[ch] { 1 } else { 0 },
        ];
        for (idx, val) in vals.iter().enumerate() {
            mch.clrsetbits32(base + idx as u32 * 4, 7 << 11, (*val << 11) | mixed);
        }
    }
}

fn ddr2_select_clock_mux(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let base = 0x14b0 + ch as u32 * 0x100;
        for off in (0..0x20u32).step_by(4) {
            mch.clrbits32(base + off, 7 << 11);
        }
    }
}

fn ddr3_write_io_init(info: &RaminitInfo, mch: &MchBar) {
    const DDR3_667_800: [[[[u32; 4]; 2]; 2]; 2] = [
        [
            [
                [0xa325_5008, 0x2688_8209, 0x2628_8208, 0x6188_040f],
                [0x7524_240b, 0xa525_5608, 0x232b_8508, 0x5528_040f],
            ],
            [
                [0xa625_5308, 0x2688_8209, 0x212b_7508, 0x6188_040f],
                [0x7524_240b, 0xa625_5708, 0x132b_7508, 0x5528_040f],
            ],
        ],
        [
            [
                [0xc525_7208, 0x2688_8209, 0x2628_8208, 0x6188_040f],
                [0x7524_240b, 0xc525_7608, 0x232b_8508, 0x5528_040f],
            ],
            [
                [0xb625_6308, 0x2688_8209, 0x212b_7508, 0x6188_040f],
                [0x7524_240b, 0xb625_6708, 0x132b_7508, 0x5528_040f],
            ],
        ],
    ];
    const DDR3_1067: [[[u32; 4]; 2]; 2] = [
        [
            [0xb225_4708, 0x002b_7408, 0x132b_8008, 0x7228_060f],
            [0xb025_5008, 0xa425_4108, 0x4528_b409, 0x9428_230f],
        ],
        [
            [0xa425_4208, 0x022b_6108, 0x132b_8208, 0x9228_210f],
            [0x6024_140b, 0x9224_4408, 0x252b_a409, 0x9328_360c],
        ],
    ];

    let card_f = [channel_card_f(info, 0), channel_card_f(info, 1)];
    let a1_step = usize::from(stepping() >= STEPPING_CONVERSION_A1);
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        if ch == 1 && channel_populated(info, 0) && card_f[0] == card_f[1] {
            continue;
        }
        let card = usize::from(card_f[ch]);
        let data = if info.timings.mem_clock == MemClock::Ddr3_1067 {
            DDR3_1067[ch][card]
        } else {
            let clk = match info.timings.mem_clock {
                MemClock::Ddr3_667 => 0,
                MemClock::Ddr3_800 => 1,
                _ => 0,
            };
            DDR3_667_800[a1_step][clk][card]
        };
        let io = mch.io_training_channel(ch);
        for (group, val) in data.iter().enumerate() {
            io.wrty(group).set(*val);
        }
    }
    let io0 = mch.io_training_channel_const::<0>();
    let io1 = mch.io_training_channel_const::<1>();
    io0.train_pi[0].set(0x00e7_0067);
    io0.train_pi[1].set(0x000d_8000);
    io1.train_pi[0].set(0x00e7_0067);
    io1.train_pi[1].set(0x000d_8000);
}

fn ddr2_write_io_init(mch: &MchBar) {
    let io0 = mch.io_training_channel_const::<0>();
    let io1 = mch.io_training_channel_const::<1>();
    {
        let reg = io0.wrty(0);
        reg.set((reg.get() & !0xf7bf_f71f) | 0x008b_0008);
    }
    for group in 1..4 {
        {
            let reg = io0.wrty(group);
            reg.set((reg.get() & !0xf7bf_f71f) | 0x0080_0000);
        }
    }
    io0.train_pi[0].set((io0.train_pi[0].get() & !0xf7ff_f77f) | 0x0080_0000);
    io0.train_pi[1].set((io0.train_pi[1].get() & !0xf71f_8000) | 0x0004_0000);
    {
        let reg = io1.wrty(0);
        reg.set((reg.get() & !0xf7bf_f71f) | 0x0089_0008);
    }
    for group in 1..4 {
        {
            let reg = io1.wrty(group);
            reg.set((reg.get() & !0xf7bf_f71f) | 0x0089_0000);
        }
    }
    io1.train_pi[0].set((io1.train_pi[0].get() & !0xf7ff_f77f) | 0x0080_0000);
    io1.train_pi[1].set((io1.train_pi[1].get() & !0xf71f_8000) | 0x0004_0000);
}

fn ddr_read_io_init(info: &RaminitInfo, mch: &MchBar) {
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let io = mch.io_training_channel(ch);
        for lane in 0..8 {
            let freq = match info.timings.mem_clock {
                MemClock::Ddr2_667 | MemClock::Ddr3_667 => (1 << 16) | (4 << 20),
                MemClock::Ddr2_533 | MemClock::Ddr2_800 | MemClock::Ddr3_800 => {
                    (2 << 16) | (3 << 20)
                }
                MemClock::Ddr3_1067 => (2 << 16) | (1 << 20),
            };
            io.rdty[lane].set(
                (io.rdty[lane].get()
                    & !((3 << 25) | (1 << 8) | (7 << 16) | (0x0f << 20) | (1 << 27)))
                    | (1 << 27)
                    | freq,
            );
        }
    }
}

fn exact_memory_io_tables(info: &RaminitInfo, mch: &MchBar) {
    if info.ddr_type == DdrType::Ddr3 {
        ddr3_select_clock_mux(info, mch);
        ddr3_write_io_init(info, mch);
    } else {
        ddr2_select_clock_mux(info, mch);
        ddr2_write_io_init(mch);
    }
    ddr_read_io_init(info, mch);
}

fn memory_io_init_setup(info: &RaminitInfo, mch: &MchBar) {
    // Mirror coreboot's DDR2/DDR3-specific memory-I/O init register
    // programming without the old fstart scaffolding writes.
    let io0 = mch.io_training_channel_const::<0>();
    if info.ddr_type == DdrType::Ddr3 {
        io0.io_init_cfg
            .set((io0.io_init_cfg.get() & !(3 << 13)) | (1 << 9) | (1 << 13));
        let clk_dep_freq = match info.timings.mem_clock {
            MemClock::Ddr3_667 => 9 << 28,
            MemClock::Ddr3_800 => 7 << 28,
            MemClock::Ddr3_1067 => 8 << 28,
            _ => 0,
        };
        io0.io_init_clk_dep.set(
            (io0.io_init_clk_dep.get()
                & !(0xff
                    | (1 << 11)
                    | (1 << 12)
                    | (1 << 16)
                    | (1 << 18)
                    | (1 << 27)
                    | (0x0f << 28)))
                | (1 << 7)
                | (1 << 11)
                | (1 << 16)
                | clk_dep_freq,
        );
        io0.io_init_cfg7.set(io0.io_init_cfg7.get() & !1);
        let cfg2_freq = match info.timings.mem_clock {
            MemClock::Ddr3_667 => (2 << 24) | (10 << 16),
            MemClock::Ddr3_800 => (3 << 24) | (7 << 16),
            MemClock::Ddr3_1067 => (4 << 24) | (4 << 16),
            _ => 0,
        };
        io0.io_init_cfg2.set(
            (io0.io_init_cfg2.get() & !((1 << 20) | (7 << 11) | (0x0f << 24) | (0x0f << 16)))
                | (3 << 11)
                | cfg2_freq,
        );
        io0.io_init_cfg3
            .set(io0.io_init_cfg3.get() & !((1 << 3) | (1 << 11) | (1 << 19) | (1 << 27)));
        io0.io_init_cfg4
            .set(io0.io_init_cfg4.get() & !((1 << 3) | (1 << 11) | (1 << 19) | (1 << 27)));
        io0.undoc_1428.set(io0.undoc_1428.get() | (1 << 14));
        let cfg5_freq = match info.timings.mem_clock {
            MemClock::Ddr3_667 => (2 << 8) | 0x0c,
            MemClock::Ddr3_800 => (3 << 8) | 0x0a,
            MemClock::Ddr3_1067 => (4 << 8) | 0x07,
            _ => 0,
        };
        io0.io_init_cfg5.set(
            (io0.io_init_cfg5.get() & !((0x0f << 8) | (0x07 << 20) | 0x0f | (0x0f << 24)))
                | (0x03 << 20)
                | (5 << 24)
                | cfg5_freq,
        );
    } else {
        io0.io_init_clk_dep.set(
            (io0.io_init_clk_dep.get() & !(0xff | (1 << 11) | (0x0f << 28)))
                | (1 << 0)
                | (1 << 12)
                | (1 << 16)
                | (1 << 18)
                | (1 << 27),
        );
        io0.io_init_cfg7.set(
            (io0.io_init_cfg7.get() & !(1 << 5))
                | (1 << 0)
                | (1 << 2)
                | (1 << 3)
                | (1 << 4)
                | (1 << 6),
        );
        let cfg2_freq = match info.timings.mem_clock {
            MemClock::Ddr2_533 | MemClock::Ddr2_667 => (2 << 24) | (10 << 16),
            MemClock::Ddr2_800 => (3 << 24) | (7 << 16),
            _ => 0,
        };
        let cfg5_freq = match info.timings.mem_clock {
            MemClock::Ddr2_533 | MemClock::Ddr2_667 => (2 << 8) | 0x0c,
            MemClock::Ddr2_800 => (3 << 8) | 0x0a,
            _ => 0,
        };
        io0.io_init_cfg2.set(
            (io0.io_init_cfg2.get() & !((1 << 20) | (7 << 11) | (0x0f << 24) | (0x0f << 16)))
                | (3 << 11)
                | cfg2_freq,
        );
        io0.io_init_cfg5.set(
            (io0.io_init_cfg5.get() & !((0x0f << 8) | (0x07 << 20) | 0x0f))
                | (0x03 << 20)
                | cfg5_freq,
        );
        io0.io_init_cfg3
            .set(io0.io_init_cfg3.get() & !((1 << 3) | (1 << 11) | (1 << 19) | (1 << 27)));
        io0.io_init_cfg4
            .set(io0.io_init_cfg4.get() & !((1 << 3) | (1 << 11) | (1 << 19) | (1 << 27)));
    }

    // Coreboot DDR2/DDR3 memory-I/O init programs these RCOMP_CTRL fields
    // back to the `2` encoding after the RCOMP handshake.
    mch.clrsetbits32(
        mchbar::RCOMP_CTRL,
        (3 << 30) | (3 << 16) | (3 << 4),
        (2 << 16) | (2 << 4),
    );
    mch.clrbits32(mchbar::RCOMP_STATUS, 0x0f << 20);
    mch.clrbits32(mchbar::RCOMP_CFG, 1 << 6);
    mch.clrsetbits32(
        mchbar::RCOMP_CFG2,
        if info.ddr_type == DdrType::Ddr3 {
            7 << 28
        } else {
            0x0f << 28
        },
        2 << 28,
    );
    let rcomp_cfg4_set = if info.ddr_type == DdrType::Ddr3 {
        0x11
    } else {
        (1 << 0) | (1 << 3) | (1 << 4) | (1 << 7)
    };
    mch.clrsetbits32(mchbar::RCOMP_CFG4, 0x77, rcomp_cfg4_set);

    exact_memory_io_tables(info, mch);
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
    let reg = mch.dram_channel(prev_ch).drby[prev_rank / 2].get();
    let shift = (prev_rank % 2) * 16;
    (((reg >> shift) & 0x1fc) << 25) as usize
}

fn jedec_command(mch: &MchBar, addr: usize, cmd: u32, val: u32) {
    mch.clrsetbits32(mchbar::DCC, DCC_SET_EREG_MASK, cmd);
    // SAFETY: after pre-JEDEC map programming, `addr | val` is a DRAM command
    // address used by the memory controller to latch the selected command.
    unsafe { core::ptr::read_volatile((addr | val as usize) as *const u32) };
}

fn jedec_init(info: &RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    // Coreboot `jedec_init()` pre-sequence immediately before DDR2/DDR3 JEDEC
    // commands.
    mch.setbits32(mchbar::FSBPMC3, 1 << 1);
    mch.setbits32(mchbar::SBTEST, 3 << 1);
    mch.setbits32(mchbar::POST_JEDEC_TIM0, 3 << 24);
    mch.setbits32(mchbar::POST_JEDEC_TIM1, 3 << 24);
    mch.io_training_channel_const::<0>()
        .rw_ptr_ctrl
        .set(mch.io_training_channel_const::<0>().rw_ptr_ctrl.get() | (1 << 9));
    mch.io_training_channel_const::<1>()
        .rw_ptr_ctrl
        .set(mch.io_training_channel_const::<1>().rw_ptr_ctrl.get() | (1 << 9));
    mch.clrsetbits32(mchbar::DCC, DCC_CMD_MASK, DCC_CMD_NOP);

    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    hb.write8(0xf0, hb.read8(0xf0) & !(1 << 2));
    hb.write8(0xf0, hb.read8(0xf0) | (1 << 2));
    fstart_arch_x86::udelay(2);

    match info.ddr_type {
        DdrType::Ddr2 => {
            jedec_init_ddr2(info, mch);
            Ok(())
        }
        DdrType::Ddr3 => jedec_init_ddr3(info, mch),
    }
}

fn jedec_init_ddr3(info: &RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    if !(5..=12).contains(&info.timings.twr) {
        return Err(ServiceError::HardwareError);
    }

    // Coreboot's GM45 DDR3 JEDEC sequence: EMRS2(write latency), EMRS3,
    // EMRS1(120 ohm ODT, 34 ohm drive), then MR with and without DLL reset.
    const WR_LUT: [u8; 8] = [1, 2, 3, 4, 5, 5, 6, 6];
    let wl = (((info.timings.twl - 5) as u32) & 7) << 6;
    let odt_120 = 1 << 9;
    let ods_34 = 1 << 4;
    let wr = (WR_LUT[(info.timings.twr - 5) as usize] as u32 & 7) << 12;
    let dll_reset = 1 << 11;
    let cas = (((info.timings.cas - 4) as u32) & 7) << 7;
    let interleaved_burst = 1 << 6;

    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let ranks = if channel_dual_rank(info, ch) { 2 } else { 1 };
        for r in 0..ranks {
            let addr = rank_addr(mch, ch, r);
            jedec_command(mch, addr, dcc_set_eregx(2), wl);
            jedec_command(mch, addr, dcc_set_eregx(3), 0);
            jedec_command(mch, addr, DCC_SET_EREG, odt_120 | ods_34);
            jedec_command(
                mch,
                addr,
                DCC_SET_MREG,
                wr | dll_reset | cas | interleaved_burst,
            );
            jedec_command(mch, addr, DCC_SET_MREG, wr | cas | interleaved_burst);
        }
    }
    Ok(())
}

fn ddr3_calibrate_zq(mch: &MchBar) {
    for _ in 0..2000 {
        core::hint::spin_loop();
    }
    mch.clrsetbits32(mchbar::DCC, DCC_CMD_MASK, 5 << 16);
    for ch in 0..2 {
        mch.setbits32(mchbar::cx_drt6(ch), 1 << 3);
    }
    for _ in 0..1000 {
        core::hint::spin_loop();
    }
    for ch in 0..2 {
        mch.clrbits32(mchbar::cx_drt6(ch), 1 << 3);
    }
    mch.setbits32(mchbar::DCC, DCC_CMD_MASK);
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

fn cpu_cores_per_package() -> u8 {
    let (max_leaf, _, _, _) = fstart_arch_x86::cpuid(0);
    if max_leaf < 4 {
        return 1;
    }
    let (eax, _, _, _) = fstart_arch_x86::cpuid_count(4, 0);
    (((eax >> 26) & 0x3f) as u8).saturating_add(1)
}

fn post_jedec_normal_operation(mch: &MchBar) {
    // Coreboot announces normal DRAM operation immediately after
    // `post_jedec_sequence()` and before DDR3 ZQ / receive-enable training.
    let dcc = &mch.regs().dcc;
    dcc.set(dcc.get() | DCC_CMD_NORMAL_OPERATION | DCC_INIT_COMPLETE);

    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    // SAFETY: GM45 host bridge is fixed at 00:00.0 and ECAM is live during
    // raminit because early northbridge init has already installed ECAM.
    let hb_regs = unsafe { hb.regs::<Gm45HostBridgePciConfig>() };
    let strobe = HOST_BRIDGE_UNDOC_F0_REG::NORMAL_OPERATION_STROBE::SET.value;
    hb_regs.undoc_f0.set(hb_regs.undoc_f0.get() | strobe);
    hb_regs.undoc_f0.set(hb_regs.undoc_f0.get() & !strobe);
}

fn post_jedec_sequence(mch: &MchBar) {
    mch.clrbits32(mchbar::FSBPMC3, 1 << 1);
    mch.clrbits32(mchbar::SBTEST, 3 << 1);
    mch.setbits32(mchbar::SBTEST, 1 << 15);
    mch.clrbits32(mchbar::SBTEST, 1 << 19);
    for ch in 0..2 {
        mch.write32(mchbar::cx_ait_lo(ch), 0x0000_06c4);
        mch.write32(mchbar::cx_ait_hi(ch), 0x871a_066d);
    }
    mch.setbits32(mchbar::POST_JEDEC_TIM0, 1 << 26);
    mch.clrbits32(mchbar::POST_JEDEC_TIM0, 3 << 24);
    mch.setbits32(mchbar::POST_JEDEC_TIM0, 1 << 23);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 7 << 20, 3 << 20);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 7 << 17, 6 << 17);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 7 << 14, 6 << 14);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 7 << 11, 6 << 11);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM0, 7 << 8, 6 << 8);
    mch.clrbits32(mchbar::POST_JEDEC_TIM1, 3 << 24);
    mch.clrbits32(mchbar::POST_JEDEC_TIM1, 1 << 23);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 7 << 20, 3 << 20);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 7 << 17, 6 << 17);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 7 << 14, 6 << 14);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 7 << 11, 6 << 11);
    mch.clrsetbits32(mchbar::POST_JEDEC_TIM1, 7 << 8, 6 << 8);
    if cpu_cores_per_package() == 4 {
        mch.setbits32(0x0b14, 0xbfbf << 16);
    }
}

fn final_memory_map_and_optimizations(info: &mut RaminitInfo, mch: &MchBar) {
    igd_compute_ggc();
    program_map(info, mch, false);
    if info.timings.channel_mode == ChannelMode::DualInterleaved {
        mch.clrbits32(mchbar::DCC, 0x600);
    }

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
}

#[derive(Clone, Copy)]
struct RecTiming {
    c: i32,
    pre: i32,
    ph: i32,
    t: i32,
    t_bound: i32,
    p: i32,
    p_bound: i32,
}

fn normalize_rec_timing(t: &mut RecTiming) -> Result<(), ServiceError> {
    while t.p >= t.p_bound {
        t.t += 1;
        t.p -= t.p_bound;
    }
    while t.p < 0 {
        t.t -= 1;
        t.p += t.p_bound;
    }
    while t.t >= t.t_bound {
        t.ph += 2;
        t.t -= t.t_bound;
    }
    while t.t < 0 {
        t.ph -= 2;
        t.t += t.t_bound;
    }
    while t.ph >= 4 {
        t.c += 1;
        t.ph -= 4;
    }
    while t.ph < 0 {
        t.c -= 1;
        t.ph += 4;
    }
    if !(0..16).contains(&t.c) {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn rec_quarter_step(t: &mut RecTiming) {
    t.t += t.t_bound >> 1;
    t.p += (t.t_bound & 1) * (t.p_bound >> 1);
}

fn rec_quarter_backstep(t: &mut RecTiming) {
    t.t -= t.t_bound >> 1;
    t.p -= (t.t_bound & 1) * (t.p_bound >> 1);
}

fn program_receive_timing(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timings: &mut [[RecTiming; 4]; 2],
) -> Result<(), ServiceError> {
    let timing = &mut timings[ch][group];
    normalize_rec_timing(timing)?;

    mch.clrsetbits32(
        mchbar::cx_drt3(ch),
        0x0f << 7,
        ((timing.c as u32) & 0x0f) << 7,
    );
    {
        let reg = mch.io_training_channel(ch).recy(group);
        reg.set(
            (reg.get() & !((0x0f << 28) | (0x07 << 24) | (0x03 << 22) | (0x03 << 20)))
                | (((timing.t as u32) & 0x0f) << 28)
                | (((timing.p as u32) & 0x07) << 24)
                | (((timing.ph as u32) & 0x03) << 22)
                | (((timing.pre as u32) & 0x03) << 20),
        );
    }
    Ok(())
}

fn read_dqs_level(mch: &MchBar, ch: usize, lane: usize) -> bool {
    let io = mch.io_training_channel(ch);
    io.rw_ptr_ctrl.set(io.rw_ptr_ctrl.get() & !(1 << 9));
    io.rw_ptr_ctrl.set(io.rw_ptr_ctrl.get() | (1 << 9));
    let addr = rank_addr(mch, ch, 0);
    // SAFETY: rank address points at initialized DRAM and is used only to
    // trigger controller DQS sampling during receive-enable calibration.
    unsafe { core::ptr::read_volatile(addr as *const u32) };
    (io.rdty(lane).get() & (1 << 30)) != 0
}

fn find_dqs_low(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timings: &mut [[RecTiming; 4]; 2],
    lane_map: &[[usize; 2]; 4],
) -> Result<(), ServiceError> {
    let mut timeout = 256u32;
    while read_dqs_level(mch, ch, lane_map[group][0]) || read_dqs_level(mch, ch, lane_map[group][1])
    {
        rec_quarter_step(&mut timings[ch][group]);
        program_receive_timing(mch, ch, group, timings)?;
        timeout = timeout.saturating_sub(1);
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    Ok(())
}

fn find_dqs_high(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timings: &mut [[RecTiming; 4]; 2],
    lane_map: &[[usize; 2]; 4],
) -> Result<(), ServiceError> {
    let mut timeout = 256u32;
    while !read_dqs_level(mch, ch, lane_map[group][0])
        && !read_dqs_level(mch, ch, lane_map[group][1])
    {
        rec_quarter_step(&mut timings[ch][group]);
        program_receive_timing(mch, ch, group, timings)?;
        timeout = timeout.saturating_sub(1);
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    Ok(())
}

fn find_dqs_edge_lowhigh(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timings: &mut [[RecTiming; 4]; 2],
    lane_map: &[[usize; 2]; 4],
) -> Result<(), ServiceError> {
    timings[ch][group].t += 2;
    program_receive_timing(mch, ch, group, timings)?;
    find_dqs_high(mch, ch, group, timings, lane_map)?;
    rec_quarter_backstep(&mut timings[ch][group]);
    program_receive_timing(mch, ch, group, timings)?;
    let mut timeout = 256u32;
    while !read_dqs_level(mch, ch, lane_map[group][0])
        || !read_dqs_level(mch, ch, lane_map[group][1])
    {
        timings[ch][group].p += 1;
        program_receive_timing(mch, ch, group, timings)?;
        timeout = timeout.saturating_sub(1);
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    Ok(())
}

fn find_preamble(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timings: &mut [[RecTiming; 4]; 2],
    lane_map: &[[usize; 2]; 4],
) -> Result<(), ServiceError> {
    let mut timeout = 256u32;
    while read_dqs_level(mch, ch, lane_map[group][0]) || read_dqs_level(mch, ch, lane_map[group][1])
    {
        timings[ch][group].c -= 1;
        program_receive_timing(mch, ch, group, timings)?;
        timeout = timeout.saturating_sub(1);
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    Ok(())
}

fn reset_readwrite_pointers(mch: &MchBar) {
    for ch in 0..2 {
        mch.dram_channel(ch)
            .drc1
            .set(mch.dram_channel(ch).drc1.get() | (1 << 6));
        mch.dram_channel(ch)
            .drc1
            .set(mch.dram_channel(ch).drc1.get() & !(1 << 6));
        let io = mch.io_training_channel(ch);
        io.rw_ptr_ctrl.set(io.rw_ptr_ctrl.get() & !(1 << 9));
        io.rw_ptr_ctrl.set(io.rw_ptr_ctrl.get() | (1 << 9));
        io.rw_ptr_ctrl.set(io.rw_ptr_ctrl.get() | (1 << 10));
    }
}

fn receive_enable_training(info: &mut RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    // Coreboot `raminit_receive_enable_calibration()`: set byte-lane mapping,
    // enable receive training, perform per-group DQS edge/preamble searches,
    // then normalize group pre offsets to the channel minimum coarse value.
    const BYTE_LANE_MAP: [[[usize; 2]; 4]; 2] = [
        [[0, 1], [2, 3], [4, 5], [6, 7]],
        [[0, 2], [1, 3], [4, 6], [5, 7]],
    ];
    const OVER_BYTE_LANE_MAP: [[[usize; 2]; 4]; 2] = [
        [[0, 1], [2, 3], [4, 5], [6, 7]],
        [[0, 0], [3, 3], [6, 6], [5, 5]],
    ];

    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let map = &BYTE_LANE_MAP[usize::from(channel_card_f(info, ch))];
        for (group, lanes) in map.iter().enumerate() {
            let low = lanes[0] as i32 - group as i32;
            let high = lanes[1] as i32 - group as i32 - 1;
            {
                let reg = mch.io_training_channel(ch).recy(group);
                reg.set(
                    (reg.get() & !((3 << 16) | (1 << 8) | 3))
                        | ((low as u32) & 3)
                        | (((high as u32) & 3) << 16),
                );
            }
        }
    }

    for ch in 0..2 {
        mch.dram_channel(ch)
            .train_enable
            .set(mch.dram_channel(ch).train_enable.get() | TRAIN_ENABLE_BIT);
    }
    mch.io_training_channel_const::<0>()
        .rw_ptr_ctrl
        .set((mch.io_training_channel_const::<0>().rw_ptr_ctrl.get() & !(3 << 9)) | (1 << 9));
    mch.io_training_channel_const::<1>()
        .rw_ptr_ctrl
        .set((mch.io_training_channel_const::<1>().rw_ptr_ctrl.get() & !(3 << 9)) | (1 << 9));

    let t_bound = if info.timings.mem_clock == MemClock::Ddr3_1067 {
        9
    } else if info.ddr_type == DdrType::Ddr3 {
        12
    } else {
        15
    };
    let p_bound = if info.timings.mem_clock == MemClock::Ddr3_1067 {
        8
    } else {
        1
    };
    let init = RecTiming {
        c: info.timings.cas as i32 + 1,
        pre: 0,
        ph: 0,
        t: 0,
        t_bound,
        p: 0,
        p_bound,
    };
    let mut timings = [[init; 4]; 2];

    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let map = &OVER_BYTE_LANE_MAP[usize::from(channel_card_f(info, ch))];
        for group in 0..4 {
            program_receive_timing(mch, ch, group, &mut timings)?;
            find_dqs_low(mch, ch, group, &mut timings, map)?;
            find_dqs_edge_lowhigh(mch, ch, group, &mut timings, map)?;
            rec_quarter_step(&mut timings[ch][group]);
            program_receive_timing(mch, ch, group, &mut timings)?;
            find_preamble(mch, ch, group, &mut timings, map)?;
            find_dqs_edge_lowhigh(mch, ch, group, &mut timings, map)?;
            timings[ch][group].ph -= 2;
            normalize_rec_timing(&mut timings[ch][group])?;
            if channel_card_f(info, ch) {
                timings[ch][group].t += 1;
                program_receive_timing(mch, ch, group, &mut timings)?;
            }
        }

        let c_min = timings[ch].iter().map(|t| t.c).min().unwrap_or(0);
        for group in 0..4 {
            timings[ch][group].pre = timings[ch][group].c - c_min;
            timings[ch][group].c = c_min;
            program_receive_timing(mch, ch, group, &mut timings)?;
        }
        info.rec_coarse[ch] = c_min as u8;
        info.rec_coarse_low[ch] = timings[ch][0].ph as u8;
        info.rec_fine[ch] = timings[ch][0].p as u8;
    }

    for ch in 0..2 {
        mch.dram_channel(ch)
            .train_enable
            .set(mch.dram_channel(ch).train_enable.get() & !TRAIN_ENABLE_BIT);
    }
    reset_readwrite_pointers(mch);
    Ok(())
}

fn lend_clock_values_from_receive_enable(mch: &MchBar) {
    // Coreboot copies the receive-enable coarse result into CxDRT5[7:4]
    // after calibration.
    for ch in 0..2 {
        let channel = mch.dram_channel(ch);
        let coarse = ((channel.drt[3].get() >> 7).saturating_sub(1)) & 0x0f;
        channel.drt[5].set((channel.drt[5].get() & !0x00f0) | (coarse << 4));
    }
}

fn wait_rcomp(mch: &MchBar) -> Result<(), ServiceError> {
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

fn cmos_write(index: u8, value: u8) {
    // SAFETY: ports 0x70/0x71 are the standard PC CMOS index/data ports.
    unsafe {
        fstart_pio::outb(0x70, index);
        fstart_pio::outb(0x71, value);
    }
}

#[derive(Clone, Copy)]
struct AddressBunch {
    addr: [usize; 8],
    count: usize,
}

impl AddressBunch {
    const fn new() -> Self {
        Self {
            addr: [0; 8],
            count: 0,
        }
    }

    fn push(&mut self, addr: usize) {
        if self.count < self.addr.len() {
            self.addr[self.count] = addr;
            self.count += 1;
        }
    }
}

fn collect_channel_rank_addresses(info: &RaminitInfo, mch: &MchBar, ch: usize) -> AddressBunch {
    let mut addresses = AddressBunch::new();
    if channel_populated(info, ch) {
        for rank in 0..channel_rank_count(info, ch).max(1) {
            addresses.push(rank_addr(mch, ch, rank.min(3)));
        }
    }
    addresses
}

fn collect_write_training_addresses(info: &RaminitInfo, mch: &MchBar) -> [AddressBunch; 2] {
    let card_f = [channel_card_f(info, 0), channel_card_f(info, 1)];
    let mut addresses = [AddressBunch::new(), AddressBunch::new()];
    if channel_populated(info, 0) && card_f[0] == card_f[1] {
        for ch in 0..2 {
            if channel_populated(info, ch) {
                for rank in 0..channel_rank_count(info, ch).max(1) {
                    addresses[0].push(rank_addr(mch, ch, rank.min(3)));
                }
            }
        }
    } else {
        for (ch, bunch) in addresses.iter_mut().enumerate() {
            if channel_populated(info, ch) {
                for rank in 0..channel_rank_count(info, ch).max(1) {
                    bunch.push(rank_addr(mch, ch, rank.min(3)));
                }
            }
        }
    }
    addresses
}

const READ_TRAINING_SCHEDULE: [u32; 40] = [
    0xfefe_fefe,
    0x7f7f_7f7f,
    0xbebe_bebe,
    0xdfdf_dfdf,
    0xeeee_eeee,
    0xf7f7_f7f7,
    0xfafa_fafa,
    0xfdfd_fdfd,
    0x0000_0000,
    0x8181_8181,
    0x4040_4040,
    0x2121_2121,
    0x1010_1010,
    0x0909_0909,
    0x0404_0404,
    0x0303_0303,
    0x1010_1010,
    0x1111_1111,
    0xeeee_eeee,
    0xefef_efef,
    0x1010_1010,
    0x1111_1111,
    0xeeee_eeee,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0x0000_0000,
];

#[derive(Clone, Copy)]
struct ReadTiming {
    t: i32,
    p: i32,
}

fn normalize_read_timing(timing: &mut ReadTiming) -> Result<(), ServiceError> {
    while timing.p >= 8 {
        timing.t += 1;
        timing.p -= 8;
    }
    while timing.p < 0 {
        timing.t -= 1;
        timing.p += 8;
    }
    if timing.t < 0 {
        timing.t = 0;
        timing.p = 0;
        return Err(ServiceError::HardwareError);
    }
    if timing.t >= 14 {
        timing.t = 13;
        timing.p = 7;
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn program_read_timing(
    mch: &MchBar,
    ch: usize,
    lane: usize,
    timing: &mut ReadTiming,
) -> Result<(), ServiceError> {
    normalize_read_timing(timing)?;
    mch.clrsetbits32(
        mchbar::cx_rdty(ch, lane),
        (0x0f << 20) | (0x07 << 16),
        ((timing.t as u32) << 20) | ((timing.p as u32) << 16),
    );
    Ok(())
}

fn read_training_test(lane: usize, addresses: &AddressBunch) -> bool {
    let lane_offset = lane & 4;
    let lane_mask = 0xffu32 << ((lane & !4) * 8);
    for addr in addresses.addr.iter().take(addresses.count) {
        for offset in (lane_offset..320usize).step_by(8) {
            // SAFETY: training addresses are initialized DRAM ranks.
            let read = unsafe { core::ptr::read_volatile((*addr + offset) as *const u32) };
            let good = READ_TRAINING_SCHEDULE[offset >> 3];
            if (read & lane_mask) != (good & lane_mask) {
                return false;
            }
        }
    }
    true
}

fn read_training_per_lane(
    mch: &MchBar,
    ch: usize,
    lane: usize,
    addresses: &AddressBunch,
) -> Result<(), ServiceError> {
    mch.setbits32(mchbar::cx_rdty(ch, lane), 3 << 25);
    let mut lower = ReadTiming { t: 0, p: 0 };
    program_read_timing(mch, ch, lane, &mut lower)?;
    let mut guard = 256u32;
    while !read_training_test(lane, addresses) {
        lower.t += 1;
        program_read_timing(mch, ch, lane, &mut lower)?;
        guard = guard.saturating_sub(1);
        if guard == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    if lower.t > 0 {
        lower.t -= 1;
        program_read_timing(mch, ch, lane, &mut lower)?;
        guard = 256;
        while !read_training_test(lane, addresses) {
            lower.p += 1;
            program_read_timing(mch, ch, lane, &mut lower)?;
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
    }

    let mut upper = ReadTiming {
        t: lower.t + 1,
        p: lower.p,
    };
    if program_read_timing(mch, ch, lane, &mut upper).is_ok() && read_training_test(lane, addresses)
    {
        guard = 256;
        while read_training_test(lane, addresses) {
            upper.t += 1;
            if program_read_timing(mch, ch, lane, &mut upper).is_err() {
                break;
            }
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
        upper.t -= 1;
        program_read_timing(mch, ch, lane, &mut upper)?;
        guard = 256;
        while read_training_test(lane, addresses) {
            upper.p += 1;
            if program_read_timing(mch, ch, lane, &mut upper).is_err() {
                break;
            }
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
    }

    let lower_p = lower.p + (lower.t << 3);
    let upper_p = upper.p + (upper.t << 3);
    let mean = (lower_p + upper_p) >> 1;
    let mut final_timing = ReadTiming {
        t: mean >> 3,
        p: mean & 7,
    };
    program_read_timing(mch, ch, lane, &mut final_timing)
}

fn perform_read_training(info: &RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    for ch in 0..2 {
        if !channel_populated(info, ch) {
            continue;
        }
        let addresses = collect_channel_rank_addresses(info, mch, ch);
        for addr in addresses.addr.iter().take(addresses.count) {
            for offset in (0..320usize).step_by(4) {
                // SAFETY: training addresses are initialized DRAM ranks.
                unsafe {
                    core::ptr::write_volatile(
                        (*addr + offset) as *mut u32,
                        READ_TRAINING_SCHEDULE[offset >> 3],
                    )
                };
            }
        }
        for lane in 0..8 {
            read_training_per_lane(mch, ch, lane, &addresses)?;
        }
    }
    let mut idx = 0u8;
    for ch in 0..2 {
        for lane in 0..8 {
            let reg = mch.read32(mchbar::cx_rdty(ch, lane));
            let byte = ((((reg >> 20) & 0x0f) << 4) | ((reg >> 16) & 0x07)) as u8;
            cmos_write(CMOS_READ_TRAINING + idx, byte);
            idx += 1;
        }
    }
    reset_readwrite_pointers(mch);
    Ok(())
}

const WRITE_TRAINING_SCHEDULE: [u32; 80] = [
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xffff_ffff,
    0x0000_0000,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0x1010_1010,
    0xefef_efef,
    0xeeee_eeee,
    0x1111_1111,
    0x1010_1010,
    0xefef_efef,
    0xeeee_eeee,
    0x1111_1111,
    0x1010_1010,
    0xefef_efef,
    0xeeee_eeee,
    0x1111_1111,
    0x1010_1010,
    0xefef_efef,
    0xeeee_eeee,
    0x1111_1111,
    0x1010_1010,
    0x0303_0303,
    0x0404_0404,
    0x0909_0909,
    0x1010_1010,
    0x2121_2121,
    0x4040_4040,
    0x8181_8181,
    0x0000_0000,
    0x0303_0303,
    0x0404_0404,
    0x0909_0909,
    0x1010_1010,
    0x2121_2121,
    0x4040_4040,
    0x8181_8181,
    0x0000_0000,
    0xfdfd_fdfd,
    0xfafa_fafa,
    0xf7f7_f7f7,
    0xeeee_eeee,
    0xdfdf_dfdf,
    0xbebe_bebe,
    0x7f7f_7f7f,
    0xfefe_fefe,
    0xfdfd_fdfd,
    0xfafa_fafa,
    0xf7f7_f7f7,
    0xeeee_eeee,
    0xdfdf_dfdf,
    0xbebe_bebe,
    0x7f7f_7f7f,
    0xfefe_fefe,
];

#[derive(Clone, Copy)]
struct WriteTiming {
    f: i32,
    t: i32,
    t_bound: i32,
    p: i32,
}

fn normalize_write_timing(timing: &mut WriteTiming) -> Result<(), ServiceError> {
    while timing.p >= 8 {
        timing.t += 1;
        timing.p -= 8;
    }
    while timing.p < 0 {
        timing.t -= 1;
        timing.p += 8;
    }
    while timing.t >= timing.t_bound {
        timing.f += 1;
        timing.t -= timing.t_bound;
    }
    while timing.t < 0 {
        timing.f -= 1;
        timing.t += timing.t_bound;
    }
    if timing.f < 0 {
        timing.f = 0;
        timing.t = 0;
        timing.p = 0;
        return Err(ServiceError::HardwareError);
    }
    if timing.f >= 4 {
        timing.f = 3;
        timing.t = timing.t_bound - 1;
        timing.p = 7;
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn program_write_timing(
    mch: &MchBar,
    ch: usize,
    group: usize,
    timing: &mut WriteTiming,
    memclk1067: bool,
) -> Result<(), ServiceError> {
    normalize_write_timing(timing)?;
    let d_bounds = if memclk1067 { [2, 9] } else { [1, 6] };
    let p = if memclk1067 && ((timing.t == 9 && timing.p >= 4) || (timing.t == 10 && timing.p < 4))
    {
        4
    } else {
        timing.p
    };
    let d = if timing.t <= d_bounds[0] {
        3 << 16
    } else if timing.t > d_bounds[1] {
        1 << 16
    } else {
        0
    };
    mch.clrsetbits32(
        mchbar::cx_wrty(ch, group),
        (0x0f << 28) | (0x07 << 24) | (0x03 << 18) | (0x03 << 16),
        ((timing.t as u32) << 28) | ((p as u32) << 24) | ((timing.f as u32) << 18) | d,
    );
    Ok(())
}

fn write_training_test(mch: &MchBar, addresses: &AddressBunch, masks: &[u32; 2]) -> bool {
    let mmarb0 = mch.read32(mchbar::MMARB0);
    let wrcctl = mch.read8(mchbar::WRITE_CTRL);
    mch.setbits32(mchbar::MMARB0, 0x0f << 28);
    mch.setbits8(mchbar::WRITE_CTRL, 1 << 4);
    let mut ret = true;
    'outer: for addr in addresses.addr.iter().take(addresses.count) {
        for off in (0..640usize).step_by(8) {
            let pattern = WRITE_TRAINING_SCHEDULE[off >> 3];
            // SAFETY: training addresses are initialized DRAM ranks.
            unsafe {
                core::ptr::write_volatile((*addr + off) as *mut u32, pattern);
                core::ptr::write_volatile((*addr + off + 4) as *mut u32, pattern);
            }
        }
        mch.setbits8(0x78, 1);
        for off in (0..640usize).step_by(8) {
            let good = WRITE_TRAINING_SCHEDULE[off >> 3];
            // SAFETY: training addresses are initialized DRAM ranks.
            let read1 = unsafe { core::ptr::read_volatile((*addr + off) as *const u32) };
            if (read1 & masks[0]) != (good & masks[0]) {
                ret = false;
                break 'outer;
            }
            let read2 = unsafe { core::ptr::read_volatile((*addr + off + 4) as *const u32) };
            if (read2 & masks[1]) != (good & masks[1]) {
                ret = false;
                break 'outer;
            }
        }
    }
    mch.write32(mchbar::MMARB0, mmarb0);
    mch.write8(mchbar::WRITE_CTRL, wrcctl);
    ret
}

fn write_training_per_group(
    mch: &MchBar,
    ch: usize,
    group: usize,
    addresses: &AddressBunch,
    masks: &[[u32; 2]; 4],
    memclk1067: bool,
) -> Result<(), ServiceError> {
    let t_bound = if memclk1067 { 12 } else { 11 };
    let reg = mch.read32(mchbar::cx_wrty(ch, group));
    let mut lower = WriteTiming {
        // Coreboot seeds the lower-bound search from the pre-training
        // CxWRTy fields at bits 12/8/2, then starts one F step below.
        f: (((reg >> 2) & 3) as i32) - 1,
        t: ((reg >> 12) & 0x0f) as i32,
        t_bound,
        p: ((reg >> 8) & 7) as i32,
    };
    program_write_timing(mch, ch, group, &mut lower, memclk1067)?;
    let mut guard = 256u32;
    while !write_training_test(mch, addresses, &masks[group]) {
        lower.t += 1;
        program_write_timing(mch, ch, group, &mut lower, memclk1067)?;
        guard = guard.saturating_sub(1);
        if guard == 0 {
            return Err(ServiceError::Timeout);
        }
    }
    if lower.f > 0 || lower.t > 0 {
        lower.t -= 1;
        program_write_timing(mch, ch, group, &mut lower, memclk1067)?;
        guard = 256;
        while !write_training_test(mch, addresses, &masks[group]) {
            lower.p += 1;
            program_write_timing(mch, ch, group, &mut lower, memclk1067)?;
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
    }
    let mut upper = WriteTiming {
        f: lower.f,
        t: lower.t + 3,
        t_bound,
        p: lower.p,
    };
    if program_write_timing(mch, ch, group, &mut upper, memclk1067).is_ok()
        && write_training_test(mch, addresses, &masks[group])
    {
        guard = 256;
        while write_training_test(mch, addresses, &masks[group]) {
            upper.t += 1;
            if program_write_timing(mch, ch, group, &mut upper, memclk1067).is_err() {
                break;
            }
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
        upper.t -= 1;
        program_write_timing(mch, ch, group, &mut upper, memclk1067)?;
        guard = 256;
        while write_training_test(mch, addresses, &masks[group]) {
            upper.p += 1;
            if program_write_timing(mch, ch, group, &mut upper, memclk1067).is_err() {
                break;
            }
            guard = guard.saturating_sub(1);
            if guard == 0 {
                return Err(ServiceError::Timeout);
            }
        }
    }
    let lower_p = lower.p + ((lower.t + lower.f * lower.t_bound) << 3);
    let upper_p = upper.p + ((upper.t + upper.f * upper.t_bound) << 3);
    let mean = (lower_p + upper_p) >> 1;
    let mut final_timing = WriteTiming {
        f: mean / (t_bound << 3),
        t: (mean >> 3) % t_bound,
        t_bound,
        p: mean & 7,
    };
    program_write_timing(mch, ch, group, &mut final_timing, memclk1067)
}

fn perform_write_training(info: &RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    const MASKS_ABC: [[[u32; 2]; 4]; 2] = [
        [[0xffff_ffff, 0], [0, 0], [0, 0xffff_ffff], [0, 0]],
        [
            [0x0000_ffff, 0],
            [0xffff_0000, 0],
            [0, 0x0000_ffff],
            [0, 0xffff_0000],
        ],
    ];
    const MASKS_F: [[u32; 2]; 4] = [
        [0xff00_ff00, 0],
        [0x00ff_00ff, 0],
        [0, 0xff00_ff00],
        [0, 0x00ff_00ff],
    ];
    let memclk1067 = info.timings.mem_clock == MemClock::Ddr3_1067;
    let card_f = [channel_card_f(info, 0), channel_card_f(info, 1)];
    let addresses = collect_write_training_addresses(info, mch);
    for ch in 0..2 {
        if addresses[ch].count == 0 {
            continue;
        }
        let masks = if card_f[ch] {
            &MASKS_F
        } else {
            &MASKS_ABC[usize::from(memclk1067)]
        };
        for group in 0..4 {
            if masks[group][0] == 0 && masks[group][1] == 0 {
                continue;
            }
            write_training_per_group(mch, ch, group, &addresses[ch], masks, memclk1067)?;
        }
    }
    let mut idx = 0u8;
    for ch in 0..2 {
        for group in 0..4 {
            let reg = mch.read32(mchbar::cx_wrty(ch, group));
            cmos_write(
                CMOS_WRITE_TRAINING + idx,
                ((((reg >> 28) & 0x0f) << 4) | ((reg >> 24) & 0x07)) as u8,
            );
            cmos_write(CMOS_WRITE_TRAINING + idx + 1, ((reg >> 18) & 0x03) as u8);
            idx += 2;
        }
    }
    reset_readwrite_pointers(mch);
    Ok(())
}

fn ddr3_1067_read_write_training(info: &RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    if info.ddr_type == DdrType::Ddr3 && info.timings.mem_clock == MemClock::Ddr3_1067 {
        // Coreboot runs read training before write training.
        perform_read_training(info, mch)?;
        perform_write_training(info, mch)?;
    }
    Ok(())
}

fn dram_power_mgmt(mch: &MchBar) {
    // Vendor/coreboot-derived late DRAM power-management register sequence.
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

/// Probe and decode the SPD EEPROMs wired to GM45.
///
/// `spd_addresses` follows coreboot's GM45 slot mapping: index 0/1 are
/// channel 0, index 2/3 are channel 1. A zero address means the slot is not
/// wired. Lenovo X200 uses `[0x50, 0, 0x51, 0]`.
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

        let mut spd = [0u8; 256];
        if fstart_spd::read_spd(bus, addr, &mut spd).is_err() || spd.iter().all(|b| *b == 0) {
            fstart_log::info!("gm45 raminit: no DIMM SPD at {:#x}", addr);
            continue;
        }

        let spd_type = match spd[SPD_MEMORY_TYPE as usize] {
            fstart_spd::ddr2::DDR2 => DdrType::Ddr2,
            DDR3 => DdrType::Ddr3,
            other => {
                fstart_log::error!(
                    "gm45 raminit: unsupported SPD type {:#x} at {:#x}",
                    other,
                    addr
                );
                return Err(ServiceError::HardwareError);
            }
        };
        if info.dimm_count != 0 && info.ddr_type != spd_type {
            fstart_log::error!("gm45 raminit: mixed DDR2/DDR3 DIMMs are unsupported");
            return Err(ServiceError::HardwareError);
        }
        info.ddr_type = spd_type;

        let slot_info = match spd_type {
            DdrType::Ddr2 => {
                let Some(dimm) = fstart_spd::ddr2::decode_dimm(&spd) else {
                    fstart_log::error!("gm45 raminit: invalid DDR2 SPD at {:#x}", addr);
                    return Err(ServiceError::HardwareError);
                };
                DimmSlot::from_spd(slot, addr, &dimm)
            }
            DdrType::Ddr3 => {
                let dimm = fstart_spd::ddr3::decode_dimm_gm45(&spd).map_err(|_| {
                    fstart_log::error!("gm45 raminit: invalid/unsupported DDR3 SPD at {:#x}", addr);
                    ServiceError::HardwareError
                })?;
                DimmSlot::from_ddr3_spd(slot, addr, &dimm)
            }
        };

        let channel = if slot < 2 { 0 } else { 1 };
        info.dimms[slot] = slot_info;
        info.dimm_count += 1;
        info.total_mb = info.total_mb.saturating_add(slot_info.capacity_mb);
        channel_populated[channel] = true;

        fstart_log::info!(
            "gm45 raminit: slot {} ch{} addr {:#x}: {} {} MiB, {} ranks, {} banks, x{} raw_card={:#x}",
            slot as u32,
            channel as u32,
            addr,
            slot_info.ddr_type as u32 + 2,
            slot_info.capacity_mb,
            slot_info.ranks as u32,
            slot_info.banks as u32,
            if slot_info.x16 { 16 } else { 8 },
            slot_info.raw_card_type as u32,
        );
    }

    info.channels = channel_populated.iter().filter(|p| **p).count() as u8;
    if info.dimm_count == 0 {
        fstart_log::error!("gm45 raminit: no DIMMs detected");
        return Err(ServiceError::HardwareError);
    }

    Ok(info)
}

/// Run GM45 cold-boot DDR2/DDR3 initialization.
///
/// The sequence follows coreboot's GM45 `raminit.c`: SPD/timing selection,
/// DRAM type/frequency setup, DRAM control mode, RCOMP, power-up,
/// timing/bank/ODT/misc/clock-crossing/io setup, temporary pre-JEDEC address
/// map, JEDEC commands, post-JEDEC normal operation, DDR3 ZQ calibration,
/// receive-enable calibration, final memory map, guarded EPD channel
/// population, and DRAM power-management setup.
pub fn cold_boot_train(info: &mut RaminitInfo, mch: &MchBar) -> Result<(), ServiceError> {
    reset_on_stale_rcomp(mch);
    init_pmcon(mch);

    let hb = fstart_ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
    let fsb_clock = read_fsb_clock(mch)?;
    let capid0 = hb.read32(hostbridge::CAPID0);

    select_frequency_and_cas(info, fsb_clock, capid0)?;
    calculate_timings(info)?;

    check_bad_warmboot(mch);
    mch.setbits32(mchbar::PMSTS, PMSTS_SELFREFRESH);
    program_dram_type(info, mch);
    program_clkcfg_lock(info, mch);
    program_gcfgc(info, mch);

    mch.setbits32(mchbar::FSBPMC3, 1 << 1);
    mch.setbits32(mchbar::SBTEST, 6);
    mch.setbits32(mchbar::POST_JEDEC_TIM0, 0x0300_0000);
    mch.setbits32(mchbar::POST_JEDEC_TIM1, 0x0300_0000);

    program_dram_control(info, mch);
    rcomp_init(info, mch);
    raminit_rcomp_calibration(mch)?;
    run_rcomp_handshake(mch)?;
    dram_powerup(info, mch);
    program_timings(info, mch);
    program_dram_banks(info, mch);
    for ch in 0..2 {
        if channel_populated(info, ch) {
            mch.setbits32(mchbar::cx_dclkdis(ch), 0x3);
        }
    }
    odt_misc_setup(info, mch);
    set_clkcross_frequencies(info, mch);
    vc1_program_timings(info.timings.fsb_clock);
    memory_io_init_setup(info, mch);
    program_map(info, mch, true);

    jedec_init(info, mch)?;
    post_jedec_sequence(mch);
    post_jedec_normal_operation(mch);

    if info.ddr_type == DdrType::Ddr3 {
        ddr3_calibrate_zq(mch);
    }
    receive_enable_training(info, mch)?;
    lend_clock_values_from_receive_enable(mch);
    ddr3_1067_read_write_training(info, mch)?;
    final_memory_map_and_optimizations(info, mch);

    if stepping() != 0 {
        mch.setbits8(mchbar::RCOMP_CTRL, 2);
    }
    mch.clrbits32(mchbar::IO_RCOMP_CLK_EN, 1 << 12);

    // Vendor/GM965-derived RAMINIT only programs the EPD/channel-population
    // path when EPD_2E[4:0] shows a prior successful initialization. There is
    // no matching GM45 coreboot path; on a first cold boot these registers
    // contain hardware defaults, and programming this partial EPD path too
    // early can break otherwise-working DRB/DRA decode.
    if (mch.read8(0x0a2e) & 0x1f) != 0 {
        program_channel_population(info, mch);
    }

    clear_dram_init_in_progress();
    dram_power_mgmt(mch);
    mch.write32(mchbar::SSKPD, 0xcafe);

    fstart_log::info!(
        "gm45 raminit: complete total={} MiB tolud={} MiB rec=({},{}),({},{})",
        info.tom_mb,
        info.tolud_mb,
        info.rec_coarse[0] as u32,
        info.rec_coarse_low[0] as u32,
        info.rec_coarse[1] as u32,
        info.rec_coarse_low[1] as u32,
    );
    Ok(())
}
