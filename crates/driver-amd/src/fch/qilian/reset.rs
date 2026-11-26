// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian resets: the vendor's recipes ([`reset`]), what the last reset
//! was ([`LastReset`]) and the guard that bounds the warm resets firmware
//! asks for ([`Guard`]) (reports/research/WARM_RESET.md §2.1, §2.3, §5).
//!
//! # Recipes
//!
//! A Venice warm reset is a reset of the whole SoC with the power kept on:
//! the PSP and the ABL run again (memory is trained again). What tells a
//! warm reset from a cold one is PMx10\[1\] (ToggleAllPwrGood) at the CF9h
//! write, not the CF9h value:
//!
//! | Kind | Vendor | Here |
//! |---|---|---|
//! | [`ResetKind::Warm`] | PEI `FchPeiReset2` warm: PMx10\[1\] = 0, CF9h = 06h; DXE/runtime warm: PMxC2 \|= 10h, CF9h = 06h | both: PMx10\[1\] = 0, PMxC2 \|= 10h, CF9h = 06h |
//! | [`ResetKind::Cold`] | ABL `AblFchReset` cold: PMx10\[1\] = 1, PMxC5 = 06h | PMx10\[1\] = 1, CF9h = 06h |
//! | [`ResetKind::Full`] | PEI and DXE cold: PMx10\[1\] = 1, CF9h = 0Eh | the same |
//!
//! PMxC2 \|= 10h is a byte read-modify-write of PMxC0\[23:16\]: it clears
//! bit 20 and every status bit of that byte already set (bits 19 and 22
//! among them), so that after the reset bit 19 means this CF9h 06h. Every
//! kind clears PMxD2\[6\] (Cf9RstDisable) first, which `take_over` and the
//! ABL clear too. One CF9h write, as the vendor (no 00h first).
//!
//! # The last reset
//!
//! PMxC0 keeps the resets since the last G3 (write-1-to-clear; the
//! vendor's ReadyToBoot clear, [`super::pm::pre_os`], leaves bits 19 and
//! 22), PM1_CNT's SLP_TYP the sleep state the platform woke from.
//! [`LastReset::read`] takes both before anything of fstart's writes
//! them; FchPei's after-G3 test is `PMxC0 & !(1 << 12) == 0`. Which bit a
//! PSP or ABL reset through PMxC5, or the SMU's sync-flood reset, sets is
//! not known (WARM_RESET §4 Q3).
//!
//! # The guard
//!
//! Neither donor bounds its firmware warm resets. fstart keeps a 4-byte
//! record in CMOS bank 1 [`GUARD`] (40h–43h, which no AMD, PSP or ABL code
//! of either donor references; [`super::scratch`]): `{F5h, reason, count,
//! !(F5h ^ reason ^ count)}`. A record that parses counts, whatever
//! PMxC0 says: the PSP's boot loader reads and writes PMxC0 on a reset
//! (WARM_RESET §2.1, §4 Q3), and a guard that trusted only a PMxC0 that
//! shows the CF9h 06h would fail open, into the vendor's unbounded loop,
//! if the PSP cleared it. [`Guard::arm`] writes the next record before
//! each firmware warm reset and refuses past [`Reason::limit`] resets for
//! the same reason in a row or [`TOTAL_LIMIT`] in all; the platform
//! erases it once the PSP took BootDone, so that the OS's resets never
//! count. A record a power loss left in the middle of a series (CMOS
//! keeps it) costs the next series at most its remaining resets, and that
//! boot's BootDone erases it: the guard fails safe both ways. A refusal is
//! never worse than the vendor's unbounded loop: the boot goes on.
//!
//! # The boot timer
//!
//! Both donors enable the FCH boot timer in PEI (`PcdBootTimerEnable` 1,
//! `FchHwAcpiEnableBootTimer`: [`PM_BOOT_TIMER`] `= (x & 77h) | 88h`,
//! [`PM_BOOT_TIMER_TYPE`]\[7\] = reset type 1), and no later write to it
//! was found in their DXE, SMM or NBIO modules: what stops it, and what
//! it does when it expires, is not known (WARM_RESET §4 Q5). Nothing here
//! enables it: a timer that resets a boot that runs long (2P training,
//! a slow payload) would be a second reset loop, driven by the FCH, that
//! the guard does not see.
//!
//! [`PM_BOOT_TIMER`]: crate::fch::acpimmio::PM_BOOT_TIMER
//! [`PM_BOOT_TIMER_TYPE`]: crate::fch::acpimmio::PM_BOOT_TIMER_TYPE

use core::ops::RangeInclusive;

use tock_registers::LocalRegisterCopy;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::scratch::{AMD_BANK1, bank1};
use crate::fch::acpimmio::{
    ACPI_PM1_CONTROL, Cf9, PM_D2, PM_RESET_CONTROL0, PM_RESET_STATUS, PM_RESET_STATUS2, Pm1Control,
    PmioDebug, RESET_PORT, ResetControl0, ResetStatus, reset_status2,
};
use crate::window::{Window, update};

/// What a reset keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    /// Power kept: CF9h 06h with PMx10\[1\] clear.
    Warm,
    /// The power goods toggled: CF9h 06h with PMx10\[1\] set (the ABL's).
    Cold,
    /// CF9h 0Eh with PMx10\[1\] set: the vendor x86 cold reset.
    Full,
}

impl ResetKind {
    /// The CF9h value.
    pub fn cf9(self) -> FieldValue<u8, Cf9::Register> {
        let hard = Cf9::SYS_RST::SET + Cf9::RST_CPU::SET;
        match self {
            Self::Warm | Self::Cold => hard,
            Self::Full => hard + Cf9::FULL_RST::SET,
        }
    }
}

/// Everything of a `kind` reset but the CF9h write.
pub fn prepare<A: Window>(acpi: &A, kind: ResetKind) {
    acpi.at(PM_D2).modify(PmioDebug::CF9_RST_DISABLE::CLEAR);
    let control = acpi.at(PM_RESET_CONTROL0);
    match kind {
        ResetKind::Warm => {
            control.modify(ResetControl0::TOGGLE_ALL_PWR_GOOD::CLEAR);
            let full = reset_status2(ResetStatus::CF9_FULL_RESET::SET);
            update::<u8, _>(acpi, PM_RESET_STATUS2.offset, 0, full);
        }
        ResetKind::Cold | ResetKind::Full => {
            control.modify(ResetControl0::TOGGLE_ALL_PWR_GOOD::SET)
        }
    }
}

/// [`prepare`], then the CF9h write: on hardware, the reset.
pub fn request<A: Window, I: Window>(acpi: &A, io: &I, kind: ResetKind) {
    prepare(acpi, kind);
    io.at(RESET_PORT).write(kind.cf9());
}

/// A `kind` reset of the platform ([`request`]); waits for it.
pub fn reset<A: Window, I: Window>(acpi: &A, io: &I, kind: ResetKind) -> ! {
    request(acpi, io, kind);
    loop {
        core::hint::spin_loop();
    }
}

/// What the platform went through before this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastReset {
    /// PMxC0 ([`ResetStatus`]).
    pub status: u32,
    /// PM1_CNT\[12:10\].
    pub slp_typ: u8,
}

/// [`LastReset`] as one cause, for the log: the first of power on, a
/// sleep state, a sync flood, CF9h 0Eh, CF9h 06h, KBC. The status bits
/// stick until a G3 or a write-1-to-clear (fstart's warm reset clears
/// PMxC0\[23:16\], the vendor's ReadyToBoot bit 20 and low ones), so an
/// older reset shows through a newer one that sets no bit (a reset
/// button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Power on from G3 (nothing in PMxC0 but bit 12).
    PowerOn,
    S3Resume,
    S4Resume,
    /// Woken from S5 (the OS's power off).
    S5Wake,
    /// The SMU's reset after a sync flood.
    SyncFlood,
    /// CF9h 0Eh.
    FullReset,
    /// CF9h 06h: fstart's warm reset, the OS's (FADT reset register) or
    /// the vendor's.
    WarmReset,
    /// A keyboard-controller reset.
    KbcReset,
    /// None of the bits known (a reset button, a PSP or SMU reset, ...).
    Other,
}

impl Cause {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PowerOn => "power on (G3)",
            Self::S3Resume => "S3 resume",
            Self::S4Resume => "S4 resume",
            Self::S5Wake => "S5 wake",
            Self::SyncFlood => "sync flood",
            Self::FullReset => "CF9 0Eh (full) reset",
            Self::WarmReset => "CF9 06h (warm) reset",
            Self::KbcReset => "KBC reset",
            Self::Other => "other reset",
        }
    }
}

impl LastReset {
    /// PMxC0 and PM1_CNT's SLP_TYP (through the ACPI MMIO, whatever
    /// PMx62 decodes), before anything writes them.
    pub fn read<A: Window>(acpi: &A) -> Self {
        Self {
            status: acpi.at(PM_RESET_STATUS).get(),
            slp_typ: acpi.at(ACPI_PM1_CONTROL).read(Pm1Control::SLP_TYP) as u8,
        }
    }

    fn bits(&self) -> LocalRegisterCopy<u32, ResetStatus::Register> {
        LocalRegisterCopy::new(self.status)
    }

    /// FchPei's after-G3 test (both donors): nothing but bit 12.
    pub fn after_g3(&self) -> bool {
        self.status & !ResetStatus::BIT12::SET.value == 0
    }

    /// A CF9h 06h reset, not a full one.
    pub fn cf9_warm(&self) -> bool {
        self.bits().is_set(ResetStatus::CF9_RESET) && !self.cf9_full()
    }

    pub fn cf9_full(&self) -> bool {
        self.bits().is_set(ResetStatus::CF9_FULL_RESET)
    }

    pub fn kbc(&self) -> bool {
        self.bits().is_set(ResetStatus::KBC_RESET)
    }

    pub fn sync_flood(&self) -> bool {
        self.bits().is_set(ResetStatus::SYNC_FLOOD_RESET)
    }

    /// FchPei's boot modes from SLP_TYP (3 S3, 4 S4, 5 S5).
    pub fn s3(&self) -> bool {
        self.slp_typ == 3
    }

    pub fn s4(&self) -> bool {
        self.slp_typ == 4
    }

    pub fn s5_wake(&self) -> bool {
        self.slp_typ == 5
    }

    pub fn cause(&self) -> Cause {
        if self.after_g3() {
            Cause::PowerOn
        } else if self.s3() {
            Cause::S3Resume
        } else if self.s4() {
            Cause::S4Resume
        } else if self.s5_wake() {
            Cause::S5Wake
        } else if self.sync_flood() {
            Cause::SyncFlood
        } else if self.cf9_full() {
            Cause::FullReset
        } else if self.cf9_warm() {
            Cause::WarmReset
        } else if self.kbc() {
            Cause::KbcReset
        } else {
            Cause::Other
        }
    }
}

/// CMOS bank 1 bytes of the guard's record.
pub const GUARD: RangeInclusive<u8> = 0x40..=0x43;
const _: () = {
    let mut i = 0;
    while i < AMD_BANK1.len() {
        assert!(AMD_BANK1[i] < *GUARD.start() || AMD_BANK1[i] > *GUARD.end());
        i += 1;
    }
};
/// The record's first byte.
pub const MAGIC: u8 = 0xf5;
/// Firmware warm resets in a row, whatever their reasons.
pub const TOTAL_LIMIT: u8 = 3;

/// Why firmware warm-resets.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The PSP's C2P bit 29 (A/B flash layouts only, as the vendor).
    PspResetRequired = 1,
    /// The CXL APCB regions were rewritten (CxlV2Manager).
    CxlApcb = 2,
    /// The PSP's boot partition was switched (C2P 26h).
    PartitionSwitch = 3,
    /// A core-map change that takes effect after the reset.
    Downcore = 4,
    Other = 5,
}

impl Reason {
    /// Warm resets in a row for this reason: the PSP may ask twice (the
    /// ISH slots' `update_retries` is 2); the vendor's CXL APCB update
    /// takes two for a failing device (its failure count, then its
    /// exclusion, CXL_COMPLETE §2.1.3); every other reason converges
    /// after one.
    pub const fn limit(self) -> u8 {
        match self {
            Self::PspResetRequired | Self::CxlApcb => 2,
            _ => 1,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::PspResetRequired,
            2 => Self::CxlApcb,
            3 => Self::PartitionSwitch,
            4 => Self::Downcore,
            5 => Self::Other,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::PspResetRequired => "PSP ResetRequired",
            Self::CxlApcb => "CXL APCB update",
            Self::PartitionSwitch => "PSP partition switch",
            Self::Downcore => "downcore",
            Self::Other => "other",
        }
    }
}

/// The guard's record: the last firmware warm reset's reason and how many
/// were made in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub reason: Reason,
    pub count: u8,
}

impl Record {
    const fn check(reason: u8, count: u8) -> u8 {
        !(MAGIC ^ reason ^ count)
    }

    /// The bytes at [`GUARD`].
    pub const fn bytes(self) -> [u8; 4] {
        let reason = self.reason as u8;
        [MAGIC, reason, self.count, Self::check(reason, self.count)]
    }

    /// A record from [`GUARD`]'s bytes, if they hold one.
    pub fn parse([magic, reason, count, check]: [u8; 4]) -> Option<Self> {
        (magic == MAGIC && check == Self::check(reason, count) && count != 0)
            .then_some(())
            .and(Reason::from_code(reason))
            .map(|reason| Self { reason, count })
    }
}

/// Why [`Guard::arm`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// [`TOTAL_LIMIT`] firmware warm resets in a row already.
    Total { count: u8 },
    /// [`Reason::limit`] warm resets for this reason in a row already.
    Reason { reason: Reason, count: u8 },
    /// The record did not read back.
    Write,
}

/// The warm-reset guard as [`Guard::load`] found it at the start of the
/// boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Guard {
    live: Option<Record>,
}

impl Guard {
    /// The record at [`GUARD`], if one is there: it counts whatever the
    /// last reset was ([module documentation](self)).
    pub fn load<I: Window>(io: &I) -> Self {
        Self::from_bytes(read(io))
    }

    pub fn from_bytes(bytes: [u8; 4]) -> Self {
        Self {
            live: Record::parse(bytes),
        }
    }

    /// The record this boot continues, if any.
    pub const fn live(&self) -> Option<Record> {
        self.live
    }

    /// The record a warm reset for `reason` writes, or why it is refused.
    pub fn next(&self, reason: Reason) -> Result<Record, Refused> {
        let count = self.live.map_or(0, |r| r.count);
        let same = self
            .live
            .filter(|r| r.reason == reason)
            .map_or(0, |r| r.count);
        if count >= TOTAL_LIMIT {
            return Err(Refused::Total { count });
        }
        if same >= reason.limit() {
            return Err(Refused::Reason {
                reason,
                count: same,
            });
        }
        Ok(Record {
            reason,
            count: count + 1,
        })
    }

    /// Write the record of a warm reset for `reason` ([`Self::next`]) and
    /// read it back; the caller then resets ([`reset`] with
    /// [`ResetKind::Warm`]).
    pub fn arm<I: Window>(&self, io: &I, reason: Reason) -> Result<Record, Refused> {
        let record = self.next(reason)?;
        let bytes = record.bytes();
        for (index, byte) in GUARD.zip(bytes) {
            bank1::<_, ()>(io, index).set(byte);
        }
        if read(io) != bytes {
            return Err(Refused::Write);
        }
        Ok(record)
    }

    /// Erase the record: the magic, then the check byte.
    pub fn clear<I: Window>(io: &I) {
        bank1::<_, ()>(io, *GUARD.start()).set(0);
        bank1::<_, ()>(io, *GUARD.end()).set(0);
    }
}

fn read<I: Window>(io: &I) -> [u8; 4] {
    let mut bytes = [0; 4];
    for (index, byte) in GUARD.zip(bytes.iter_mut()) {
        *byte = bank1::<_, ()>(io, index).get();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::{ACPI, PM};
    use crate::fch::model::{Mem, fch};

    #[test]
    fn reset_warm_clears_toggle_and_full_status() {
        let hw = fch();
        hw.acpi.put(PM + 0x10, 1, 0x03);
        hw.acpi.put(PM + 0xc2, 1, 0x48);
        hw.acpi.put(PM + 0xd2, 1, 0x40);
        request(&hw.acpi, &hw.io, ResetKind::Warm);
        assert_eq!(hw.acpi.get(PM + 0x10, 1), 0x01);
        assert_eq!(hw.acpi.writes_to(PM + 0xc2), [0x58]);
        assert_eq!(hw.acpi.get(PM + 0xd2, 1), 0);
        assert_eq!(hw.io.writes_to(0xcf9), [0x06]);
    }

    #[test]
    fn cold_and_full_set_toggle() {
        for (kind, cf9) in [(ResetKind::Cold, 0x06), (ResetKind::Full, 0x0e)] {
            let hw = fch();
            hw.acpi.put(PM + 0x10, 1, 0x01);
            request(&hw.acpi, &hw.io, kind);
            assert_eq!(hw.acpi.get(PM + 0x10, 1), 0x03);
            assert!(hw.acpi.writes_to(PM + 0xc2).is_empty());
            assert_eq!(hw.io.writes_to(0xcf9), [cf9]);
        }
    }

    #[test]
    fn last_reset_predicates() {
        let last = |status, slp_typ| LastReset { status, slp_typ };
        assert!(last(0, 0).after_g3());
        assert_eq!(last(0, 0).cause(), Cause::PowerOn);
        assert!(last(0x1000, 0).after_g3());
        assert!(last(0x8_0000, 0).cf9_warm());
        assert_eq!(last(0x8_0000, 0).cause(), Cause::WarmReset);
        assert!(!last(0x18_0000, 0).cf9_warm());
        assert!(last(0x18_0000, 0).cf9_full());
        assert_eq!(last(0x18_0000, 0).cause(), Cause::FullReset);
        assert!(last(0x40_0000, 0).kbc());
        assert_eq!(last(0x40_0000, 0).cause(), Cause::KbcReset);
        assert!(last(0x8_0000, 5).s5_wake());
        assert_eq!(last(0x8_0000, 5).cause(), Cause::S5Wake);
        assert_eq!(last(0x8_0000, 3).cause(), Cause::S3Resume);
        assert_eq!(last(0x2, 0).cause(), Cause::Other);
        assert_eq!(last(1 << 27 | 1 << 19, 0).cause(), Cause::SyncFlood);
        // Through the registers: PMxC0 and PM1_CNT through the ACPI MMIO.
        let acpi = Mem::default();
        acpi.put(PM + 0xc0, 4, 0x48_0002);
        acpi.put(ACPI + 4, 2, 5 << 10 | 1);
        assert_eq!(LastReset::read(&acpi), last(0x48_0002, 5));
    }

    #[test]
    fn guard_bounds_psp_resets() {
        let io = Mem::ports();
        let boot = |io: &Mem| Guard::load(io);
        // First boot: nothing live, the record is written.
        let first = Guard::load(&io);
        assert_eq!(first.live(), None);
        let record = first.arm(&io, Reason::PspResetRequired).unwrap();
        assert_eq!(record.count, 1);
        assert_eq!(
            (0x40..=0x43).map(|i| io.at(0x73, i)).collect::<Vec<_>>(),
            [0xf5, 1, 1, !(0xf5 ^ 1 ^ 1) & 0xff]
        );
        // After the warm reset: live, a second allowed, the third refused.
        assert_eq!(
            boot(&io).arm(&io, Reason::PspResetRequired).unwrap().count,
            2
        );
        assert_eq!(
            boot(&io).arm(&io, Reason::PspResetRequired),
            Err(Refused::Reason {
                reason: Reason::PspResetRequired,
                count: 2
            })
        );
        // Another reason: one more, then the total stops it.
        assert_eq!(boot(&io).arm(&io, Reason::CxlApcb).unwrap().count, 3);
        assert_eq!(
            boot(&io).arm(&io, Reason::PspResetRequired),
            Err(Refused::Total { count: 3 })
        );
        // Cleared after BootDone.
        Guard::clear(&io);
        assert_eq!(boot(&io).live(), None);
        assert_eq!(io.writes_to(0x73).len(), 3 * 4 + 2);
    }

    #[test]
    fn guard_counts_on_the_cmos_record_alone() {
        // A PSP that cleared PMxC0 (it reads as after G3), an S5 wake or a
        // full reset between: the series still counts, so a PSP that resets
        // PMxC0 cannot make the guard fail open.
        let io = Mem::ports();
        let armed = Guard::load(&io).arm(&io, Reason::CxlApcb).unwrap();
        assert_eq!(armed.count, 1);
        assert_eq!(Guard::load(&io).live(), Some(armed));
        assert_eq!(Guard::load(&io).arm(&io, Reason::CxlApcb).unwrap().count, 2);
        assert_eq!(
            Guard::load(&io).arm(&io, Reason::CxlApcb),
            Err(Refused::Reason {
                reason: Reason::CxlApcb,
                count: 2
            })
        );
        // A record a power loss left: one series bounded by what it has
        // left, then erased at BootDone and the next series starts at 0.
        assert_eq!(Guard::load(&io).arm(&io, Reason::Other).unwrap().count, 3);
        assert_eq!(
            Guard::load(&io).arm(&io, Reason::PspResetRequired),
            Err(Refused::Total { count: 3 })
        );
        Guard::clear(&io);
        assert_eq!(Guard::load(&io).live(), None);
        assert_eq!(
            Guard::load(&io)
                .arm(&io, Reason::PspResetRequired)
                .unwrap()
                .count,
            1
        );
    }

    #[test]
    fn check_byte_catches_a_torn_write() {
        let good = Record {
            reason: Reason::CxlApcb,
            count: 1,
        }
        .bytes();
        assert!(Record::parse(good).is_some());
        let mut torn = good;
        torn[2] = 2;
        assert_eq!(Record::parse(torn), None);
        assert_eq!(Record::parse([0xf5, 9, 1, !(0xf5 ^ 9 ^ 1)]), None);
        assert_eq!(Guard::from_bytes(torn).live(), None);
        // A write that does not read back is refused.
        let io = Mem::ports();
        io.0.borrow_mut().reads.insert(0x73, vec![0]);
        assert_eq!(
            Guard::default().arm(&io, Reason::Other),
            Err(Refused::Write)
        );
    }

    #[test]
    fn guard_bytes_are_not_amds() {
        assert!(AMD_BANK1.iter().all(|i| !GUARD.contains(i)));
    }
}
