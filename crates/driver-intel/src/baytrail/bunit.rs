//! B-unit (system memory arbiter) registers, IOSF port 0x03.

use super::iosf::{BUNIT, IosfReg};
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// Memory/IO boundary below 4 GiB, 128 MiB granular (address bits 31:27).
    pub BMBOUND [ ADDRESS OFFSET(27) NUMBITS(5) [] ],
    /// Memory boundary above 4 GiB: register bits 31:24 hold address bits
    /// 35:28.
    pub BMBOUND_HI [ ADDRESS OFFSET(24) NUMBITS(8) [] ],
    /// PCI MMCONFIG (ECAM) base.
    pub MMCONF [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(28) NUMBITS(4) []
    ],
    pub BMISC [
        /// Route the E segment (0xe0000) to DRAM.
        EDSEG_TO_DRAM OFFSET(0) NUMBITS(1) [],
        /// Route the F segment (0xf0000) to DRAM.
        FDSEG_TO_DRAM OFFSET(1) NUMBITS(1) [],
        DUAL_CHAN OFFSET(3) NUMBITS(2) [],
        /// PnP configuration (B0+ stepping).
        PNP OFFSET(5) NUMBITS(4) []
    ],
    pub BISOC [ PRIORITY OFFSET(0) NUMBITS(7) [] ],
    /// SMM range base and limit, 1 MiB granular; the limit is inclusive.
    pub SMRR [
        MB OFFSET(0) NUMBITS(16) [],
        /// Bits 31:30 of the limit register: range enable and lock.
        ENABLE OFFSET(30) NUMBITS(2) []
    ],
    /// Channel 0 address hashing and interleave.
    pub BC0AHASHCFG [
        ADDR_HASH_EN OFFSET(0) NUMBITS(1) [],
        RANK_INTLV_EN OFFSET(1) NUMBITS(1) [],
        /// Memory interface width: 1 = 64 bit.
        MEM_INT_WIDTH OFFSET(2) NUMBITS(1) [],
        RANK0_EN OFFSET(3) NUMBITS(1) [],
        RANK1_EN OFFSET(4) NUMBITS(1) [],
        RANK2_EN OFFSET(5) NUMBITS(1) [],
        RANK3_EN OFFSET(6) NUMBITS(1) [],
        /// Device width: x8 = 0, x16 = 1, x32 = 2.
        DIMM0_WIDTH OFFSET(7) NUMBITS(2) [],
        /// log2 of the rank size in 128 MiB units.
        DIMM0_DENSITY OFFSET(9) NUMBITS(3) [],
        DIMM1_WIDTH OFFSET(13) NUMBITS(2) [],
        DIMM1_DENSITY OFFSET(15) NUMBITS(3) []
    ],
    /// Bandwidth throttling.
    pub BTHCTRL [
        RANK_PICK_MASK OFFSET(0) NUMBITS(8) [],
        BW_CNT_UPD_EN OFFSET(8) NUMBITS(1) [],
        CNT_CORE_CLKS OFFSET(9) NUMBITS(1) [],
        CLR_BW_ON_RD OFFSET(10) NUMBITS(1) [],
        AGENT_THRT_EN OFFSET(16) NUMBITS(16) []
    ],
    /// Isolated memory region base or limit: the MiB address shifted left
    /// by 10; the limit carries the valid bit.
    pub BIMR [
        ADDRESS OFFSET(0) NUMBITS(31) [],
        VALID OFFSET(31) NUMBITS(1) []
    ]
];

/// DRAM bank address mask.
pub const BBANKMASK: IosfReg = BUNIT.reg(0x08);
/// DRAM row address mask.
pub const BROWMASK: IosfReg = BUNIT.reg(0x09);
/// DRAM rank address mask.
pub const BRANKMASK: IosfReg = BUNIT.reg(0x0a);
pub const BISOC_REG: IosfReg<BISOC::Register> = BUNIT.reg(0x11);
pub const BNOCACHE: IosfReg = BUNIT.reg(0x23);
pub const BMBOUND_REG: IosfReg<BMBOUND::Register> = BUNIT.reg(0x25);
pub const BMBOUND_HI_REG: IosfReg<BMBOUND_HI::Register> = BUNIT.reg(0x26);
pub const MMCONF_REG: IosfReg<MMCONF::Register> = BUNIT.reg(0x27);
pub const BMISC_REG: IosfReg<BMISC::Register> = BUNIT.reg(0x28);
/// SMM range control policy.
pub const SMRCP: IosfReg = BUNIT.reg(0x2b);
/// SMM range read access control.
pub const SMRRAC: IosfReg = BUNIT.reg(0x2c);
/// SMM range write access control.
pub const SMRWAC: IosfReg = BUNIT.reg(0x2d);
/// SMM range base (MiB).
pub const SMRRL: IosfReg<SMRR::Register> = BUNIT.reg(0x2e);
/// SMM range limit (MiB, inclusive).
pub const SMRRH: IosfReg<SMRR::Register> = BUNIT.reg(0x2f);
pub const BC0AHASHCFG_REG: IosfReg<BC0AHASHCFG::Register> = BUNIT.reg(0x30);
pub const BTHCTRL_REG: IosfReg<BTHCTRL::Register> = BUNIT.reg(0x3e);

/// Isolated memory region `n` (0 or 1): base, limit and the read and
/// write access-control masks.
pub const fn bimr_low(n: u32) -> IosfReg<BIMR::Register> {
    BUNIT.reg(0x80 + 4 * n)
}
pub const fn bimr_high(n: u32) -> IosfReg<BIMR::Register> {
    BUNIT.reg(0x81 + 4 * n)
}
pub const fn bimr_rac(n: u32) -> IosfReg {
    BUNIT.reg(0x82 + 4 * n)
}
pub const fn bimr_wac(n: u32) -> IosfReg {
    BUNIT.reg(0x83 + 4 * n)
}
/// IMR access mask: graphics only.
pub const IMR_AC_GFX: u32 = 0x600;
/// IMR access mask: the security engine only (host excluded).
pub const IMR_AC_SEC: u32 = 0x200;
