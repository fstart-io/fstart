// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SPI controller registers of the FCH at [`super::SPI_BASE`].
use super::{Reg, SPI_BASE};
use tock_registers::register_bitfields;

register_bitfields![u32,
    pub Cntrl0 [
        READ_MODE_LOW OFFSET(18) NUMBITS(1) [],
        /// The Zeppelin vendor reset values: bit 19 and [26:24].
        BIT19 OFFSET(19) NUMBITS(1) [],
        BITS24 OFFSET(24) NUMBITS(3) [],
        READ_MODE_HIGH OFFSET(29) NUMBITS(2) []
    ],
    pub RestrictedCmd2 [BITS OFFSET(16) NUMBITS(6) []],
    pub AltSpiCs [BIT10 OFFSET(10) NUMBITS(1) []],
    pub Status [BUSY OFFSET(31) NUMBITS(1) []]
];
register_bitfields![u8,
    pub Spi100EnableByte [ENABLE OFFSET(0) NUMBITS(1) []]
];
register_bitfields![u16,
    pub Spi100Enable [ENABLE OFFSET(0) NUMBITS(1) []],
    pub Spi100Speed [
        TPM OFFSET(0) NUMBITS(4) [],
        ALTERNATE OFFSET(4) NUMBITS(4) [],
        FAST OFFSET(8) NUMBITS(4) [],
        NORMAL OFFSET(12) NUMBITS(4) [Mhz33 = 1]
    ],
    pub Spi100HostPrefetch [BIT14 OFFSET(14) NUMBITS(1) [], RD4DW_EN_HOST OFFSET(15) NUMBITS(1) []]
];

pub const CNTRL0: Reg<u32, Cntrl0::Register> = Reg::mmio(SPI_BASE);
pub const RESTRICTED_CMD2: Reg<u32, RestrictedCmd2::Register> = Reg::mmio(SPI_BASE + 0x0c);
pub const ALT_SPI_CS: Reg<u32, AltSpiCs::Register> = Reg::mmio(SPI_BASE + 0x1c);
pub const SPI100_ENABLE: Reg<u16, Spi100Enable::Register> = Reg::mmio(SPI_BASE + 0x20);
/// The same register as a byte, as the Zeppelin vendor code accesses it.
pub const SPI100_ENABLE_BYTE: Reg<u8, Spi100EnableByte::Register> = Reg::mmio(SPI_BASE + 0x20);
pub const SPI100_SPEED: Reg<u16, Spi100Speed::Register> = Reg::mmio(SPI_BASE + 0x22);
pub const SPI100_HOST_PREFETCH: Reg<u16, Spi100HostPrefetch::Register> = Reg::mmio(SPI_BASE + 0x2c);
pub const STATUS: Reg<u32, Status::Register> = Reg::mmio(SPI_BASE + 0x4c);
