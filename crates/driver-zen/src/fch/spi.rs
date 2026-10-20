//! The SPI ROM controller (FCH::ITF::SPI at `FEC1_0000h`) and the LPC ROM
//! protect ranges.
//!
//! [`reset`] is PEI task af43, identical on all three generations at the
//! donor defaults (FCH_SP3 §3.1); speed and read mode are policy (§8,
//! Rome PCD 0xDB/0xDC/0xDD).

use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

use super::lpc::{self, SPI_BASE};
use super::{FchHw, Window, update};
use crate::soc::Generation;

/// Physical base of the SPI controller (D14F3xA0 reset value).
pub const BASE: u32 = 0xFEC1_0000;

pub const CNTRL0: u32 = 0x00;
pub const CNTRL1: u32 = 0x0C;
pub const ALT_SPI_CS: u32 = 0x1D;
pub const SPI100_ENABLE: u32 = 0x20;
pub const SPI100_SPEED: u32 = 0x22;
pub const SPI100_HOST_PREFETCH: u32 = 0x2C;

register_bitfields![u32,
    /// SPIx00 FCH::ITF::SPI::SPICntrl0.
    pub CNTRL0 [
        BIT16 OFFSET(16) NUMBITS(1) [],
        READ_MODE_0 OFFSET(18) NUMBITS(1) [],
        BIT19 OFFSET(19) NUMBITS(1) [],
        ARB_WAIT_COUNT OFFSET(24) NUMBITS(3) [],
        READ_MODE_21 OFFSET(29) NUMBITS(2) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    /// SPIx0C FCH::ITF::SPI::SPICntrl1.
    pub CNTRL1 [
        WAIT_COUNT OFFSET(16) NUMBITS(6) []
    ],
    /// D14F3x50..5C FCH::ITF::LPC::RomProtect.
    pub ROM_PROTECT [
        RANGE OFFSET(0) NUMBITS(8) [],
        UNIT_64K OFFSET(8) NUMBITS(1) [],
        READ_PROTECT OFFSET(9) NUMBITS(1) [],
        WRITE_PROTECT OFFSET(10) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(20) []
    ]
];

register_bitfields![u16,
    /// SPIx22 FCH::ITF::SPI::SPI100SpeedCfg.
    pub SPEED [
        TPM OFFSET(0) NUMBITS(4) [],
        ALT OFFSET(4) NUMBITS(4) [],
        FAST OFFSET(8) NUMBITS(4) [],
        NORMAL OFFSET(12) NUMBITS(4) []
    ],
    /// SPIx2C FCH::ITF::SPI::SPI100HostPrefetchCfg.
    pub PREFETCH [
        HOST_BURST_EN OFFSET(14) NUMBITS(1) [],
        RD4DW_EN_HOST OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u8,
    /// SPIx1D FCH::ITF::SPI::AltSPICS.
    pub ALT_SPI_CS [
        WRITE_BUFFER_EN OFFSET(2) NUMBITS(1) [],
        PROTECT_EN0 OFFSET(3) NUMBITS(1) [],
        PROTECT_EN1 OFFSET(4) NUMBITS(1) [],
        PROTECT_LOCK OFFSET(5) NUMBITS(1) []
    ]
];

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
    M66 = 0,
    M33 = 1,
    M22 = 2,
    M16 = 3,
    M100 = 4,
    K800 = 5,
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
            normal_speed: Speed::M22,
            fast_speed: Speed::M22,
        }
    }
}

/// PEI reset task af43: SPI BAR, SPI100, speeds, prefetch, read mode.
pub fn reset<H: FchHw>(hw: &H, generation: Generation, policy: &SpiPolicy) {
    let lpc = hw.lpc();
    // SPI BAR at its reset value, keeping the enables (FCH_SP3 §3.1 af43).
    update::<u32, _>(&lpc, SPI_BASE, 0x013e_ffe0, BASE);
    let spi = hw.mmio();
    let cntrl0 = spi.reg::<u32, CNTRL0::Register>(BASE + CNTRL0);
    // BUSY is read-only; never write back a sampled 1 (nor reserved bit 16,
    // as the donor).
    cntrl0.modify(
        CNTRL0::BUSY::CLEAR
            + CNTRL0::BIT16::CLEAR
            + CNTRL0::ARB_WAIT_COUNT.val(7)
            + CNTRL0::BIT19::SET,
    );
    spi.reg::<u32, CNTRL1::Register>(BASE + CNTRL1)
        .modify(CNTRL1::WAIT_COUNT.val(0));
    update::<u8, _>(spi, BASE + SPI100_ENABLE, 0, 1);
    spi.reg::<u16, SPEED::Register>(BASE + SPI100_SPEED).modify(
        SPEED::NORMAL.val(policy.normal_speed as u16) + SPEED::FAST.val(policy.fast_speed as u16),
    );
    spi.reg::<u8, ALT_SPI_CS::Register>(BASE + ALT_SPI_CS)
        .modify(ALT_SPI_CS::WRITE_BUFFER_EN::CLEAR);
    spi.reg::<u16, PREFETCH::Register>(BASE + SPI100_HOST_PREFETCH)
        .modify(PREFETCH::HOST_BURST_EN::SET + PREFETCH::RD4DW_EN_HOST::CLEAR);
    let mode = policy.read_mode as u32;
    cntrl0.modify(
        CNTRL0::BUSY::CLEAR
            + CNTRL0::BIT16::CLEAR
            + CNTRL0::ARB_WAIT_COUNT.val(7)
            + CNTRL0::BIT19::SET
            + CNTRL0::READ_MODE_21.val(mode >> 1)
            + CNTRL0::READ_MODE_0.val(mode & 1),
    );
    // D14F3xBA: ZP sets bit 7, Huashan bits 8 then 7 (unnamed).
    match generation {
        Generation::Naples => update::<u16, _>(&lpc, lpc::LPC_BA, 0, 0x0080),
        Generation::Rome | Generation::Milan => {
            update::<u16, _>(&lpc, lpc::LPC_BA, 0, 0x0100);
            update::<u16, _>(&lpc, lpc::LPC_BA, 0, 0x0180);
        }
    }
}

/// One LPC ROM protect range (write-once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RomProtect {
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
pub fn protect<H: FchHw>(hw: &H, index: u8, range: &RomProtect) {
    assert!(index < 4 && (1..=256).contains(&range.size_64k));
    hw.lpc()
        .reg::<u32, ROM_PROTECT::Register>(0x50 + 4 * u32::from(index))
        .write(
            ROM_PROTECT::BASE.val(range.base >> 12)
                + ROM_PROTECT::UNIT_64K::SET
                + ROM_PROTECT::RANGE.val(u32::from(range.size_64k - 1))
                + ROM_PROTECT::READ_PROTECT.val(range.read.into())
                + ROM_PROTECT::WRITE_PROTECT.val(range.write.into()),
        );
    hw.mmio()
        .reg::<u8, ALT_SPI_CS::Register>(BASE + ALT_SPI_CS)
        .modify(ALT_SPI_CS::PROTECT_EN0::SET);
}

/// Lock the protection enables (AltSPICS\[SpiProtectLock\]).
pub fn lock<H: FchHw>(hw: &H) {
    hw.mmio()
        .reg::<u8, ALT_SPI_CS::Register>(BASE + ALT_SPI_CS)
        .modify(ALT_SPI_CS::PROTECT_LOCK::SET);
}

#[cfg(test)]
mod tests {
    use super::super::model::fch;
    use super::*;

    #[test]
    fn reset_matches_the_donor_defaults() {
        let hw = fch();
        let spi = &hw.mmio;
        spi.put(BASE, 4, 0x8001_0000);
        spi.put(BASE + 0x0c, 4, 0x0022_0000);
        spi.put(BASE + 0x22, 2, 0xd1d3);
        spi.put(BASE + 0x2c, 2, 0x8000);
        reset(&hw, Generation::Rome, &SpiPolicy::default());
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
