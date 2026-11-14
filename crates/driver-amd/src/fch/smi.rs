// SPDX-License-Identifier: MIT OR Apache-2.0
//! FCH SMI block (FCH::SMI, ACPI-MMIO `0x200`) of every FCH: source status
//! and routing, EOS, SCI map, I/O traps and the SMI command port; and the
//! ACPI mode handshake of the die SoCs, in normal mode and in the SMM
//! handler.
//!
//! Status registers SmiStat0..4 are at `0x80..0x90`; each source bit `b` of
//! status register `k` has a 2-bit control at SmiCtl `0xA0 + 8k + 4(b/16)`,
//! bits `2(b%16)` (PPR 55570 FCH::SMI::SmiCtl0..9).
use super::acpimmio::{
    AcpiPorts, PM_ACPI_SMI_CMD, PM_PCI_CONTROL, PM_RESET_CONTROL1, PciControl, Pm1Control,
    ResetControl1, misc, smi_bank,
};
use crate::window::{At, Window};
use tock_registers::{
    fields::Field,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// SMI command port (PMx6A).
pub const APM_CNT: u16 = 0xb2;
/// SMI command values of the die SoCs' FADT.
pub const ACPI_ENABLE: u8 = 0xe1;
pub const ACPI_DISABLE: u8 = 0x1e;

register_bitfields![u32,
    /// SMIx08 and SMIx0C: SCI trigger polarity and level, bit per GEVENT.
    pub SciTrigger [
        GEVENTS OFFSET(0) NUMBITS(24) []
    ],
    pub SciLevel [
        GEVENTS OFFSET(0) NUMBITS(24) []
    ],
    /// SMIx98 FCH::SMI::SmiTrig0.
    pub Trigger0 [
        /// The GEVENT SMI trigger polarity, bit per GEVENT.
        GEVENT_HIGH OFFSET(0) NUMBITS(24) [],
        PSP OFFSET(25) NUMBITS(1) [],
        EOS OFFSET(28) NUMBITS(1) [],
        SMI_TIMER_SEL OFFSET(29) NUMBITS(1) [],
        /// SmiEnB: 1 stops SMI generation.
        SMI_DISABLE OFFSET(31) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    /// SMIx94: which status registers hold events.
    pub SmiPointer [
        SCI OFFSET(0) NUMBITS(1) [],
        STATUS OFFSET(1) NUMBITS(5) []
    ]
];

pub const SCI_TRIGGER: At<u32, SciTrigger::Register> = smi_bank::at(0x08);
pub const SCI_LEVEL: At<u32, SciLevel::Register> = smi_bank::at(0x0c);
pub const SCI_STATUS: At<u32> = smi_bank::at(0x10);
/// SMIx40 + n (FCH::SMI::SciMap): the SCI (GPE) GEVENT/source `n` raises.
pub const fn sci_map(n: u8) -> At<u8> {
    smi_bank::at(0x40 + n as u32)
}
/// SMIx80 + 4 * n, n < 5.
pub const fn status(n: usize) -> At<u32> {
    smi_bank::at(0x80 + 4 * n as u32)
}
const STATUS_COUNT: usize = 5;
pub const POINTER: At<u16, SmiPointer::Register> = smi_bank::at(0x94);
pub const TRIGGER0: At<u32, Trigger0::Register> = smi_bank::at(0x98);
/// SMIxA0 + 4 * n, n < 10: two bits per SMI source.
pub const fn control(n: usize) -> At<u32> {
    smi_bank::at(0xa0 + 4 * n as u32)
}
/// MISCxC0/C4: I/O trap addresses, MISCxCC\[3:0\] read/write select.
const fn io_trap_address(n: u8) -> At<u16> {
    misc::at(0xc0 + 2 * n as u32)
}
const TRAP_RW: At<u32> = misc::at(0xcc);

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

    const fn status(self) -> At<u32> {
        status(self.reg as usize)
    }

    /// The source's control register and the shift of its two bits.
    const fn control(self) -> (At<u32>, usize) {
        (
            control(2 * self.reg as usize + self.bit as usize / 16),
            2 * (self.bit as usize % 16),
        )
    }

    /// The source's bit in its status register (none for a bit past it;
    /// SMM images may not link a panic).
    pub const fn mask(self) -> u32 {
        match 1u32.checked_shl(self.bit as u32) {
            Some(mask) => mask,
            None => 0,
        }
    }
}

/// SMI sources, as the vendor dispatchers name them (FCH_SP3 §5 source
/// table, `SMITYPE_*`).
pub mod source {
    use super::Source;

    /// GEVENT 0..23 are SmiStat0\[0..23\].
    pub const GEVENTS: u8 = 24;
    pub const fn gevent(n: u8) -> Source {
        Source::new(0x80, n)
    }
    /// SmiStat1\[1\]: the PSP.
    pub const PSP: Source = Source::new(0x84, 1);
    /// SmiStat1\[19\] power button (released).
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
    acpi.at(reg)
        .modify(Field::<u32, ()>::new(3, shift).val(route as u32));
}

pub fn is_pending<W: Window>(acpi: &W, source: Source) -> bool {
    acpi.at(source.status()).get() & source.mask() != 0
}

/// Clear a source's status (write-1-to-clear).
pub fn clear<W: Window>(acpi: &W, source: Source) {
    acpi.at(source.status()).set(source.mask());
}

/// The five status registers.
pub fn statuses<W: Window>(acpi: &W) -> [u32; STATUS_COUNT] {
    core::array::from_fn(|k| acpi.at(status(k)).get())
}

/// End of SMI: re-arm SMI generation (SmiTrig0\[Eos\]).
pub fn eos<W: Window>(acpi: &W) {
    let trigger = acpi.at(TRIGGER0);
    trigger.modify(Trigger0::EOS::SET);
    // Drain the posted MMIO write before RSM permits another SMI request.
    let _ = trigger.get();
}

/// Dispatcher init (FCH_SP3 §5): SMIx98 = (x & 6FFFFFFF) | EOS, i.e. SMI
/// enabled and EOS set.
pub fn init<W: Window>(acpi: &W) {
    acpi.at(TRIGGER0)
        .modify(Trigger0::SMI_DISABLE::CLEAR + Trigger0::EOS::SET);
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
    acpi.at(sci_map(index)).set(gpe);
}

/// Program I/O trap `n` (0..4) for `port`, on writes or reads, routed to SMI.
pub fn io_trap<W: Window>(acpi: &W, n: u8, port: u16, on_write: bool) {
    assert!(n < 4);
    acpi.at(io_trap_address(n)).set(port);
    let rw = acpi.at(TRAP_RW);
    rw.set((rw.get() & !(1 << n)) | u32::from(on_write) << n);
    set_route(acpi, source::io_trap(n), Route::Smi);
}

/// Raise a software SMI by writing `value` to the SMI command port.
pub fn software_smi<I: Window>(io: &I, value: u8) {
    io.write8(APM_CNT.into(), value);
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

/// Clear every SMI status (no event status: GEVENT SCIs stay).
pub fn clear_all<W: Window>(acpi: &W) {
    let sci = acpi.at(SCI_STATUS);
    sci.set(sci.get());
    for n in 0..STATUS_COUNT {
        let status = acpi.at(status(n));
        status.set(status.get());
    }
}

/// What an FCH's SMIs are set up with: its ACPI blocks, the SMI command
/// values of ACPI enable and disable (the FADT's), the GEVENTs the board
/// handles in SMM (bit n = GEVENTn) and the FCH's generation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub ports: AcpiPorts,
    pub acpi_enable: u8,
    pub acpi_disable: u8,
    pub gevents: u32,
    pub version: super::sx::Version,
}

/// Stop SMI generation (SmiEnB) while TSEG and SMBASE change.
pub fn disable<W: Window>(acpi: &W) {
    acpi.at(TRIGGER0).modify(Trigger0::SMI_DISABLE::SET);
}

/// The SMI command port, SLP_TYP and power button SMIs and the board's
/// GEVENT SMIs; the FCH stays in legacy mode until the OS sends ACPI
/// enable, the power button powering off through SMM. Then SMIs on.
pub fn enable<H: super::FchHw>(fch: &H, config: &Config) {
    let acpi = fch.acpi();
    acpi.at(PM_ACPI_SMI_CMD).set(APM_CNT);
    set_route(acpi, source::SMI_CMD, Route::Smi);
    // SMI on SlpTyp requires sending SMI before completion response of
    // the I/O write; the SMM handler takes over the sleep entry.
    acpi.at(PM_PCI_CONTROL)
        .modify(PciControl::FORCE_SLPSTATE_RETRY::SET);
    acpi.at(PM_RESET_CONTROL1)
        .modify(ResetControl1::SLP_TYPE_EN::CLEAR);
    set_route(acpi, source::SLP_TYP, Route::Smi);
    (0..source::GEVENTS)
        .filter(|gevent| config.gevents & 1 << gevent != 0)
        .for_each(|gevent| set_route(acpi, source::gevent(gevent), Route::Smi));
    super::sx::acpi_mode(fch, &config.ports, false);
    clear_all(acpi);
    init(acpi);
}

/// The sources of one SMI, read once at its start: the status registers
/// the SMI pointer names. An SCI status is cleared at once.
pub struct Pending {
    status: [u32; STATUS_COUNT],
}

impl Pending {
    #[inline(always)]
    pub fn take<W: Window>(acpi: &W) -> Self {
        let pointer = acpi.at(POINTER).extract();
        if pointer.is_set(SmiPointer::SCI) {
            let sci = acpi.at(SCI_STATUS);
            sci.set(sci.get());
        }
        let pending = pointer.read(SmiPointer::STATUS);
        let mut status = [0; STATUS_COUNT];
        for (index, status) in status.iter_mut().enumerate() {
            if pending & 1 << index != 0 {
                *status = acpi.at(super::smi::status(index)).get();
            }
        }
        Self { status }
    }

    /// Whether `source` raised this SMI.
    pub fn has(&self, source: Source) -> bool {
        self.status
            .get(usize::from(source.reg))
            .is_some_and(|status| status & source.mask() != 0)
    }

    /// The GEVENTs that raised this SMI, bit n = GEVENTn.
    pub fn gevents(&self) -> u32 {
        self.status[0] & ((1 << source::GEVENTS) - 1)
    }

    /// Clear the sources read and end the SMI (EOS).
    #[inline(always)]
    pub fn finish<W: Window>(&self, acpi: &W) {
        for (index, status) in self.status.iter().enumerate() {
            if *status != 0 {
                acpi.at(super::smi::status(index)).set(*status);
            }
        }
        eos(acpi);
    }
}

/// A SoC's SMM handler: what [`dispatch`] does with the FCH's sources.
pub trait Handler {
    /// The SMI command port value `command` (the FADT's ACPI enable and
    /// disable included).
    fn command(&self, command: u8);
    /// The SoC's and the board's own sources.
    fn sources(&self, pending: &Pending);
    /// The power button while it raises an SMI (the OS does not own ACPI).
    fn power_button(&self);
    /// The OS wrote sleep state `slp_typ` to PM1_CNT. Returns if the state
    /// isn't entered.
    fn sleep(&self, slp_typ: u8);
}

/// The FCH SMI dispatcher of every SoC: read the sources, hand the SMI
/// command, the SoC's sources, the power button and the sleep request to
/// `handler`, then clear them and end the SMI.
#[inline(always)]
pub fn dispatch<A: Window, I: Window>(acpi: &A, io: &I, ports: &AcpiPorts, handler: &impl Handler) {
    let pending = Pending::take(acpi);
    if pending.has(source::SMI_CMD) {
        handler.command(io.read8(APM_CNT.into()));
    }
    handler.sources(&pending);
    if pending.has(source::PWR_BTN) {
        handler.power_button();
    }
    if pending.has(source::SLP_TYP) {
        let slp_typ = io
            .at(At::<u16, Pm1Control::Register>::new(ports.control.into()))
            .read(Pm1Control::SLP_TYP) as u8;
        handler.sleep(slp_typ);
    }
    pending.finish(acpi);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::ACPI_MMIO;

    #[test]
    fn sources_map_to_control_slots() {
        let slot = |source: Source| (source.control().0.offset, source.control().1);
        let control = |n| control(n).offset;
        // SMI_REG_CONTROL0 + 4 * (n / 16), 2 bits each.
        assert_eq!(slot(source::SMI_CMD), (control(4), 22));
        assert_eq!(slot(source::SLP_TYP), (control(4), 2));
        assert_eq!(slot(source::PWR_BTN), (control(3), 6));
        assert_eq!(slot(source::PSP), (control(2), 2));
        assert_eq!(slot(source::gevent(11)), (control(0), 22));
    }

    #[test]
    fn the_psp_smm_info_names_these_smi_registers() {
        let info = crate::psp::command::SmmInfo::new(0, 0, 0, 0);
        let trigger0 = u64::from(ACPI_MMIO + TRIGGER0.offset);
        let psp = Trigger0::PSP::SET.value;
        assert_eq!((info.trigger.address, info.trigger.mask), (trigger0, !psp));
        assert_eq!(info.trigger.value, psp);
        assert_eq!(info.smi_enable.mask, Trigger0::SMI_DISABLE::SET.value);
        assert_eq!(info.eos.value, Trigger0::EOS::SET.value);
        let (control, shift) = source::PSP.control();
        assert_eq!(
            info.psp_smi_control.address,
            u64::from(ACPI_MMIO + control.offset)
        );
        assert_eq!(
            (info.psp_smi_control.mask, info.psp_smi_control.value),
            (3 << shift, (Route::Smi as u32) << shift)
        );
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use crate::fch::model::Mem;

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
        acpi.put(TRIGGER0.offset, 4, 0xffff_ffff);
        init(&acpi);
        assert_eq!(acpi.get(TRIGGER0.offset, 4), 0x7fff_ffff);
        io_trap(&acpi, 1, 0xcf9, true);
        assert_eq!(acpi.get(0xec2, 2), 0xcf9);
        assert_eq!(acpi.get(0xecc, 4), 2);
    }
}
