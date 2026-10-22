// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric address maps Raven/Picasso and Zeppelin share: the MMIO
//! and I/O base/limit pairs and the VGA route of D18F0.
use super::registers::DfReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    pub MmioCtrl [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST_FABRIC_ID OFFSET(4) NUMBITS(8) [],
        NON_POSTED OFFSET(12) NUMBITS(1) []
    ],
    pub IoBase [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(13) []
    ],
    pub IoLimit [
        DST_FABRIC_ID OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(12) NUMBITS(13) []
    ],
    pub VgaEn [
        ENABLE OFFSET(0) NUMBITS(1) [],
        NP OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST_FABRIC_ID OFFSET(4) NUMBITS(8) []
    ]
];

pub const MMIO_PAIRS: u16 = 8;
pub const IO_PAIRS: u16 = 8;

/// [31:0] base >> 16.
pub const fn mmio_base(i: u16) -> DfReg {
    DfReg::new(0, 0x200 + 0x10 * i)
}
/// [31:0] limit >> 16.
pub const fn mmio_limit(i: u16) -> DfReg {
    DfReg::new(0, 0x204 + 0x10 * i)
}
pub const fn mmio_ctrl(i: u16) -> DfReg<MmioCtrl::Register> {
    DfReg::new(0, 0x208 + 0x10 * i)
}
pub const fn io_base(i: u16) -> DfReg<IoBase::Register> {
    DfReg::new(0, 0x0c0 + 8 * i)
}
pub const fn io_limit(i: u16) -> DfReg<IoLimit::Register> {
    DfReg::new(0, 0x0c4 + 8 * i)
}
pub const VGA_EN: DfReg<VgaEn::Register> = DfReg::new(0, 0x080);
