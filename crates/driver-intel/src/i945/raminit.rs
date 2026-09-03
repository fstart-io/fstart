//! Intel i945 DDR2 SDRAM initialization (raminit).
//!
//! Ported from coreboot `northbridge/intel/i945/raminit.c` (including the
//! receive-enable training from `rcven.c`). The port follows the C file's
//! POST order function by function; register offsets come from `i945.h`
//! (see [`super::mchbar`]).
//!
//! Desktop (G/P/GC) and mobile (GM) paths are selected at runtime from
//! [`super::I945Variant`]: GC tables/values for
//! [`super::I945Variant::Desktop`] and [`super::I945Variant::DesktopGc`],
//! GM tables/values for [`super::I945Variant::Mobile`].
//!
//! Deviations from coreboot:
//!
//! - SPD decoding reuses [`crate::generic::spd::ddr2`]; ECC/registered /
//!   stacked / burst-length checks read the raw SPD bytes directly.
//! - `die()` becomes `Err(ServiceError::...)`; the fixed platform flow
//!   halts the boot on error. `full_reset()` cases call
//!   [`super::cf9_reset`].
//! - S3 resume and warm reset reboot instead of resuming: fstart has no
//!   MRC cache, so the CMOS receive-enable save/restore pair is omitted
//!   (same policy as the GM965 port).

use super::fields::*;
use super::{MchBar, hostbridge, mchbar, IntelI945, I945Variant};
use crate::MmioBar;
use crate::generic::spd::ddr2;
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam::EcamDevice;

// ---------------------------------------------------------------------------
// MCHBAR register block (offsets from coreboot `i945.h`)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod r {
    pub const C0DRB0: u32 = 0x100;
    pub const C0DRA0: u32 = 0x108;
    pub const C0DCLKDIS: u32 = 0x10c;
    pub const C0BNKARC: u32 = 0x10e;
    pub const C0DRT0: u32 = 0x110;
    pub const C0DRT1: u32 = 0x114;
    pub const C0DRT2: u32 = 0x118;
    pub const C0DRT3: u32 = 0x11c;
    pub const C0DRC0: u32 = 0x120;
    pub const C0DRC1: u32 = 0x124;
    pub const C0DRC2: u32 = 0x128;
    pub const C0AIT_LO: u32 = 0x130;
    pub const C0DCCFT_LO: u32 = 0x138;
    pub const C0GTEW: u32 = 0x140;
    pub const C0GTC: u32 = 0x144;
    pub const C0DTPEW_LO: u32 = 0x148;
    pub const C0DTAEW_LO: u32 = 0x150;
    pub const C0DTC: u32 = 0x158;
    pub const C0DMC: u32 = 0x164;
    pub const C0ODT_LO: u32 = 0x168;
    pub const C1_BASE: u32 = 0x80;
    pub const DCC: u32 = 0x200;
    pub const CCCFT_LO: u32 = 0x208;
    pub const WCC: u32 = 0x218;
    pub const MMARB0: u32 = 0x220;
    pub const MMARB1: u32 = 0x224;
    pub const SBTEST: u32 = 0x230;
    pub const SBOCC: u32 = 0x238;
    pub const ODTC: u32 = 0x284;
    pub const SMVREFC: u32 = 0x2a0;
    pub const DRTST: u32 = 0x2a8;
    pub const REPC: u32 = 0x2e0;
    pub const DQSMT: u32 = 0x2f4;
    pub const RCVENMT: u32 = 0x2f8;
    pub const C0R0B00DQST: u32 = 0x300;
    pub const C0WL0REOST: u32 = 0x340;
    pub const WDLLBYPMODE: u32 = 0x360;
    pub const C0WDLLCMC: u32 = 0x36c;
    pub const C0HCTC: u32 = 0x37c;
    pub const GBRCOMPCTL: u32 = 0x400;
    pub const SMSRCTL: u32 = 0x408;
    pub const C0DRAMW: u32 = 0x40c;
    pub const G1SC: u32 = 0x410;
    pub const G1SRPUT: u32 = 0x500;
    pub const G2SRPUT: u32 = 0x540;
    pub const G3SRPUT: u32 = 0x580;
    pub const G4SRPUT: u32 = 0x5c0;
    pub const G5SRPUT: u32 = 0x600;
    pub const G6SRPUT: u32 = 0x640;
    pub const G7SRPUT: u32 = 0x680;
    pub const G8SRPUT: u32 = 0x6c0;
    pub const UPMC2: u32 = 0x0c20;
    pub const UPMC3: u32 = 0x0fc0;
    pub const UPMC4: u32 = 0x0c30;
    pub const CPCTL: u32 = 0x0c16;
    pub const PLLMON: u32 = 0x0c34;
    pub const HGIPMC2: u32 = 0x0c38;
    pub const FSBPMC4: u32 = 0x0044;
    pub const SLPCTL: u32 = 0x0090;
    pub const MISC_B00: u32 = 0x0b00;
    pub const MISC_B18: u32 = 0x0b18;
    pub const MIPMC3: u32 = 0x0bd8;
    pub const C2C3TT: u32 = 0x0f00;
    pub const C3C4TT: u32 = 0x0f04;
    pub const MIPMC4: u32 = 0x0f08;
    pub const MIPMC5: u32 = 0x0f0a;
    pub const MIPMC6: u32 = 0x0f0c;
    pub const PMCFG: u32 = 0x0f10;
    pub const GIPMC1: u32 = 0x0fb0;
    pub const FSBPMC1: u32 = 0x0fb8;
    pub const ECO: u32 = 0x0ffc;
}

// ---------------------------------------------------------------------------
// DRAM command encoding (`do_ram_command`)
// ---------------------------------------------------------------------------

const RAM_COMMAND_NOP: u32 = 0x1 << 16;
const RAM_COMMAND_PRECHARGE: u32 = 0x2 << 16;
const RAM_COMMAND_MRS: u32 = 0x3 << 16;
const RAM_COMMAND_EMRS: u32 = 0x4 << 16;
const RAM_COMMAND_CBR: u32 = 0x6 << 16;
const RAM_COMMAND_NORMAL: u32 = 0x7 << 16;

const RAM_EMRS_1: u32 = 0x0 << 21;
const RAM_EMRS_2: u32 = 0x1 << 21;
const RAM_EMRS_3: u32 = 0x2 << 21;

/// Raminit failure reasons.
///
/// Converted to [`ServiceError::HardwareError`] at the `?` boundary after
/// logging the variant, so a failed boot names the check instead of dying
/// silent. ufmt has no `{:?}` for core `Debug`, hence the `as_str` match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaminitError {
    EccUnsupported,
    RegisteredUnsupported,
    UnsupportedWidth,
    NoBurstLength8,
    RankTooSmall,
    NoMemory,
    NoCommonCas,
    NoCommonFrequency,
    BadTras,
    BadTrp,
    BadTrcd,
    BadTwr,
    BadRefresh,
    BadRowsCols,
    BadDrt3Frequency,
    UnsupportedFsb,
    BadMemClkFrequency,
    BadOdtCas,
    BadJedecCas,
    BadJedecTwr,
}

impl RaminitError {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EccUnsupported => "ECC memory not supported by this chipset",
            Self::RegisteredUnsupported => "registered memory not supported by this chipset",
            Self::UnsupportedWidth => "unsupported DDR2 memory width",
            Self::NoBurstLength8 => "only DDR2 with burst length 8 is supported",
            Self::RankTooSmall => "DDR2 rank smaller than 128MB is not supported",
            Self::NoMemory => "no memory installed",
            Self::NoCommonCas => "no common CAS latency",
            Self::NoCommonFrequency => "no common memory frequency and CAS",
            Self::BadTras => "tRAS error",
            Self::BadTrp => "tRP error",
            Self::BadTrcd => "tRCD error",
            Self::BadTwr => "tWR error",
            Self::BadRefresh => "unsupported refresh value",
            Self::BadRowsCols => "unsupported rows/columns (DRA)",
            Self::BadDrt3Frequency => "bad memory frequency",
            Self::UnsupportedFsb => "unsupported FSB speed",
            Self::BadMemClkFrequency => "target memory frequency error",
            Self::BadOdtCas => "bad CAS for ODT",
            Self::BadJedecCas => "JEDEC CAS error",
            Self::BadJedecTwr => "JEDEC tWR error",
        }
    }
}

impl From<RaminitError> for ServiceError {
    fn from(e: RaminitError) -> Self {
        fstart_log::error!("i945: {}", e.as_str());
        ServiceError::HardwareError
    }
}

// tCK values in 1/256 ns (coreboot `device/dram/common.h`).
const TCK_266MHZ: u32 = 960;
const TCK_200MHZ: u32 = 1280;

// DIMM population kinds (`SYSINFO_DIMM_*`).
const DIMM_X16DS: u8 = 0x00;
const DIMM_X8DS: u8 = 0x01;
const DIMM_X16SS: u8 = 0x02;
const DIMM_X8DDS: u8 = 0x03;
const DIMM_NOT_POPULATED: u8 = 0x04;

// Package kinds.
const PACKAGE_PLANAR: u8 = 0x00;
const PACKAGE_STACKED: u8 = 0x01;

// Refresh encodings.
const REFRESH_15_6US: u8 = 0;
const REFRESH_7_8US: u8 = 1;

// LPC D31:F0 power-management registers (ICH7).
const GEN_PMCON_2: u16 = 0xa2;
const GEN_PMCON_3: u16 = 0xa4;

// ---------------------------------------------------------------------------
// Slew-rate and strength tables
// ---------------------------------------------------------------------------

const DQ2030: [u32; 16] = [
    0x0807_0706, 0x0a09_0908, 0x0d0c_0b0a, 0x1210_0f0e, 0x1a18_1614, 0x2220_1e1c, 0x2a28_2624,
    0x3934_302d, 0x0a09_0908, 0x0c0b_0b0a, 0x0e0d_0d0c, 0x1211_100f, 0x1917_1513, 0x211f_1d1b,
    0x2d29_2623, 0x3f39_3531,
];

const DQ2330: [u32; 16] = DQ2030;

const CMD2710: [u32; 16] = [
    0x0706_0605, 0x0f0d_0b09, 0x1917_1411, 0x1f1f_1d1b, 0x1f1f_1f1f, 0x1f1f_1f1f, 0x1f1f_1f1f,
    0x1f1f_1f1f, 0x1110_100f, 0x0f0d_0b09, 0x1917_1411, 0x1f1f_1d1b, 0x1f1f_1f1f, 0x1f1f_1f1f,
    0x1f1f_1f1f, 0x1f1f_1f1f,
];

const CMD3210: [u32; 16] = [
    0x0f0d_0b0a, 0x1715_1311, 0x1f1d_1b19, 0x1f1f_1f1f, 0x1f1f_1f1f, 0x1f1f_1f1f, 0x1f1f_1f1f,
    0x1f1f_1f1f, 0x1817_1615, 0x1f1f_1c1a, 0x1f1f_1f1f, 0x1f1f_1f1f, 0x1f1f_1f1f, 0x1f1f_1f1f,
    0x1f1f_1f1f, 0x1f1f_1f1f,
];

const CLK2030: [u32; 16] = [
    0x0e0d_0d0c, 0x100f_0f0e, 0x100f_0e0d, 0x1513_1211, 0x1d1b_1917, 0x2523_211f, 0x2a28_2927,
    0x3230_2e2c, 0x1716_1514, 0x1b1a_1918, 0x1f1e_1d1c, 0x2322_2120, 0x2726_2524, 0x2d2b_2928,
    0x3533_312f, 0x3d3b_3937,
];

const CTL3215: [u32; 16] = [
    0x0101_0000, 0x0302_0101, 0x0706_0504, 0x0b0a_0908, 0x100f_0e0d, 0x1413_1211, 0x1817_1615,
    0x1c1b_1a19, 0x0504_0403, 0x0706_0605, 0x0a09_0807, 0x0f0d_0c0b, 0x1413_1211, 0x1817_1615,
    0x1c1b_1a19, 0x201f_1e1d,
];

const CTL3220: [u32; 16] = [
    0x0504_0403, 0x0706_0505, 0x0e0c_0a08, 0x1a17_1411, 0x2825_221f, 0x3532_2f2b, 0x3e3e_3b38,
    0x3e3e_3e3e, 0x0908_0807, 0x0b0a_0a09, 0x0f0d_0c0b, 0x1b17_1311, 0x2825_221f, 0x3532_2f2b,
    0x3e3e_3b38, 0x3e3e_3e3e,
];

const NC: [u32; 16] = [0; 16];

/// Slew-group selector values.
const G_DQ2030: u8 = 0;
const G_DQ2330: u8 = 1;
const G_CMD2710: u8 = 2;
const G_CMD3210: u8 = 3;
const G_CLK2030: u8 = 4;
const G_CTL3215: u8 = 5;
const G_CTL3220: u8 = 6;
const G_NC: u8 = 7;

const DUAL_CHANNEL_SLEW_GROUP_LOOKUP: [u8; 192] = [
    0, 3, 5, 5, 4, 4, 0, 3, 0, 3, 5, 5, 4, 4, 0, 3,
    0, 3, 7, 5, 7, 4, 0, 3, 0, 3, 5, 5, 4, 4, 0, 2,
    0, 3, 7, 5, 7, 4, 7, 7, 0, 3, 5, 5, 4, 4, 0, 3,
    0, 3, 5, 7, 4, 7, 0, 3, 0, 3, 5, 5, 4, 4, 0, 3,
    0, 3, 5, 7, 4, 7, 0, 2, 0, 3, 5, 7, 4, 7, 7, 7,
    0, 3, 7, 5, 7, 4, 0, 3, 0, 3, 5, 5, 4, 4, 0, 3,
    0, 3, 7, 5, 7, 4, 0, 3, 0, 3, 5, 5, 4, 4, 0, 2,
    0, 3, 7, 5, 7, 4, 7, 7, 0, 2, 5, 5, 4, 4, 0, 3,
    0, 2, 5, 7, 4, 7, 0, 3, 0, 2, 5, 5, 4, 4, 0, 3,
    0, 2, 5, 7, 4, 7, 0, 2, 0, 2, 5, 7, 4, 7, 7, 7,
    7, 7, 7, 5, 7, 4, 0, 3, 7, 7, 5, 7, 4, 7, 0, 3,
    7, 7, 7, 5, 7, 4, 0, 3, 7, 7, 5, 7, 4, 4, 0, 2,
];

const SINGLE_CHANNEL_SLEW_GROUP_LOOKUP: [u8; 192] = [
    1, 3, 5, 5, 4, 4, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 7, 5, 7, 4, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 7, 5, 7, 4, 7, 7, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 5, 7, 4, 7, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 5, 7, 4, 7, 1, 3, 1, 3, 5, 7, 4, 7, 7, 7,
    1, 3, 7, 5, 7, 4, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 7, 5, 7, 4, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 7, 5, 7, 4, 7, 7, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 5, 7, 4, 7, 1, 3, 1, 3, 5, 5, 4, 4, 1, 3,
    1, 3, 5, 7, 4, 7, 1, 3, 1, 3, 5, 7, 4, 7, 7, 7,
    1, 7, 7, 5, 7, 4, 0, 3, 1, 7, 5, 7, 4, 7, 0, 3,
    1, 7, 7, 5, 7, 4, 0, 3, 1, 7, 5, 7, 4, 4, 0, 3,
];

fn slew_group_lookup(dual_channel: bool, index: usize) -> &'static [u32; 16] {
    let table = if dual_channel {
        &DUAL_CHANNEL_SLEW_GROUP_LOOKUP
    } else {
        &SINGLE_CHANNEL_SLEW_GROUP_LOOKUP
    };
    match table[index] {
        G_DQ2030 => &DQ2030,
        G_DQ2330 => &DQ2330,
        G_CMD2710 => &CMD2710,
        G_CMD3210 => &CMD3210,
        G_CLK2030 => &CLK2030,
        G_CTL3215 => &CTL3215,
        G_CTL3220 => &CTL3220,
        G_NC | 8.. => &NC,
    }
}

/// GC (desktop) strength multipliers, dual channel (24 groups of 8).
const GC_DUAL_STRENGTH: [u8; 192] = [
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x33,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x33, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x33,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x33, 0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x22, 0x44, 0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x33,
];

/// GC (desktop) strength multipliers, single channel (24 groups of 8).
const GC_SINGLE_STRENGTH: [u8; 192] = [
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x44, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x55, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x44, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x55, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x44, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x88, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x44, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x55, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x55, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x88, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x55, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x88, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
    0x44, 0x22, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00, 0x44, 0x33, 0x00, 0x00, 0x44, 0x44, 0x44, 0x00,
];

/// GM (mobile) strength multipliers, dual channel (24 groups of 8).
const GM_DUAL_STRENGTH: [u8; 192] = [
    0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x11, 0x00, 0x11, 0x00, 0x44, 0x44, 0x11, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x11, 0x00, 0x11, 0x00, 0x44, 0x00, 0x00, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x11, 0x11, 0x00, 0x44, 0x00, 0x44, 0x11, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x11, 0x11, 0x00, 0x44, 0x00, 0x44, 0x22, 0x44, 0x11, 0x11, 0x00, 0x44, 0x00, 0x00, 0x00,
    0x44, 0x11, 0x00, 0x11, 0x00, 0x44, 0x44, 0x11, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x11, 0x00, 0x11, 0x00, 0x44, 0x44, 0x11, 0x44, 0x11, 0x11, 0x11, 0x44, 0x44, 0x44, 0x22,
    0x44, 0x11, 0x00, 0x11, 0x00, 0x44, 0x00, 0x00, 0x44, 0x22, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x22, 0x11, 0x00, 0x44, 0x00, 0x44, 0x11, 0x44, 0x22, 0x11, 0x11, 0x44, 0x44, 0x44, 0x11,
    0x44, 0x22, 0x11, 0x00, 0x44, 0x00, 0x44, 0x22, 0x44, 0x22, 0x11, 0x00, 0x44, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x11, 0x00, 0x44, 0x44, 0x11, 0x00, 0x00, 0x11, 0x00, 0x44, 0x00, 0x44, 0x11,
    0x00, 0x00, 0x00, 0x11, 0x00, 0x44, 0x44, 0x11, 0x00, 0x00, 0x11, 0x00, 0x44, 0x44, 0x44, 0x22,
];

/// GM (mobile) strength multipliers, single channel (24 groups of 8).
const GM_SINGLE_STRENGTH: [u8; 192] = [
    0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x00, 0x11, 0x00, 0x44, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x00, 0x11, 0x00, 0x44, 0x00, 0x00, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x33, 0x11, 0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x00, 0x00,
    0x33, 0x11, 0x00, 0x11, 0x00, 0x44, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x00, 0x11, 0x00, 0x44, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x00, 0x11, 0x00, 0x44, 0x00, 0x00, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x33, 0x11, 0x33, 0x11, 0x11, 0x11, 0x44, 0x44, 0x33, 0x11,
    0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x33, 0x11, 0x33, 0x11, 0x11, 0x00, 0x44, 0x00, 0x00, 0x00,
    0x33, 0x00, 0x00, 0x11, 0x00, 0x44, 0x33, 0x11, 0x33, 0x00, 0x11, 0x00, 0x44, 0x00, 0x33, 0x11,
    0x33, 0x00, 0x00, 0x11, 0x00, 0x44, 0x33, 0x11, 0x33, 0x00, 0x11, 0x00, 0x44, 0x44, 0x33, 0x11,
];

/// GC (desktop G/P) data clock-crossing pairs, indexed by MEM/FSB.
const GC_DATA_CROSSING: [u32; 30] = [
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x1008_0201,
    0x0000_0000, 0x0010_0401, 0x0000_0000, 0x0001_0402, 0x0000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x0402_0108, 0x0000_0000, 0x0002_0108,
    0x0000_0000, 0x0008_0201, 0x0000_0000, 0x0001_0402, 0x0000_0000, 0x0402_0108, 0x0000_0000,
    0x0804_0110, 0x0000_0000,
];

/// GC (desktop G/P) command clock-crossing pairs, indexed by MEM/FSB.
const GC_COMMAND_CROSSING: [u32; 30] = [
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x0001_0800,
    0x0000_0402, 0x0100_0400, 0x0000_0200, 0x0002_0904, 0x0000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x0201_0804, 0x0000_0000, 0x0001_0402,
    0x0000_0000, 0x0402_0130, 0x0000_0008, 0x0002_0904, 0x0000_0000, 0x0201_0804, 0x0000_0000,
    0x1806_01c0, 0x0000_0020,
];

/// GM (mobile) data clock-crossing pairs, indexed by MEM/FSB.
const GM_DATA_CROSSING: [u32; 30] = [
    0x0010_0401, 0x0000_0000, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x0804_0120,
    0x0000_0000, 0x0010_0401, 0x0000_0000, 0x0001_0402, 0x0000_0000, 0x0402_0120, 0x0000_0010,
    0x1004_0280, 0x0000_0040, 0x0010_0401, 0x0000_0000, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];

/// GM (mobile) command clock-crossing pairs, indexed by MEM/FSB.
const GM_COMMAND_CROSSING: [u32; 30] = [
    0x0402_0208, 0x0000_0000, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0x0006_0108,
    0x0000_0000, 0x0402_0108, 0x0000_0000, 0xffff_ffff, 0xffff_ffff, 0x0004_0318, 0x0000_0000,
    0x0402_0118, 0x0000_0000, 0x0201_0804, 0x0000_0000, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];

// ---------------------------------------------------------------------------
// Raminit context
// ---------------------------------------------------------------------------

/// Central memory-controller state (`struct sys_info`).
#[derive(Clone)]
struct SysInfo {
    memory_frequency: u16,
    fsb_frequency: u16,
    tclk: u32,
    trp: u8,
    trcd: u8,
    tras: u8,
    trfc: u32,
    twr: u8,
    cas: u8,
    refresh: u8,
    dual_channel: bool,
    interleaved: bool,
    mvco4x: u8,
    clkcfg_bit7: bool,
    package: u8,
    dimm: [u8; 4],
    rows: [u8; 4],
    cols: [u8; 4],
    banks: [u8; 4],
    banksize: [u32; 8],
}

impl SysInfo {
    const fn new() -> Self {
        Self {
            memory_frequency: 0,
            fsb_frequency: 0,
            tclk: 0,
            trp: 0,
            trcd: 0,
            tras: 0,
            trfc: 0,
            twr: 0,
            cas: 0,
            refresh: REFRESH_15_6US,
            dual_channel: false,
            interleaved: false,
            mvco4x: 0,
            clkcfg_bit7: false,
            package: PACKAGE_PLANAR,
            dimm: [DIMM_NOT_POPULATED; 4],
            rows: [0; 4],
            cols: [0; 4],
            banks: [0; 4],
            banksize: [0; 8],
        }
    }
}

/// SPD-derived common timings (`struct timings`).
struct CommonTimings {
    min_tclk_cas: [u32; 8],
    min_tras: u32,
    min_trp: u32,
    min_trcd: u32,
    min_twr: u32,
    min_trfc: u32,
    max_trr: u32,
    cas_mask: u8,
}

impl CommonTimings {
    const fn new() -> Self {
        Self {
            min_tclk_cas: [0; 8],
            min_tras: 0,
            min_trp: 0,
            min_trcd: 0,
            min_twr: 0,
            min_trfc: 0,
            max_trr: u32::MAX,
            cas_mask: 0x1c,
        }
    }
}

/// Live raminit context: BARs, PCI devices, board config, SPD bus.
struct Ctx<'a> {
    mch: MchBar,
    hb: EcamDevice,
    lpc: EcamDevice,
    igd: EcamDevice,
    mobile: bool,
    smbus: &'a mut dyn SmBus,
    spd_addresses: [u8; 4],
    pci_mmio_size: u32,
}

impl Ctx<'_> {
    fn hw_dual_channel(&self) -> bool {
        (self.hb.read32(0xe4) >> 24) & 1 == 0
    }

    fn interleave_capable(&self) -> bool {
        (self.hb.read32(0xe4) >> 25) & 1 == 0
    }

    fn xor_capable(&self) -> bool {
        self.hb.read8(0xe5) & (1 << 7) == 0
    }

    fn max_supported_frequency(&self) -> u32 {
        match self.hb.read32(0xe4) & 7 {
            4 => 400,
            3 => 533,
            2 => 667,
            _ => 667,
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn udelay(us: u32) {
    fstart_arch::x86::udelay(us);
}

#[cfg(not(target_arch = "x86_64"))]
fn udelay(_us: u32) {}

#[cfg(target_arch = "x86_64")]
fn ram_read32(addr: u32) {
    // SAFETY: JEDEC/training strobe to a DRAM address; the read itself is
    // the command trigger. Mirrors coreboot `read32p()`.
    unsafe { fstart_arch::x86::read_phys32(addr as usize) };
}

#[cfg(not(target_arch = "x86_64"))]
fn ram_read32(_addr: u32) {}

#[cfg(target_arch = "x86_64")]
fn full_reset() -> ! {
    super::cf9_reset();
}

#[cfg(not(target_arch = "x86_64"))]
fn full_reset() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Issue a DRAM command through DCC.
///
/// Marked `inline(never)` like coreboot's `noinline`: the DCC write itself
/// is the magic trigger and must not be merged with neighbors.
#[inline(never)]
fn do_ram_command(mch: &MchBar, command: u32) {
    let mut reg = mch.read32(r::DCC);
    reg &= !DCC_CMD_MASK;
    reg |= command;
    if command == RAM_COMMAND_NORMAL {
        reg |= DCC_REG::INIT_COMPLETE::SET.value;
    }
    mch.write32(r::DCC, reg);
    udelay(1);
}

/// Log nonzero MCHBAR registers (`sdram_dump_mchbar_registers`).
pub(crate) fn dump_mchbar_registers(mch: &MchBar) {
    for off in (0..0xfffu32).step_by(4) {
        let v = mch.read32(off);
        if v != 0 {
            fstart_log::debug!("i945 MCHBAR {:#06x}: {:#010x}", off, v);
        }
    }
}

fn spd_address(ctx: &Ctx<'_>, device: usize) -> u8 {
    ctx.spd_addresses[device]
}

fn memclk(ctx: &Ctx<'_>) -> i32 {
    let offset: u32 = if ctx.mobile { 1 } else { 0 };
    match ((ctx.mch.read32(mchbar::CLKCFG) >> 4) & 7).wrapping_sub(offset) {
        1 => 400,
        2 => 533,
        3 => 667,
        other => {
            fstart_log::debug!("i945 memclk: unknown register value {:#x}", other);
            -1
        }
    }
}

fn fsbclk(ctx: &Ctx<'_>) -> u16 {
    let clkcfg = ctx.mch.read32(mchbar::CLKCFG) & 7;
    if ctx.mobile {
        match clkcfg {
            0 => 400,
            1 => 533,
            3 => 667,
            _ => {
                fstart_log::debug!("i945 fsbclk: unknown register value {:#x}", clkcfg);
                0xffff
            }
        }
    } else {
        match clkcfg {
            0 => 1066,
            1 => 533,
            2 => 800,
            _ => {
                fstart_log::debug!("i945 fsbclk: unknown register value {:#x}", clkcfg);
                0xffff
            }
        }
    }
}

fn normalize_tck(tclk: &mut u32) {
    if *tclk <= 320 {
        *tclk = 320;
    } else if *tclk <= 365 {
        *tclk = 365;
    } else if *tclk <= 384 {
        *tclk = 384;
    } else if *tclk <= 480 {
        *tclk = 480;
    } else if *tclk <= 640 {
        *tclk = 640;
    } else if *tclk <= 768 {
        *tclk = 768;
    } else if *tclk <= 960 {
        *tclk = 960;
    } else if *tclk <= 1280 {
        *tclk = 1280;
    } else {
        *tclk = 0;
        fstart_log::error!("i945: too slow common tCLK found");
    }
}

/// Error/status pre-check (`sdram_detect_errors`).
fn detect_errors(ctx: &mut Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    let mut do_reset = false;

    let pmcon2 = ctx.lpc.read8(GEN_PMCON_2);
    if pmcon2 & ((1 << 7) | (1 << 2)) != 0 {
        if pmcon2 & (1 << 2) != 0 {
            fstart_log::debug!("i945: SLP S4# assertion width violation");
            // Write-back clears bit 2.
            ctx.lpc.write8(GEN_PMCON_2, pmcon2);
            do_reset = true;
        }
        if pmcon2 & (1 << 7) != 0 {
            fstart_log::debug!("i945: DRAM initialization was interrupted");
            ctx.lpc.and8(GEN_PMCON_2, !(1 << 7));
            do_reset = true;
        }

        // Set SLP_S3# assertion stretch enable.
        ctx.lpc.or8(GEN_PMCON_3, 1 << 3);

        if do_reset {
            fstart_log::debug!("i945: reset required");
            full_reset();
        }
    }

    // Set DRAM initialization bit in ICH7.
    ctx.lpc.or8(GEN_PMCON_2, 1 << 7);

    // Without resume support the self-refresh check always clears status.
    ctx.mch.setbits8(
        mchbar::SLFRCS,
        (SLFRCS_REG::SR_CH1::SET + SLFRCS_REG::SR_CH0::SET).value,
    );

    if do_reset {
        fstart_log::debug!("i945: reset required");
        full_reset();
    }
    let _ = sys;
    Ok(())
}

/// Decode SPD byte 12 (refresh rate) to 1/256 us.
fn decode_trr_us(raw: u8) -> Option<u32> {
    match raw & !0x80 {
        0x0 => Some(15625 << 8),
        0x1 => Some(15625 << 6),
        0x2 => Some(15625 << 7),
        0x3 => Some(15625 << 9),
        0x4 => Some(15625 << 10),
        0x5 => Some(15625 << 11),
        _ => None,
    }
}

/// Gather SPD timings and geometry for all slots (`gather_common_timing`).
fn gather_common_timing(
    ctx: &mut Ctx<'_>,
    sys: &mut SysInfo,
    saved: &mut CommonTimings,
) -> Result<(), ServiceError> {
    if ctx.hw_dual_channel() {
        sys.dual_channel = true;
        fstart_log::debug!("i945: dual channel operation");
    } else {
        sys.dual_channel = false;
        fstart_log::debug!("i945: single channel operation only");
    }

    let mut dimm_mask = 0u8;
    for i in 0..4 {
        sys.dimm[i] = DIMM_NOT_POPULATED;

        // Dual channel unsupported but channel 1 slot: skip.
        if !ctx.hw_dual_channel() && i >> 1 != 0 {
            continue;
        }

        let device = spd_address(ctx, i);
        let mem_type = ctx.smbus.read_byte(device, 2).unwrap_or(0xff);
        if mem_type != ddr2::DDR2 {
            fstart_log::debug!("i945: DDR2 ch{} slot{}: N/A", i >> 1, i & 1);
            continue;
        }

        // Prefer a block read; fall back to byte reads like coreboot.
        let mut raw = [0u8; 256];
        let mut ok = matches!(
            ctx.smbus.block_read(device, 0, &mut raw[..64]),
            Ok(64)
        );
        if !ok {
            ok = true;
            for j in 0..64 {
                match ctx.smbus.read_byte(device, j as u8) {
                    Ok(v) => raw[j] = v,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if !ok {
            fstart_log::debug!("i945: SPD read failed at {:#04x}", device);
            continue;
        }

        let Some(dimm) = ddr2::decode_dimm(&raw) else {
            fstart_log::error!("i945: SPD decode failed, skipping DIMM");
            continue;
        };

        if raw[11] & 0x3 != 0 {
            return Err(RaminitError::EccUnsupported.into());
        }
        if matches!(raw[20] & 0x3f, 0x01 | 0x07 | 0x10) {
            return Err(RaminitError::RegisteredUnsupported.into());
        }

        let width = match dimm.width {
            crate::generic::spd::ChipWidth::X8 => 8,
            crate::generic::spd::ChipWidth::X16 => 16,
            _ => {
                return Err(RaminitError::UnsupportedWidth.into());
            }
        };
        match (width, dimm.ranks) {
            (8, 2) => sys.dimm[i] = DIMM_X8DDS,
            (8, 1) => sys.dimm[i] = DIMM_X8DS,
            (16, 2) => sys.dimm[i] = DIMM_X16DS,
            (16, 1) => sys.dimm[i] = DIMM_X16SS,
            _ => fstart_log::debug!("i945: unsupported rank/width combo"),
        }

        if raw[5] & 0x10 != 0 {
            sys.package = PACKAGE_STACKED;
        }
        if raw[16] & 0x08 == 0 {
            return Err(RaminitError::NoBurstLength8.into());
        }
        if dimm.rank_capacity_mb < 128 {
            return Err(RaminitError::RankTooSmall.into());
        }

        sys.banksize[i * 2] = dimm.rank_capacity_mb / 32;
        if dimm.ranks == 2 {
            sys.banksize[i * 2 + 1] = dimm.rank_capacity_mb / 32;
        }
        sys.rows[i] = dimm.rows;
        sys.cols[i] = dimm.cols;
        sys.banks[i] = dimm.banks;

        saved.min_tras = saved.min_tras.max(dimm.tras_256ns);
        saved.min_trp = saved.min_trp.max(dimm.trp_256ns);
        saved.min_trcd = saved.min_trcd.max(dimm.trcd_256ns);
        saved.min_twr = saved.min_twr.max(dimm.twr_256ns);
        saved.min_trfc = saved.min_trfc.max(dimm.trfc_256ns);
        if let Some(trr) = decode_trr_us(raw[12]) {
            // tRR is 1/256 us; common timings use 1/256 ns.
            saved.max_trr = saved.max_trr.min(trr * 1000);
        }
        saved.cas_mask &= dimm.cas_latencies;
        for cas in 0..8 {
            if saved.cas_mask & (1 << cas) == 0 {
                saved.min_tclk_cas[cas] = 0;
            } else {
                saved.min_tclk_cas[cas] =
                    saved.min_tclk_cas[cas].max(dimm.cycle_time_256ns[cas]);
            }
        }
        dimm_mask |= 1 << i;
    }

    if dimm_mask == 0 {
        return Err(RaminitError::NoMemory.into());
    }
    if dimm_mask & 0x03 == 0 {
        fstart_log::info!("i945: channel 0 has no memory populated");
    }
    Ok(())
}

/// Pick tCK and CAS (`choose_tclk`).
fn choose_tclk(ctx: &Ctx<'_>, sys: &mut SysInfo, saved: &CommonTimings) -> Result<(), ServiceError> {
    let mut ctrl_min_tclk = 2 * 256 * 1000 / ctx.max_supported_frequency();
    normalize_tck(&mut ctrl_min_tclk);

    let Some(mut try_cas) = ddr2::msb_index(saved.cas_mask) else {
        return Err(RaminitError::NoCommonCas.into());
    };
    while saved.cas_mask & (1 << try_cas) != 0 && try_cas > 0 {
        sys.cas = try_cas;
        sys.tclk = saved.min_tclk_cas[try_cas as usize];
        if sys.tclk >= ctrl_min_tclk
            && saved.min_tclk_cas[try_cas as usize]
                != saved.min_tclk_cas[try_cas as usize - 1]
        {
            break;
        }
        try_cas -= 1;
    }
    normalize_tck(&mut sys.tclk);

    if sys.cas < 3 || sys.tclk == 0 {
        return Err(RaminitError::NoCommonFrequency.into());
    }
    if sys.tclk < ctrl_min_tclk {
        sys.tclk = ctrl_min_tclk;
    }

    sys.memory_frequency = match sys.tclk {
        TCK_200MHZ => 400,
        TCK_266MHZ => 533,
        768 => 667,
        _ => 0,
    };
    fstart_log::debug!(
        "i945: memory at {}MT CAS={}",
        sys.memory_frequency,
        sys.cas
    );
    Ok(())
}

fn div_round_up(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

/// Derive cycle timings (`derive_timings`).
fn derive_timings(sys: &mut SysInfo, saved: &CommonTimings) -> Result<(), ServiceError> {
    sys.tras = div_round_up(saved.min_tras, sys.tclk) as u8;
    if sys.tras > 0x18 {
        return Err(RaminitError::BadTras.into());
    }
    sys.trp = div_round_up(saved.min_trp, sys.tclk) as u8;
    if sys.trp > 6 {
        return Err(RaminitError::BadTrp.into());
    }
    sys.trcd = div_round_up(saved.min_trcd, sys.tclk) as u8;
    if sys.trcd > 6 {
        return Err(RaminitError::BadTrcd.into());
    }
    sys.twr = div_round_up(saved.min_twr, sys.tclk) as u8;
    if sys.twr > 5 {
        return Err(RaminitError::BadTwr.into());
    }
    sys.trfc = div_round_up(saved.min_trfc, sys.tclk);

    // tRR thresholds in 1/256 ns: 7.8us = 2000000, 15.6us = 4000000.
    if saved.max_trr < 2_000_000 {
        return Err(RaminitError::BadRefresh.into());
    } else if saved.max_trr < 4_000_000 {
        sys.refresh = REFRESH_7_8US;
    } else {
        sys.refresh = REFRESH_15_6US;
    }
    fstart_log::debug!(
        "i945: tRAS={} tRP={} tRCD={} tWR={} tRFC={} refresh={}",
        sys.tras,
        sys.trp,
        sys.trcd,
        sys.twr,
        sys.trfc,
        if sys.refresh == REFRESH_7_8US {
            "7.8us"
        } else {
            "15.6us"
        }
    );
    Ok(())
}

fn get_dram_configuration(
    ctx: &mut Ctx<'_>,
    sys: &mut SysInfo,
) -> Result<(), ServiceError> {
    let mut saved = CommonTimings::new();
    gather_common_timing(ctx, sys, &mut saved)?;
    choose_tclk(ctx, sys, &saved)?;
    derive_timings(sys, &saved)
}

fn dram_width_nibble(kind: u8, channel1: bool) -> u16 {
    // X8DS encodes as 0x1 (ch0) / 0x10 (ch1), X8DDS as 0x5 / 0x50;
    // every other population encodes as 0x0.
    match (kind, channel1) {
        (DIMM_X8DS, false) => 0x1,
        (DIMM_X8DS, true) => 0x10,
        (DIMM_X8DDS, false) => 0x5,
        (DIMM_X8DDS, true) => 0x50,
        _ => 0x0,
    }
}

/// Program DRAM width (`sdram_program_dram_width`).
fn program_dram_width(ctx: &Ctx<'_>, sys: &SysInfo) {
    let idx = if ctx.hw_dual_channel() { 2 } else { 1 };
    let mut c0dramw = 0u16;
    for (i, kind) in sys.dimm[0..2].iter().enumerate() {
        c0dramw |= dram_width_nibble(*kind, false) << (4 * (i % 2));
    }
    let mut c1dramw = 0u16;
    for (i, kind) in sys.dimm[2..2 * idx].iter().enumerate() {
        c1dramw |= dram_width_nibble(*kind, true) << (4 * (i % 2));
    }
    ctx.mch.write16(r::C0DRAMW, c0dramw);
    ctx.mch.write16(r::C0DRAMW + r::C1_BASE, c1dramw);
}

fn write_slew_rates(mch: &MchBar, offset: u32, table: &[u32; 16]) {
    for (i, v) in table.iter().enumerate() {
        mch.write32(offset + (i as u32) * 4, *v);
    }
}

/// Program RCOMP strength and slew (`sdram_rcomp_buffer_strength_and_slew`).
fn rcomp_buffer_strength_and_slew(ctx: &Ctx<'_>, sys: &SysInfo) {
    let strength: &[u8; 192] = match (ctx.mobile, ctx.hw_dual_channel()) {
        (true, true) => &GM_DUAL_STRENGTH,
        (true, false) => &GM_SINGLE_STRENGTH,
        (false, true) => &GC_DUAL_STRENGTH,
        (false, false) => &GC_SINGLE_STRENGTH,
    };
    let (dual_channel, idx) = if ctx.hw_dual_channel() {
        (true, 5 * sys.dimm[0] + sys.dimm[2])
    } else {
        (false, 5 * sys.dimm[0] + sys.dimm[1])
    };
    let idx = idx as usize;
    fstart_log::debug!("i945: RCOMP table index {}", idx);

    let sc = [
        r::G1SC,
        r::G1SC + 8,
        r::G1SC + 16,
        r::G1SC + 24,
        r::G1SC + 32,
        r::G1SC + 40,
        0x490,
        0x498,
    ];
    for (k, off) in sc.iter().enumerate() {
        ctx.mch.write8(*off, strength[idx * 8 + k]);
    }

    write_slew_rates(&ctx.mch, r::G1SRPUT, slew_group_lookup(dual_channel, idx * 8));
    write_slew_rates(
        &ctx.mch,
        r::G2SRPUT,
        slew_group_lookup(dual_channel, idx * 8 + 1),
    );
    if slew_group_lookup(dual_channel, idx * 8 + 2) != &NC && sys.package == PACKAGE_STACKED {
        write_slew_rates(&ctx.mch, r::G3SRPUT, &CTL3220);
    } else {
        write_slew_rates(
            &ctx.mch,
            r::G3SRPUT,
            slew_group_lookup(dual_channel, idx * 8 + 2),
        );
    }
    write_slew_rates(
        &ctx.mch,
        r::G4SRPUT,
        slew_group_lookup(dual_channel, idx * 8 + 3),
    );
    write_slew_rates(&ctx.mch, r::G5SRPUT, slew_group_lookup(dual_channel, idx * 8 + 4));
    write_slew_rates(&ctx.mch, r::G6SRPUT, slew_group_lookup(dual_channel, idx * 8 + 5));

    if sys.dual_channel {
        write_slew_rates(
            &ctx.mch,
            r::G7SRPUT,
            slew_group_lookup(dual_channel, idx * 8 + 6),
        );
        write_slew_rates(
            &ctx.mch,
            r::G8SRPUT,
            slew_group_lookup(dual_channel, idx * 8 + 7),
        );
    } else {
        write_slew_rates(&ctx.mch, r::G7SRPUT, &NC);
        write_slew_rates(&ctx.mch, r::G8SRPUT, &NC);
    }
}

/// Enable periodic RCOMP (`sdram_enable_rcomp`).
fn enable_rcomp(ctx: &Ctx<'_>) {
    udelay(300);
    ctx.mch.clrbits32(r::GBRCOMPCTL, GBRCOMPCTL_REG::PERIODIC_DIS::SET.value);
}

/// Program DLL timings (`sdram_program_dll_timings`).
fn program_dll_timings(ctx: &Ctx<'_>, sys: &SysInfo) {
    ctx.mch.clrbits16(
        r::DQSMT,
        (DQSMT_REG::DQSMT_B13::SET
            + DQSMT_REG::DQSMT_B12::SET
            + DQSMT_REG::DQSMT_B10::SET
            + DQSMT_REG::DQSMT_LO.val(0xf))
        .value,
    );
    ctx.mch.setbits16(
        r::DQSMT,
        (DQSMT_REG::DQSMT_B13::SET + DQSMT_REG::DQSMT_LO.val(0xc)).value,
    );

    let channeldll = if ctx.mobile {
        match sys.memory_frequency {
            400 => 0x2626_2626,
            533 => 0x2222_2222,
            667 => 0x1111_1111,
            _ => 0,
        }
    } else {
        match sys.memory_frequency {
            400 => 0x3333_3333,
            533 => 0x2424_2424,
            667 => 0x2525_2525,
            _ => 0,
        }
    };
    for i in 0..4u32 {
        for (base, wl) in [
            (r::C0R0B00DQST, r::C0WL0REOST),
            (r::C0R0B00DQST + r::C1_BASE, r::C0WL0REOST + r::C1_BASE),
        ] {
            ctx.mch.write32(base + i * 0x10, channeldll);
            ctx.mch.write32(base + i * 0x10 + 4, channeldll);
            if !ctx.mobile {
                ctx.mch.write8(wl + i * 0x10 + 8, (channeldll & 0xff) as u8);
            }
        }
    }
}

/// Force an RCOMP cycle (`sdram_force_rcomp`).
fn force_rcomp(ctx: &Ctx<'_>) {
    ctx.mch.setbits32(r::ODTC, ODTC_REG::RCOMP_FORCE_ODT::SET.value);
    ctx.mch.setbits32(r::SMSRCTL, SMSRCTL_REG::SM_RCOMP_EN::SET.value);
    // Start initial RCOMP.
    ctx.mch.setbits32(r::GBRCOMPCTL, GBRCOMPCTL_REG::RCOMP_FORCE::SET.value);

    let rev = IntelI945::silicon_revision();
    if (rev == 0 && ctx.mch.read32(r::DCC) & 3 == 0) || rev == 1 {
        ctx.mch.setbits32(r::GBRCOMPCTL, GBRCOMPCTL_REG::RCOMP_ALT.val(3).value);
    }
}

/// System-memory IO init (`sdram_initialize_system_memory_io`).
fn initialize_system_memory_io(ctx: &mut Ctx<'_>, sys: &SysInfo) {
    ctx.mch.clrsetbits8(r::C0HCTC, 0x1f, HCTC_REG::HCTC_MODE.val(1).value);
    ctx.mch
        .clrsetbits8(r::C0HCTC + r::C1_BASE, 0x1f, HCTC_REG::HCTC_MODE.val(1).value);
    ctx.mch.clrbits16(
        r::WDLLBYPMODE,
        (1 << 9) | (1 << 6) | (1 << 4) | (1 << 3) | (1 << 1),
    );
    ctx.mch.setbits16(
        r::WDLLBYPMODE,
        (1 << 8) | (1 << 7) | (1 << 5) | (1 << 2) | (1 << 0),
    );
    ctx.mch.write8(r::C0WDLLCMC, 0);
    ctx.mch.write8(r::C0WDLLCMC + r::C1_BASE, 0);

    program_dram_width(ctx, sys);
    rcomp_buffer_strength_and_slew(ctx, sys);

    // Indicate that RCOMP programming is done.
    ctx.mch.clrsetbits32(
        r::GBRCOMPCTL,
        (GBRCOMPCTL_REG::RCOMP_DONE::SET
            + GBRCOMPCTL_REG::RCOMP_CFG26::SET
            + GBRCOMPCTL_REG::RCOMP_CFG21.val(3)
            + GBRCOMPCTL_REG::RCOMP_CFG2.val(3))
        .value,
        (GBRCOMPCTL_REG::RCOMP_CFG27.val(3) + GBRCOMPCTL_REG::RCOMP_EN.val(3)).value,
    );
    ctx.mch.setbits32(r::GBRCOMPCTL, GBRCOMPCTL_REG::RCOMP_DONE_FLAG::SET.value);

    program_dll_timings(ctx, sys);
    force_rcomp(ctx);
}

/// System-memory IO buffer enable (`sdram_enable_system_memory_io`).
fn enable_system_memory_io(ctx: &Ctx<'_>, sys: &SysInfo) {
    ctx.mch.clrbits32(r::RCVENMT, 0x3f << 6); // [11:6] = 0, see RCVENMT_REG
    ctx.mch.setbits32(
        r::RCVENMT,
        (RCVENMT_REG::CH0_EN::SET + RCVENMT_REG::CH0_MED::SET).value,
    );
    ctx.mch.setbits32(
        r::DRTST,
        (DRTST_REG::IO_EN::SET + DRTST_REG::IO_MODE::SET).value,
    );
    ctx.mch.setbits32(
        r::DRTST,
        (DRTST_REG::TEST_EN::SET + DRTST_REG::TEST_MODE::SET).value,
    );

    // NOP-ish barrier: two no-ops before sampling DRTST.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("nop", "nop", options(nomem, nostack, preserves_flags));
    }

    if sys.dimm[0] != DIMM_NOT_POPULATED || sys.dimm[1] != DIMM_NOT_POPULATED {
        ctx.mch.setbits32(
            r::DRTST,
            (DRTST_REG::CH0_IO0::SET + DRTST_REG::CH0_IO1::SET).value,
        );
    } else {
        ctx.mch.setbits32(r::DRTST, DRTST_REG::CH0_EMPTY::SET.value);
    }
    if sys.dimm[2] != DIMM_NOT_POPULATED || sys.dimm[3] != DIMM_NOT_POPULATED {
        ctx.mch.setbits32(
            r::DRTST,
            (DRTST_REG::CH1_IO0::SET + DRTST_REG::CH1_IO1::SET).value,
        );
    } else {
        ctx.mch.setbits32(r::DRTST, DRTST_REG::CH1_EMPTY::SET.value);
    }

    if sys.dimm[0] != DIMM_NOT_POPULATED || sys.dimm[1] != DIMM_NOT_POPULATED {
        ctx.mch.setbits32(r::C0DRC1, DRC1_REG::IO_BUF_EN::SET.value);
    }
    if sys.dimm[2] != DIMM_NOT_POPULATED || sys.dimm[3] != DIMM_NOT_POPULATED {
        ctx.mch.setbits32(r::C0DRC1 + r::C1_BASE, DRC1_REG::IO_BUF_EN::SET.value);
    }
}

/// Program DRB/TOLUD/TOM (`sdram_program_row_boundaries`).
fn program_row_boundaries(ctx: &Ctx<'_>, sys: &SysInfo) {
    let mut cum0 = 0u32;
    for i in 0..4 {
        cum0 += sys.banksize[i];
        ctx.mch.write8(r::C0DRB0 + i as u32, cum0 as u8);
    }

    let mut cum1 = if sys.interleaved { 0 } else { cum0 };
    for i in 0..4 {
        cum1 += sys.banksize[i + 4];
        ctx.mch
            .write8(r::C0DRB0 + r::C1_BASE + i as u32, cum1 as u8);
    }

    let tolud = if sys.interleaved {
        (cum0 + cum1) << 1
    } else if cum1 != 0 {
        cum1 << 1
    } else {
        cum0 << 1
    };
    let tom = tolud >> 3;

    let mut pci_mmio_size = ctx.pci_mmio_size;
    if pci_mmio_size <= 768 {
        pci_mmio_size = 768;
    }
    let tolud = (((4096 - pci_mmio_size) / 128) << 3).min(tolud);

    ctx.hb.write8(hostbridge::TOLUD, tolud as u8);
    ctx.hb.write16(hostbridge::TOM, tom as u16);
}

/// Program page sizes (`sdram_set_row_attributes`).
fn set_row_attributes(ctx: &Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    let (mut dra0, mut dra1) = (0u16, 0u16);
    for i in 0..4 {
        if sys.dimm[i] == DIMM_NOT_POPULATED {
            continue;
        }
        let columnsrows = (sys.rows[i] & 0x0f) | ((sys.cols[i] & 0xf) << 4);
        let mut dra = match columnsrows {
            0x9d => 2,
            0xad | 0xae => 3,
            0xbd | 0xbe => 4,
            _ => {
                return Err(RaminitError::BadRowsCols.into());
            }
        };
        if sys.banksize[2 * i + 1] != 0 {
            dra = (dra << 4) | dra;
        }
        if i < 2 {
            dra0 |= dra << (i * 8);
        } else {
            dra1 |= dra << ((i - 2) * 8);
        }
    }
    ctx.mch.write16(r::C0DRA0, dra0);
    ctx.mch.write16(r::C0DRA0 + r::C1_BASE, dra1);
    Ok(())
}

/// Program bank architecture (`sdram_set_bank_architecture`).
fn set_bank_architecture(ctx: &Ctx<'_>, sys: &SysInfo) {
    ctx.mch.clrbits16(r::C0BNKARC, 0xff);
    ctx.mch.clrbits16(r::C0BNKARC + r::C1_BASE, 0xff);
    for (i, banks) in sys.banks.iter().enumerate() {
        if sys.dimm[i] == DIMM_NOT_POPULATED || *banks != 8 {
            continue;
        }
        let off = if i < 2 { r::C0BNKARC } else { r::C0BNKARC + r::C1_BASE };
        if i & 1 != 0 {
            ctx.mch.setbits16(off, 5 << 4);
        } else {
            ctx.mch.setbits16(off, 5);
        }
    }
}

/// Program refresh rate (`sdram_program_refresh_rate`).
fn program_refresh_rate(ctx: &Ctx<'_>, sys: &SysInfo) {
    let refresh = if sys.refresh == REFRESH_7_8US { 2 } else { 1 };
    ctx.mch
        .clrsetbits32(r::C0DRC0, 7 << 8, DRC0_REG::REFRESH.val(refresh).value);
    ctx.mch
        .clrsetbits32(r::C0DRC0 + r::C1_BASE, 7 << 8, DRC0_REG::REFRESH.val(refresh).value);
}

/// Program CKE tristate (`sdram_program_cke_tristate`).
fn program_cke_tristate(ctx: &Ctx<'_>, sys: &SysInfo) {
    let mut reg = ctx.mch.read32(r::C0DRC1);
    for i in 0..4 {
        if sys.banksize[i] == 0 {
            reg |= 1 << (16 + i);
        }
    }
    reg |= DRC1_REG::CKE.val(3).value;
    ctx.mch.write32(r::C0DRC1, reg);

    let mut reg = ctx.mch.read32(r::C0DRC1 + r::C1_BASE);
    for i in 4..8 {
        if sys.banksize[i] == 0 {
            reg |= 1 << (12 + i);
        }
    }
    reg |= DRC1_REG::CKE.val(3).value;
    ctx.mch.write32(r::C0DRC1 + r::C1_BASE, reg);
}

/// Program ODT tristate (`sdram_program_odt_tristate`).
fn program_odt_tristate(ctx: &Ctx<'_>, sys: &SysInfo) {
    let mut reg = ctx.mch.read32(r::C0DRC2);
    for i in 0..4 {
        if sys.banksize[i] == 0 {
            reg |= 1 << (24 + i);
        }
    }
    ctx.mch.write32(r::C0DRC2, reg);

    let mut reg = ctx.mch.read32(r::C0DRC2 + r::C1_BASE);
    for i in 4..8 {
        if sys.banksize[i] == 0 {
            reg |= 1 << (20 + i);
        }
    }
    ctx.mch.write32(r::C0DRC2 + r::C1_BASE, reg);
}

// DRT1 CAS encodings indexed by CAS-3.
const CAS_TABLE: [u32; 4] = [2, 1, 0, 3];

/// Program DRAM timing and control (`sdram_set_timing_and_control`).
fn set_timing_and_control(ctx: &Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    for ch in [0u32, r::C1_BASE] {
        ctx.mch.clrsetbits32(
            r::C0DRC0 + ch,
            (1 << 13) | (1 << 12),
            DRC0_REG::BURST8::SET.value,
        );
    }
    if !sys.dual_channel && sys.dimm[1] != DIMM_NOT_POPULATED {
        ctx.mch.setbits32(r::C0DRC0, DRC0_REG::SC1_SECOND_DIMM::SET.value);
    }

    program_refresh_rate(ctx, sys);
    program_cke_tristate(ctx, sys);
    program_odt_tristate(ctx, sys);

    // DRT0.
    let w2r_same = (u32::from(sys.cas) - 1) + (8 / 2) + u32::from(sys.twr);
    let twtr = if sys.memory_frequency == 667 { 3 } else { 2 };
    let mut trd_min = u32::from(sys.cas);
    match sys.fsb_frequency {
        667 => trd_min += 1,
        800 => trd_min += 2,
        1066 => trd_min += 3,
        _ => {}
    }
    let temp_drt = (DRT0_REG::B2B_W_PCHG.val(w2r_same)
        + DRT0_REG::W_AUTO_PCHG.val(w2r_same + u32::from(sys.trp))
        + DRT0_REG::B2B_W2R.val((u32::from(sys.cas) - 1) + (8 / 2) + twtr)
        + DRT0_REG::TRD.val(trd_min)
        + DRT0_REG::R_AP_TO_ACT.val(8))
    .value
        // Fixed per coreboot: (1 << 22) | (3 << 20) | (1 << 18).
        | 0x0074_0000;
    ctx.mch.write32(r::C0DRT0, temp_drt);
    ctx.mch.write32(r::C0DRT0 + r::C1_BASE, temp_drt);

    // DRT1.
    let mut temp_drt = ctx.mch.read32(r::C0DRT1) & 0x0002_0088;
    temp_drt |= (DRT1_REG::TRP.val(u32::from(sys.trp) - 2)
        + DRT1_REG::TRCD.val(u32::from(sys.trcd) - 2)
        + DRT1_REG::CAS.val(CAS_TABLE[(sys.cas - 3) as usize])
        + DRT1_REG::TRAS.val(u32::from(sys.tras)))
    .value;
    // tRFC keeps its numeric shift: the DDR2 range can exceed the 6-bit
    // hardware field, and coreboot writes the unmasked value through.
    temp_drt |= sys.trfc << 10;
    if sys.memory_frequency == 667 {
        temp_drt |= DRT1_REG::TRTP_667::SET.value;
    }
    let mut page = 0u32;
    let mut page_size = 1u32;
    for kind in sys.dimm {
        if kind == DIMM_X16DS || kind == DIMM_X16SS {
            page_size = 2;
        }
    }
    if sys.memory_frequency == 533 && page_size == 2 {
        page = 1;
    }
    if sys.memory_frequency == 667 {
        page = page_size;
    }
    temp_drt |= DRT1_REG::PAGE.val(page).value;
    ctx.mch.write32(r::C0DRT1, temp_drt);
    ctx.mch.write32(r::C0DRT1 + r::C1_BASE, temp_drt);

    // DRT2 bit 8 clear.
    ctx.mch.clrbits32(r::C0DRT2, 1 << 8);
    ctx.mch.clrbits32(r::C0DRT2 + r::C1_BASE, 1 << 8);

    // DRT3: 788ns - tRFC plus the 1us field.
    let mut temp_drt = ctx.mch.read32(r::C0DRT3) & !0x07ff_ffff;
    let old_trfc = (ctx.mch.read32(r::C0DRT1) >> 10) & 0x3f;
    let (divisor, hi, lo) = match sys.memory_frequency {
        400 => (500u32, 0x8cu32, 0x0cu32),
        533 => (375, 0xba, 0x10),
        667 => (300, 0xe9, 0x14),
        _ => {
            return Err(RaminitError::BadDrt3Frequency.into());
        }
    };
    temp_drt |= DRT3_REG::REF_MINUS_TRFC.val(((78800 / divisor) - old_trfc) & 0x1ff).value;
    temp_drt |= (DRT3_REG::US_HI.val(hi) + DRT3_REG::US_LO.val(lo)).value;
    ctx.mch.write32(r::C0DRT3, temp_drt);
    ctx.mch.write32(r::C0DRT3 + r::C1_BASE, temp_drt);
    Ok(())
}

/// Determine channel mode (`sdram_set_channel_mode`).
fn set_channel_mode(ctx: &Ctx<'_>, sys: &mut SysInfo) {
    let ch0: u32 = sys.banksize[0..4].iter().sum();
    let ch1: u32 = sys.banksize[4..8].iter().sum();
    sys.interleaved = ctx.interleave_capable() && ch0 == ch1;

    let mut dcc = ctx.mch.read32(r::DCC) & !7;
    if sys.interleaved {
        dcc |= 1 << 1;
    } else if sys.dimm[0] == DIMM_NOT_POPULATED && sys.dimm[1] == DIMM_NOT_POPULATED {
        dcc |= 1 << 2;
    } else if ctx.hw_dual_channel()
        && (sys.dimm[2] != DIMM_NOT_POPULATED || sys.dimm[3] != DIMM_NOT_POPULATED)
    {
        dcc |= 1 << 0;
    }
    // Disable channel XORing (re-enabled for interleave after JEDEC).
    dcc |= 1 << 10;
    ctx.mch.write32(r::DCC, dcc);
    fstart_log::debug!(
        "i945: {}",
        if sys.interleaved {
            "dual channel interleaved"
        } else if dcc & (1 << 2) != 0 {
            "single channel 1 only"
        } else if dcc & (1 << 0) != 0 {
            "dual channel asymmetric"
        } else {
            "single channel 0 only"
        }
    );
}

/// Program PLL (`sdram_program_pll_settings`).
fn program_pll_settings(ctx: &mut Ctx<'_>, sys: &mut SysInfo) -> Result<(), ServiceError> {
    ctx.mch.write32(r::PLLMON, 0x8080_0000);
    sys.fsb_frequency = fsbclk(ctx);
    if sys.fsb_frequency == 0xffff {
        return Err(RaminitError::UnsupportedFsb.into());
    }
    match sys.fsb_frequency {
        400 => ctx.mch.write8(r::CPCTL, 0x90),
        533 => ctx.mch.write8(r::CPCTL, 0x95),
        667 => ctx.mch.write8(r::CPCTL, 0x8d),
        _ => {}
    }
    ctx.mch.clrbits16(r::CPCTL, 1 << 11);
    // Read back to activate settings.
    let _ = ctx.mch.read16(r::CPCTL);
    Ok(())
}

/// Mobile graphics frequency (`sdram_program_graphics_frequency`).
fn program_graphics_frequency(ctx: &mut Ctx<'_>, sys: &mut SysInfo) {
    const CRCLK_166MHZ: u8 = 0x00;
    const CRCLK_200MHZ: u8 = 0x01;
    const CRCLK_250MHZ: u8 = 0x03;
    const CRCLK_400MHZ: u8 = 0x05;
    const CDCLK_200MHZ: u8 = 0x00;
    const CDCLK_320MHZ: u8 = 0x40;

    let voltage_1_50 = ctx.mch.read32(mchbar::DFT_STRAP1) & (1 << 20) != 0;
    // Gate graphics hardware for the frequency change.
    ctx.igd.or8(hostbridge::IGD_GCFC + 1, (1 << 3) | (1 << 1));

    let caps = (ctx.hb.read8(0xe5) >> 1) & 7;
    let mut freq = CRCLK_250MHZ;
    match caps {
        0 => {
            freq = if voltage_1_50 {
                CRCLK_400MHZ
            } else {
                CRCLK_250MHZ
            }
        }
        2 => freq = CRCLK_250MHZ,
        3 => freq = CRCLK_200MHZ,
        4 => freq = CRCLK_166MHZ,
        _ => {}
    }
    if freq != CRCLK_400MHZ && (ctx.hb.read8(0xe7) & 0x70) >> 4 == 2 {
        freq = CRCLK_166MHZ;
    }

    sys.mvco4x = u8::from(IntelI945::silicon_revision() == 0);
    let mut second_vco = voltage_1_50;
    if !voltage_1_50 && IntelI945::silicon_revision() > 0 && freq == CRCLK_250MHZ {
        let (fsb, mem) = (sys.fsb_frequency, sys.memory_frequency);
        if (fsb == 667 && mem == 533) || (fsb == 533 && mem == 533) || (fsb == 533 && mem == 400)
        {
            second_vco = true;
        }
        if fsb == 667 && mem == 533 {
            sys.mvco4x = 1;
        }
    }
    sys.clkcfg_bit7 = second_vco;

    ctx.igd.and16(hostbridge::IGD_GCFC, !(7 | (1 << 13)));
    ctx.igd.or16(hostbridge::IGD_GCFC, u16::from(freq));
    let mut reg = ctx.igd.read8(hostbridge::IGD_GCFC) & !((1 << 7) | (7 << 4));
    reg |= if voltage_1_50 {
        CDCLK_320MHZ
    } else {
        CDCLK_200MHZ
    };
    ctx.igd.write8(hostbridge::IGD_GCFC, reg);
    ctx.igd.or8(hostbridge::IGD_GCFC + 1, (1 << 3) | (1 << 1));
    ctx.igd.or8(hostbridge::IGD_GCFC + 1, 0x0f);
    // Ungate core render and display clocks.
    ctx.igd.and8(hostbridge::IGD_GCFC + 1, 0xf0);
}

/// Program memory frequency with the VCO update dance
/// (`sdram_program_memory_frequency`).
fn program_memory_frequency(ctx: &mut Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    let offset: u32 = if ctx.mobile { 1 } else { 0 };
    let mut clkcfg = ctx.mch.read32(mchbar::CLKCFG);
    clkcfg &= !((1 << 12) | (1 << 7) | (7 << 4));
    if sys.mvco4x != 0 {
        clkcfg &= !(1 << 12);
    }
    if sys.clkcfg_bit7 {
        clkcfg |= 1 << 7;
    }
    clkcfg |= match sys.memory_frequency {
        400 => (1 + offset) << 4,
        533 => (2 + offset) << 4,
        667 => (3 + offset) << 4,
        _ => {
            return Err(RaminitError::BadMemClkFrequency.into());
        }
    };

    if ctx.mch.read32(mchbar::CLKCFG) == clkcfg {
        return Ok(());
    }
    ctx.mch.write32(mchbar::CLKCFG, clkcfg);

    // VCO update: prefetch the update path into cache (CAR/XIP), clear the
    // DRAM-init-interrupted latch, then toggle the update bit with a delay.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("jmp 2f", options(nomem, nostack));
        core::arch::asm!("3:", options(nomem, nostack));
    }
    ctx.lpc.and8(GEN_PMCON_2, !(1 << 7));
    let mut clkcfg = clkcfg & !(1 << 10);
    ctx.mch.write32(mchbar::CLKCFG, clkcfg);
    clkcfg |= 1 << 10;
    ctx.mch.write32(mchbar::CLKCFG, clkcfg);
    #[cfg(target_arch = "x86_64")]
    for _ in 0..0x100 {
        unsafe {
            core::arch::asm!("nop", "nop", "nop", "nop", options(nomem, nostack, preserves_flags));
        }
    }
    ctx.mch.write32(mchbar::CLKCFG, clkcfg & !(1 << 10));
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("jmp 4f", "2:", "jmp 3b", "4:", options(nomem, nostack));
    }
    Ok(())
}

/// Program clock crossing (`sdram_program_clock_crossing`).
fn program_clock_crossing(ctx: &Ctx<'_>) {
    let (data, command) = if ctx.mobile {
        (&GM_DATA_CROSSING, &GM_COMMAND_CROSSING)
    } else {
        (&GC_DATA_CROSSING, &GC_COMMAND_CROSSING)
    };
    let mut idx = match memclk(ctx) {
        400 => 0,
        533 => 2,
        667 => 4,
        _ => return,
    };
    idx += match fsbclk(ctx) {
        400 => 0,
        533 => 6,
        667 => 12,
        800 => 18,
        1066 => 24,
        _ => return,
    };
    let idx = idx as usize;
    if command[idx] == 0xffff_ffff {
        fstart_log::debug!("i945: invalid MEM/FSB combination");
    }
    ctx.mch.write32(r::CCCFT_LO, command[idx]);
    ctx.mch.write32(r::CCCFT_LO + 4, command[idx + 1]);
    ctx.mch.write32(r::C0DCCFT_LO, data[idx]);
    ctx.mch.write32(r::C0DCCFT_LO + 4, data[idx + 1]);
    ctx.mch
        .write32(r::C0DCCFT_LO + r::C1_BASE, data[idx]);
    ctx.mch
        .write32(r::C0DCCFT_LO + r::C1_BASE + 4, data[idx + 1]);
}

/// Disable fast dispatch (`sdram_disable_fast_dispatch`).
fn disable_fast_dispatch(ctx: &Ctx<'_>) {
    ctx.mch.setbits32(mchbar::FSBPMC3, 1 << 1);
    ctx.mch.setbits32(r::SBTEST, 3 << 1);
}

/// Pre-JEDEC init (`sdram_pre_jedec_initialization`).
fn pre_jedec_initialization(ctx: &Ctx<'_>) {
    ctx.mch
        .clrsetbits32(
            r::WCC,
            !WCC_BASE_MASK,
            (WCC_REG::WRITE_DIS.val(4) + WCC_REG::READ_DIS.val(3) + WCC_REG::POSTED_WRITE::SET)
                .value,
        );
    ctx.mch.setbits32(r::SMVREFC, SMVREFC_REG::SMVREF_EN::SET.value);
    ctx.mch.clrsetbits32(r::MMARB0, 3 << 17, (1 << 21) | (1 << 16));
    ctx.mch.clrsetbits32(r::MMARB1, 7 << 8, 3 << 8);
    ctx.mch.write32(r::C0AIT_LO, 0x0000_06c4);
    ctx.mch.write32(r::C0AIT_LO + 4, 0x871a_066d);
    ctx.mch.write32(r::C0AIT_LO + r::C1_BASE, 0x0000_06c4);
    ctx.mch
        .write32(r::C0AIT_LO + r::C1_BASE + 4, 0x871a_066d);
}

// Enhanced-addressing modes.
const EA_DC_XOR_BANK_RANK: u32 = 0xd4 << 24;
const EA_DC_XOR_BANK: u32 = 0xf4 << 24;
const EA_DC_BANK_RANK: u32 = 0xc2 << 24;
const EA_DC_BANK: u32 = 0xe2 << 24;
const EA_SC_XOR_BANK_RANK: u32 = 0x91 << 24;
const EA_SC_XOR_BANK: u32 = 0xb1 << 24;
const EA_SC_BANK_RANK: u32 = 0x80 << 24;
const EA_SC_BANK: u32 = 0xa0 << 24;

/// Enhanced addressing mode (`sdram_enhanced_addressing_mode`).
fn enhanced_addressing_mode(ctx: &Ctx<'_>, sys: &SysInfo) {
    let ch0_pop = sys.dimm[0] != DIMM_NOT_POPULATED || sys.dimm[1] != DIMM_NOT_POPULATED;
    let ch1_pop = sys.dimm[2] != DIMM_NOT_POPULATED || sys.dimm[3] != DIMM_NOT_POPULATED;
    let ch0_dual = sys.banksize[1] != 0 || sys.banksize[3] != 0;
    let ch1_dual = sys.banksize[5] != 0 || sys.banksize[7] != 0;

    let (mut chan0, mut chan1) = (0u32, 0u32);
    if ctx.xor_capable() {
        if !sys.interleaved {
            if ch0_pop {
                chan0 = if ch0_dual {
                    EA_SC_XOR_BANK_RANK
                } else {
                    EA_SC_XOR_BANK
                };
            }
            if ch1_pop {
                chan1 = if ch1_dual {
                    EA_SC_XOR_BANK_RANK
                } else {
                    EA_SC_XOR_BANK
                };
            }
        } else if ch0_dual {
            chan0 = EA_DC_XOR_BANK_RANK;
        } else {
            chan0 = EA_DC_XOR_BANK;
        }
        if sys.interleaved {
            chan1 = if ch1_dual {
                EA_DC_XOR_BANK_RANK
            } else {
                EA_DC_XOR_BANK
            };
        }
    } else if !sys.interleaved {
        if ch0_pop {
            chan0 = if ch0_dual {
                EA_SC_BANK_RANK
            } else {
                EA_SC_BANK
            };
        }
        if ch1_pop {
            chan1 = if ch1_dual {
                EA_SC_BANK_RANK
            } else {
                EA_SC_BANK
            };
        }
    } else {
        chan0 = if ch0_dual {
            EA_DC_BANK_RANK
        } else {
            EA_DC_BANK
        };
        chan1 = if ch1_dual {
            EA_DC_BANK_RANK
        } else {
            EA_DC_BANK
        };
    }
    ctx.mch.clrsetbits32(r::C0DRC1, 0xff << 24, chan0);
    ctx.mch
        .clrsetbits32(r::C0DRC1 + r::C1_BASE, 0xff << 24, chan1);
}

/// Post-JEDEC init (`sdram_post_jedec_initialization`).
fn post_jedec_initialization(ctx: &Ctx<'_>, sys: &SysInfo) {
    if sys.interleaved {
        ctx.mch.clrsetbits32(r::DCC, 1 << 10, 1 << 9);
    }
    enhanced_addressing_mode(ctx, sys);
    ctx.mch.clrbits32(mchbar::FSBPMC3, 1 << 1);
    ctx.mch.clrbits32(r::SBTEST, 1 << 2);
    ctx.mch.clrsetbits32(
        r::SBOCC,
        !SBOCC_BASE_MASK,
        (SBOCC_REG::OCC_TIMER.val(0xbdb6) + SBOCC_REG::OCC_EN::SET).value,
    );
}

/// Power management (`sdram_power_management`).
fn power_management(ctx: &Ctx<'_>, sys: &SysInfo) {
    let integrated_graphics = ctx.hb.read8(hostbridge::DEVEN)
        & (hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1) as u8
        != 0;

    for ch in [0u32, r::C1_BASE] {
        ctx.mch.clrsetbits32(
            r::C0DRT2 + ch,
            0xff,
            (DRT2_REG::CKE_IDLE.val(3) + DRT2_REG::TIMER_LO.val(0)).value,
        );
        ctx.mch.setbits32(r::C0DRC1 + ch, DRC1_REG::CKE.val(3).value);
    }

    if ctx.mobile {
        let peg_bits = (1 << 5) | (1 << 0);
        if IntelI945::silicon_revision() > 1 {
            ctx.mch.write16(mchbar::UPMC1, 0x1010 | peg_bits);
        } else {
            ctx.mch.write16(mchbar::UPMC1, 0x0010 | peg_bits);
        }
    }
    ctx.mch.clrsetbits16(r::UPMC2, !0xfc00, 0x0100);
    ctx.mch.write32(r::UPMC3, 0x000f_06ff);
    for _ in 0..5 {
        ctx.mch.clrbits32(r::UPMC3, 1 << 16);
        ctx.mch.setbits32(r::UPMC3, 1 << 16);
    }
    ctx.mch.write32(r::GIPMC1, 0x8000_000c);
    if IntelI945::silicon_revision() > 2 {
        ctx.mch.clrsetbits16(r::CPCTL, 7 << 11, CPCTL_REG::PM_DIV.val(6).value);
    } else {
        ctx.mch.clrsetbits16(r::CPCTL, 7 << 11, CPCTL_REG::PM_DIV.val(4).value);
    }

    if IntelI945::silicon_revision() != 0 {
        ctx.mch.write32(
            r::HGIPMC2,
            match sys.fsb_frequency {
                667 => 0x0d59_0d59,
                533 => 0x155b_155b,
                _ => 0,
            },
        );
    } else {
        ctx.mch.write32(
            r::HGIPMC2,
            match sys.fsb_frequency {
                667 => 0x09c4_09c4,
                533 => 0x0fa0_0fa0,
                _ => 0,
            },
        );
    }
    // Only program defined FSB cases; other values keep reset state.
    if matches!(sys.fsb_frequency, 533 | 667) {
        ctx.mch.write32(r::FSBPMC1, 0x8000_000c);
        let (c2c3, c3c4) = match sys.fsb_frequency {
            667 => (0x0600, 0x0b80),
            _ => (0x0480, 0x0980),
        };
        ctx.mch.clrsetbits32(r::C2C3TT, !0xffff_0000, c2c3);
        ctx.mch.clrsetbits32(r::C3C4TT, !0xffff_0000, c3c4);
    } else {
        ctx.mch.write32(r::FSBPMC1, 0x8000_000c);
    }

    if IntelI945::silicon_revision() == 0 {
        ctx.mch.clrbits32(r::ECO, ECO_REG::ECO_BIT16::SET.value);
    } else {
        ctx.mch.setbits32(r::ECO, ECO_REG::ECO_BIT16::SET.value);
    }
    ctx.mch.clrbits32(mchbar::FSBPMC3, FSBPMC3_REG::DIS_QPML::SET.value);
    ctx.mch.setbits32(mchbar::FSBPMC3, FSBPMC3_REG::PM_ENABLE::SET.value);
    ctx.mch.clrbits32(mchbar::FSBPMC3, FSBPMC3_REG::FAST_DISPATCH::SET.value);
    ctx.mch.clrbits32(mchbar::FSBPMC3, FSBPMC3_REG::GM_ERRATA::SET.value);
    ctx.mch.clrsetbits32(
        r::FSBPMC4,
        3 << 24,
        FSBPMC4_REG::PM_MODE.val(2).value,
    );
    ctx.mch.setbits32(r::FSBPMC4, FSBPMC4_REG::PM_EN21::SET.value);
    ctx.mch.setbits32(r::FSBPMC4, FSBPMC4_REG::PM_EN5::SET.value);
    if IntelI945::silicon_revision() < 2 {
        ctx.mch.clrbits32(r::FSBPMC4, FSBPMC4_REG::PM_POLARITY::SET.value);
    } else {
        ctx.mch.setbits32(r::FSBPMC4, FSBPMC4_REG::PM_POLARITY::SET.value);
    }

    ctx.hb.or8(0xfc, 1 << 4);
    ctx.igd.or8(0xc1, 1 << 2);

    let (mipmc4, mipmc5, mipmc6) = if integrated_graphics {
        (0x04f8, 0x04fc, 0x04fc)
    } else {
        (0x64f8, 0x64fc, 0x64fc)
    };
    ctx.mch.write16(r::MIPMC4, mipmc4);
    ctx.mch.write16(r::MIPMC5, mipmc5);
    ctx.mch.write16(r::MIPMC6, mipmc6);
    ctx.mch.clrsetbits32(r::PMCFG, 3 << 17, PMCFG_REG::PM_MODE.val(2).value);
    ctx.mch.setbits32(r::PMCFG, PMCFG_REG::PM_EN::SET.value);
    ctx.mch.clrsetbits32(r::UPMC4, 0xff, 0x01);
    ctx.mch.clrbits32(r::MISC_B18, MISC_B18_REG::MISC_CTRL21::SET.value);
}

/// Thermal management (`sdram_thermal_management`): DIMM sensors unimplemented.
fn thermal_management(ctx: &Ctx<'_>) {
    ctx.mch.write8(0xc92, 0);
    ctx.mch.write8(0xce2, 0);
}

// ODT tables indexed by CAS-3.
const ODT_LO: [u32; 3] = [0x0002_4911, 0x0004_9211, 0x0006_db11];
const ODT_HI: [u32; 3] = [0xe001_0000, 0xe002_0000, 0xe003_0000];

/// On-die termination (`sdram_on_die_termination`).
fn on_die_termination(ctx: &Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    ctx.mch.clrsetbits32(
        r::ODTC,
        3 << 16,
        (ODTC_REG::ODT_MODE.val(2) + ODTC_REG::ODT_EN::SET + ODTC_REG::ODT_REF::SET).value,
    );

    if sys.dimm[0] == DIMM_NOT_POPULATED || sys.dimm[1] == DIMM_NOT_POPULATED {
        ctx.mch.clrbits32(r::C0ODT_LO, 7 << 28);
        ctx.mch.clrbits32(r::C0ODT_LO + r::C1_BASE, 7 << 28);
    }

    if !(3..=5).contains(&sys.cas) {
        return Err(RaminitError::BadOdtCas.into());
    }
    let odt = (sys.cas - 3) as usize;
    for ch in [0u32, r::C1_BASE] {
        ctx.mch
            .clrsetbits32(r::C0ODT_LO + ch, !0xfff0_0000, ODT_LO[odt]);
        ctx.mch
            .clrsetbits32(r::C0ODT_LO + ch + 4, !0x1fc8_ffff, ODT_HI[odt]);
    }
    Ok(())
}

/// Enable clocks to populated sockets (`sdram_enable_memory_clocks`).
fn enable_memory_clocks(ctx: &Ctx<'_>, sys: &SysInfo) {
    // GC desktop parts gate 3 clock pairs per channel, GM parts 2.
    let width: u32 = if ctx.mobile { 2 } else { 3 };
    let pair = (1 << width) - 1;
    let mut clocks = [0u8; 2];
    if sys.dimm[0] != DIMM_NOT_POPULATED {
        clocks[0] |= pair as u8;
    }
    if sys.dimm[1] != DIMM_NOT_POPULATED {
        clocks[0] |= (pair << width) as u8;
    }
    if sys.dimm[2] != DIMM_NOT_POPULATED {
        clocks[1] |= pair as u8;
    }
    if sys.dimm[3] != DIMM_NOT_POPULATED {
        clocks[1] |= (pair << width) as u8;
    }
    ctx.mch.write8(r::C0DCLKDIS, clocks[0]);
    ctx.mch.write8(r::C0DCLKDIS + r::C1_BASE, clocks[1]);
}

// JEDEC MRS encodings.
const RTT_ODT_75_OHM: u32 = 1 << 5;
const RTT_ODT_150_OHM: u32 = 1 << 9;
const EMRS_OCD_DEFAULT: u32 = (1 << 12) | (1 << 11) | (1 << 10);

/// JEDEC init sequence (`sdram_jedec_enable`).
fn jedec_enable(ctx: &mut Ctx<'_>, sys: &SysInfo) -> Result<(), ServiceError> {
    let mut bankaddr = 0u32;
    let mut nonzero: Option<usize> = None;

    for i in 0..8 {
        if sys.banksize[i] == 0 {
            continue;
        }
        if let Some(prev) = nonzero {
            if sys.interleaved && prev < 4 && i >= 4 {
                bankaddr = 0x40;
            } else {
                bankaddr += sys.banksize[prev] << (if sys.interleaved { 26 } else { 25 });
            }
        }
        nonzero = Some(i);

        let mut mrsaddr = match sys.cas {
            5 => 5 << 7,
            4 => 4 << 7,
            3 => 3 << 7,
            _ => {
                return Err(RaminitError::BadJedecCas.into());
            }
        };
        mrsaddr |= match sys.twr {
            5 => 4 << 12,
            4 => 3 << 12,
            3 => 2 << 12,
            _ => {
                return Err(RaminitError::BadJedecTwr.into());
            }
        };
        mrsaddr |= 1 << 6;
        if sys.interleaved {
            mrsaddr <<= 1;
        }
        mrsaddr |= 3 << 3;

        do_ram_command(&ctx.mch, RAM_COMMAND_NOP);
        ram_read32(bankaddr);
        do_ram_command(&ctx.mch, RAM_COMMAND_PRECHARGE);
        ram_read32(bankaddr);
        do_ram_command(&ctx.mch, RAM_COMMAND_EMRS | RAM_EMRS_2);
        ram_read32(bankaddr);
        do_ram_command(&ctx.mch, RAM_COMMAND_EMRS | RAM_EMRS_3);
        ram_read32(bankaddr);
        do_ram_command(&ctx.mch, RAM_COMMAND_EMRS | RAM_EMRS_1);
        let mut tmpaddr = bankaddr;
        if !ctx.hw_dual_channel() {
            tmpaddr |= RTT_ODT_75_OHM;
        } else if sys.interleaved {
            tmpaddr |= RTT_ODT_150_OHM << 1;
        } else {
            tmpaddr |= RTT_ODT_150_OHM;
        }
        ram_read32(tmpaddr);

        do_ram_command(&ctx.mch, RAM_COMMAND_MRS);
        tmpaddr = bankaddr | mrsaddr;
        tmpaddr |= 1 << (if sys.interleaved { 12 } else { 11 });
        ram_read32(tmpaddr);

        do_ram_command(&ctx.mch, RAM_COMMAND_PRECHARGE);
        ram_read32(bankaddr);
        do_ram_command(&ctx.mch, RAM_COMMAND_CBR);
        ram_read32(bankaddr);
        ram_read32(bankaddr);

        do_ram_command(&ctx.mch, RAM_COMMAND_MRS);
        ram_read32(bankaddr | mrsaddr);

        do_ram_command(&ctx.mch, RAM_COMMAND_EMRS | RAM_EMRS_1);
        tmpaddr = bankaddr;
        if !ctx.hw_dual_channel() {
            tmpaddr |= RTT_ODT_75_OHM | EMRS_OCD_DEFAULT;
        } else if sys.interleaved {
            tmpaddr |= (RTT_ODT_150_OHM | EMRS_OCD_DEFAULT) << 1;
        } else {
            tmpaddr |= RTT_ODT_150_OHM | EMRS_OCD_DEFAULT;
        }
        ram_read32(tmpaddr);

        do_ram_command(&ctx.mch, RAM_COMMAND_EMRS | RAM_EMRS_1);
        tmpaddr = bankaddr;
        if !ctx.hw_dual_channel() {
            tmpaddr |= RTT_ODT_75_OHM;
        } else if sys.interleaved {
            tmpaddr |= RTT_ODT_150_OHM << 1;
        } else {
            tmpaddr |= RTT_ODT_150_OHM;
        }
        ram_read32(tmpaddr);
    }
    Ok(())
}

fn init_complete(ctx: &Ctx<'_>) {
    do_ram_command(&ctx.mch, RAM_COMMAND_NORMAL);
}

fn setup_processor_side(ctx: &Ctx<'_>) {
    if IntelI945::silicon_revision() == 0 {
        ctx.mch.setbits32(mchbar::FSBPMC3, 1 << 2);
    }
    ctx.mch.setbits8(r::MISC_B00, MISC_B00_REG::PROC_SIDE_INIT::SET.value);
    if IntelI945::silicon_revision() == 0 {
        ctx.mch.setbits32(r::SLPCTL, SLPCTL_REG::SLPCTL_B8::SET.value);
    }
}

// ---------------------------------------------------------------------------
// Receive-enable training (`rcven.c`)
// ---------------------------------------------------------------------------

/// Sample the strobes (`sample_strobes`).
fn sample_strobes(ctx: &Ctx<'_>, channel_offset: u32, sys: &SysInfo) -> u32 {
    ctx.mch.setbits32(r::C0DRC1 + channel_offset, DRC1_REG::RCVEN_TOGGLE::SET.value);
    ctx.mch.clrbits32(r::C0DRC1 + channel_offset, DRC1_REG::RCVEN_TOGGLE::SET.value);

    let mut addr = 0u32;
    if channel_offset != 0 {
        if sys.interleaved {
            addr |= 1 << 6;
        } else {
            addr = u32::from(ctx.mch.read8(r::C0DRB0 + 3)) << 25;
        }
    }
    for _ in 0..28 {
        ram_read32(addr);
        ram_read32(addr + 0x80);
    }

    let mut reg = ctx.mch.read32(r::RCVENMT);
    if channel_offset == 0 {
        reg <<= 2;
    }
    reg
}

/// Program receive-enable coarse/medium timing (`set_receive_enable`).
fn set_receive_enable(ctx: &Ctx<'_>, channel_offset: u32, medium: u8, coarse: u8) {
    ctx.mch.clrsetbits32(
        r::C0DRT1 + channel_offset,
        0x0f00_0000,
        DRT1_REG::RCVEN_COARSE.val(u32::from(coarse) & 0x0f).value,
    );
    if coarse > 0x0f {
        fstart_log::debug!("i945: coarse overflow {:#04x}", coarse);
    }
    if channel_offset == 0 {
        ctx.mch.clrsetbits32(
            r::RCVENMT,
            3 << 2,
            RCVENMT_REG::CH0_MEDIUM.val(u32::from(medium)).value,
        );
    } else {
        ctx.mch.clrsetbits32(
            r::RCVENMT,
            3,
            RCVENMT_REG::CH1_MEDIUM.val(u32::from(medium)).value,
        );
    }
}

fn normalize(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    mediumcoarse: &mut u8,
    fine: &mut u8,
) -> Result<(), ()> {
    if *fine < 0x80 {
        return Ok(());
    }
    *fine = fine.wrapping_sub(0x80);
    *mediumcoarse = mediumcoarse.wrapping_add(1);
    if *mediumcoarse >= 0x40 {
        fstart_log::debug!("i945: normalize error");
        return Err(());
    }
    set_receive_enable(
        ctx,
        channel_offset,
        *mediumcoarse & 3,
        *mediumcoarse >> 2,
    );
    ctx.mch.write8(r::C0WL0REOST + channel_offset, *fine);
    Ok(())
}

fn find_preamble(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    mediumcoarse: &mut u8,
    sys: &SysInfo,
) -> Result<(), ()> {
    loop {
        if *mediumcoarse < 4 {
            fstart_log::debug!("i945: no preamble found");
            return Err(());
        }
        *mediumcoarse = mediumcoarse.wrapping_sub(4);
        set_receive_enable(
            ctx,
            channel_offset,
            *mediumcoarse & 3,
            *mediumcoarse >> 2,
        );
        let reg = sample_strobes(ctx, channel_offset, sys);
        if reg & (1 << 19) == 0 {
            break;
        }
    }
    let reg = sample_strobes(ctx, channel_offset, sys);
    if reg & (1 << 18) == 0 {
        fstart_log::debug!("i945: no preamble found (neither high nor low)");
        return Err(());
    }
    Ok(())
}

fn add_quarter_clock(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    mediumcoarse: &mut u8,
    fine: &mut u8,
) -> Result<(), ()> {
    if *fine >= 0x80 {
        *fine = fine.wrapping_sub(0x80);
        *mediumcoarse = mediumcoarse.wrapping_add(2);
        if *mediumcoarse >= 0x40 {
            fstart_log::debug!("i945: clocks at max");
            return Err(());
        }
        set_receive_enable(
            ctx,
            channel_offset,
            *mediumcoarse & 3,
            *mediumcoarse >> 2,
        );
    } else {
        *fine = fine.wrapping_add(0x80);
    }
    ctx.mch.write8(r::C0WL0REOST + channel_offset, *fine);
    Ok(())
}

fn find_strobes_low(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    mediumcoarse: &mut u8,
    fine: &mut u8,
    sys: &SysInfo,
) {
    loop {
        ctx.mch.write8(r::C0WL0REOST + channel_offset, *fine);
        set_receive_enable(
            ctx,
            channel_offset,
            *mediumcoarse & 3,
            *mediumcoarse >> 2,
        );
        if sample_strobes(ctx, channel_offset, sys) & (1 << 18) != 0 {
            return;
        }
        *fine = fine.wrapping_sub(0x80);
        if *fine == 0 {
            continue;
        }
        *mediumcoarse = mediumcoarse.wrapping_sub(2);
        if *mediumcoarse < 0xfe {
            continue;
        }
        break;
    }
    fstart_log::debug!("i945: could not find low strobe");
}

fn find_strobes_edge(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    mediumcoarse: &mut u8,
    fine: &mut u8,
    sys: &SysInfo,
) -> Result<(), ()> {
    let mut counter = 8;
    set_receive_enable(
        ctx,
        channel_offset,
        *mediumcoarse & 3,
        *mediumcoarse >> 2,
    );
    loop {
        ctx.mch.write8(r::C0WL0REOST + channel_offset, *fine);
        if sample_strobes(ctx, channel_offset, sys) & (1 << 19) == 0 {
            counter = 8;
        } else {
            counter -= 1;
            if counter == 0 {
                break;
            }
        }
        *fine = fine.wrapping_add(1);
        if *fine < 0xf8 {
            if *fine & (1 << 3) != 0 {
                *fine &= !(1 << 3);
                *fine = fine.wrapping_add(0x10);
            }
            continue;
        }
        *fine = 0;
        *mediumcoarse = mediumcoarse.wrapping_add(2);
        if *mediumcoarse <= 0x40 {
            set_receive_enable(
                ctx,
                channel_offset,
                *mediumcoarse & 3,
                *mediumcoarse >> 2,
            );
            continue;
        }
        fstart_log::debug!("i945: could not find rising edge");
        return Err(());
    }

    *fine = fine.wrapping_sub(7);
    if *fine >= 0xf9 {
        *mediumcoarse = mediumcoarse.wrapping_sub(2);
        set_receive_enable(
            ctx,
            channel_offset,
            *mediumcoarse & 3,
            *mediumcoarse >> 2,
        );
    }
    *fine &= !(1 << 3);
    ctx.mch.write8(r::C0WL0REOST + channel_offset, *fine);
    Ok(())
}

fn receive_enable_autoconfig(
    ctx: &Ctx<'_>,
    channel_offset: u32,
    sys: &SysInfo,
) -> Result<(), ()> {
    let mut mediumcoarse = (sys.cas << 2) | 3;
    let mut fine = 0u8;

    find_strobes_low(ctx, channel_offset, &mut mediumcoarse, &mut fine, sys);
    find_strobes_edge(ctx, channel_offset, &mut mediumcoarse, &mut fine, sys)?;
    add_quarter_clock(ctx, channel_offset, &mut mediumcoarse, &mut fine)?;
    find_preamble(ctx, channel_offset, &mut mediumcoarse, sys)?;
    add_quarter_clock(ctx, channel_offset, &mut mediumcoarse, &mut fine)?;
    normalize(ctx, channel_offset, &mut mediumcoarse, &mut fine)?;

    if ctx.mch.read8(r::C0WL0REOST + channel_offset) == 0 {
        fstart_log::debug!(
            "i945: weird, no C{}WL0REOST",
            if channel_offset != 0 { "1" } else { "0" }
        );
    }
    Ok(())
}

/// Train receive enable per populated channel (`receive_enable_adjust`).
fn receive_enable_adjust(ctx: &Ctx<'_>, sys: &SysInfo) {
    if sys.dimm[0] != DIMM_NOT_POPULATED || sys.dimm[1] != DIMM_NOT_POPULATED {
        let _ = receive_enable_autoconfig(ctx, 0, sys);
    }
    if sys.dimm[2] != DIMM_NOT_POPULATED || sys.dimm[3] != DIMM_NOT_POPULATED {
        let _ = receive_enable_autoconfig(ctx, 0x80, sys);
    }
}

/// Program receive-enable timings (`sdram_program_receive_enable`).
///
/// Resume always reboots (no MRC cache), so only the training path exists.
fn program_receive_enable(ctx: &Ctx<'_>, sys: &SysInfo) {
    ctx.mch.setbits32(r::REPC, REPC_REG::RCVEN_EN::SET.value);
    receive_enable_adjust(ctx, sys);
    ctx.mch.setbits32(r::C0DRC1, DRC1_REG::RCVEN_TOGGLE::SET.value);
    ctx.mch.setbits32(r::C0DRC1 + r::C1_BASE, DRC1_REG::RCVEN_TOGGLE::SET.value);
    ctx.mch.clrbits32(r::C0DRC1, DRC1_REG::RCVEN_TOGGLE::SET.value);
    ctx.mch.clrbits32(r::C0DRC1 + r::C1_BASE, DRC1_REG::RCVEN_TOGGLE::SET.value);
    // MIPMC3 receive-enable done bits (plain 0x0f per coreboot).
    ctx.mch.setbits32(r::MIPMC3, 0x0f);
}

// ---------------------------------------------------------------------------
// Top-level entry
// ---------------------------------------------------------------------------

/// Full i945 SDRAM initialization (`sdram_initialize`).
///
/// `boot_path` comes from the fixed platform flow. Anything but a cold
/// normal boot reboots: without an MRC cache fstart cannot resume, matching
/// the GM965 port's policy.
pub fn sdram_initialize(
    nb: &IntelI945,
    smbus: &mut dyn SmBus,
) -> Result<(), ServiceError> {
    use crate::BootPath;

    if nb.boot_path != BootPath::Normal {
        fstart_log::info!("i945: non-cold boot path, issuing reset");
        full_reset();
    }

    fstart_log::info!("i945: setting up RAM controller");
    let mut sys = SysInfo::new();
    let mut ctx = Ctx {
        mch: MchBar::new(nb.config.mchbar as usize),
        hb: EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC),
        lpc: EcamDevice::new(0, 0x1f, 0),
        igd: EcamDevice::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC),
        mobile: nb.config.variant == I945Variant::Mobile,
        smbus,
        spd_addresses: nb.config.spd_addresses,
        pci_mmio_size: nb.config.pci_mmio_size,
    };

    get_dram_configuration(&mut ctx, &mut sys)?;
    detect_errors(&mut ctx, &sys)?;
    program_pll_settings(&mut ctx, &mut sys)?;

    if ctx.mobile {
        program_graphics_frequency(&mut ctx, &mut sys);
    } else {
        ctx.igd.write16(hostbridge::IGD_GCFC, 0x0534);
    }

    program_memory_frequency(&mut ctx, &sys)?;
    set_channel_mode(&ctx, &mut sys);
    program_clock_crossing(&ctx);
    disable_fast_dispatch(&ctx);
    ctx.mch.setbits32(r::C0DMC, DMC_REG::PWR_DOWN_ACPI::SET.value);
    ctx.mch.setbits32(r::C0DMC + r::C1_BASE, DMC_REG::PWR_DOWN_ACPI::SET.value);

    program_row_boundaries(&ctx, &sys);
    set_row_attributes(&ctx, &sys)?;
    set_bank_architecture(&ctx, &sys);
    set_timing_and_control(&ctx, &sys)?;
    on_die_termination(&ctx, &sys)?;
    pre_jedec_initialization(&ctx);
    initialize_system_memory_io(&mut ctx, &sys);
    enable_system_memory_io(&ctx, &sys);
    enable_memory_clocks(&ctx, &sys);

    // Cold boot only (resume reboots above).
    jedec_enable(&mut ctx, &sys)?;

    power_management(&ctx, &sys);
    post_jedec_initialization(&ctx, &sys);
    thermal_management(&ctx);
    init_complete(&ctx);
    program_receive_enable(&ctx, &sys);
    enable_rcomp(&ctx);

    // Tell ICH7 that we're done.
    ctx.lpc.and8(GEN_PMCON_2, !(1 << 7));

    fstart_log::info!("i945: RAM initialization finished");
    setup_processor_side(&ctx);
    Ok(())
}
