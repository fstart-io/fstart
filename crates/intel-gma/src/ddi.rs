//! DDI connector planning helpers.
//!
//! This module mirrors the register-selection and bit-encoding portions of
//! libgfxinit's shared DDI connector code. It performs no MMIO access itself:
//! it produces plans that the Haswell, Broxton and Skylake generation modules
//! execute. The Haswell and Skylake executors currently apply the DDI clock,
//! routing and buffer/transport programming; pipe and plane commit is not yet
//! wired for those families.

use tock_registers::registers::ReadWrite;
use tock_registers::{register_bitfields, register_structs};

use crate::error::GmaError;
use crate::types::{Cpu, Pipe, Port};

register_bitfields! [u32,
    /// Skylake/Kabylake per-DPLL control register.
    pub SKL_DPLL_CTL_REG [
        PLL_ENABLE OFFSET(31) NUMBITS(1) []
    ],

    /// Skylake/Kabylake DPLL status register.
    pub SKL_DPLL_STATUS_REG [
        LOCK OFFSET(0) NUMBITS(32) []
    ],

    /// Skylake/Kabylake DPLL control register 1 fields.
    pub SKL_DPLL_CTRL1_REG [
        HDMI_MODE OFFSET(5) NUMBITS(1) [],
        SSC OFFSET(4) NUMBITS(1) [],
        LINK_RATE OFFSET(1) NUMBITS(3) [
            Rate2700 = 0,
            Rate1350 = 1,
            Rate810 = 2
        ],
        OVERRIDE OFFSET(0) NUMBITS(1) []
    ],

    /// DDI buffer control register fields.
    pub DDI_BUF_CTL_REG [
        BUFFER_ENABLE OFFSET(31) NUMBITS(1) [],
        TRANS_SELECT OFFSET(24) NUMBITS(2) [],
        PORT_REVERSAL OFFSET(16) NUMBITS(1) [],
        DDI_A_LANE_CAP OFFSET(4) NUMBITS(1) [],
        PORT_WIDTH OFFSET(1) NUMBITS(2) [
            OneLane = 0,
            TwoLanes = 1,
            FourLanes = 3
        ],
        INIT_DISPLAY_DETECT OFFSET(0) NUMBITS(1) []
    ],

    /// Haswell/Broadwell WRPLL control register fields.
    pub HSW_WRPLL_CTL_REG [
        /// Raw WRPLL control payload produced by the PLL calculator.
        RAW OFFSET(0) NUMBITS(32) []
    ],

    /// DDI port clock select register fields.
    pub PORT_CLK_SEL_REG [
        /// Clock source selector in bits 31:29.
        CLOCK_SELECT OFFSET(29) NUMBITS(3) []
    ],

    /// Skylake/Kabylake shared DPLL_CTRL2 routing register fields.
    pub SKL_DPLL_CTRL2_REG [
        /// DDI E clock is disabled.
        DDI_E_CLOCK_OFF OFFSET(19) NUMBITS(1) [],
        /// DDI D clock is disabled.
        DDI_D_CLOCK_OFF OFFSET(18) NUMBITS(1) [],
        /// DDI C clock is disabled.
        DDI_C_CLOCK_OFF OFFSET(17) NUMBITS(1) [],
        /// DDI B clock is disabled.
        DDI_B_CLOCK_OFF OFFSET(16) NUMBITS(1) [],
        /// DDI A clock is disabled.
        DDI_A_CLOCK_OFF OFFSET(15) NUMBITS(1) [],
        /// DDI E PLL select.
        DDI_E_SELECT OFFSET(13) NUMBITS(2) [],
        /// DDI E PLL select override.
        DDI_E_SELECT_OVERRIDE OFFSET(12) NUMBITS(1) [],
        /// DDI D PLL select.
        DDI_D_SELECT OFFSET(10) NUMBITS(2) [],
        /// DDI D PLL select override.
        DDI_D_SELECT_OVERRIDE OFFSET(9) NUMBITS(1) [],
        /// DDI C PLL select.
        DDI_C_SELECT OFFSET(7) NUMBITS(2) [],
        /// DDI C PLL select override.
        DDI_C_SELECT_OVERRIDE OFFSET(6) NUMBITS(1) [],
        /// DDI B PLL select.
        DDI_B_SELECT OFFSET(4) NUMBITS(2) [],
        /// DDI B PLL select override.
        DDI_B_SELECT_OVERRIDE OFFSET(3) NUMBITS(1) [],
        /// DDI A PLL select.
        DDI_A_SELECT OFFSET(1) NUMBITS(2) [],
        /// DDI A PLL select override.
        DDI_A_SELECT_OVERRIDE OFFSET(0) NUMBITS(1) []
    ],

    /// DisplayPort transport control register fields.
    pub DP_TP_CTL_REG [
        TRANSPORT_ENABLE OFFSET(31) NUMBITS(1) [],
        MODE OFFSET(27) NUMBITS(1) [
            Sst = 0,
            Mst = 1
        ],
        FORCE_ACT OFFSET(25) NUMBITS(1) [],
        ENHANCED_FRAME_ENABLE OFFSET(18) NUMBITS(1) [],
        FDI_AUTOTRAIN OFFSET(15) NUMBITS(1) [],
        LINK_TRAIN OFFSET(8) NUMBITS(3) [
            Pattern1 = 0,
            Pattern2 = 1,
            Idle = 2,
            Normal = 3,
            Pattern3 = 4
        ],
        SCRAMBLE_DISABLE OFFSET(7) NUMBITS(1) []
    ]
];

register_structs! {
    /// Haswell/Broadwell WRPLL control registers, relative to WRPLL0.
    pub HswWrpllRegs {
        (0x00 => pub wrpll0: ReadWrite<u32, HSW_WRPLL_CTL_REG::Register>),
        (0x04 => _reserved0),
        (0x20 => pub wrpll1: ReadWrite<u32, HSW_WRPLL_CTL_REG::Register>),
        (0x24 => @END),
    }
}

register_structs! {
    /// HSW/SKL-style DDI port-local register block, relative to `DDI_BUF_CTL`.
    pub DdiPortRegs {
        (0x00 => pub buf_ctl: ReadWrite<u32, DDI_BUF_CTL_REG::Register>),
        (0x04 => _reserved1),
        (0x40 => pub dp_tp_ctl: ReadWrite<u32, DP_TP_CTL_REG::Register>),
        (0x44 => pub dp_tp_status: ReadWrite<u32>),
        (0x48 => @END),
    }
}

register_structs! {
    /// Per-DDI port clock select registers, relative to DDI A `PORT_CLK_SEL`.
    pub HswPortClockSelectRegs {
        (0x00 => pub ddi_a: ReadWrite<u32, PORT_CLK_SEL_REG::Register>),
        (0x04 => pub ddi_b: ReadWrite<u32, PORT_CLK_SEL_REG::Register>),
        (0x08 => pub ddi_c: ReadWrite<u32, PORT_CLK_SEL_REG::Register>),
        (0x0c => pub ddi_d: ReadWrite<u32, PORT_CLK_SEL_REG::Register>),
        (0x10 => pub ddi_e: ReadWrite<u32, PORT_CLK_SEL_REG::Register>),
        (0x14 => @END),
    }
}

/// Haswell/Broadwell WRPLL0 register offset.
pub const HSW_WRPLL_BASE: usize = 0x46040;
/// Haswell/Broadwell DDI A port-clock select register offset.
pub const HSW_PORT_CLK_SEL_BASE: usize = 0x46100;

/// Intel digital/DDI port identifier used by libgfxinit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiPort {
    /// DDI A, commonly eDP or CPU DP A.
    A,
    /// DDI B.
    B,
    /// DDI C.
    C,
    /// DDI D.
    D,
    /// DDI E.
    E,
}

impl DdiPort {
    /// Map a board-facing logical port to a DDI port.
    pub const fn from_port(port: Port) -> Result<Self, GmaError> {
        match port {
            Port::Edp | Port::DpA | Port::HdmiA => Ok(Self::A),
            Port::DpB | Port::HdmiB => Ok(Self::B),
            Port::DpC | Port::HdmiC => Ok(Self::C),
            Port::DpD => Ok(Self::D),
            Port::Lvds | Port::Vga => Err(GmaError::UnsupportedPort),
        }
    }
}

/// DDI register offsets used by the shared HSW/SKL-style connector model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdiRegisters {
    /// DDI buffer control register.
    pub buf_ctl: usize,
    /// DP transport control register.
    pub dp_tp_ctl: usize,
    /// DP transport status register, when the port has one.
    pub dp_tp_status: Option<usize>,
    /// Port clock select register.
    pub port_clk_sel: usize,
}

impl DdiRegisters {
    /// Return HSW/SKL-style shared DDI register offsets.
    pub const fn for_port(port: DdiPort) -> Self {
        match port {
            DdiPort::A => Self {
                buf_ctl: 0x64000,
                dp_tp_ctl: 0x64040,
                dp_tp_status: None,
                port_clk_sel: 0x46100,
            },
            DdiPort::B => Self {
                buf_ctl: 0x64100,
                dp_tp_ctl: 0x64140,
                dp_tp_status: Some(0x64144),
                port_clk_sel: 0x46104,
            },
            DdiPort::C => Self {
                buf_ctl: 0x64200,
                dp_tp_ctl: 0x64240,
                dp_tp_status: Some(0x64244),
                port_clk_sel: 0x46108,
            },
            DdiPort::D => Self {
                buf_ctl: 0x64300,
                dp_tp_ctl: 0x64340,
                dp_tp_status: Some(0x64344),
                port_clk_sel: 0x4610c,
            },
            DdiPort::E => Self {
                buf_ctl: 0x64400,
                dp_tp_ctl: 0x64440,
                dp_tp_status: Some(0x64444),
                port_clk_sel: 0x46110,
            },
        }
    }
}

/// DP lane count for DDI buffer/transport planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiLaneCount {
    /// One lane.
    One,
    /// Two lanes.
    Two,
    /// Four lanes.
    Four,
}

impl DdiLaneCount {
    /// Convert a numeric lane count into a typed value.
    pub const fn new(lanes: u8) -> Result<Self, GmaError> {
        match lanes {
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            4 => Ok(Self::Four),
            _ => Err(GmaError::InvalidConfig),
        }
    }

    const fn buf_ctl_bits(self) -> u32 {
        match self {
            Self::One => DDI_BUF_CTL_PORT_WIDTH_1_LANE,
            Self::Two => DDI_BUF_CTL_PORT_WIDTH_2_LANES,
            Self::Four => DDI_BUF_CTL_PORT_WIDTH_4_LANES,
        }
    }
}

/// Transcoder/pipe selector used by DDI buffer control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiTranscoder {
    /// Transcoder A.
    A,
    /// Transcoder B.
    B,
    /// Transcoder C.
    C,
    /// Embedded DisplayPort transcoder.
    Edp,
}

impl DdiTranscoder {
    /// Return a basic transcoder mapping for a pipe.
    pub const fn from_pipe(pipe: Pipe) -> Self {
        match pipe {
            Pipe::A => Self::A,
            Pipe::B => Self::B,
            Pipe::C => Self::C,
        }
    }

    const fn select_index(self) -> u32 {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::C => 2,
            Self::Edp => 0xf,
        }
    }
}

/// DP link-training pattern used by DP_TP_CTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpTrainingPattern {
    /// Training pattern 1 with scrambling disabled.
    Pattern1,
    /// Training pattern 2 with scrambling disabled.
    Pattern2,
    /// Training pattern 3 with scrambling disabled.
    Pattern3,
    /// Idle pattern.
    Idle,
    /// Normal/no training pattern.
    None,
}

/// Haswell/Broadwell DDI buffer translation table kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HswDdiBufferTableKind {
    /// Haswell DP/eDP digital table.
    HaswellDp,
    /// Haswell FDI table for DDI E.
    HaswellFdi,
    /// Broadwell eDP low-voltage-swing table.
    BroadwellEdpLowVswing,
    /// Broadwell DP/eDP/HDMI digital table.
    BroadwellDp,
    /// Broadwell FDI table for DDI E.
    BroadwellFdi,
}

/// One DDI buffer translation table: 10 pairs, matching libgfxinit's `Buf_Trans_Array`.
pub type DdiBufferTranslations = [u32; 20];

const HASWELL_TRANS_DP: DdiBufferTranslations = [
    0x00ff_ffff,
    0x0006_000e,
    0x00d7_5fff,
    0x0005_000a,
    0x00c3_0fff,
    0x0004_0006,
    0x80aa_afff,
    0x000b_0000,
    0x00ff_ffff,
    0x0005_000a,
    0x00d7_5fff,
    0x000c_0004,
    0x80c3_0fff,
    0x000b_0000,
    0x00ff_ffff,
    0x0004_0006,
    0x80d7_5fff,
    0x000b_0000,
    0,
    0,
];

const HASWELL_TRANS_FDI: DdiBufferTranslations = [
    0x00ff_ffff,
    0x0007_000e,
    0x00d7_5fff,
    0x000f_000a,
    0x00c3_0fff,
    0x0006_0006,
    0x00aa_afff,
    0x001e_0000,
    0x00ff_ffff,
    0x000f_000a,
    0x00d7_5fff,
    0x0016_0004,
    0x00c3_0fff,
    0x001e_0000,
    0x00ff_ffff,
    0x0006_0006,
    0x00d7_5fff,
    0x001e_0000,
    0,
    0,
];

const BROADWELL_TRANS_EDP: DdiBufferTranslations = [
    0x00ff_ffff,
    0x0000_0012,
    0x00eb_afff,
    0x0002_0011,
    0x00c7_1fff,
    0x0006_000f,
    0x00aa_afff,
    0x000e_000a,
    0x00ff_ffff,
    0x0002_0011,
    0x00db_6fff,
    0x0005_000f,
    0x00be_efff,
    0x000a_000c,
    0x00ff_ffff,
    0x0005_000f,
    0x00db_6fff,
    0x000a_000c,
    0,
    0,
];

const BROADWELL_TRANS_DP: DdiBufferTranslations = [
    0x00ff_ffff,
    0x0007_000e,
    0x00d7_5fff,
    0x000e_000a,
    0x00be_ffff,
    0x0014_0006,
    0x80b2_cfff,
    0x001b_0002,
    0x00ff_ffff,
    0x000e_000a,
    0x00db_6fff,
    0x0016_0005,
    0x80c7_1fff,
    0x001a_0002,
    0x00f7_dfff,
    0x0018_0004,
    0x80d7_5fff,
    0x001b_0002,
    0,
    0,
];

const BROADWELL_TRANS_FDI: DdiBufferTranslations = [
    0x00ff_ffff,
    0x0001_000e,
    0x00d7_5fff,
    0x0004_000a,
    0x00c3_0fff,
    0x0007_0006,
    0x00aa_afff,
    0x000c_0000,
    0x00ff_ffff,
    0x0004_000a,
    0x00d7_5fff,
    0x0009_0004,
    0x00c3_0fff,
    0x000c_0000,
    0x00ff_ffff,
    0x0007_0006,
    0x00d7_5fff,
    0x000c_0000,
    0,
    0,
];

const HASWELL_TRANS_HDMI: [(u32, u32); 12] = [
    (0x00ff_ffff, 0x0006_000e),
    (0x00e7_9fff, 0x000e_000c),
    (0x00d7_5fff, 0x0005_000a),
    (0x00ff_ffff, 0x0005_000a),
    (0x00e7_9fff, 0x001d_0007),
    (0x00d7_5fff, 0x000c_0004),
    (0x00ff_ffff, 0x0004_0006),
    (0x80e7_9fff, 0x0003_0002),
    (0x00ff_ffff, 0x0014_0005),
    (0x00ff_ffff, 0x000c_0004),
    (0x00ff_ffff, 0x001c_0003),
    (0x80ff_ffff, 0x0003_0002),
];

const BROADWELL_TRANS_HDMI: [(u32, u32); 10] = [
    (0x00ff_ffff, 0x0007_000e),
    (0x00d7_5fff, 0x000e_000a),
    (0x00be_ffff, 0x0014_0006),
    (0x00ff_ffff, 0x0009_000d),
    (0x00ff_ffff, 0x000e_000a),
    (0x00d7_ffff, 0x0014_0006),
    (0x80cb_2fff, 0x001b_0002),
    (0x00ff_ffff, 0x0014_0006),
    (0x80e7_9fff, 0x001b_0002),
    (0x80ff_ffff, 0x001b_0002),
];

/// Select the Haswell/Broadwell DDI translation table kind like libgfxinit.
pub const fn hsw_ddi_buffer_table_kind(
    cpu: Cpu,
    ddi: DdiPort,
    edp_low_voltage_swing: bool,
) -> Result<HswDdiBufferTableKind, GmaError> {
    match cpu {
        Cpu::Haswell => match ddi {
            DdiPort::A | DdiPort::B | DdiPort::C | DdiPort::D => {
                Ok(HswDdiBufferTableKind::HaswellDp)
            }
            DdiPort::E => Ok(HswDdiBufferTableKind::HaswellFdi),
        },
        Cpu::Broadwell => match ddi {
            DdiPort::A if edp_low_voltage_swing => Ok(HswDdiBufferTableKind::BroadwellEdpLowVswing),
            DdiPort::A | DdiPort::B | DdiPort::C | DdiPort::D => {
                Ok(HswDdiBufferTableKind::BroadwellDp)
            }
            DdiPort::E => Ok(HswDdiBufferTableKind::BroadwellFdi),
        },
        _ => Err(GmaError::UnsupportedPlatform),
    }
}

/// Return DDI buffer translations with HDMI entries 18/19 patched like libgfxinit.
pub fn hsw_ddi_buffer_translations(
    cpu: Cpu,
    ddi: DdiPort,
    edp_low_voltage_swing: bool,
    hdmi_translation: u8,
) -> Result<DdiBufferTranslations, GmaError> {
    let kind = hsw_ddi_buffer_table_kind(cpu, ddi, edp_low_voltage_swing)?;
    let mut table = match kind {
        HswDdiBufferTableKind::HaswellDp => HASWELL_TRANS_DP,
        HswDdiBufferTableKind::HaswellFdi => HASWELL_TRANS_FDI,
        HswDdiBufferTableKind::BroadwellEdpLowVswing => BROADWELL_TRANS_EDP,
        HswDdiBufferTableKind::BroadwellDp => BROADWELL_TRANS_DP,
        HswDdiBufferTableKind::BroadwellFdi => BROADWELL_TRANS_FDI,
    };
    let hdmi = match cpu {
        Cpu::Haswell => HASWELL_TRANS_HDMI
            .get(hdmi_translation as usize)
            .copied()
            .ok_or(GmaError::InvalidConfig)?,
        Cpu::Broadwell => BROADWELL_TRANS_HDMI
            .get(hdmi_translation as usize)
            .copied()
            .ok_or(GmaError::InvalidConfig)?,
        _ => return Err(GmaError::UnsupportedPlatform),
    };
    table[18] = hdmi.0;
    table[19] = hdmi.1;
    Ok(table)
}

/// Skylake/Kabylake configurable DPLL selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SklDpll {
    /// DPLL1, backed by LCPLL2_CTL.
    Dpll1,
    /// DPLL2, backed by WRPLL_CTL_1.
    Dpll2,
    /// DPLL3, backed by WRPLL_CTL_2.
    Dpll3,
}

/// Register offsets used by Skylake/Kabylake configurable DPLLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklDpllRegisters {
    /// PLL enable/control register.
    pub ctl: usize,
    /// DPLL CFGR1 register.
    pub cfgr1: usize,
    /// DPLL CFGR2 register.
    pub cfgr2: usize,
    /// Shift for this DPLL's field in DPLL_CTRL1.
    pub ctrl1_shift: u32,
    /// Shift for this DPLL's lock bit in DPLL_STATUS.
    pub status_shift: u32,
}

impl SklDpll {
    /// Return register offsets and shifts matching libgfxinit's DPLL tables.
    pub const fn registers(self) -> SklDpllRegisters {
        match self {
            Self::Dpll1 => SklDpllRegisters {
                ctl: 0x46014,
                cfgr1: 0x6c040,
                cfgr2: 0x6c044,
                ctrl1_shift: 6,
                status_shift: 8,
            },
            Self::Dpll2 => SklDpllRegisters {
                ctl: 0x46040,
                cfgr1: 0x6c048,
                cfgr2: 0x6c04c,
                ctrl1_shift: 12,
                status_shift: 16,
            },
            Self::Dpll3 => SklDpllRegisters {
                ctl: 0x46060,
                cfgr1: 0x6c050,
                cfgr2: 0x6c054,
                ctrl1_shift: 18,
                status_shift: 24,
            },
        }
    }
}

/// Skylake/Kabylake DCO central frequency bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SklCentralFrequency {
    /// 9.6 GHz central frequency.
    Cf9600,
    /// 9.0 GHz central frequency.
    Cf9000,
    /// 8.4 GHz central frequency.
    Cf8400,
}

impl SklCentralFrequency {
    const fn hz(self) -> u64 {
        match self {
            Self::Cf9600 => 9_600_000_000,
            Self::Cf9000 => 9_000_000_000,
            Self::Cf8400 => 8_400_000_000,
        }
    }

    const fn cfgr2_bits(self) -> u32 {
        match self {
            Self::Cf9600 => 0,
            Self::Cf9000 => 1,
            Self::Cf8400 => 3,
        }
    }
}

/// Pure Skylake/Kabylake HDMI DPLL divider plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklDpllPlan {
    /// Selected central frequency bucket.
    pub central_frequency: SklCentralFrequency,
    /// DCO frequency in Hz.
    pub dco_hz: u64,
    /// P divider.
    pub pdiv: u32,
    /// Q divider.
    pub qdiv: u32,
    /// K divider.
    pub kdiv: u32,
}

impl SklDpllPlan {
    /// Encode the DPLL_CFGR1 value used by libgfxinit for HDMI/DVI modes.
    pub const fn encode_cfgr1(self) -> u32 {
        (1 << 31)
            | ((((self.dco_hz * (1 << 15)) / 24_000_000) as u32 & 0x7fff) << 9)
            | ((self.dco_hz / 24_000_000) as u32 & 0x01ff)
    }

    /// Encode the DPLL_CFGR2 value used by libgfxinit for HDMI/DVI modes.
    pub const fn encode_cfgr2(self) -> u32 {
        skl_qdiv_bits(self.qdiv)
            | skl_kdiv_bits(self.kdiv)
            | skl_pdiv_bits(self.pdiv)
            | self.central_frequency.cfgr2_bits()
    }
}

const SKL_DPLL_CTRL1: usize = 0x6c058;
/// Skylake/Kabylake DPLL_CTRL2 register offset.
pub const SKL_DPLL_CTRL2: usize = 0x6c05c;
const SKL_DPLL_STATUS: usize = 0x6c060;
const SKL_DPLL_CTL_PLL_ENABLE: u32 = SKL_DPLL_CTL_REG::PLL_ENABLE::SET.value;
const SKL_DPLL_CTRL1_HDMI_MODE: u32 = SKL_DPLL_CTRL1_REG::HDMI_MODE::SET.value;
const SKL_DPLL_CTRL1_SSC: u32 = SKL_DPLL_CTRL1_REG::SSC::SET.value;
const SKL_DPLL_CTRL1_LINK_RATE_MASK: u32 = SKL_DPLL_CTRL1_REG::LINK_RATE.val(0x7).value;
const SKL_DPLL_CTRL1_LINK_RATE_2700: u32 = SKL_DPLL_CTRL1_REG::LINK_RATE::Rate2700.value;
const SKL_DPLL_CTRL1_LINK_RATE_1350: u32 = SKL_DPLL_CTRL1_REG::LINK_RATE::Rate1350.value;
const SKL_DPLL_CTRL1_LINK_RATE_810: u32 = SKL_DPLL_CTRL1_REG::LINK_RATE::Rate810.value;
const SKL_DPLL_CTRL1_OVERRIDE: u32 = SKL_DPLL_CTRL1_REG::OVERRIDE::SET.value;

const SKL_EVEN_DIVS: [u32; 36] = [
    4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 28, 30, 32, 36, 40, 42, 44, 48, 52, 54, 56, 60, 64, 66,
    68, 70, 72, 76, 78, 80, 84, 88, 90, 92, 96, 98,
];
const SKL_ODD_DIVS: [u32; 7] = [3, 5, 7, 9, 15, 21, 35];

/// Calculate Skylake/Kabylake HDMI DPLL dividers from a dotclock in Hz.
pub fn calculate_skl_hdmi_dpll(dotclock_hz: u64) -> Result<SklDpllPlan, GmaError> {
    if dotclock_hz == 0 {
        return Err(GmaError::InvalidConfig);
    }
    let mut selected: Option<(SklCentralFrequency, u64, u32)> = None;
    for divs in [&SKL_EVEN_DIVS[..], &SKL_ODD_DIVS[..]] {
        for cf in [
            SklCentralFrequency::Cf9600,
            SklCentralFrequency::Cf9000,
            SklCentralFrequency::Cf8400,
        ] {
            let central = cf.hz();
            for div in divs.iter().copied() {
                let temp = u64::from(div) * 5 * dotclock_hz;
                let deviation = temp.abs_diff(central);
                let allowed = if temp > central {
                    central / 100
                } else {
                    6 * central / 100
                };
                if deviation < allowed
                    && selected
                        .map(|(_, best, _)| deviation < best)
                        .unwrap_or(true)
                {
                    selected = Some((cf, deviation, div));
                }
            }
        }
        if selected.is_some() {
            break;
        }
    }
    let (central_frequency, _, div) = selected.ok_or(GmaError::PllNoSolution)?;
    let (pdiv, qdiv, kdiv) = skl_decompose_divider(div)?;
    Ok(SklDpllPlan {
        central_frequency,
        dco_hz: u64::from(div) * 5 * dotclock_hz,
        pdiv,
        qdiv,
        kdiv,
    })
}

fn skl_decompose_divider(mut div: u32) -> Result<(u32, u32, u32), GmaError> {
    if div.is_multiple_of(2) {
        div /= 2;
        if matches!(div, 1 | 3 | 5) {
            Ok((2, 1, div))
        } else if div.is_multiple_of(2) {
            Ok((2, div / 2, 2))
        } else if div.is_multiple_of(3) {
            Ok((3, div / 3, 2))
        } else if div.is_multiple_of(7) {
            Ok((7, div / 7, 2))
        } else {
            Err(GmaError::PllNoSolution)
        }
    } else if matches!(div, 7 | 21 | 35) {
        Ok((7, 1, div / 7))
    } else if matches!(div, 3 | 9 | 15) {
        Ok((3, 1, div / 3))
    } else if div == 5 {
        Ok((5, 1, 1))
    } else {
        Err(GmaError::PllNoSolution)
    }
}

const fn skl_pdiv_bits(pdiv: u32) -> u32 {
    let encoded = match pdiv {
        1 => 0,
        2 => 1,
        3 => 2,
        7 => 4,
        _ => 4,
    };
    encoded << 2
}

const fn skl_qdiv_bits(qdiv: u32) -> u32 {
    (qdiv << 8) | if qdiv != 1 { 1 << 7 } else { 0 }
}

const fn skl_kdiv_bits(kdiv: u32) -> u32 {
    let encoded = match kdiv {
        5 => 0,
        2 => 1,
        3 => 2,
        1 => 3,
        _ => 0,
    };
    encoded << 5
}

/// Return DPLL_CTRL1 update masks for Skylake/Kabylake HDMI mode on a configurable DPLL.
pub const fn skl_hdmi_dpll_ctrl1_update(pll: SklDpll) -> (usize, u32, u32) {
    let shift = pll.registers().ctrl1_shift;
    (
        SKL_DPLL_CTRL1,
        SKL_DPLL_CTRL1_SSC << shift,
        (SKL_DPLL_CTRL1_HDMI_MODE | SKL_DPLL_CTRL1_OVERRIDE) << shift,
    )
}

/// Return DPLL_CTRL1 update masks for Skylake/Kabylake DP mode on a configurable DPLL.
pub const fn skl_dp_dpll_ctrl1_update(
    pll: SklDpll,
    clock: DdiClockSelect,
) -> Result<(usize, u32, u32), GmaError> {
    let link_rate = match clock {
        DdiClockSelect::Lcpll2700 => SKL_DPLL_CTRL1_LINK_RATE_2700,
        DdiClockSelect::Lcpll1350 => SKL_DPLL_CTRL1_LINK_RATE_1350,
        DdiClockSelect::Lcpll810 => SKL_DPLL_CTRL1_LINK_RATE_810,
        _ => return Err(GmaError::InvalidConfig),
    };
    let shift = pll.registers().ctrl1_shift;
    Ok((
        SKL_DPLL_CTRL1,
        (SKL_DPLL_CTRL1_HDMI_MODE | SKL_DPLL_CTRL1_SSC | SKL_DPLL_CTRL1_LINK_RATE_MASK) << shift,
        (link_rate | SKL_DPLL_CTRL1_OVERRIDE) << shift,
    ))
}

/// Return the PLL enable register write and DPLL_STATUS lock mask for a Skylake/Kabylake DPLL.
pub const fn skl_dpll_enable_ops(pll: SklDpll) -> (usize, u32, usize, u32) {
    let regs = pll.registers();
    (
        regs.ctl,
        SKL_DPLL_CTL_PLL_ENABLE,
        SKL_DPLL_STATUS,
        1 << regs.status_shift,
    )
}

/// Generic register operation used by DDI plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdiRegisterOp {
    /// Register offset.
    pub register: usize,
    /// Bits to clear before setting `mask_set`.
    pub mask_unset: u32,
    /// Bits to set after clearing `mask_unset`.
    pub mask_set: u32,
}

/// DDI port clock source selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiClockSelect {
    /// LCPLL 2700 MHz source.
    Lcpll2700,
    /// LCPLL 1350 MHz source.
    Lcpll1350,
    /// LCPLL 810 MHz source.
    Lcpll810,
    /// SPLL source.
    Spll,
    /// WRPLL1 source.
    Wrpll1,
    /// WRPLL2 source.
    Wrpll2,
    /// No clock routed to the port.
    None,
}

impl DdiClockSelect {
    /// Encode a Haswell/Broadwell PORT_CLK_SEL value.
    pub const fn encode(self) -> u32 {
        match self {
            Self::Lcpll2700 => PORT_CLK_SEL_REG::CLOCK_SELECT.val(0).value,
            Self::Lcpll1350 => PORT_CLK_SEL_REG::CLOCK_SELECT.val(1).value,
            Self::Lcpll810 => PORT_CLK_SEL_REG::CLOCK_SELECT.val(2).value,
            Self::Spll => PORT_CLK_SEL_REG::CLOCK_SELECT.val(3).value,
            Self::Wrpll1 => PORT_CLK_SEL_REG::CLOCK_SELECT.val(4).value,
            Self::Wrpll2 => PORT_CLK_SEL_REG::CLOCK_SELECT.val(5).value,
            Self::None => PORT_CLK_SEL_REG::CLOCK_SELECT.val(7).value,
        }
    }
}

/// Haswell/Broadwell PLL selected for a DDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HswPllSelect {
    /// LCPLL0, 2700 MHz.
    Lcpll0,
    /// LCPLL1, 1350 MHz.
    Lcpll1,
    /// LCPLL2, 810 MHz.
    Lcpll2,
    /// SPLL.
    Spll,
    /// WRPLL0.
    Wrpll0,
    /// WRPLL1.
    Wrpll1,
}

impl HswPllSelect {
    /// Return libgfxinit's `PLLs.Register_Value` hint for the selected PLL.
    pub const fn register_value(self) -> u32 {
        match self {
            Self::Lcpll0 => DdiClockSelect::Lcpll2700.encode(),
            Self::Lcpll1 => DdiClockSelect::Lcpll1350.encode(),
            Self::Lcpll2 => DdiClockSelect::Lcpll810.encode(),
            Self::Spll => DdiClockSelect::Spll.encode(),
            Self::Wrpll0 => DdiClockSelect::Wrpll1.encode(),
            Self::Wrpll1 => DdiClockSelect::Wrpll2.encode(),
        }
    }

    /// Return the matching DDI clock selector for older helpers.
    pub const fn clock_select(self) -> DdiClockSelect {
        match self {
            Self::Lcpll0 => DdiClockSelect::Lcpll2700,
            Self::Lcpll1 => DdiClockSelect::Lcpll1350,
            Self::Lcpll2 => DdiClockSelect::Lcpll810,
            Self::Spll => DdiClockSelect::Spll,
            Self::Wrpll0 => DdiClockSelect::Wrpll1,
            Self::Wrpll1 => DdiClockSelect::Wrpll2,
        }
    }
}

/// Skylake/Kabylake PLL selected for a DDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SklPllSelect {
    /// Fixed DPLL0, only valid for DDI A DP/eDP rates accepted by DPLL0.
    Dpll0,
    /// Configurable DPLL1.
    Dpll1,
    /// Configurable DPLL2.
    Dpll2,
    /// Configurable DPLL3.
    Dpll3,
}

impl SklPllSelect {
    /// Return libgfxinit's `PLLs.Register_Value` hint for PORT_CLK_SEL.
    pub const fn register_value(self) -> u32 {
        match self {
            Self::Dpll0 => 0,
            Self::Dpll1 => 1,
            Self::Dpll2 => 2,
            Self::Dpll3 => 3,
        }
    }

    /// Return the configurable DPLL enum when this is not DPLL0.
    pub const fn configurable(self) -> Option<SklDpll> {
        match self {
            Self::Dpll0 => None,
            Self::Dpll1 => Some(SklDpll::Dpll1),
            Self::Dpll2 => Some(SklDpll::Dpll2),
            Self::Dpll3 => Some(SklDpll::Dpll3),
        }
    }
}

/// Pure Haswell WRPLL divider plan, matching libgfxinit's WRPLL R2/N2/P search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswWrpllPlan {
    /// Reference divider encoded as twice R.
    pub r2: u32,
    /// Feedback divider encoded as twice N.
    pub n2: u32,
    /// Post divider.
    pub p: u32,
}

impl HswWrpllPlan {
    /// Encode the WRPLL_CTL value programmed by libgfxinit.
    pub const fn encode_ctl(self) -> u32 {
        (1 << 31) | (3 << 28) | (self.n2 << 16) | (self.p << 8) | self.r2
    }
}

const WRPLL_LC_FREQ_MHZ: u64 = 2700;
const WRPLL_LC_FREQ_2K: u64 = WRPLL_LC_FREQ_MHZ * 2000;
const WRPLL_P_MIN: u32 = 2;
const WRPLL_P_MAX: u32 = 62;
const WRPLL_REF_MIN_MHZ: u64 = 48;
const WRPLL_REF_MAX_MHZ: u64 = 400;
const WRPLL_VCO_MIN_MHZ: u64 = 2400;
const WRPLL_VCO_MAX_MHZ: u64 = 4800;

/// Return libgfxinit's Haswell WRPLL PPM budget for a pixel clock in Hz.
pub const fn hsw_wrpll_budget_ppm(clock_hz: u64) -> u64 {
    match clock_hz {
        25_175_000 | 25_200_000 | 27_000_000 | 27_027_000 | 37_762_500 | 37_800_000
        | 40_500_000 | 40_541_000 | 54_000_000 | 54_054_000 | 59_341_000 | 59_400_000
        | 72_000_000 | 74_176_000 | 74_250_000 | 81_000_000 | 81_081_000 | 89_012_000
        | 89_100_000 | 108_000_000 | 108_108_000 | 111_264_000 | 111_375_000 | 148_352_000
        | 148_500_000 | 162_000_000 | 162_162_000 | 222_525_000 | 222_750_000 | 296_703_000
        | 297_000_000 => 0,
        233_500_000 | 245_250_000 | 247_750_000 | 253_250_000 | 298_000_000 => 1500,
        169_128_000 | 169_500_000 | 179_500_000 | 202_000_000 => 2000,
        256_250_000 | 262_500_000 | 270_000_000 | 272_500_000 | 273_750_000 | 280_750_000
        | 281_250_000 | 286_000_000 | 291_750_000 => 4000,
        267_250_000 | 268_500_000 => 5000,
        _ => 1000,
    }
}

/// Calculate Haswell WRPLL dividers for a pixel clock in Hz.
pub fn calculate_hsw_wrpll(clock_hz: u64) -> Result<HswWrpllPlan, GmaError> {
    if clock_hz == 540_000_000 {
        return Ok(HswWrpllPlan { r2: 2, n2: 2, p: 1 });
    }
    let freq_2k = clock_hz / 100;
    if freq_2k == 0 {
        return Err(GmaError::InvalidConfig);
    }
    let budget = hsw_wrpll_budget_ppm(clock_hz);
    let mut best: Option<HswWrpllPlan> = None;
    for r2 in (WRPLL_LC_FREQ_MHZ * 2 / WRPLL_REF_MAX_MHZ + 1)
        ..=(WRPLL_LC_FREQ_MHZ * 2 / WRPLL_REF_MIN_MHZ)
    {
        let n2_min = WRPLL_VCO_MIN_MHZ * r2 / WRPLL_LC_FREQ_MHZ + 1;
        let n2_max = WRPLL_VCO_MAX_MHZ * r2 / WRPLL_LC_FREQ_MHZ;
        for n2 in n2_min..=n2_max {
            let mut p = WRPLL_P_MIN;
            while p <= WRPLL_P_MAX {
                update_hsw_wrpll(
                    freq_2k,
                    budget,
                    HswWrpllPlan {
                        r2: r2 as u32,
                        n2: n2 as u32,
                        p,
                    },
                    &mut best,
                );
                p += 2;
            }
        }
    }
    best.ok_or(GmaError::PllNoSolution)
}

fn update_hsw_wrpll(
    freq_2k: u64,
    budget: u64,
    candidate: HswWrpllPlan,
    best: &mut Option<HswWrpllPlan>,
) {
    let Some(current) = *best else {
        *best = Some(candidate);
        return;
    };
    let c = candidate;
    let b = current;
    let c_diff =
        (freq_2k * u64::from(c.p) * u64::from(c.r2)).abs_diff(WRPLL_LC_FREQ_2K * u64::from(c.n2));
    let b_diff =
        (freq_2k * u64::from(b.p) * u64::from(b.r2)).abs_diff(WRPLL_LC_FREQ_2K * u64::from(b.n2));
    let c_budget = freq_2k * budget * u64::from(c.p) * u64::from(c.r2);
    let b_budget = freq_2k * budget * u64::from(b.p) * u64::from(b.r2);
    let c_scaled = 1_000_000 * c_diff;
    let b_scaled = 1_000_000 * b_diff;

    let replace = if c_budget < c_scaled && b_budget < b_scaled {
        u64::from(b.p) * u64::from(b.r2) * c_diff < u64::from(c.p) * u64::from(c.r2) * b_diff
    } else if c_budget >= c_scaled && b_budget < b_scaled {
        true
    } else if c_budget >= c_scaled && b_budget >= b_scaled {
        u64::from(c.n2) * u64::from(b.r2) * u64::from(b.r2)
            > u64::from(b.n2) * u64::from(c.r2) * u64::from(c.r2)
    } else {
        false
    };
    if replace {
        *best = Some(candidate);
    }
}

const DDI_BUF_CTL_BUFFER_ENABLE: u32 = DDI_BUF_CTL_REG::BUFFER_ENABLE::SET.value;
const DDI_BUF_CTL_TRANS_SELECT_SHIFT: u32 = 24;
const DDI_BUF_CTL_PORT_REVERSAL: u32 = DDI_BUF_CTL_REG::PORT_REVERSAL::SET.value;
const DDI_BUF_CTL_DDI_A_LANE_CAP: u32 = DDI_BUF_CTL_REG::DDI_A_LANE_CAP::SET.value;
const DDI_BUF_CTL_PORT_WIDTH_1_LANE: u32 = DDI_BUF_CTL_REG::PORT_WIDTH::OneLane.value;
const DDI_BUF_CTL_PORT_WIDTH_2_LANES: u32 = DDI_BUF_CTL_REG::PORT_WIDTH::TwoLanes.value;
const DDI_BUF_CTL_PORT_WIDTH_4_LANES: u32 = DDI_BUF_CTL_REG::PORT_WIDTH::FourLanes.value;
const DDI_BUF_CTL_INIT_DISPLAY_DETECT: u32 = DDI_BUF_CTL_REG::INIT_DISPLAY_DETECT::SET.value;

const DP_TP_CTL_TRANSPORT_ENABLE: u32 = DP_TP_CTL_REG::TRANSPORT_ENABLE::SET.value;
const DP_TP_CTL_MODE_SST: u32 = DP_TP_CTL_REG::MODE::Sst.value;
const DP_TP_CTL_MODE_MST: u32 = DP_TP_CTL_REG::MODE::Mst.value;
const DP_TP_CTL_FORCE_ACT: u32 = DP_TP_CTL_REG::FORCE_ACT::SET.value;
const DP_TP_CTL_ENHANCED_FRAME_ENABLE: u32 = DP_TP_CTL_REG::ENHANCED_FRAME_ENABLE::SET.value;
const DP_TP_CTL_FDI_AUTOTRAIN: u32 = DP_TP_CTL_REG::FDI_AUTOTRAIN::SET.value;
const DP_TP_CTL_LINK_TRAIN_PAT1: u32 = DP_TP_CTL_REG::LINK_TRAIN::Pattern1.value;
const DP_TP_CTL_LINK_TRAIN_PAT2: u32 = DP_TP_CTL_REG::LINK_TRAIN::Pattern2.value;
const DP_TP_CTL_LINK_TRAIN_IDLE: u32 = DP_TP_CTL_REG::LINK_TRAIN::Idle.value;
const DP_TP_CTL_LINK_TRAIN_NORMAL: u32 = DP_TP_CTL_REG::LINK_TRAIN::Normal.value;
const DP_TP_CTL_LINK_TRAIN_PAT3: u32 = DP_TP_CTL_REG::LINK_TRAIN::Pattern3.value;
const DP_TP_CTL_SCRAMBLE_DISABLE: u32 = DP_TP_CTL_REG::SCRAMBLE_DISABLE::SET.value;

/// Build a DDI_BUF_CTL value for a digital port.
pub const fn encode_ddi_buf_ctl(
    transcoder: DdiTranscoder,
    lanes: DdiLaneCount,
    enable: bool,
    port_reversal: bool,
    init_display_detect: bool,
    ddi_a_lane_cap: bool,
) -> u32 {
    bool_bit(enable, DDI_BUF_CTL_BUFFER_ENABLE)
        | (transcoder.select_index() << DDI_BUF_CTL_TRANS_SELECT_SHIFT)
        | bool_bit(port_reversal, DDI_BUF_CTL_PORT_REVERSAL)
        | bool_bit(ddi_a_lane_cap, DDI_BUF_CTL_DDI_A_LANE_CAP)
        | lanes.buf_ctl_bits()
        | bool_bit(init_display_detect, DDI_BUF_CTL_INIT_DISPLAY_DETECT)
}

/// Build a DP_TP_CTL value for DP/eDP transport.
pub const fn encode_dp_tp_ctl(
    enable: bool,
    mst: bool,
    force_act: bool,
    enhanced_framing: bool,
    fdi_autotrain: bool,
    pattern: DpTrainingPattern,
) -> u32 {
    bool_bit(enable, DP_TP_CTL_TRANSPORT_ENABLE)
        | if mst {
            DP_TP_CTL_MODE_MST
        } else {
            DP_TP_CTL_MODE_SST
        }
        | bool_bit(force_act, DP_TP_CTL_FORCE_ACT)
        | bool_bit(enhanced_framing, DP_TP_CTL_ENHANCED_FRAME_ENABLE)
        | bool_bit(fdi_autotrain, DP_TP_CTL_FDI_AUTOTRAIN)
        | encode_training_pattern(pattern)
}

/// Encode libgfxinit's DP_TP_CTL link-training pattern mapping.
pub const fn encode_training_pattern(pattern: DpTrainingPattern) -> u32 {
    match pattern {
        DpTrainingPattern::Pattern1 => DP_TP_CTL_LINK_TRAIN_PAT1 | DP_TP_CTL_SCRAMBLE_DISABLE,
        DpTrainingPattern::Pattern2 => DP_TP_CTL_LINK_TRAIN_PAT2 | DP_TP_CTL_SCRAMBLE_DISABLE,
        DpTrainingPattern::Pattern3 => DP_TP_CTL_LINK_TRAIN_PAT3 | DP_TP_CTL_SCRAMBLE_DISABLE,
        DpTrainingPattern::Idle => DP_TP_CTL_LINK_TRAIN_IDLE,
        DpTrainingPattern::None => DP_TP_CTL_LINK_TRAIN_NORMAL,
    }
}

const fn bool_bit(condition: bool, bit: u32) -> u32 {
    if condition { bit } else { 0 }
}

/// DDI clock-routing mechanism used by a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiClockRouting {
    /// Per-DDI PORT_CLK_SEL register write used by Haswell/Broadwell and Skylake/Kabylake.
    PortClkSel {
        /// Register write operation.
        op: DdiRegisterOp,
    },
    /// Shared DPLL_CTRL2 update used by Haswell/Broadwell variants without per-DDI clock select.
    DpllCtrl2 {
        /// Register update operation.
        op: DdiRegisterOp,
    },
}

impl DdiClockRouting {
    /// Build libgfxinit's per-DDI PORT_CLK_SEL write.
    pub const fn port_clk_sel(ddi: DdiPort, pll_hint: u32) -> Self {
        Self::PortClkSel {
            op: DdiRegisterOp {
                register: DdiRegisters::for_port(ddi).port_clk_sel,
                mask_unset: u32::MAX,
                mask_set: pll_hint,
            },
        }
    }

    /// Build libgfxinit's DPLL_CTRL2 route/override update for older DDI routing.
    pub const fn dpll_ctrl2(ddi: DdiPort, pll_hint: u32) -> Result<Self, GmaError> {
        match dpll_ctrl2_fields(ddi) {
            Ok(fields) => Ok(Self::DpllCtrl2 {
                op: DdiRegisterOp {
                    register: SKL_DPLL_CTRL2,
                    mask_unset: fields.clock_off | fields.select_mask,
                    mask_set: (pll_hint << fields.select_shift) | fields.select_override,
                },
            }),
            Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DpllCtrl2Fields {
    clock_off: u32,
    select_mask: u32,
    select_shift: u32,
    select_override: u32,
}

const fn dpll_ctrl2_fields(ddi: DdiPort) -> Result<DpllCtrl2Fields, GmaError> {
    match ddi {
        DdiPort::A => Ok(DpllCtrl2Fields {
            clock_off: SKL_DPLL_CTRL2_REG::DDI_A_CLOCK_OFF::SET.value,
            select_mask: SKL_DPLL_CTRL2_REG::DDI_A_SELECT.val(3).value,
            select_shift: SKL_DPLL_CTRL2_REG::DDI_A_SELECT.shift as u32,
            select_override: SKL_DPLL_CTRL2_REG::DDI_A_SELECT_OVERRIDE::SET.value,
        }),
        DdiPort::B => Ok(DpllCtrl2Fields {
            clock_off: SKL_DPLL_CTRL2_REG::DDI_B_CLOCK_OFF::SET.value,
            select_mask: SKL_DPLL_CTRL2_REG::DDI_B_SELECT.val(3).value,
            select_shift: SKL_DPLL_CTRL2_REG::DDI_B_SELECT.shift as u32,
            select_override: SKL_DPLL_CTRL2_REG::DDI_B_SELECT_OVERRIDE::SET.value,
        }),
        DdiPort::C => Ok(DpllCtrl2Fields {
            clock_off: SKL_DPLL_CTRL2_REG::DDI_C_CLOCK_OFF::SET.value,
            select_mask: SKL_DPLL_CTRL2_REG::DDI_C_SELECT.val(3).value,
            select_shift: SKL_DPLL_CTRL2_REG::DDI_C_SELECT.shift as u32,
            select_override: SKL_DPLL_CTRL2_REG::DDI_C_SELECT_OVERRIDE::SET.value,
        }),
        DdiPort::D => Ok(DpllCtrl2Fields {
            clock_off: SKL_DPLL_CTRL2_REG::DDI_D_CLOCK_OFF::SET.value,
            select_mask: SKL_DPLL_CTRL2_REG::DDI_D_SELECT.val(3).value,
            select_shift: SKL_DPLL_CTRL2_REG::DDI_D_SELECT.shift as u32,
            select_override: SKL_DPLL_CTRL2_REG::DDI_D_SELECT_OVERRIDE::SET.value,
        }),
        DdiPort::E => Ok(DpllCtrl2Fields {
            clock_off: SKL_DPLL_CTRL2_REG::DDI_E_CLOCK_OFF::SET.value,
            select_mask: SKL_DPLL_CTRL2_REG::DDI_E_SELECT.val(3).value,
            select_shift: SKL_DPLL_CTRL2_REG::DDI_E_SELECT.shift as u32,
            select_override: SKL_DPLL_CTRL2_REG::DDI_E_SELECT_OVERRIDE::SET.value,
        }),
    }
}

/// plan for one DDI output path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdiPortPlan {
    /// Logical DDI port.
    pub port: DdiPort,
    /// Register offsets for this port.
    pub regs: DdiRegisters,
    /// Initial buffer-control value.
    pub buf_ctl: u32,
    /// Initial DP transport-control value for DP/eDP paths.
    pub dp_tp_ctl: Option<u32>,
    /// Clock-select value to route to the port.
    pub port_clk_sel: u32,
}

/// DDI output plan including generation-specific PLL/routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedDdiPortPlan<PllPlan> {
    /// DDI connector programming plan.
    pub port: DdiPortPlan,
    /// Selected PLL identifier.
    pub pll: PllPlan,
    /// Clock routing operation required before enabling/training the port.
    pub routing: DdiClockRouting,
}

/// Generic CPU pipe placeholder used by high-level DDI sequence planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdiPipePlaceholder {
    /// Pipe selected for the future CPU pipe timing program.
    pub pipe: Pipe,
    /// Pixel clock that the pipe timing program must consume.
    pub dotclock_hz: u64,
}

/// Generic transcoder placeholder used by high-level DDI sequence planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdiTranscoderPlaceholder {
    /// DDI transcoder selected from the pipe.
    pub transcoder: DdiTranscoder,
}

/// Ordered steps for a Haswell/Broadwell HDMI DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HswHdmiInitStep {
    /// Calculate and program the selected WRPLL.
    ProgramWrpll,
    /// Load DDI buffer translation table entries.
    LoadBufferTranslations,
    /// Route the selected PLL clock to the DDI port.
    RouteDdiClock,
    /// Program generic CPU pipe timings.
    ProgramPipe,
    /// Program generic CPU transcoder state.
    ProgramTranscoder,
    /// Program DDI HDMI buffer/port control.
    ProgramDdiPort,
}

/// Number of high-level steps in the Haswell/Broadwell HDMI DDI sequence plan.
pub const HSW_HDMI_INIT_STEP_COUNT: usize = 6;

/// High-level Haswell/Broadwell HDMI DDI init sequence plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswHdmiInitSequencePlan {
    /// CPU generation this plan targets.
    pub cpu: Cpu,
    /// Routed DDI HDMI port and WRPLL plan.
    pub routed: RoutedDdiPortPlan<HswDdiPllPlan>,
    /// DDI buffer translation values with HDMI entries patched.
    pub buffer_translations: DdiBufferTranslations,
    /// Placeholder for the future generic CPU pipe program.
    pub pipe: DdiPipePlaceholder,
    /// Placeholder for the future generic transcoder program.
    pub transcoder: DdiTranscoderPlaceholder,
    /// Ordered high-level initialization steps.
    pub steps: [HswHdmiInitStep; HSW_HDMI_INIT_STEP_COUNT],
}

/// Ordered steps for a Skylake/Kabylake HDMI DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SklHdmiInitStep {
    /// Calculate and program the selected DPLL.
    ProgramDpll,
    /// Route the selected DPLL clock to the DDI port.
    RouteDdiClock,
    /// Program generic CPU pipe timings.
    ProgramPipe,
    /// Program generic CPU transcoder state.
    ProgramTranscoder,
    /// Program DDI HDMI buffer/port control.
    ProgramDdiPort,
}

/// Number of high-level steps in the Skylake/Kabylake HDMI sequence plan.
pub const SKL_HDMI_INIT_STEP_COUNT: usize = 5;

/// High-level Skylake/Kabylake HDMI DDI init sequence plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklHdmiInitSequencePlan {
    /// CPU generation this plan targets.
    pub cpu: Cpu,
    /// Routed DDI HDMI port and DPLL plan.
    pub routed: RoutedDdiPortPlan<SklDdiPllPlan>,
    /// Placeholder for the generic CPU pipe program.
    pub pipe: DdiPipePlaceholder,
    /// Placeholder for the generic transcoder program.
    pub transcoder: DdiTranscoderPlaceholder,
    /// Ordered high-level initialization steps.
    pub steps: [SklHdmiInitStep; SKL_HDMI_INIT_STEP_COUNT],
}

/// Ordered steps for a DDI DP/eDP init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdiDpInitStep {
    /// Program or select the generation-specific DP PLL.
    ProgramDpPll,
    /// Load DDI buffer translation table entries.
    LoadBufferTranslations,
    /// Route the selected PLL clock to the DDI port.
    RouteDdiClock,
    /// Program generic CPU pipe timings.
    ProgramPipe,
    /// Program generic CPU transcoder state.
    ProgramTranscoder,
    /// Program initial DDI DP buffer/transport control.
    ProgramDdiPort,
    /// Run DisplayPort AUX link training.
    TrainDpLink,
}

/// Number of high-level steps in the DDI DP/eDP sequence plan.
pub const DDI_DP_INIT_STEP_COUNT: usize = 7;

/// High-level Haswell/Broadwell DP/eDP DDI init sequence plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswDpInitSequencePlan {
    /// CPU generation this plan targets.
    pub cpu: Cpu,
    /// Routed DDI DP/eDP port and fixed PLL plan.
    pub routed: RoutedDdiPortPlan<HswDdiPllPlan>,
    /// DDI buffer translation values.
    pub buffer_translations: DdiBufferTranslations,
    /// Placeholder for the future generic CPU pipe program.
    pub pipe: DdiPipePlaceholder,
    /// Placeholder for the future generic transcoder program.
    pub transcoder: DdiTranscoderPlaceholder,
    /// Ordered high-level initialization steps.
    pub steps: [DdiDpInitStep; DDI_DP_INIT_STEP_COUNT],
}

/// High-level Skylake/Kabylake DP/eDP DDI init sequence plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklDpInitSequencePlan {
    /// CPU generation this plan targets.
    pub cpu: Cpu,
    /// Routed DDI DP/eDP port and DPLL plan.
    pub routed: RoutedDdiPortPlan<SklDdiPllPlan>,
    /// Placeholder for the future generic CPU pipe program.
    pub pipe: DdiPipePlaceholder,
    /// Placeholder for the future generic transcoder program.
    pub transcoder: DdiTranscoderPlaceholder,
    /// Ordered high-level initialization steps.
    pub steps: [DdiDpInitStep; DDI_DP_INIT_STEP_COUNT],
}

/// Haswell/Broadwell PLL programming plan for one DDI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HswDdiPllPlan {
    /// DP/eDP fixed LCPLL/SPLL selection.
    Fixed(HswPllSelect),
    /// HDMI WRPLL selection and divider plan.
    Wrpll {
        /// Selected WRPLL.
        pll: HswPllSelect,
        /// Divider programming plan.
        plan: HswWrpllPlan,
    },
}

impl HswDdiPllPlan {
    /// Return the PLL hint consumed by libgfxinit's shared DDI routing code.
    pub const fn register_value(self) -> u32 {
        match self {
            Self::Fixed(pll) => pll.register_value(),
            Self::Wrpll { pll, .. } => pll.register_value(),
        }
    }

    /// Return the legacy `DdiClockSelect` represented by this plan.
    pub const fn clock_select(self) -> DdiClockSelect {
        match self {
            Self::Fixed(pll) => pll.clock_select(),
            Self::Wrpll { pll, .. } => pll.clock_select(),
        }
    }
}

/// Skylake/Kabylake PLL programming plan for one DDI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SklDdiPllPlan {
    /// DPLL0 fixed DP/eDP selection.
    Fixed(SklPllSelect),
    /// Configurable HDMI DPLL selection and divider plan.
    Hdmi {
        /// Selected DPLL.
        pll: SklPllSelect,
        /// Divider programming plan.
        plan: SklDpllPlan,
    },
    /// Configurable DP DPLL selection and link clock.
    Dp {
        /// Selected DPLL.
        pll: SklPllSelect,
        /// DP link clock source/rate selector for DPLL_CTRL1.
        clock: DdiClockSelect,
    },
}

impl SklDdiPllPlan {
    /// Return the PLL hint consumed by libgfxinit's shared DDI routing code.
    pub const fn register_value(self) -> u32 {
        match self {
            Self::Fixed(pll) => pll.register_value(),
            Self::Hdmi { pll, .. } | Self::Dp { pll, .. } => pll.register_value(),
        }
    }
}

impl DdiPortPlan {
    /// Build a HDMI/DVI-style DDI plan.
    pub const fn hdmi(port: Port, pipe: Pipe, clock: DdiClockSelect) -> Result<Self, GmaError> {
        match DdiPort::from_port(port) {
            Ok(ddi) => Ok(Self {
                port: ddi,
                regs: DdiRegisters::for_port(ddi),
                buf_ctl: encode_ddi_buf_ctl(
                    DdiTranscoder::from_pipe(pipe),
                    DdiLaneCount::Four,
                    true,
                    false,
                    false,
                    false,
                ),
                dp_tp_ctl: None,
                port_clk_sel: clock.encode(),
            }),
            Err(err) => Err(err),
        }
    }

    /// Build a DP/eDP DDI plan.
    pub const fn dp(
        port: Port,
        pipe: Pipe,
        lanes: DdiLaneCount,
        clock: DdiClockSelect,
        pattern: DpTrainingPattern,
        enhanced_framing: bool,
    ) -> Result<Self, GmaError> {
        match DdiPort::from_port(port) {
            Ok(ddi) => Ok(Self {
                port: ddi,
                regs: DdiRegisters::for_port(ddi),
                buf_ctl: encode_ddi_buf_ctl(
                    DdiTranscoder::from_pipe(pipe),
                    lanes,
                    true,
                    false,
                    false,
                    ddi as u8 == DdiPort::A as u8,
                ),
                dp_tp_ctl: Some(encode_dp_tp_ctl(
                    true,
                    false,
                    false,
                    enhanced_framing,
                    false,
                    pattern,
                )),
                port_clk_sel: clock.encode(),
            }),
            Err(err) => Err(err),
        }
    }
}

/// Parameters for a Haswell/Broadwell routed HDMI DDI plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswHdmiRouteParams {
    /// Logical HDMI port to route through a DDI encoder.
    pub port: Port,
    /// CPU pipe that feeds the DDI transcoder.
    pub pipe: Pipe,
    /// HDMI pixel clock in hertz.
    pub dotclock_hz: u64,
    /// WRPLL selected for this HDMI stream.
    pub pll: HswPllSelect,
    /// Use per-DDI `PORT_CLK_SEL` routing instead of shared `DPLL_CTRL2` routing.
    pub per_ddi_clock_sel: bool,
}

/// Build a Haswell/Broadwell routed HDMI DDI plan.
pub fn hsw_routed_hdmi_plan(
    params: HswHdmiRouteParams,
) -> Result<RoutedDdiPortPlan<HswDdiPllPlan>, GmaError> {
    let HswHdmiRouteParams {
        port,
        pipe,
        dotclock_hz,
        pll,
        per_ddi_clock_sel,
    } = params;
    if !matches!(pll, HswPllSelect::Wrpll0 | HswPllSelect::Wrpll1) {
        return Err(GmaError::InvalidConfig);
    }
    let plan = HswDdiPllPlan::Wrpll {
        pll,
        plan: calculate_hsw_wrpll(dotclock_hz)?,
    };
    let ddi = DdiPort::from_port(port)?;
    Ok(RoutedDdiPortPlan {
        port: DdiPortPlan::hdmi(port, pipe, plan.clock_select())?,
        pll: plan,
        routing: if per_ddi_clock_sel {
            DdiClockRouting::port_clk_sel(ddi, plan.register_value())
        } else {
            DdiClockRouting::dpll_ctrl2(ddi, plan.register_value())?
        },
    })
}

/// Parameters for a Haswell/Broadwell HDMI DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswHdmiInitParams {
    /// Haswell-family CPU selector.
    pub cpu: Cpu,
    /// Logical HDMI port to initialize.
    pub port: Port,
    /// CPU pipe that feeds the HDMI transcoder.
    pub pipe: Pipe,
    /// HDMI pixel clock in hertz.
    pub dotclock_hz: u64,
    /// WRPLL selected for this HDMI stream.
    pub pll: HswPllSelect,
    /// Use per-DDI `PORT_CLK_SEL` routing instead of shared `DPLL_CTRL2` routing.
    pub per_ddi_clock_sel: bool,
    /// DDI buffer translation-table entry selected for HDMI drive strength.
    pub hdmi_translation: u8,
}

/// Build a high-level Haswell/Broadwell HDMI DDI init sequence.
pub fn hsw_hdmi_init_sequence_plan(
    params: HswHdmiInitParams,
) -> Result<HswHdmiInitSequencePlan, GmaError> {
    let HswHdmiInitParams {
        cpu,
        port,
        pipe,
        dotclock_hz,
        pll,
        per_ddi_clock_sel,
        hdmi_translation,
    } = params;
    if !matches!(cpu, Cpu::Haswell | Cpu::Broadwell) {
        return Err(GmaError::UnsupportedPlatform);
    }
    let routed = hsw_routed_hdmi_plan(HswHdmiRouteParams {
        port,
        pipe,
        dotclock_hz,
        pll,
        per_ddi_clock_sel,
    })?;
    let buffer_translations =
        hsw_ddi_buffer_translations(cpu, routed.port.port, false, hdmi_translation)?;
    Ok(HswHdmiInitSequencePlan {
        cpu,
        routed,
        buffer_translations,
        pipe: DdiPipePlaceholder { pipe, dotclock_hz },
        transcoder: DdiTranscoderPlaceholder {
            transcoder: DdiTranscoder::from_pipe(pipe),
        },
        steps: [
            HswHdmiInitStep::ProgramWrpll,
            HswHdmiInitStep::LoadBufferTranslations,
            HswHdmiInitStep::RouteDdiClock,
            HswHdmiInitStep::ProgramPipe,
            HswHdmiInitStep::ProgramTranscoder,
            HswHdmiInitStep::ProgramDdiPort,
        ],
    })
}

/// Parameters for a Haswell/Broadwell routed DP/eDP DDI plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswDpRouteParams {
    /// Logical DP/eDP port to route through a DDI encoder.
    pub port: Port,
    /// CPU pipe that feeds the DDI transcoder.
    pub pipe: Pipe,
    /// Number of DP lanes to request in the DDI port control value.
    pub lanes: DdiLaneCount,
    /// Fixed PLL source selected for the DP link.
    pub pll: HswPllSelect,
    /// Initial DP training pattern encoded in transport control.
    pub pattern: DpTrainingPattern,
    /// Whether DP enhanced framing is enabled.
    pub enhanced_framing: bool,
    /// Use per-DDI `PORT_CLK_SEL` routing instead of shared `DPLL_CTRL2` routing.
    pub per_ddi_clock_sel: bool,
}

/// Build a Haswell/Broadwell routed DP/eDP DDI plan.
pub fn hsw_routed_dp_plan(
    params: HswDpRouteParams,
) -> Result<RoutedDdiPortPlan<HswDdiPllPlan>, GmaError> {
    let HswDpRouteParams {
        port,
        pipe,
        lanes,
        pll,
        pattern,
        enhanced_framing,
        per_ddi_clock_sel,
    } = params;
    let plan = HswDdiPllPlan::Fixed(pll);
    let ddi = DdiPort::from_port(port)?;
    Ok(RoutedDdiPortPlan {
        port: DdiPortPlan::dp(
            port,
            pipe,
            lanes,
            plan.clock_select(),
            pattern,
            enhanced_framing,
        )?,
        pll: plan,
        routing: if per_ddi_clock_sel {
            DdiClockRouting::port_clk_sel(ddi, plan.register_value())
        } else {
            DdiClockRouting::dpll_ctrl2(ddi, plan.register_value())?
        },
    })
}

/// Parameters for a Haswell/Broadwell DP/eDP DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HswDpInitParams {
    /// Haswell-family CPU selector.
    pub cpu: Cpu,
    /// Logical DP/eDP port to initialize.
    pub port: Port,
    /// CPU pipe that feeds the DP transcoder.
    pub pipe: Pipe,
    /// Number of DP lanes to request in the DDI port control value.
    pub lanes: DdiLaneCount,
    /// Fixed PLL source selected for the DP link.
    pub pll: HswPllSelect,
    /// Initial DP training pattern encoded in transport control.
    pub pattern: DpTrainingPattern,
    /// Whether DP enhanced framing is enabled.
    pub enhanced_framing: bool,
    /// Use per-DDI `PORT_CLK_SEL` routing instead of shared `DPLL_CTRL2` routing.
    pub per_ddi_clock_sel: bool,
    /// Select the Broadwell iBoost/eDP low-voltage-swing buffer table variant.
    pub iboost_enabled: bool,
}

/// Build a high-level Haswell/Broadwell DP/eDP DDI init sequence.
pub fn hsw_dp_init_sequence_plan(
    params: HswDpInitParams,
) -> Result<HswDpInitSequencePlan, GmaError> {
    let HswDpInitParams {
        cpu,
        port,
        pipe,
        lanes,
        pll,
        pattern,
        enhanced_framing,
        per_ddi_clock_sel,
        iboost_enabled,
    } = params;
    if !matches!(cpu, Cpu::Haswell | Cpu::Broadwell) {
        return Err(GmaError::UnsupportedPlatform);
    }
    let routed = hsw_routed_dp_plan(HswDpRouteParams {
        port,
        pipe,
        lanes,
        pll,
        pattern,
        enhanced_framing,
        per_ddi_clock_sel,
    })?;
    let buffer_translations =
        hsw_ddi_buffer_translations(cpu, routed.port.port, iboost_enabled, 0)?;
    Ok(HswDpInitSequencePlan {
        cpu,
        routed,
        buffer_translations,
        pipe: DdiPipePlaceholder {
            pipe,
            dotclock_hz: 0,
        },
        transcoder: DdiTranscoderPlaceholder {
            transcoder: DdiTranscoder::from_pipe(pipe),
        },
        steps: [
            DdiDpInitStep::ProgramDpPll,
            DdiDpInitStep::LoadBufferTranslations,
            DdiDpInitStep::RouteDdiClock,
            DdiDpInitStep::ProgramPipe,
            DdiDpInitStep::ProgramTranscoder,
            DdiDpInitStep::ProgramDdiPort,
            DdiDpInitStep::TrainDpLink,
        ],
    })
}

/// Parameters for a Skylake/Kabylake routed HDMI DDI plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklHdmiRouteParams {
    /// Logical HDMI port to route through a DDI encoder.
    pub port: Port,
    /// CPU pipe that feeds the DDI transcoder.
    pub pipe: Pipe,
    /// HDMI pixel clock in hertz.
    pub dotclock_hz: u64,
    /// Configurable DPLL selected for this HDMI stream.
    pub pll: SklPllSelect,
}

/// Build a Skylake/Kabylake routed HDMI DDI plan.
pub fn skl_routed_hdmi_plan(
    params: SklHdmiRouteParams,
) -> Result<RoutedDdiPortPlan<SklDdiPllPlan>, GmaError> {
    let SklHdmiRouteParams {
        port,
        pipe,
        dotclock_hz,
        pll,
    } = params;
    if matches!(pll, SklPllSelect::Dpll0) {
        return Err(GmaError::InvalidConfig);
    }
    let plan = SklDdiPllPlan::Hdmi {
        pll,
        plan: calculate_skl_hdmi_dpll(dotclock_hz)?,
    };
    let ddi = DdiPort::from_port(port)?;
    Ok(RoutedDdiPortPlan {
        port: DdiPortPlan::hdmi(port, pipe, DdiClockSelect::None)?,
        pll: plan,
        routing: DdiClockRouting::port_clk_sel(ddi, plan.register_value()),
    })
}

/// Parameters for a Skylake/Kabylake HDMI DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklHdmiInitParams {
    /// Skylake-family CPU selector.
    pub cpu: Cpu,
    /// Logical HDMI port to initialize.
    pub port: Port,
    /// CPU pipe that feeds the HDMI transcoder.
    pub pipe: Pipe,
    /// HDMI pixel clock in hertz.
    pub dotclock_hz: u64,
    /// Configurable DPLL selected for this HDMI stream.
    pub pll: SklPllSelect,
}

/// Build a high-level Skylake/Kabylake HDMI DDI init sequence.
pub fn skl_hdmi_init_sequence_plan(
    params: SklHdmiInitParams,
) -> Result<SklHdmiInitSequencePlan, GmaError> {
    let SklHdmiInitParams {
        cpu,
        port,
        pipe,
        dotclock_hz,
        pll,
    } = params;
    if !matches!(cpu, Cpu::Skylake | Cpu::Kabylake) {
        return Err(GmaError::UnsupportedPlatform);
    }
    let routed = skl_routed_hdmi_plan(SklHdmiRouteParams {
        port,
        pipe,
        dotclock_hz,
        pll,
    })?;
    Ok(SklHdmiInitSequencePlan {
        cpu,
        routed,
        pipe: DdiPipePlaceholder { pipe, dotclock_hz },
        transcoder: DdiTranscoderPlaceholder {
            transcoder: DdiTranscoder::from_pipe(pipe),
        },
        steps: [
            SklHdmiInitStep::ProgramDpll,
            SklHdmiInitStep::RouteDdiClock,
            SklHdmiInitStep::ProgramPipe,
            SklHdmiInitStep::ProgramTranscoder,
            SklHdmiInitStep::ProgramDdiPort,
        ],
    })
}

/// Parameters for a Skylake/Kabylake routed DP/eDP DDI plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklDpRouteParams {
    /// Logical DP/eDP port to route through a DDI encoder.
    pub port: Port,
    /// CPU pipe that feeds the DDI transcoder.
    pub pipe: Pipe,
    /// Number of DP lanes to request in the DDI port control value.
    pub lanes: DdiLaneCount,
    /// Fixed or configurable DPLL selected for the DP link.
    pub pll: SklPllSelect,
    /// Link-rate clock selector for configurable DP DPLLs.
    pub clock: DdiClockSelect,
    /// Initial DP training pattern encoded in transport control.
    pub pattern: DpTrainingPattern,
    /// Whether DP enhanced framing is enabled.
    pub enhanced_framing: bool,
}

/// Build a Skylake/Kabylake routed DP/eDP DDI plan.
pub fn skl_routed_dp_plan(
    params: SklDpRouteParams,
) -> Result<RoutedDdiPortPlan<SklDdiPllPlan>, GmaError> {
    let SklDpRouteParams {
        port,
        pipe,
        lanes,
        pll,
        clock,
        pattern,
        enhanced_framing,
    } = params;
    let plan = if matches!(pll, SklPllSelect::Dpll0) {
        SklDdiPllPlan::Fixed(pll)
    } else {
        SklDdiPllPlan::Dp { pll, clock }
    };
    let ddi = DdiPort::from_port(port)?;
    Ok(RoutedDdiPortPlan {
        port: DdiPortPlan::dp(
            port,
            pipe,
            lanes,
            DdiClockSelect::None,
            pattern,
            enhanced_framing,
        )?,
        pll: plan,
        routing: DdiClockRouting::port_clk_sel(ddi, plan.register_value()),
    })
}

/// Parameters for a Skylake/Kabylake DP/eDP DDI init sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SklDpInitParams {
    /// Skylake-family CPU selector.
    pub cpu: Cpu,
    /// Logical DP/eDP port to initialize.
    pub port: Port,
    /// CPU pipe that feeds the DP transcoder.
    pub pipe: Pipe,
    /// Number of DP lanes to request in the DDI port control value.
    pub lanes: DdiLaneCount,
    /// Fixed or configurable DPLL selected for the DP link.
    pub pll: SklPllSelect,
    /// Link-rate clock selector for configurable DP DPLLs.
    pub clock: DdiClockSelect,
    /// Initial DP training pattern encoded in transport control.
    pub pattern: DpTrainingPattern,
    /// Whether DP enhanced framing is enabled.
    pub enhanced_framing: bool,
}

/// Build a high-level Skylake/Kabylake DP/eDP DDI init sequence.
pub fn skl_dp_init_sequence_plan(
    params: SklDpInitParams,
) -> Result<SklDpInitSequencePlan, GmaError> {
    let SklDpInitParams {
        cpu,
        port,
        pipe,
        lanes,
        pll,
        clock,
        pattern,
        enhanced_framing,
    } = params;
    if !matches!(cpu, Cpu::Skylake | Cpu::Kabylake) {
        return Err(GmaError::UnsupportedPlatform);
    }
    let routed = skl_routed_dp_plan(SklDpRouteParams {
        port,
        pipe,
        lanes,
        pll,
        clock,
        pattern,
        enhanced_framing,
    })?;
    Ok(SklDpInitSequencePlan {
        cpu,
        routed,
        pipe: DdiPipePlaceholder {
            pipe,
            dotclock_hz: 0,
        },
        transcoder: DdiTranscoderPlaceholder {
            transcoder: DdiTranscoder::from_pipe(pipe),
        },
        steps: [
            DdiDpInitStep::ProgramDpPll,
            DdiDpInitStep::LoadBufferTranslations,
            DdiDpInitStep::RouteDdiClock,
            DdiDpInitStep::ProgramPipe,
            DdiDpInitStep::ProgramTranscoder,
            DdiDpInitStep::ProgramDdiPort,
            DdiDpInitStep::TrainDpLink,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_logical_ports_to_ddi_ports() {
        assert_eq!(DdiPort::from_port(Port::Edp), Ok(DdiPort::A));
        assert_eq!(DdiPort::from_port(Port::HdmiA), Ok(DdiPort::A));
        assert_eq!(DdiPort::from_port(Port::DpB), Ok(DdiPort::B));
        assert_eq!(DdiPort::from_port(Port::HdmiC), Ok(DdiPort::C));
        assert_eq!(DdiPort::from_port(Port::DpD), Ok(DdiPort::D));
        assert_eq!(
            DdiPort::from_port(Port::Vga),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn register_offsets_match_libgfxinit_shared_ddi() {
        assert_eq!(DdiRegisters::for_port(DdiPort::A).buf_ctl, 0x64000);
        assert_eq!(DdiRegisters::for_port(DdiPort::B).dp_tp_ctl, 0x64140);
        assert_eq!(
            DdiRegisters::for_port(DdiPort::C).dp_tp_status,
            Some(0x64244)
        );
        assert_eq!(DdiRegisters::for_port(DdiPort::D).port_clk_sel, 0x4610c);
        assert_eq!(
            DdiRegisters::for_port(DdiPort::E).dp_tp_status,
            Some(0x64444)
        );
    }

    #[test]
    fn encodes_ddi_buf_ctl_like_libgfxinit() {
        assert_eq!(
            encode_ddi_buf_ctl(
                DdiTranscoder::B,
                DdiLaneCount::Four,
                true,
                true,
                true,
                false,
            ),
            (1 << 31) | (1 << 24) | (1 << 16) | (3 << 1) | 1
        );
        assert_eq!(
            encode_ddi_buf_ctl(
                DdiTranscoder::Edp,
                DdiLaneCount::One,
                false,
                false,
                false,
                true,
            ),
            (0xf << 24) | (1 << 4)
        );
    }

    #[test]
    fn encodes_dp_transport_control_like_libgfxinit() {
        assert_eq!(encode_training_pattern(DpTrainingPattern::Pattern1), 1 << 7);
        assert_eq!(
            encode_training_pattern(DpTrainingPattern::Pattern2),
            (1 << 8) | (1 << 7)
        );
        assert_eq!(
            encode_training_pattern(DpTrainingPattern::Pattern3),
            (4 << 8) | (1 << 7)
        );
        assert_eq!(encode_training_pattern(DpTrainingPattern::Idle), 2 << 8);
        assert_eq!(encode_training_pattern(DpTrainingPattern::None), 3 << 8);
        assert_eq!(
            encode_dp_tp_ctl(true, false, true, true, false, DpTrainingPattern::Pattern2),
            (1 << 31) | (1 << 25) | (1 << 18) | (1 << 8) | (1 << 7)
        );
    }

    #[test]
    fn encodes_port_clock_selection_like_libgfxinit() {
        assert_eq!(DdiClockSelect::Lcpll2700.encode(), 0 << 29);
        assert_eq!(DdiClockSelect::Lcpll1350.encode(), 1 << 29);
        assert_eq!(DdiClockSelect::Lcpll810.encode(), 2 << 29);
        assert_eq!(DdiClockSelect::Spll.encode(), 3 << 29);
        assert_eq!(DdiClockSelect::Wrpll1.encode(), 4 << 29);
        assert_eq!(DdiClockSelect::Wrpll2.encode(), 5 << 29);
        assert_eq!(DdiClockSelect::None.encode(), 7 << 29);
    }

    #[test]
    fn haswell_broadwell_buffer_table_selection_matches_libgfxinit() {
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Haswell, DdiPort::A, true),
            Ok(HswDdiBufferTableKind::HaswellDp)
        );
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Haswell, DdiPort::E, false),
            Ok(HswDdiBufferTableKind::HaswellFdi)
        );
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Broadwell, DdiPort::A, true),
            Ok(HswDdiBufferTableKind::BroadwellEdpLowVswing)
        );
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Broadwell, DdiPort::A, false),
            Ok(HswDdiBufferTableKind::BroadwellDp)
        );
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Broadwell, DdiPort::E, false),
            Ok(HswDdiBufferTableKind::BroadwellFdi)
        );
        assert_eq!(
            hsw_ddi_buffer_table_kind(Cpu::Skylake, DdiPort::A, false),
            Err(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn haswell_broadwell_buffer_translations_patch_hdmi_entries() {
        let hsw = hsw_ddi_buffer_translations(Cpu::Haswell, DdiPort::B, false, 7).unwrap();
        assert_eq!(
            &hsw[0..4],
            &[0x00ff_ffff, 0x0006_000e, 0x00d7_5fff, 0x0005_000a]
        );
        assert_eq!((hsw[18], hsw[19]), (0x80e7_9fff, 0x0003_0002));

        let bdw_edp = hsw_ddi_buffer_translations(Cpu::Broadwell, DdiPort::A, true, 6).unwrap();
        assert_eq!(
            &bdw_edp[0..4],
            &[0x00ff_ffff, 0x0000_0012, 0x00eb_afff, 0x0002_0011]
        );
        assert_eq!((bdw_edp[18], bdw_edp[19]), (0x80cb_2fff, 0x001b_0002));
        assert_eq!(
            hsw_ddi_buffer_translations(Cpu::Broadwell, DdiPort::B, false, 10),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn haswell_wrpll_planning_matches_libgfxinit_examples() {
        assert_eq!(
            calculate_hsw_wrpll(65_000_000).unwrap(),
            HswWrpllPlan {
                r2: 19,
                n2: 32,
                p: 14,
            }
        );
        assert_eq!(
            calculate_hsw_wrpll(148_500_000).unwrap(),
            HswWrpllPlan {
                r2: 20,
                n2: 33,
                p: 6,
            }
        );
        let plan = calculate_hsw_wrpll(297_000_000).unwrap();
        assert_eq!(
            plan,
            HswWrpllPlan {
                r2: 20,
                n2: 22,
                p: 2,
            }
        );
        assert_eq!(
            plan.encode_ctl(),
            (1 << 31) | (3 << 28) | (22 << 16) | (2 << 8) | 20
        );
        assert_eq!(
            calculate_hsw_wrpll(540_000_000).unwrap(),
            HswWrpllPlan { r2: 2, n2: 2, p: 1 }
        );
    }

    #[test]
    fn skylake_dpll_registers_and_ops_match_libgfxinit() {
        let regs = SklDpll::Dpll2.registers();
        assert_eq!(regs.ctl, 0x46040);
        assert_eq!(regs.cfgr1, 0x6c048);
        assert_eq!(regs.cfgr2, 0x6c04c);
        assert_eq!(regs.ctrl1_shift, 12);
        assert_eq!(regs.status_shift, 16);
        assert_eq!(
            skl_hdmi_dpll_ctrl1_update(SklDpll::Dpll2),
            (
                SKL_DPLL_CTRL1,
                SKL_DPLL_CTRL1_SSC << 12,
                (SKL_DPLL_CTRL1_HDMI_MODE | SKL_DPLL_CTRL1_OVERRIDE) << 12,
            )
        );
        assert_eq!(
            skl_dp_dpll_ctrl1_update(SklDpll::Dpll3, DdiClockSelect::Lcpll1350),
            Ok((
                SKL_DPLL_CTRL1,
                (SKL_DPLL_CTRL1_HDMI_MODE | SKL_DPLL_CTRL1_SSC | SKL_DPLL_CTRL1_LINK_RATE_MASK)
                    << 18,
                (SKL_DPLL_CTRL1_LINK_RATE_1350 | SKL_DPLL_CTRL1_OVERRIDE) << 18,
            ))
        );
        assert_eq!(
            skl_dp_dpll_ctrl1_update(SklDpll::Dpll1, DdiClockSelect::Wrpll1),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            skl_dpll_enable_ops(SklDpll::Dpll1),
            (0x46014, SKL_DPLL_CTL_PLL_ENABLE, SKL_DPLL_STATUS, 1 << 8)
        );
        assert_eq!(SKL_DPLL_CTRL2, 0x6c05c);
    }

    #[test]
    fn skylake_hdmi_dpll_planning_matches_libgfxinit_examples() {
        let xga = calculate_skl_hdmi_dpll(65_000_000).unwrap();
        assert_eq!(
            xga,
            SklDpllPlan {
                central_frequency: SklCentralFrequency::Cf9600,
                dco_hz: 9_100_000_000,
                pdiv: 2,
                qdiv: 7,
                kdiv: 2,
            }
        );
        assert_eq!(xga.encode_cfgr1(), 0x802a_ab7b);
        assert_eq!(xga.encode_cfgr2(), 0x07a4);

        let fhd = calculate_skl_hdmi_dpll(148_500_000).unwrap();
        assert_eq!(
            fhd,
            SklDpllPlan {
                central_frequency: SklCentralFrequency::Cf9000,
                dco_hz: 8_910_000_000,
                pdiv: 2,
                qdiv: 3,
                kdiv: 2,
            }
        );
        assert_eq!(fhd.encode_cfgr1(), 0x8040_0173);
        assert_eq!(fhd.encode_cfgr2(), 0x03a5);

        let uhd = calculate_skl_hdmi_dpll(297_000_000).unwrap();
        assert_eq!(uhd.central_frequency, SklCentralFrequency::Cf9000);
        assert_eq!(uhd.dco_hz, 8_910_000_000);
        assert_eq!((uhd.pdiv, uhd.qdiv, uhd.kdiv), (2, 1, 3));
        assert_eq!(calculate_skl_hdmi_dpll(0), Err(GmaError::InvalidConfig));
    }

    #[test]
    fn builds_data_only_hdmi_and_dp_plans_without_enabling_caps() {
        let hdmi = DdiPortPlan::hdmi(Port::HdmiB, Pipe::B, DdiClockSelect::Wrpll1).unwrap();
        assert_eq!(hdmi.port, DdiPort::B);
        assert_eq!(hdmi.regs.buf_ctl, 0x64100);
        assert_eq!(hdmi.dp_tp_ctl, None);
        assert_eq!(hdmi.port_clk_sel, 4 << 29);

        let dp = DdiPortPlan::dp(
            Port::Edp,
            Pipe::A,
            DdiLaneCount::Two,
            DdiClockSelect::Lcpll1350,
            DpTrainingPattern::Pattern1,
            true,
        )
        .unwrap();
        assert_eq!(dp.port, DdiPort::A);
        assert_ne!(dp.buf_ctl & DDI_BUF_CTL_DDI_A_LANE_CAP, 0);
        assert_eq!(dp.dp_tp_ctl, Some((1 << 31) | (1 << 18) | (1 << 7)));
        assert_eq!(DdiLaneCount::new(3), Err(GmaError::InvalidConfig));
    }

    #[test]
    fn routes_haswell_ports_with_port_clk_sel_and_dpll_ctrl2_fields() {
        assert_eq!(HswPllSelect::Wrpll0.register_value(), 4 << 29);
        assert_eq!(HswPllSelect::Wrpll1.register_value(), 5 << 29);
        assert_eq!(HswPllSelect::Lcpll2.register_value(), 2 << 29);

        let hdmi = hsw_routed_hdmi_plan(HswHdmiRouteParams {
            port: Port::HdmiB,
            pipe: Pipe::B,
            dotclock_hz: 148_500_000,
            pll: HswPllSelect::Wrpll0,
            per_ddi_clock_sel: true,
        })
        .unwrap();
        assert_eq!(hdmi.port.port, DdiPort::B);
        assert_eq!(hdmi.port.port_clk_sel, 4 << 29);
        assert!(matches!(
            hdmi.pll,
            HswDdiPllPlan::Wrpll {
                pll: HswPllSelect::Wrpll0,
                plan: HswWrpllPlan {
                    r2: 20,
                    n2: 33,
                    p: 6
                }
            }
        ));
        assert_eq!(
            hdmi.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46104,
                    mask_unset: u32::MAX,
                    mask_set: 4 << 29,
                }
            }
        );
        assert_eq!(
            hsw_routed_hdmi_plan(HswHdmiRouteParams {
                port: Port::HdmiB,
                pipe: Pipe::B,
                dotclock_hz: 148_500_000,
                pll: HswPllSelect::Lcpll0,
                per_ddi_clock_sel: true,
            }),
            Err(GmaError::InvalidConfig)
        );

        assert_eq!(
            DdiClockRouting::dpll_ctrl2(DdiPort::C, 2).unwrap(),
            DdiClockRouting::DpllCtrl2 {
                op: DdiRegisterOp {
                    register: SKL_DPLL_CTRL2,
                    mask_unset: (1 << 17) | (3 << 7),
                    mask_set: (2 << 7) | (1 << 6),
                }
            }
        );
    }

    #[test]
    fn haswell_hdmi_sequence_composes_wrpll_routing_translations_and_placeholders() {
        let plan = hsw_hdmi_init_sequence_plan(HswHdmiInitParams {
            cpu: Cpu::Haswell,
            port: Port::HdmiB,
            pipe: Pipe::B,
            dotclock_hz: 148_500_000,
            pll: HswPllSelect::Wrpll0,
            per_ddi_clock_sel: true,
            hdmi_translation: 7,
        })
        .unwrap();

        assert_eq!(plan.cpu, Cpu::Haswell);
        assert_eq!(plan.routed.port.port, DdiPort::B);
        assert_eq!(plan.routed.port.regs.buf_ctl, 0x64100);
        assert_eq!(plan.routed.port.dp_tp_ctl, None);
        assert!(matches!(
            plan.routed.pll,
            HswDdiPllPlan::Wrpll {
                pll: HswPllSelect::Wrpll0,
                plan: HswWrpllPlan {
                    r2: 20,
                    n2: 33,
                    p: 6
                }
            }
        ));
        assert_eq!(
            plan.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46104,
                    mask_unset: u32::MAX,
                    mask_set: 4 << 29,
                }
            }
        );
        assert_eq!(
            &plan.buffer_translations[0..4],
            &[0x00ff_ffff, 0x0006_000e, 0x00d7_5fff, 0x0005_000a]
        );
        assert_eq!(
            (plan.buffer_translations[18], plan.buffer_translations[19]),
            (0x80e7_9fff, 0x0003_0002)
        );
        assert_eq!(
            plan.pipe,
            DdiPipePlaceholder {
                pipe: Pipe::B,
                dotclock_hz: 148_500_000,
            }
        );
        assert_eq!(
            plan.transcoder,
            DdiTranscoderPlaceholder {
                transcoder: DdiTranscoder::B,
            }
        );
        assert_eq!(
            plan.steps,
            [
                HswHdmiInitStep::ProgramWrpll,
                HswHdmiInitStep::LoadBufferTranslations,
                HswHdmiInitStep::RouteDdiClock,
                HswHdmiInitStep::ProgramPipe,
                HswHdmiInitStep::ProgramTranscoder,
                HswHdmiInitStep::ProgramDdiPort,
            ]
        );
    }

    #[test]
    fn haswell_dp_sequence_composes_fixed_pll_routing_and_training_step() {
        let plan = hsw_dp_init_sequence_plan(HswDpInitParams {
            cpu: Cpu::Broadwell,
            port: Port::DpB,
            pipe: Pipe::B,
            lanes: DdiLaneCount::Four,
            pll: HswPllSelect::Spll,
            pattern: DpTrainingPattern::Pattern1,
            enhanced_framing: true,
            per_ddi_clock_sel: true,
            iboost_enabled: false,
        })
        .unwrap();
        assert_eq!(plan.cpu, Cpu::Broadwell);
        assert_eq!(plan.routed.port.port, DdiPort::B);
        assert!(plan.routed.port.dp_tp_ctl.is_some());
        assert_eq!(plan.routed.pll, HswDdiPllPlan::Fixed(HswPllSelect::Spll));
        assert_eq!(
            plan.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46104,
                    mask_unset: u32::MAX,
                    mask_set: 3 << 29,
                }
            }
        );
        assert_eq!(
            plan.pipe,
            DdiPipePlaceholder {
                pipe: Pipe::B,
                dotclock_hz: 0,
            }
        );
        assert_eq!(
            plan.steps,
            [
                DdiDpInitStep::ProgramDpPll,
                DdiDpInitStep::LoadBufferTranslations,
                DdiDpInitStep::RouteDdiClock,
                DdiDpInitStep::ProgramPipe,
                DdiDpInitStep::ProgramTranscoder,
                DdiDpInitStep::ProgramDdiPort,
                DdiDpInitStep::TrainDpLink,
            ]
        );
        assert_eq!(
            hsw_dp_init_sequence_plan(HswDpInitParams {
                cpu: Cpu::Skylake,
                port: Port::DpB,
                pipe: Pipe::B,
                lanes: DdiLaneCount::Four,
                pll: HswPllSelect::Spll,
                pattern: DpTrainingPattern::Pattern1,
                enhanced_framing: true,
                per_ddi_clock_sel: true,
                iboost_enabled: false,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn haswell_hdmi_sequence_rejects_non_hsw_cpu_and_non_wrpll() {
        assert_eq!(
            hsw_hdmi_init_sequence_plan(HswHdmiInitParams {
                cpu: Cpu::Skylake,
                port: Port::HdmiB,
                pipe: Pipe::B,
                dotclock_hz: 148_500_000,
                pll: HswPllSelect::Wrpll0,
                per_ddi_clock_sel: true,
                hdmi_translation: 7,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
        assert_eq!(
            hsw_hdmi_init_sequence_plan(HswHdmiInitParams {
                cpu: Cpu::Haswell,
                port: Port::HdmiB,
                pipe: Pipe::B,
                dotclock_hz: 148_500_000,
                pll: HswPllSelect::Lcpll0,
                per_ddi_clock_sel: true,
                hdmi_translation: 7,
            }),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn routes_skylake_ports_with_pll_register_values() {
        assert_eq!(SklPllSelect::Dpll0.register_value(), 0);
        assert_eq!(SklPllSelect::Dpll1.register_value(), 1);
        assert_eq!(SklPllSelect::Dpll2.register_value(), 2);
        assert_eq!(SklPllSelect::Dpll3.register_value(), 3);
        assert_eq!(SklPllSelect::Dpll2.configurable(), Some(SklDpll::Dpll2));
        assert_eq!(SklPllSelect::Dpll0.configurable(), None);

        let hdmi = skl_routed_hdmi_plan(SklHdmiRouteParams {
            port: Port::HdmiC,
            pipe: Pipe::C,
            dotclock_hz: 297_000_000,
            pll: SklPllSelect::Dpll3,
        })
        .unwrap();
        assert_eq!(hdmi.port.port, DdiPort::C);
        assert_eq!(hdmi.port.port_clk_sel, 7 << 29);
        assert_eq!(
            hdmi.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46108,
                    mask_unset: u32::MAX,
                    mask_set: 3,
                }
            }
        );
        assert!(matches!(
            hdmi.pll,
            SklDdiPllPlan::Hdmi {
                pll: SklPllSelect::Dpll3,
                plan: SklDpllPlan {
                    central_frequency: SklCentralFrequency::Cf9000,
                    dco_hz: 8_910_000_000,
                    pdiv: 2,
                    qdiv: 1,
                    kdiv: 3,
                }
            }
        ));

        let dp = skl_routed_dp_plan(SklDpRouteParams {
            port: Port::Edp,
            pipe: Pipe::A,
            lanes: DdiLaneCount::Two,
            pll: SklPllSelect::Dpll0,
            clock: DdiClockSelect::Lcpll2700,
            pattern: DpTrainingPattern::Pattern1,
            enhanced_framing: true,
        })
        .unwrap();
        assert_eq!(dp.port.port, DdiPort::A);
        assert_eq!(dp.pll, SklDdiPllPlan::Fixed(SklPllSelect::Dpll0));
        assert_eq!(
            dp.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46100,
                    mask_unset: u32::MAX,
                    mask_set: 0,
                }
            }
        );
        assert_eq!(
            skl_routed_hdmi_plan(SklHdmiRouteParams {
                port: Port::HdmiA,
                pipe: Pipe::A,
                dotclock_hz: 65_000_000,
                pll: SklPllSelect::Dpll0,
            }),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn skylake_hdmi_sequence_composes_dpll_routing_and_port_step() {
        let plan = skl_hdmi_init_sequence_plan(SklHdmiInitParams {
            cpu: Cpu::Kabylake,
            port: Port::HdmiC,
            pipe: Pipe::C,
            dotclock_hz: 297_000_000,
            pll: SklPllSelect::Dpll3,
        })
        .unwrap();
        assert_eq!(plan.cpu, Cpu::Kabylake);
        assert_eq!(plan.routed.port.port, DdiPort::C);
        assert_eq!(plan.routed.port.dp_tp_ctl, None);
        assert!(matches!(
            plan.routed.pll,
            SklDdiPllPlan::Hdmi {
                pll: SklPllSelect::Dpll3,
                plan: SklDpllPlan {
                    central_frequency: SklCentralFrequency::Cf9000,
                    dco_hz: 8_910_000_000,
                    pdiv: 2,
                    qdiv: 1,
                    kdiv: 3,
                }
            }
        ));
        assert_eq!(
            plan.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46108,
                    mask_unset: u32::MAX,
                    mask_set: 3,
                }
            }
        );
        assert_eq!(
            plan.steps,
            [
                SklHdmiInitStep::ProgramDpll,
                SklHdmiInitStep::RouteDdiClock,
                SklHdmiInitStep::ProgramPipe,
                SklHdmiInitStep::ProgramTranscoder,
                SklHdmiInitStep::ProgramDdiPort,
            ]
        );
        assert_eq!(
            skl_hdmi_init_sequence_plan(SklHdmiInitParams {
                cpu: Cpu::Haswell,
                port: Port::HdmiC,
                pipe: Pipe::C,
                dotclock_hz: 297_000_000,
                pll: SklPllSelect::Dpll3,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn skylake_dp_sequence_composes_dpll_routing_and_training_step() {
        let plan = skl_dp_init_sequence_plan(SklDpInitParams {
            cpu: Cpu::Skylake,
            port: Port::DpC,
            pipe: Pipe::C,
            lanes: DdiLaneCount::Four,
            pll: SklPllSelect::Dpll2,
            clock: DdiClockSelect::Lcpll2700,
            pattern: DpTrainingPattern::Pattern2,
            enhanced_framing: true,
        })
        .unwrap();
        assert_eq!(plan.cpu, Cpu::Skylake);
        assert_eq!(plan.routed.port.port, DdiPort::C);
        assert_eq!(
            plan.routed.pll,
            SklDdiPllPlan::Dp {
                pll: SklPllSelect::Dpll2,
                clock: DdiClockSelect::Lcpll2700,
            }
        );
        assert_eq!(
            plan.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46108,
                    mask_unset: u32::MAX,
                    mask_set: 2,
                }
            }
        );
        assert_eq!(
            plan.steps,
            [
                DdiDpInitStep::ProgramDpPll,
                DdiDpInitStep::LoadBufferTranslations,
                DdiDpInitStep::RouteDdiClock,
                DdiDpInitStep::ProgramPipe,
                DdiDpInitStep::ProgramTranscoder,
                DdiDpInitStep::ProgramDdiPort,
                DdiDpInitStep::TrainDpLink,
            ]
        );
        assert_eq!(
            skl_dp_init_sequence_plan(SklDpInitParams {
                cpu: Cpu::Haswell,
                port: Port::DpC,
                pipe: Pipe::C,
                lanes: DdiLaneCount::Four,
                pll: SklPllSelect::Dpll2,
                clock: DdiClockSelect::Lcpll2700,
                pattern: DpTrainingPattern::Pattern2,
                enhanced_framing: true,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
    }
}
