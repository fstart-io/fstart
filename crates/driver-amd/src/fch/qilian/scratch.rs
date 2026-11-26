// SPDX-License-Identifier: MIT OR Apache-2.0
//! The bytes of a Qilian FCH that survive a warm reset, and who owns them
//! (reports/research/WARM_RESET.md §2.4, both donors).
//!
//! - **CMOS bank 1**: ports `72h/73h` (the x86 side of both donors), the
//!   same bytes as ACPI MMIO `+0x600` and SMN `0x02D0_1600` (the PSP and
//!   the ABL). AMD owns [`AMD_BANK1`]: the APCB-recovery handshake
//!   ([`APCB_RECOVERY`]), FchDxe's NVS pointer (08h–0Bh, a DXE fstart does
//!   not have) and the memory-restore bitmap ([`MEM_RESTORE`]). Nothing
//!   else of bank 1 is referenced by either donor's PSP, ABL or AMD x86
//!   modules; fstart's reset guard takes 40h–43h
//!   ([`super::reset::GUARD`]).
//! - **BIOS RAM**: `0xFED1_0000 + offset` for the x86 once PMx04\[0\] is
//!   set ([`enable_bios_ram`], both donors' `FchBiosRamWrite`); the PSP and
//!   the ABL address the same bytes at ACPI MMIO `+0x500`. AMD and the
//!   ABL own [`BIOS_RAM_RESERVED`]; 88h–8Bh is GIGABYTE/Insyde's one-shot
//!   reset marker, reserved too so that a port of that board's firmware
//!   does not collide. Whether BIOS RAM survives a CF9 0Eh, S5 or G3 is
//!   not known (WARM_RESET §4 Q2).
//!
//! CMOS bank 0 (`70h/71h`, [`crate::fch::rtc`]) is the RTC's and the
//! board's; its 7Fh is AMD's IDS debug-print control.

use core::ops::RangeInclusive;

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};

use crate::fch::acpimmio::{IsaControl, PM_ISA_CONTROL};
use crate::fch::{IndexedIo, indexed_io};
use crate::window::{At, Width, Window};
use tock_registers::interfaces::ReadWriteable;

/// CMOS bank 1 index port (data at `73h`).
pub const BANK1_INDEX: u16 = 0x72;
/// The bank-1 indices the x86 reaches through [`BANK1_INDEX`]: only
/// 00h–0Dh are witnessed; whether 80h and up reach more bytes is not
/// known (WARM_RESET §4 Q7).
pub const BANK1_LIMIT: u8 = 0x7f;

/// The APCB-recovery handshake, bank 1 06h (low) and 07h (high).
pub const APCB_RECOVERY: [u8; 2] = [0x06, 0x07];
/// FchDxe's pointer to its ACPI NVS block (`SBiD`), bank 1 08h–0Bh.
pub const FCH_NVS: RangeInclusive<u8> = 0x08..=0x0b;
/// The memory-restore bitmap ([`MemRestore`]).
pub const MEM_RESTORE: u8 = 0x0d;
/// Every bank-1 byte AMD's code (x86, PSP, ABL) uses.
pub const AMD_BANK1: [u8; 7] = [0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0d];

register_bitfields![u8,
    /// CMOS bank 1 0Dh: the ABL's memory-context-restore state (ABL
    /// `abl_04`/`abl_09`/`abl_0c`, AmdMemRestorePei; WARM_RESET §2.4).
    pub MemRestore [
        /// The ABL booted from a restored context; the x86 clears it at
        /// sign-off.
        RESTORE_BOOT OFFSET(0) NUMBITS(1) [],
        /// The x86 asks the ABL to discard the saved context.
        DISCARD_MEM_CONTEXT OFFSET(1) NUMBITS(1) [],
        /// The x86 saved the APOB to the flash.
        APOB_SAVED OFFSET(2) NUMBITS(1) [],
        /// Cleared by the ABL.
        BIT3 OFFSET(3) NUMBITS(1) []
    ]
];

/// Bank-1 byte `index` through `io`'s `72h/73h`.
pub fn bank1<W: Window, R: RegisterLongName>(io: &W, index: u8) -> IndexedIo<'_, W, R> {
    debug_assert!(index <= BANK1_LIMIT);
    indexed_io(io, BANK1_INDEX, index)
}

/// What bank 1 06h/07h say to the PSP (`[7] << 8 | [6]`, PSP `0x1B`
/// `0xb942`): A55Ah or 5555h valid, anything else a cleared CMOS, after
/// which the PSP recovers the APCB (`CmosClearTriggerApcbRecovery`, set in
/// both donors' APCBs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApcbHandshake {
    /// 55h/55h: the x86 ignores APCB recovery (GIGABYTE, PCD
    /// `PcdAmdPspApcbRecoveryEnable` 0).
    Ignored,
    /// 5Ah/A5h: the x86 restored the APCB (Dell, PCD 1), or the PSP wrote
    /// it itself.
    Restored,
    /// Anything else: the PSP takes it for a cleared CMOS.
    Invalid(u16),
}

impl ApcbHandshake {
    pub const IGNORED: u16 = 0x5555;
    pub const RESTORED: u16 = 0xa55a;

    pub const fn from_word(word: u16) -> Self {
        match word {
            Self::IGNORED => Self::Ignored,
            Self::RESTORED => Self::Restored,
            other => Self::Invalid(other),
        }
    }

    pub fn read<W: Window>(io: &W) -> Self {
        let byte = |index| bank1::<_, ()>(io, index).get();
        Self::from_word(u16::from_le_bytes([
            byte(APCB_RECOVERY[0]),
            byte(APCB_RECOVERY[1]),
        ]))
    }
}

/// The GIGABYTE handshake (AmdPspDxeV2Wh EndOfDxe `0x2258`, PCD
/// `PcdAmdPspApcbRecoveryEnable` 0): 55h to 06h and 07h on every boot,
/// "the x86 ignores APCB recovery". Returns what was there before.
///
/// Dell (PCD 1, `0x2560`) instead, when APOB `1/0x10 +0x4D` says the ABL
/// recovered the APCB, copies the backup APCB over the active instance,
/// writes 5Ah/A5h and warm-resets; fstart does not rewrite the APCB.
pub fn ignore_apcb_recovery<W: Window>(io: &W) -> ApcbHandshake {
    let before = ApcbHandshake::read(io);
    for index in APCB_RECOVERY {
        bank1::<_, ()>(io, index).set(0x55);
    }
    before
}

/// The x86 BIOS RAM window, with PMx04\[0\] set.
pub const BIOS_RAM_X86: u32 = 0xfed1_0000;
/// BIOS RAM offsets others own: the ABL's breakpoint handshake (00h),
/// FchSmmDispatcher's SMI snapshot (20h–33h, 40h), the PSP recovery
/// reason (78h–7Fh), GIGABYTE/Insyde's marker (88h–8Bh), the ABL's
/// `UMCF` record (98h–BBh).
pub const BIOS_RAM_RESERVED: [RangeInclusive<u8>; 6] = [
    0x00..=0x00,
    0x20..=0x33,
    0x40..=0x40,
    0x78..=0x7f,
    0x88..=0x8b,
    0x98..=0xbb,
];

/// A BIOS RAM register at `offset` in the host's physical MMIO window
/// ([`crate::fch::FchHw::mmio`]).
pub const fn bios_ram<T: Width, R: RegisterLongName>(offset: u8) -> At<T, R> {
    At::new(BIOS_RAM_X86 + offset as u32)
}

/// Whether `offset..offset + len` touches a byte of [`BIOS_RAM_RESERVED`].
pub fn bios_ram_reserved(offset: u8, len: u8) -> bool {
    (0..len).any(|i| {
        offset
            .checked_add(i)
            .is_none_or(|byte| BIOS_RAM_RESERVED.iter().any(|r| r.contains(&byte)))
    })
}

/// Decode BIOS RAM at [`BIOS_RAM_X86`] (PMx04\[0\], as the donors'
/// `FchBiosRamWrite` before each access).
pub fn enable_bios_ram<W: Window>(acpi: &W) {
    acpi.at(PM_ISA_CONTROL).modify(IsaControl::BIOS_RAM_EN::SET);
}

/// Whether PMx04\[0\] decodes BIOS RAM.
pub fn bios_ram_enabled<W: Window>(acpi: &W) -> bool {
    acpi.at(PM_ISA_CONTROL).is_set(IsaControl::BIOS_RAM_EN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::PM;
    use crate::fch::model::Mem;

    #[test]
    fn bank1_goes_through_72_73() {
        let io = Mem::ports();
        bank1::<_, MemRestore::Register>(&io, MEM_RESTORE).write(MemRestore::APOB_SAVED::SET);
        assert_eq!(io.writes(), vec![(0x72, 1, 0x0d), (0x73, 1, 0x04)]);
        assert_eq!(io.at(0x73, 0x0d), 0x04);
    }

    #[test]
    fn apcb_handshake_writes_55_and_reports_before() {
        let io = Mem::ports();
        io.set_at(0x73, 6, 0x5a);
        io.set_at(0x73, 7, 0xa5);
        assert_eq!(ApcbHandshake::read(&io), ApcbHandshake::Restored);
        assert_eq!(ignore_apcb_recovery(&io), ApcbHandshake::Restored);
        assert_eq!((io.at(0x73, 6), io.at(0x73, 7)), (0x55, 0x55));
        assert_eq!(ApcbHandshake::read(&io), ApcbHandshake::Ignored);
        io.set_at(0x73, 7, 0xff);
        assert_eq!(ApcbHandshake::read(&io), ApcbHandshake::Invalid(0xff55));
    }

    #[test]
    fn bios_ram_window_and_reserved_bytes() {
        let acpi = Mem::default();
        enable_bios_ram(&acpi);
        assert!(bios_ram_enabled(&acpi));
        assert_eq!(acpi.get(PM + 4, 4), 1);
        assert_eq!(bios_ram::<u32, ()>(0x78).offset, 0xfed1_0078);
        assert!(bios_ram_reserved(0x76, 4));
        assert!(!bios_ram_reserved(0x60, 0x18));
        assert!(bios_ram_reserved(0xfe, 4), "past the end");
        assert!(!bios_ram_reserved(0xc0, 0x3f));
    }
}
