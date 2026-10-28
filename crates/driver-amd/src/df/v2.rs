// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF2 (Raven, ZP): one DF node per die, 8-bit fabric IDs; on ZP
//! `socket[7] | die[6:5] | component[4:0]` (SOC_DF §3.1, DF_ZP §3.2).
use super::{DfReg, regs};
use tock_registers::register_bitfields;

/// IOMS component of a ZP die (DF_ZP §3.2: component map kind 5 = (4,4)).
pub const IOMS_COMPONENT: u16 = 4;
/// ZP fabric instances of the DRAM map reads (PIE, inst 5; DF_ZP §3.8).
pub const PIE_INSTANCE: u8 = 5;
/// ZP CCM instances (DF_ZP §3.5).
pub const CCM_INSTANCES: [u8; 2] = [2, 3];
/// ZP IOMS instance.
pub const IOMS_INSTANCE: u8 = 4;
/// ZP CS (UMC) instances.
pub const CS_INSTANCES: [u8; 2] = [0, 1];

/// Fabric ID of `component` on `socket`/`die`.
pub const fn fabric_id(socket: u8, die: u8, component: u16) -> u16 {
    (socket as u16) << 7 | (die as u16) << 5 | component
}

/// DF node of `socket`/`die` (`0x18 + node` on bus 0).
pub const fn node(socket: u8, die: u8) -> u8 {
    socket * 4 + die
}

register_bitfields![u32,
    /// F1x200 SystemCfg (DF2).
    pub SYSTEM_CFG [
        DIE_PRESENT OFFSET(0) NUMBITS(4) [],
        OTHER_SOCK OFFSET(5) NUMBITS(1) [],
        MY_TYPE OFFSET(21) NUMBITS(2) [],
        DIE_ID OFFSET(24) NUMBITS(2) [],
        SOCK_ID OFFSET(27) NUMBITS(1) []
    ],
    /// F0xA0+4i CfgAddressMap (DF2).
    pub CFG_MAP [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) [],
        BUS_BASE OFFSET(16) NUMBITS(8) [],
        BUS_LIMIT OFFSET(24) NUMBITS(8) []
    ],
    /// F0xC4+8i X86IOLimitAddress (DF2).
    pub IO_LIMIT [
        DST OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(12) NUMBITS(13) []
    ],
    /// F0x208+16i MmioAddressControl (DF2).
    pub MMIO_CTRL [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) [],
        NP OFFSET(12) NUMBITS(1) []
    ],
    /// F0x80 VGAEn (DF2).
    pub VGA_EN [
        EN OFFSET(0) NUMBITS(1) [],
        NP OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) []
    ],
    /// F0x110+8i DramBaseAddress (DF2).
    pub DRAM_BASE [
        VALID OFFSET(0) NUMBITS(1) [],
        HOLE_EN OFFSET(1) NUMBITS(1) [],
        ILV_CHAN OFFSET(4) NUMBITS(4) [],
        ILV_ADDR OFFSET(8) NUMBITS(3) [],
        BASE OFFSET(12) NUMBITS(20) []
    ],
    /// F0x114+8i DramLimitAddress (DF2).
    pub DRAM_LIMIT [
        DST OFFSET(0) NUMBITS(8) [],
        /// The die of DST on ZP.
        DST_DIE OFFSET(5) NUMBITS(3) [],
        ILV_SOCK OFFSET(8) NUMBITS(1) [],
        ILV_DIE OFFSET(10) NUMBITS(2) [],
        LIMIT OFFSET(12) NUMBITS(20) []
    ]
];

pub const SYSTEM_CFG: DfReg<SYSTEM_CFG::Register> = DfReg::new(1, regs::SYSTEM_CFG_OFFSET);
/// The DRAM rules: [`DRAM_RULES`] base/limit pairs.
pub const DRAM_RULES: u8 = 16;
pub const fn dram_base(rule: u8) -> DfReg<DRAM_BASE::Register> {
    DfReg::new(0, regs::DRAM_BASE_OFFSET).at(rule, regs::DRAM_STRIDE)
}
pub const fn dram_limit(rule: u8) -> DfReg<DRAM_LIMIT::Register> {
    DfReg::new(0, regs::DRAM_LIMIT_OFFSET).at(rule, regs::DRAM_STRIDE)
}
/// PCI bus map `map`.
pub const fn cfg_map(map: u8) -> DfReg<CFG_MAP::Register> {
    DfReg::new(0, regs::CFG_MAP_OFFSET).at(map, regs::CFG_MAP_STRIDE)
}
/// MMIO rule `pair`: base and limit are address\[47:16\].
pub const fn mmio_base(pair: u8) -> DfReg {
    regs::MMIO_BASE.at(pair, regs::MMIO_STRIDE)
}
pub const fn mmio_limit(pair: u8) -> DfReg {
    regs::MMIO_LIMIT.at(pair, regs::MMIO_STRIDE)
}
pub const fn mmio_ctrl(pair: u8) -> DfReg<MMIO_CTRL::Register> {
    DfReg::new(0, regs::MMIO_CTRL_OFFSET).at(pair, regs::MMIO_STRIDE)
}
/// I/O rule `pair`.
pub const fn io_base(pair: u8) -> DfReg<regs::IO_BASE::Register> {
    regs::IO_BASE.at(pair, regs::IO_STRIDE)
}
pub const fn io_limit(pair: u8) -> DfReg<IO_LIMIT::Register> {
    DfReg::new(0, regs::IO_LIMIT_OFFSET).at(pair, regs::IO_STRIDE)
}
pub const VGA_EN: DfReg<VGA_EN::Register> = DfReg::new(0, regs::VGA_EN_OFFSET);
