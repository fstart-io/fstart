// SPDX-License-Identifier: MIT OR Apache-2.0
//! Data Fabric registers whose layout DF2 (Raven, ZP) and DF3 (Rome,
//! Milan) share. Names follow the public PPR/Linux/illumos names where they
//! exist; registers only seen as vendor values keep their `FnxOff` name.
use super::DfReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// F0x40 FabricBlockInstanceCount.
    pub FBICNT [
        COUNT OFFSET(0) NUMBITS(8) []
    ],
    /// F0x84 CfgAddressCntl: the node's (or IOMS instance's) first bus.
    pub CFG_ADDR_CTL [
        BUS_NUM OFFSET(0) NUMBITS(8) []
    ],
    /// F0xC0+8i X86IOBaseAddress.
    pub IO_BASE [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        IE OFFSET(5) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(13) []
    ],
    /// F0x104 DramHoleControl.
    pub DRAM_HOLE [
        VALID OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(24) NUMBITS(8) []
    ],
    /// F1x204 SystemComponentCnt.
    pub COMPONENT_CNT [
        PIE OFFSET(0) NUMBITS(8) [],
        GCM OFFSET(8) NUMBITS(8) [],
        IOMS OFFSET(16) NUMBITS(8) []
    ],
    /// F4x44: bit 0 is what AGESA's ReadyToBoot handler calls "extended
    /// configuration access" (SOC_DF §3.7, all generations).
    pub CFG_ACCESS_CTRL [
        EXT_CFG OFFSET(0) NUMBITS(1) []
    ],
    /// F0x3F0 clock gating; the vendor code writes it whole.
    pub CLOCK_GATING [
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// F0x3F8 (DF3 DfGlobalCtrl; Raven and ZP clear bit 3). The PIE RAS
    /// step writes these fields; their names are not public (SOC_DF §3.6,
    /// §8).
    pub GLOBAL_CTRL [
        BIT3 OFFSET(3) NUMBITS(1) [],
        RAS_A OFFSET(4) NUMBITS(2) [],
        RAS_B0 OFFSET(8) NUMBITS(3) [],
        RAS_B1 OFFSET(12) NUMBITS(3) [],
        RAS_B2 OFFSET(16) NUMBITS(3) []
    ]
];

pub const FBICNT: DfReg<FBICNT::Register> = DfReg::new(0, 0x40);
pub const VGA_EN_OFFSET: u16 = 0x80;
pub const CFG_ADDR_CTL: DfReg<CFG_ADDR_CTL::Register> = DfReg::new(0, 0x84);
pub const CFG_MAP_OFFSET: u16 = 0xA0;
pub const IO_BASE: DfReg<IO_BASE::Register> = DfReg::new(0, 0xC0);
pub const IO_LIMIT_OFFSET: u16 = 0xC4;
pub const DRAM_HOLE: DfReg<DRAM_HOLE::Register> = DfReg::new(0, 0x104);
pub const DRAM_BASE_OFFSET: u16 = 0x110;
pub const DRAM_LIMIT_OFFSET: u16 = 0x114;
/// F0x200+16i MmioBaseAddress: address\[47:16\].
pub const MMIO_BASE: DfReg = DfReg::new(0, 0x200);
/// F0x204+16i MmioLimitAddress: address\[47:16\], inclusive.
pub const MMIO_LIMIT: DfReg = DfReg::new(0, 0x204);
pub const MMIO_CTRL_OFFSET: u16 = 0x208;
/// F0x3F0: clock gating (SOC_DF §3.6).
pub const CLOCK_GATING: DfReg<CLOCK_GATING::Register> = DfReg::new(0, 0x3F0);
pub const GLOBAL_CTRL: DfReg<GLOBAL_CTRL::Register> = DfReg::new(0, 0x3F8);
pub const SYSTEM_CFG_OFFSET: u16 = 0x200;
pub const COMPONENT_CNT: DfReg<COMPONENT_CNT::Register> = DfReg::new(1, 0x204);
pub const CFG_ACCESS_CTRL: DfReg<CFG_ACCESS_CTRL::Register> = DfReg::new(4, 0x44);
/// F6x1F0/F6x1F4: PIE RAS status, cleared at init (SOC_DF §3.6).
pub const PIE_RAS_STATUS_LO: DfReg = DfReg::new(6, 0x1F0);
pub const PIE_RAS_STATUS_HI: DfReg = DfReg::new(6, 0x1F4);

/// Strides of the register arrays.
pub const MMIO_STRIDE: u16 = 0x10;
pub const IO_STRIDE: u16 = 8;
pub const CFG_MAP_STRIDE: u16 = 4;
pub const DRAM_STRIDE: u16 = 8;
