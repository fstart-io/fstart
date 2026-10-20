//! FCH SMI block (FCH::SMI, ACPI-MMIO `0x200`): source status and routing,
//! EOS, SCI map, I/O traps, and the SW SMI command port.
//!
//! Status registers SmiStat0..4 are at `0x80..0x90`; each source bit `b` of
//! status register `k` has a 2-bit control at SmiCtl `0xA0 + 8k + 4(b/16)`,
//! bits `2(b%16)` (PPR 55570 FCH::SMI::SmiCtl0..9). This is the layer an SMM
//! handler uses; there is no dispatch framework here.

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::Window;
use super::acpimmio::{MISC, SMI, SMI_CMD};

pub const SCI_TRIG: u32 = SMI + 0x08;
pub const SCI_LEVEL: u32 = SMI + 0x0C;
pub const SCI_STATUS: u32 = SMI + 0x10;
/// SCI map: one byte per GEVENT/source at `0x40 + n` (FCH::SMI::SciMap).
pub const SCI_MAP: u32 = SMI + 0x40;
pub const STATUS0: u32 = SMI + 0x80;
pub const TRIG0: u32 = SMI + 0x98;
pub const CONTROL0: u32 = SMI + 0xA0;

/// MISCxC0/C4: I/O trap addresses, MISCxCC\[3:0\] read/write select.
pub const IO_TRAP_ADDR: u32 = MISC + 0xC0;
pub const TRAP_RW: u32 = MISC + 0xCC;

register_bitfields![u32,
    /// SMIx98 FCH::SMI::SmiTrig0.
    pub TRIG0 [
        EOS OFFSET(28) NUMBITS(1) [],
        SMI_TIMER_SEL OFFSET(29) NUMBITS(1) [],
        SMI_EN_B OFFSET(31) NUMBITS(1) []
    ]
];

/// An SMI source: status register `0x80 + 4·reg` bit `bit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub reg: u8,
    pub bit: u8,
}

impl Source {
    pub const fn new(status: u32, bit: u8) -> Self {
        Self {
            reg: ((status - 0x80) / 4) as u8,
            bit,
        }
    }

    const fn status(self) -> u32 {
        STATUS0 + 4 * self.reg as u32
    }

    const fn control(self) -> (u32, u32) {
        (
            CONTROL0 + 8 * self.reg as u32 + 4 * (self.bit as u32 / 16),
            2 * (self.bit as u32 % 16),
        )
    }

    /// The source's bit in its status register.
    pub const fn mask(self) -> u32 {
        1 << self.bit
    }
}

/// Sources the donors' dispatchers register (FCH_SP3 §5 source table).
pub mod source {
    use super::Source;

    /// SmiStat1\[19\] power button.
    pub const PWR_BTN: Source = Source::new(0x84, 19);
    /// SmiStat1\[21\]: the NBIO-side USB SMI (APU hardware assertion).
    pub const USB_NBIO: Source = Source::new(0x84, 21);
    /// SmiStat2\[1\] PM1_CNT SLP_TYP write.
    pub const SLP_TYP: Source = Source::new(0x88, 1);
    /// SmiStat2\[11\] SMI command port write.
    pub const SMI_CMD: Source = Source::new(0x88, 11);
    /// SmiStat2\[12\] USB SMI.
    pub const USB: Source = Source::new(0x88, 12);
    /// SmiStat4\[13\] CF9 write with bit 2 set.
    pub const CF9_WRITE: Source = Source::new(0x90, 13);
    /// SmiStat4\[14\]/\[15\] short/long timer (periodic SMI).
    pub const SHORT_TIMER: Source = Source::new(0x90, 14);
    pub const LONG_TIMER: Source = Source::new(0x90, 15);
    /// SmiStat4\[20 + n\] I/O trap n.
    pub const fn io_trap(n: u8) -> Source {
        Source::new(0x90, 20 + n)
    }
}

/// What a source raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Route {
    Disabled = 0,
    Smi = 1,
    Nmi = 2,
    Irq13 = 3,
}

pub fn set_route<W: Window>(acpi: &W, source: Source, route: Route) {
    let (reg, shift) = source.control();
    let r = acpi.reg::<u32, ()>(reg);
    r.set((r.get() & !(3 << shift)) | (route as u32) << shift);
}

pub fn is_pending<W: Window>(acpi: &W, source: Source) -> bool {
    acpi.reg::<u32, ()>(source.status()).get() & source.mask() != 0
}

/// Clear a source's status (write-1-to-clear).
pub fn clear<W: Window>(acpi: &W, source: Source) {
    acpi.reg::<u32, ()>(source.status()).set(source.mask());
}

/// The five status registers.
pub fn status<W: Window>(acpi: &W) -> [u32; 5] {
    core::array::from_fn(|k| acpi.read32(STATUS0 + 4 * k as u32))
}

/// End of SMI: re-arm SMI generation (SmiTrig0\[Eos\]).
pub fn eos<W: Window>(acpi: &W) {
    acpi.reg::<u32, TRIG0::Register>(TRIG0)
        .modify(TRIG0::EOS::SET);
}

/// Dispatcher init (FCH_SP3 §5): SMIx98 = (x & 6FFFFFFF) | EOS, i.e. SMI
/// enabled and EOS set.
pub fn init<W: Window>(acpi: &W) {
    acpi.reg::<u32, TRIG0::Register>(TRIG0)
        .modify(TRIG0::SMI_EN_B::CLEAR + TRIG0::EOS::SET);
}

/// The boot-time routing the donors leave in place (FCH_SP3 §5 "What
/// fstart's SMM handler must do"): SW SMI, SLP_TYP and power button to SMI,
/// then EOS.
pub fn enable_boot_sources<W: Window>(acpi: &W) {
    [source::SMI_CMD, source::SLP_TYP, source::PWR_BTN]
        .into_iter()
        .for_each(|s| set_route(acpi, s, Route::Smi));
    init(acpi);
}

/// Map GEVENT/source `index` to SCI (GPE) bit `gpe`.
pub fn map_sci<W: Window>(acpi: &W, index: u8, gpe: u8) {
    acpi.write8(SCI_MAP + u32::from(index), gpe);
}

/// Program I/O trap `n` (0..4) for `port`, on writes or reads, routed to SMI.
pub fn io_trap<W: Window>(acpi: &W, n: u8, port: u16, on_write: bool) {
    assert!(n < 4);
    let addr = acpi.reg::<u16, ()>(IO_TRAP_ADDR + 2 * u32::from(n));
    addr.set(port);
    let rw = acpi.reg::<u32, ()>(TRAP_RW);
    rw.set((rw.get() & !(1 << n)) | u32::from(on_write) << n);
    set_route(acpi, source::io_trap(n), Route::Smi);
}

/// Raise a software SMI by writing `value` to the SMI command port.
pub fn software_smi<I: Window>(io: &I, value: u8) {
    io.write8(SMI_CMD, value);
}

/// SW SMI values the donors register (FCH_SP3 §5; ACPI_SMM_SP3 §3).
pub mod sw {
    /// ACPI enable / disable (FADT ACPI_ENABLE/ACPI_DISABLE).
    pub const ACPI_ENABLE: u8 = 0xA0;
    pub const ACPI_DISABLE: u8 = 0xA1;
    /// S3 resume: restore PCI_INTR and rerun the env tasks (ZP, SSP).
    pub const S3_RESTORE: u8 = 0xB3;
    /// Rerun the late tasks after S3.
    pub const S3_LATE_RESTORE: u8 = 0xB4;
    /// Milan ReadyToBoot: PSP LockFCHReg.
    pub const FCH_LOCK: u8 = 0xD2;
}

#[cfg(test)]
mod tests {
    use super::super::model::Mem;
    use super::*;

    #[test]
    fn control_fields_match_the_donor_writes() {
        let acpi = Mem::default();
        set_route(&acpi, source::SLP_TYP, Route::Smi);
        set_route(&acpi, source::SMI_CMD, Route::Smi);
        set_route(&acpi, source::PWR_BTN, Route::Smi);
        // FCH_SP3 §3.3/§5: SMIxB0[3:2] = 1, B0[23:22] = 1, AC[7:6] = 1.
        assert_eq!(acpi.get(0x2b0, 4), 0x0040_0004);
        assert_eq!(acpi.get(0x2ac, 4), 0x40);
        set_route(&acpi, source::io_trap(0), Route::Smi);
        assert_eq!(acpi.get(0x2c4, 4), 0x100);
    }

    #[test]
    fn init_and_traps() {
        let acpi = Mem::default();
        acpi.put(TRIG0, 4, 0xffff_ffff);
        init(&acpi);
        assert_eq!(acpi.get(TRIG0, 4), 0x7fff_ffff);
        io_trap(&acpi, 1, 0xcf9, true);
        assert_eq!(acpi.get(0xec2, 2), 0xcf9);
        assert_eq!(acpi.get(0xecc, 4), 2);
    }
}
