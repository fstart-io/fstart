// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SPI ROM controller (FCH::ITF::SPI at [`super::SPI_BASE`]) of every
//! FCH, the LPC ROM protect ranges, and a reserved flash region through its
//! command engine ([`flash`]).
//!
//! [`reset`] is PEI task af43, identical on all three SP3 generations at
//! the donor defaults (FCH_SP3 §3.1); speed and read mode are policy (§8,
//! Rome PCD 0xDB/0xDC/0xDD).
use super::{FchHw, SPI_BASE, lpc};
use crate::cpu::Silicon;
use crate::window::{At, Window, update};
#[cfg(feature = "spi-flash")]
pub mod flash;

use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// SPIx00 FCH::ITF::SPI::SPICntrl0.
    pub Cntrl0 [
        BIT16 OFFSET(16) NUMBITS(1) [],
        READ_MODE_LOW OFFSET(18) NUMBITS(1) [],
        BIT19 OFFSET(19) NUMBITS(1) [],
        ARB_WAIT_COUNT OFFSET(24) NUMBITS(3) [],
        READ_MODE_HIGH OFFSET(29) NUMBITS(2) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    /// SPIx0C FCH::ITF::SPI::SPICntrl1.
    pub Cntrl1 [
        WAIT_COUNT OFFSET(16) NUMBITS(6) []
    ],
    /// SPIx4C: the controller status Picasso's vendor code polls.
    pub Status [
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    /// D14F3x50..5C FCH::ITF::LPC::RomProtect.
    pub RomProtect [
        RANGE OFFSET(0) NUMBITS(8) [],
        UNIT_64K OFFSET(8) NUMBITS(1) [],
        READ_PROTECT OFFSET(9) NUMBITS(1) [],
        WRITE_PROTECT OFFSET(10) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(20) []
    ]
];
register_bitfields![u8,
    /// SPIx1D FCH::ITF::SPI::AltSPICS.
    pub AltSpiCs [
        WRITE_BUFFER_EN OFFSET(2) NUMBITS(1) [],
        PROTECT_EN0 OFFSET(3) NUMBITS(1) [],
        PROTECT_EN1 OFFSET(4) NUMBITS(1) [],
        PROTECT_LOCK OFFSET(5) NUMBITS(1) []
    ],
    /// SPIx20: SPI100 enable.
    pub Spi100EnableByte [
        ENABLE OFFSET(0) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    /// SPIx20: SPI100 enable.
    pub Spi100Enable [
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// SPIx22 FCH::ITF::SPI::SPI100SpeedCfg, one [`Speed`] per class.
    pub Spi100Speed [
        TPM OFFSET(0) NUMBITS(4) [],
        ALTERNATE OFFSET(4) NUMBITS(4) [],
        FAST OFFSET(8) NUMBITS(4) [],
        NORMAL OFFSET(12) NUMBITS(4) [Mhz33 = 1]
    ],
    /// SPIx2C FCH::ITF::SPI::SPI100HostPrefetchCfg.
    pub Spi100HostPrefetch [
        HOST_BURST_EN OFFSET(14) NUMBITS(1) [],
        RD4DW_EN_HOST OFFSET(15) NUMBITS(1) []
    ]
];

pub const CNTRL0: At<u32, Cntrl0::Register> = At::new(SPI_BASE);
pub const CNTRL1: At<u32, Cntrl1::Register> = At::new(SPI_BASE + 0x0c);
pub const ALT_SPI_CS: At<u8, AltSpiCs::Register> = At::new(SPI_BASE + 0x1d);
pub const SPI100_ENABLE: At<u16, Spi100Enable::Register> = At::new(SPI_BASE + 0x20);
/// The byte of [`SPI100_ENABLE`] the Zeppelin vendor code writes.
pub const SPI100_ENABLE_BYTE: At<u8, Spi100EnableByte::Register> = At::new(SPI_BASE + 0x20);
pub const SPI100_SPEED: At<u16, Spi100Speed::Register> = At::new(SPI_BASE + 0x22);
pub const SPI100_HOST_PREFETCH: At<u16, Spi100HostPrefetch::Register> = At::new(SPI_BASE + 0x2c);
pub const STATUS: At<u32, Status::Register> = At::new(SPI_BASE + 0x4c);
/// D14F3x50 + 4n: ROM protect range n.
const fn rom_protect(index: u8) -> At<u32, RomProtect::Register> {
    At::new(0x50 + 4 * index as u32)
}

/// SPI read modes (SPIx00 ReadMode\[2:0\]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReadMode {
    Normal33 = 0,
    Dual112 = 2,
    Quad114 = 3,
    Dual122 = 4,
    Quad144 = 5,
    Normal66 = 6,
    FastRead = 7,
}

/// SPI100 speed codes (SPIx22 nibbles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Speed {
    Mhz66 = 0,
    Mhz33 = 1,
    Mhz22 = 2,
    Mhz16 = 3,
    Mhz100 = 4,
    Khz800 = 5,
}
impl Speed {
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Khz800 => 0,
            Self::Mhz16 => 1,
            Self::Mhz22 => 2,
            Self::Mhz33 => 3,
            Self::Mhz66 => 4,
            Self::Mhz100 => 5,
        }
    }
    /// The slower of the two.
    pub const fn slower(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

/// SPI policy; `Default` is the donor default on all three generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiPolicy {
    pub read_mode: ReadMode,
    pub normal_speed: Speed,
    pub fast_speed: Speed,
}

impl Default for SpiPolicy {
    fn default() -> Self {
        Self {
            read_mode: ReadMode::FastRead,
            normal_speed: Speed::Mhz22,
            fast_speed: Speed::Mhz22,
        }
    }
}

/// PEI reset task af43: SPI BAR, SPI100, speeds, prefetch, read mode.
pub fn reset<H: FchHw>(hw: &H, generation: Silicon, policy: &SpiPolicy) {
    let lpc = hw.lpc();
    // SPI BAR at its reset value, keeping the enables (FCH_SP3 §3.1 af43).
    update::<u32, _>(&lpc, lpc::SPI_BASE_ADDRESS.offset, 0x013e_ffe0, SPI_BASE);
    let spi = hw.mmio();
    let cntrl0 = spi.at(CNTRL0);
    // BUSY is read-only; never write back a sampled 1 (nor reserved bit 16,
    // as the donor).
    cntrl0.modify(
        Cntrl0::BUSY::CLEAR
            + Cntrl0::BIT16::CLEAR
            + Cntrl0::ARB_WAIT_COUNT.val(7)
            + Cntrl0::BIT19::SET,
    );
    spi.at(CNTRL1).modify(Cntrl1::WAIT_COUNT.val(0));
    spi.at(SPI100_ENABLE_BYTE)
        .modify(Spi100EnableByte::ENABLE::SET);
    spi.at(SPI100_SPEED).modify(
        Spi100Speed::NORMAL.val(policy.normal_speed as u16)
            + Spi100Speed::FAST.val(policy.fast_speed as u16),
    );
    spi.at(ALT_SPI_CS).modify(AltSpiCs::WRITE_BUFFER_EN::CLEAR);
    spi.at(SPI100_HOST_PREFETCH)
        .modify(Spi100HostPrefetch::HOST_BURST_EN::SET + Spi100HostPrefetch::RD4DW_EN_HOST::CLEAR);
    let mode = policy.read_mode as u32;
    cntrl0.modify(
        Cntrl0::BUSY::CLEAR
            + Cntrl0::BIT16::CLEAR
            + Cntrl0::ARB_WAIT_COUNT.val(7)
            + Cntrl0::BIT19::SET
            + Cntrl0::READ_MODE_HIGH.val(mode >> 1)
            + Cntrl0::READ_MODE_LOW.val(mode & 1),
    );
    // D14F3xBA: ZP sets bit 7, Huashan bits 8 then 7 (unnamed).
    match generation {
        Silicon::Zp => update::<u16, _>(&lpc, lpc::ROM_DMA_CONTROL.offset, 0, 0x0080),
        Silicon::Ssp | Silicon::Gn => {
            update::<u16, _>(&lpc, lpc::ROM_DMA_CONTROL.offset, 0, 0x0100);
            update::<u16, _>(&lpc, lpc::ROM_DMA_CONTROL.offset, 0, 0x0180);
        }
    }
}

/// One LPC ROM protect range (write-once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectRange {
    /// Base, 4 KiB aligned.
    pub base: u32,
    /// Size in 64 KiB units (1..=256).
    pub size_64k: u16,
    pub read: bool,
    pub write: bool,
}

/// Program ROM protect register `index` (0..4) and enable the ranges.
/// Not on the donor default path (AmdSpiLockSmm runs at SmmReadyToLock and
/// was not modelled, FCH_SP3 §10); the integrator decides.
pub fn protect<H: FchHw>(hw: &H, index: u8, range: &ProtectRange) {
    assert!(index < 4 && (1..=256).contains(&range.size_64k));
    hw.lpc().at(rom_protect(index)).write(
        RomProtect::BASE.val(range.base >> 12)
            + RomProtect::UNIT_64K::SET
            + RomProtect::RANGE.val(u32::from(range.size_64k - 1))
            + RomProtect::READ_PROTECT.val(range.read.into())
            + RomProtect::WRITE_PROTECT.val(range.write.into()),
    );
    hw.mmio().at(ALT_SPI_CS).modify(AltSpiCs::PROTECT_EN0::SET);
}

/// Lock the protection enables (AltSPICS\[SpiProtectLock\]).
pub fn lock<H: FchHw>(hw: &H) {
    hw.mmio().at(ALT_SPI_CS).modify(AltSpiCs::PROTECT_LOCK::SET);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

    #[test]
    fn reset_matches_the_donor_defaults() {
        const BASE: u32 = SPI_BASE;
        let hw = fch();
        let spi = &hw.mmio;
        spi.put(BASE, 4, 0x8001_0000);
        spi.put(BASE + 0x0c, 4, 0x0022_0000);
        spi.put(BASE + 0x22, 2, 0xd1d3);
        spi.put(BASE + 0x2c, 2, 0x8000);
        reset(&hw, Silicon::Ssp, &SpiPolicy::default());
        // FCH_SP3 §3.1 af43: SPIx00 |= 670C0000 (BUSY not written back).
        assert_eq!(spi.get(BASE, 4), 0x670c_0000);
        assert_eq!(spi.get(BASE + 0x0c, 4), 0);
        assert_eq!(spi.get(BASE + 0x20, 1), 1);
        assert_eq!(spi.get(BASE + 0x22, 2), 0x22d3);
        assert_eq!(spi.get(BASE + 0x2c, 2), 0x4000);
        let ba = hw.pci.0.borrow().config[&(0, 0x14, 3, 0xb8)] >> 16;
        assert_eq!(ba & 0x180, 0x180);
    }
}
