// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SPI ROM controller (FCH::ITF::SPI at [`super::SPI_BASE`]) of every
//! FCH, the LPC ROM protect ranges, a reserved flash region through its
//! command engine ([`flash`]), and the whole flash read through SMN on
//! Qilian ([`smn`]).
//!
//! [`reset`] is PEI task af43, identical on all three SP3 generations at
//! the donor defaults (FCH_SP3 §3.1); speed and read mode are policy (§8,
//! Rome PCD 0xDB/0xDC/0xDD).
//!
//! The host shares the flash with the PSP (and on some boards a BMC).
//! The vendors' writers take no lock and ask no other master: they wait
//! for the command engine to be idle before they program a command
//! ([`engine_idle`]) and write only what the controller lets the host
//! write ([`WriteAccess`]). The PSP writes its own NV data through x86 SMM
//! (P2C 0x83..0x86), and takes the engine over only when x86 asks it to
//! (ROM Armor's EnterSmmOnlyMode), which fstart never does
//! (reports/research/SPI_ARBITRATION.md).
use super::{FchHw, SPI_BASE, lpc};
use crate::cpu::Silicon;
use crate::window::{At, Window, update};
#[cfg(feature = "spi-flash")]
pub mod flash;
pub mod smn;

use fstart_core::services::{Timer, poll_until};
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// SPIx00 FCH::ITF::SPI::SPICntrl0.
    pub Cntrl0 [
        BIT16 OFFSET(16) NUMBITS(1) [],
        READ_MODE_LOW OFFSET(18) NUMBITS(1) [],
        BIT19 OFFSET(19) NUMBITS(1) [],
        ILLEGAL_ACCESS OFFSET(21) NUMBITS(1) [],
        ACCESS_ROM_EN OFFSET(22) NUMBITS(1) [],
        HOST_ACCESS_ROM_EN OFFSET(23) NUMBITS(1) [],
        ARB_WAIT_COUNT OFFSET(24) NUMBITS(3) [],
        READ_MODE_HIGH OFFSET(29) NUMBITS(2) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    /// SPIx04 FCH::ITF::SPI::SPIRestrictedCmd: opcodes the controller
    /// refuses (0: unused).
    pub RestrictedCmd [
        CMD0 OFFSET(0) NUMBITS(8) [],
        CMD1 OFFSET(8) NUMBITS(8) [],
        CMD2 OFFSET(16) NUMBITS(8) [],
        CMD3 OFFSET(24) NUMBITS(8) []
    ],
    /// SPIx08 FCH::ITF::SPI::SPIRestrictedCmd2: one more, and three
    /// without an address.
    pub RestrictedCmd2 [
        CMD4 OFFSET(0) NUMBITS(8) [],
        WO_ADDR0 OFFSET(8) NUMBITS(8) [],
        WO_ADDR1 OFFSET(16) NUMBITS(8) [],
        WO_ADDR2 OFFSET(24) NUMBITS(8) []
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
    ],
    /// SPIx47 FCH::ITF::SPI::CmdTrig (write-1): the command engine runs.
    pub CmdTrigger [
        EXECUTE OFFSET(7) NUMBITS(1) []
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
pub const RESTRICTED_CMD: At<u32, RestrictedCmd::Register> = At::new(SPI_BASE + 0x04);
pub const RESTRICTED_CMD2: At<u32, RestrictedCmd2::Register> = At::new(SPI_BASE + 0x08);
pub const CNTRL1: At<u32, Cntrl1::Register> = At::new(SPI_BASE + 0x0c);
pub const ALT_SPI_CS: At<u8, AltSpiCs::Register> = At::new(SPI_BASE + 0x1d);
pub const SPI100_ENABLE: At<u16, Spi100Enable::Register> = At::new(SPI_BASE + 0x20);
/// The byte of [`SPI100_ENABLE`] the Zeppelin vendor code writes.
pub const SPI100_ENABLE_BYTE: At<u8, Spi100EnableByte::Register> = At::new(SPI_BASE + 0x20);
pub const SPI100_SPEED: At<u16, Spi100Speed::Register> = At::new(SPI_BASE + 0x22);
pub const SPI100_HOST_PREFETCH: At<u16, Spi100HostPrefetch::Register> = At::new(SPI_BASE + 0x2c);
pub const CMD_TRIGGER: At<u8, CmdTrigger::Register> = At::new(SPI_BASE + 0x47);
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

/// How long [`engine_idle`] waits: Dell's SpiFlashCommonLib gives up
/// after 100 polls 100 µs apart (`WaitSpiBusReady`); GIGABYTE's
/// FchMmioSpiAccessLib waits without bound (SPI_ARBITRATION §2.1).
pub const IDLE_TIMEOUT_US: u64 = 10_000;

/// Whether the command engine is free within [`IDLE_TIMEOUT_US`]: no
/// command triggered (SPIx47\[Execute\]) and the bus idle
/// (SPIx4C\[SpiBusy\]). Both Venice donors wait for this before they
/// program a command, GIGABYTE for both bits, Dell for SpiBusy.
pub fn engine_idle<W: Window>(spi: &W, clock: &dyn Timer) -> bool {
    poll_until(clock, IDLE_TIMEOUT_US, || {
        let triggered = spi.at(CMD_TRIGGER).is_set(CmdTrigger::EXECUTE);
        let busy = spi.at(STATUS).is_set(Status::BUSY);
        (!triggered && !busy).then_some(())
    })
    .is_some()
}

/// What the controller lets the host's command engine write, read once
/// before a writer binds the engine. No donor reads it before it writes;
/// these are the controls PPR 55570 §9.1.7.3.1 lists to block host
/// writes, and what the PSP or the ABL may have left (SPI_ARBITRATION §4).
#[derive(Debug, Clone, Copy)]
pub struct WriteAccess {
    cntrl0: LocalRegisterCopy<u32, Cntrl0::Register>,
    /// SPIx04 and SPIx08, byte by byte.
    restricted: [u8; 8],
    alt_spi_cs: LocalRegisterCopy<u8, AltSpiCs::Register>,
    spi_base: LocalRegisterCopy<u32, lpc::SpiBase::Register>,
    protect: [LocalRegisterCopy<u32, RomProtect::Register>; 4],
}

/// Why the host may not write the flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// The SPI registers read all-ones, or D14F3xA0 does not put them at
    /// [`SPI_BASE`].
    NotDecoded,
    /// D14F3xA0\[PspSpiMmioSel\]: the window shows the PSP's registers.
    PspRegisters,
    /// SPIx04/08 restricts this opcode.
    Restricted(u8),
    /// ROM protect range n write-protects and SpiProtectEn0 applies it.
    RomProtect(u8),
}

impl WriteAccess {
    pub fn read<H: FchHw>(hw: &H) -> Self {
        let spi = hw.mmio();
        let lpc = hw.lpc();
        let first = spi.at(RESTRICTED_CMD).get().to_le_bytes();
        let second = spi.at(RESTRICTED_CMD2).get().to_le_bytes();
        let mut restricted = [0; 8];
        restricted[..4].copy_from_slice(&first);
        restricted[4..].copy_from_slice(&second);
        Self {
            cntrl0: spi.at(CNTRL0).extract(),
            restricted,
            alt_spi_cs: spi.at(ALT_SPI_CS).extract(),
            spi_base: lpc.at(lpc::SPI_BASE_ADDRESS).extract(),
            protect: core::array::from_fn(|i| lpc.at(rom_protect(i as u8)).extract()),
        }
    }

    /// Whether SPIx04/08 restricts `opcode` (0 marks an unused slot).
    pub fn restricts(&self, opcode: u8) -> bool {
        opcode != 0 && self.restricted.contains(&opcode)
    }

    /// Why the host may not send `opcodes` to write flash
    /// `[offset, offset + size)`, if it may not. A write-protected range
    /// blocks the write when it overlaps the extent read either as flash
    /// offsets or as the low 24 bits of a ROM2 host address: which one the
    /// FCH compares is not established for every FCH (SPI_ARBITRATION §4),
    /// so a range away from the extent under both readings does not stop
    /// a writer that worked before the check existed.
    pub fn blocked(&self, opcodes: &[u8], offset: u32, size: u32) -> Option<Blocked> {
        if self.cntrl0.get() == u32::MAX || self.spi_base.read(lpc::SpiBase::BASE) != SPI_BASE >> 8
        {
            return Some(Blocked::NotDecoded);
        }
        if self.spi_base.is_set(lpc::SpiBase::PSP_SPI_MMIO_SEL) {
            return Some(Blocked::PspRegisters);
        }
        if let Some(&opcode) = opcodes.iter().find(|&&op| self.restricts(op)) {
            return Some(Blocked::Restricted(opcode));
        }
        if self.alt_spi_cs.is_set(AltSpiCs::PROTECT_EN0) {
            let start = u64::from(offset);
            let end = start + u64::from(size);
            let page = |a: u64| a & 0xff_ffff;
            let range = self.protect.iter().position(|r| {
                if !r.is_set(RomProtect::WRITE_PROTECT) {
                    return false;
                }
                let unit: u64 = if r.is_set(RomProtect::UNIT_64K) {
                    64 << 10
                } else {
                    4 << 10
                };
                let base = u64::from(r.read(RomProtect::BASE)) << 12;
                let len = (u64::from(r.read(RomProtect::RANGE)) + 1) * unit;
                let overlaps = |b: u64| b < end && start < b + len;
                // Flash offsets, or the ROM2 view: the same offsets
                // within the 16 MiB page, against any page of the extent.
                overlaps(base)
                    || (start >> 24..=(end - 1) >> 24).any(|p| overlaps((p << 24) | page(base)))
            });
            if let Some(index) = range {
                return Some(Blocked::RomProtect(index as u8));
            }
        }
        None
    }

    /// SPIx00, for the log: SpiAccessRomEn and SpiHostAccessRomEn cleared
    /// freeze the restricted opcodes.
    pub fn cntrl0(&self) -> u32 {
        self.cntrl0.get()
    }
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

    const WRITE: [u8; 4] = [0x06, 0x05, 0x02, 0x20];

    /// An FCH as the PSP hands it over: SPI BAR at its fixed base,
    /// SpiAccessRomEn/SpiHostAccessRomEn set, nothing restricted.
    fn handed_over() -> crate::fch::model::TestFch {
        let hw = fch();
        hw.mmio.put(SPI_BASE, 4, 0x00c0_0000);
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0xa0), SPI_BASE | 0x2);
        hw
    }

    #[test]
    fn writes_allowed_as_handed_over() {
        let access = WriteAccess::read(&handed_over());
        assert_eq!(access.blocked(&WRITE, 0x00ff_0000, 0x1000), None);
        assert!(!access.restricts(0));
    }

    #[test]
    fn undecoded_or_moved_registers_block_writes() {
        let hw = handed_over();
        hw.mmio.put(SPI_BASE, 4, u32::MAX);
        assert_eq!(
            WriteAccess::read(&hw).blocked(&WRITE, 0x00ff_0000, 0x1000),
            Some(Blocked::NotDecoded)
        );
        let hw = handed_over();
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0xa0), 0xfed1_0002);
        assert_eq!(
            WriteAccess::read(&hw).blocked(&WRITE, 0x00ff_0000, 0x1000),
            Some(Blocked::NotDecoded)
        );
        let hw = handed_over();
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0xa0), SPI_BASE | 0x12);
        assert_eq!(
            WriteAccess::read(&hw).blocked(&WRITE, 0x00ff_0000, 0x1000),
            Some(Blocked::PspRegisters)
        );
    }

    #[test]
    fn restricted_opcodes_block_writes() {
        // Dell's write lock (FchInitDxeRs 0x11e0): WRSR, SE, BE, PP and
        // WREN, chip erase.
        let hw = handed_over();
        hw.mmio.put(SPI_BASE + 4, 4, 0x02d8_2001);
        hw.mmio.put(SPI_BASE + 8, 4, 0x0060_c706);
        let access = WriteAccess::read(&hw);
        assert_eq!(
            access.blocked(&WRITE, 0x00ff_0000, 0x1000),
            Some(Blocked::Restricted(0x06))
        );
        assert!(access.restricts(0xc7) && access.restricts(0xd8));
        assert!(!access.restricts(0x03) && !access.restricts(0x9f));
        // Reads stay allowed.
        assert_eq!(access.blocked(&[0x03, 0x9f], 0x00ff_0000, 0x1000), None);
    }

    #[test]
    fn write_protected_ranges_block_writes_once_enabled() {
        let hw = handed_over();
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0x58), 0x00ff_0000 | 1 << 10 | 1 << 8 | 0x0f);
        // Programmed, not applied.
        assert_eq!(
            WriteAccess::read(&hw).blocked(&WRITE, 0x00ff_0000, 0x1000),
            None
        );
        hw.mmio.put(SPI_BASE + 0x1d, 1, 0x08);
        assert_eq!(
            WriteAccess::read(&hw).blocked(&WRITE, 0x00ff_0000, 0x1000),
            Some(Blocked::RomProtect(2))
        );
        // Range 2 covers 0x00FF_0000 + 16 x 64 KiB: the same offsets in the
        // upper page are covered through ROM2's view, the store below is not.
        let access = WriteAccess::read(&hw);
        assert_eq!(
            access.blocked(&WRITE, 0x01ff_8000, 0x1000),
            Some(Blocked::RomProtect(2))
        );
        assert_eq!(access.blocked(&WRITE, 0x00fe_0000, 0x1_0000), None);
    }

    #[test]
    fn engine_idle_waits_for_trigger_and_busy_within_bound() {
        use fstart_core::services::ManualTimer;
        let hw = fch();
        // Execute still set twice, then SpiBusy once.
        hw.mmio
            .0
            .borrow_mut()
            .reads
            .insert(SPI_BASE + 0x47, std::vec![0x80, 0x80, 0]);
        hw.mmio
            .0
            .borrow_mut()
            .reads
            .insert(SPI_BASE + 0x4c, std::vec![0x8000_0000, 0]);
        let clock = ManualTimer::ticking();
        assert!(engine_idle(&hw.mmio, &clock));
        // A bus that stays busy is given up on at the bound, not later.
        hw.mmio.0.borrow_mut().reads.clear();
        hw.mmio.put(SPI_BASE + 0x4c, 4, 0x8000_0000);
        let clock = ManualTimer::ticking();
        assert!(!engine_idle(&hw.mmio, &clock));
        let waited = fstart_core::services::Timer::timestamp_us(&clock);
        assert!((IDLE_TIMEOUT_US..IDLE_TIMEOUT_US + 10).contains(&waited));
    }
}
