// SPDX-License-Identifier: MIT OR Apache-2.0
//! FCH SMI block (FCH::SMI, ACPI-MMIO `0x200`) of every FCH: source status
//! and routing, EOS, SCI map, I/O traps and the SMI command port; and the
//! ACPI mode handshake of the die SoCs, in normal mode and in the SMM
//! handler.
//!
//! Status registers SmiStat0..4 are at `0x80..0x90`; each source bit `b` of
//! status register `k` has a 2-bit control at SmiCtl `0xA0 + 8k + 4(b/16)`,
//! bits `2(b%16)` (PPR 55570 FCH::SMI::SmiCtl0..9).
use super::ACPI_MMIO;
use super::acpimmio::{
    AcpiConfigHigh, AcpiPorts, PM_ACPI_CONFIG_HIGH, PM_ACPI_SMI_CMD, PM_PCI_CONTROL,
    PM_RESET_CONTROL1, PciControl, ResetControl1, misc, smi_bank,
};
use crate::window::{At, Mmio, Ports, Reg, Window};
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
    ],
    pub Pm1Control [
        SCI_EN OFFSET(0) NUMBITS(1) [],
        SLP_TYP OFFSET(10) NUMBITS(3) [S5 = 5],
        SLP_EN OFFSET(13) NUMBITS(1) []
    ]
];
pub use super::acpimmio::{Pm1Enable, Pm1Status};

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

    /// The source's bit in its status register.
    pub const fn mask(self) -> u32 {
        1 << self.bit
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

/// One FCH's SMI registers, PM registers and ACPI blocks: the shared
/// operations of normal mode and the SMM handler.
struct Smi {
    acpi: Mmio,
    io: Ports,
    ports: FchSmmConfig,
}

impl Smi {
    /// # Safety
    /// The caller owns die 0's FCH SMI and PM registers and the ACPI blocks
    /// of `config`, decoded.
    unsafe fn new(config: &FchSmmConfig) -> Self {
        // SAFETY: the caller's contract.
        unsafe {
            Self {
                acpi: Mmio::new(ACPI_MMIO as usize),
                io: Ports::new(),
                ports: *config,
            }
        }
    }

    fn pm1_status(&self) -> Reg<&Ports, u16, Pm1Status::Register> {
        self.io.at(At::new(self.ports.pm1_event.into()))
    }
    fn pm1_enable(&self) -> Reg<&Ports, u16, Pm1Enable::Register> {
        self.io.at(At::new(u32::from(self.ports.pm1_event) + 2))
    }
    fn pm1_control(&self) -> Reg<&Ports, u16, Pm1Control::Register> {
        self.io.at(At::new(self.ports.pm1_control.into()))
    }
    fn gpe0_status(&self) -> Reg<&Ports, u32> {
        self.io.at(At::new(self.ports.gpe0.into()))
    }

    fn configure(&self, source: Source, route: Route) {
        set_route(&self.acpi, source, route);
    }

    /// Clear every SMI status (no event status: GEVENT SCIs stay).
    fn clear_all_status(&self) {
        let sci = self.acpi.at(SCI_STATUS);
        sci.set(sci.get());
        for n in 0..STATUS_COUNT {
            let status = self.acpi.at(status(n));
            status.set(status.get());
        }
    }

    /// Clear the PM1 and GPE status.
    fn clear_pm_gpe_status(&self) {
        let event = self.pm1_status();
        event.set(event.get());
        let gpe = self.gpe0_status();
        gpe.set(gpe.get());
    }

    #[cfg(feature = "smm")]
    fn sci_enabled(&self) -> bool {
        self.pm1_control().is_set(Pm1Control::SCI_EN)
    }

    fn set_sci(&self, enabled: bool) {
        self.pm1_control().modify(if enabled {
            Pm1Control::SCI_EN::SET
        } else {
            Pm1Control::SCI_EN::CLEAR
        });
    }

    /// Route the power button to SMI.
    fn enable_power_button_smi(&self) {
        self.pm1_status().write(Pm1Status::PWRBTN::SET);
        self.pm1_enable().modify(Pm1Enable::PWRBTN::SET);
        self.acpi
            .at(PM_ACPI_CONFIG_HIGH)
            .modify(AcpiConfigHigh::PWRBTN_EN::SET);
        self.configure(source::PWR_BTN, Route::Smi);
    }

    #[cfg(feature = "smm")]
    fn disable_power_button_smi(&self) {
        self.configure(source::PWR_BTN, Route::Disabled);
    }
}

/// The FCH as the SMI owner of normal mode, for
/// [`fstart_arch::x86::cpu::amd::smm::AmdSmm`].
pub struct FchSmi {
    config: FchSmmConfig,
    gevents: u32,
}

impl FchSmi {
    /// The FCH with the ACPI blocks `ports`; `gevents` are the GEVENT pins
    /// (bit n = GEVENTn) the board handles in SMM.
    /// # Safety
    /// ACPI MMIO and `ports` are decoded, and the caller owns the FCH SMI and
    /// PM registers and the ACPI blocks exclusively.
    pub const unsafe fn new(ports: AcpiPorts, gevents: u32) -> Self {
        Self {
            config: FchSmmConfig {
                pm1_event: ports.event,
                pm1_control: ports.control,
                gpe0: ports.gpe,
            },
            gevents: gevents & ((1 << source::GEVENTS) - 1),
        }
    }

    fn smi(&self) -> Smi {
        // SAFETY: the constructor's contract.
        unsafe { Smi::new(&self.config) }
    }
}

impl fstart_arch::x86::cpu::amd::smm::SmiControl for FchSmi {
    type HandlerConfig = FchSmmConfig;

    fn smm_handler_config(&self) -> FchSmmConfig {
        self.config
    }

    fn disable_smi(&self) {
        self.smi()
            .acpi
            .at(TRIGGER0)
            .modify(Trigger0::SMI_DISABLE::SET);
    }

    /// The SMI command port, SLP_TYP and power button SMIs, the board's
    /// GEVENT SMIs, then SMIs on.
    fn enable_smi(&self) {
        let smi = self.smi();
        smi.acpi.at(PM_ACPI_SMI_CMD).set(APM_CNT);
        smi.configure(source::SMI_CMD, Route::Smi);
        // SMI on SlpTyp requires sending SMI before completion response of
        // the I/O write; the SMM handler takes over the sleep entry.
        smi.acpi
            .at(PM_PCI_CONTROL)
            .modify(PciControl::FORCE_SLPSTATE_RETRY::SET);
        smi.acpi
            .at(PM_RESET_CONTROL1)
            .modify(ResetControl1::SLP_TYPE_EN::CLEAR);
        smi.configure(source::SLP_TYP, Route::Smi);
        (0..source::GEVENTS)
            .filter(|gevent| self.gevents & 1 << gevent != 0)
            .for_each(|gevent| smi.configure(source::gevent(gevent), Route::Smi));
        // Legacy mode until the OS sends ACPI_ENABLE; meanwhile the power
        // button powers off through SMM.
        smi.clear_pm_gpe_status();
        smi.set_sci(false);
        smi.enable_power_button_smi();
        smi.clear_all_status();
        smi.acpi
            .at(TRIGGER0)
            .modify(Trigger0::SMI_DISABLE::CLEAR + Trigger0::EOS::SET);
    }
}

/// Configuration the installer writes into SMRAM for the board's SMI handler
/// (`PendingSmi`): the ACPI blocks of the FADT.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FchSmmConfig {
    pm1_event: u16,
    pm1_control: u16,
    gpe0: u16,
}

#[cfg(feature = "smm")]
pub use handler::{PendingSmi, Sleep, handle};

#[cfg(feature = "smm")]
mod handler {
    use super::*;
    use fstart_smm::SmmContext;

    /// The sources of one SMI on the FCH, read once by [`PendingSmi::take`];
    /// the board's SMI handler acts on the ones it owns and ends the SMI
    /// with [`PendingSmi::finish`], or puts the system to sleep.
    pub struct PendingSmi {
        fch: Smi,
        /// Status registers that reported a source, as read.
        status: [u32; STATUS_COUNT],
        /// GEVENT SMIs, bit n = GEVENTn.
        pub gevents: u32,
        /// The SMI command port value, when it raised this SMI. ACPI enable
        /// and disable are applied already.
        pub apm: Option<u8>,
        /// The sleep state (3..=5) requested: the OS's SLP_TYP, or S5 for the
        /// power button while the OS does not own ACPI. S0 and S1 requests
        /// are dropped.
        pub sleep: Option<u8>,
    }

    impl PendingSmi {
        /// Read this SMI's sources and do the FCH's own SMI command work.
        ///
        /// # Safety
        /// Called only by the SMI handler that owns the SMM rendezvous.
        #[inline(always)]
        pub unsafe fn take(config: &FchSmmConfig) -> Self {
            // SAFETY: SMM owns the FCH.
            let fch = unsafe { Smi::new(config) };
            let pointer = fch.acpi.at(POINTER).extract();
            if pointer.is_set(SmiPointer::SCI) {
                let sci = fch.acpi.at(SCI_STATUS);
                sci.set(sci.get());
            }
            let pending = pointer.read(SmiPointer::STATUS);
            let mut status = [0; STATUS_COUNT];
            for (index, status) in status.iter_mut().enumerate() {
                if pending & 1 << index != 0 {
                    *status = fch.acpi.at(super::status(index)).get();
                }
            }
            let is_pending = |source: Source| status[usize::from(source.reg)] & source.mask() != 0;
            let gevents = status[0] & ((1 << source::GEVENTS) - 1);
            let apm = is_pending(source::SMI_CMD).then(|| {
                // SAFETY: the FCH decodes the SMI command port.
                let command = unsafe { fstart_core::pio::inb(APM_CNT) };
                match command {
                    ACPI_ENABLE => {
                        fch.disable_power_button_smi();
                        fch.clear_pm_gpe_status();
                        fch.set_sci(true);
                    }
                    ACPI_DISABLE => {
                        fch.set_sci(false);
                        fch.enable_power_button_smi();
                    }
                    _ => {}
                }
                command
            });
            let sleep = if is_pending(source::PWR_BTN) && !fch.sci_enabled() {
                // Power button: S5 unless the OS owns ACPI.
                fch.clear_pm_gpe_status();
                fch.pm1_control().modify(Pm1Control::SLP_TYP::S5);
                Some(5)
            } else if is_pending(source::SLP_TYP) {
                Some(fch.pm1_control().read(Pm1Control::SLP_TYP) as u8).filter(|t| *t >= 3)
            } else {
                None
            };
            Self {
                fch,
                status,
                gevents,
                apm,
                sleep,
            }
        }

        /// Start entering sleep state `slp_typ` (3..=5): write back the
        /// caches and clear every SMI status. The SoC does its own sleep
        /// entry work next, then [`Sleep::enter`].
        #[inline(always)]
        pub fn prepare_sleep(self, slp_typ: u8) -> Sleep {
            // SAFETY: SMM owns the CPU; nothing else runs.
            unsafe { fstart_arch::x86::writeback_invalidate_caches() };
            self.fch.clear_all_status();
            Sleep {
                fch: self.fch,
                slp_typ,
            }
        }

        /// Clear the handled sources and end the SMI (EOS).
        #[inline(always)]
        pub fn finish(self) {
            for (index, status) in self.status.iter().enumerate() {
                if *status != 0 {
                    self.fch.acpi.at(super::status(index)).set(*status);
                }
            }
            self.fch.acpi.at(TRIGGER0).modify(Trigger0::EOS::SET);
        }
    }

    /// A sleep transition after [`PendingSmi::prepare_sleep`].
    pub struct Sleep {
        fch: Smi,
        slp_typ: u8,
    }

    impl Sleep {
        /// Arm the sleep state the way the vendor FchTaishanSmmInit powers
        /// off: the FCH sleeps on the OS's SLP_TYP once SlpTypeEn is set and
        /// the SLP_TYP SMI is off. The SoC's last word to its SMU goes
        /// between this and [`Armed::enter`].
        #[inline(always)]
        pub fn arm(self) -> Armed {
            let fch = &self.fch;
            fch.acpi.at(PM_PCI_CONTROL).modify(
                PciControl::FORCE_STPCLK_RETRY::SET + PciControl::FORCE_SLPSTATE_RETRY::CLEAR,
            );
            fch.configure(source::SLP_TYP, Route::Disabled);
            fch.acpi
                .at(PM_RESET_CONTROL1)
                .modify(ResetControl1::SLP_TYPE_EN::SET);
            fch.pm1_control()
                .modify(Pm1Control::SLP_TYP.val(self.slp_typ.into()) + Pm1Control::SLP_EN::CLEAR);
            Armed { fch: self.fch }
        }

        /// Arm the sleep state and enter it.
        #[inline(always)]
        pub fn enter(self) -> ! {
            self.arm().enter()
        }
    }

    /// A sleep state armed by [`Sleep::arm`].
    pub struct Armed {
        fch: Smi,
    }

    impl Armed {
        /// Set SLP_EN: the FCH enters the armed state.
        #[inline(always)]
        pub fn enter(self) -> ! {
            self.fch.pm1_control().modify(Pm1Control::SLP_EN::SET);
            loop {
                // SAFETY: halting is all that is left before the sleep.
                unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
            }
        }
    }

    /// The SMI handler of a board with no SMI sources and a SoC without sleep
    /// entry work: the FCH's SMI command, power button and sleep handling.
    #[inline(always)]
    pub fn handle(_ctx: &mut SmmContext<'_>, config: &FchSmmConfig) {
        // SAFETY: bound as the board's SMI handler, so it owns the rendezvous.
        let pending = unsafe { PendingSmi::take(config) };
        if let Some(slp_typ) = pending.sleep {
            pending.prepare_sleep(slp_typ).enter();
        }
        pending.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
