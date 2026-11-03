// SPDX-License-Identifier: MIT OR Apache-2.0
//! FCH SMI sources and the ACPI mode handshake, in normal
//! mode and in the SMM handler.
//!
//! The sources used here have the same bits on both.
use super::{
    AcpiConfigHigh, AcpiPorts, AcpiReg, Acpimmio, Bus, Fch, NativeBus, PM_ACPI_CONFIG_HIGH,
    PM_ACPI_SMI_CMD, PM_PCI_CONTROL, PM_RESET_CONTROL1, PciControl, Reg, ResetControl1,
    smi_bank as smi,
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// SMI command port (PMx6A).
pub const APM_CNT: u16 = 0xb2;
/// SMI command values of the FADT.
pub const ACPI_ENABLE: u8 = 0xe1;
pub const ACPI_DISABLE: u8 = 0x1e;

/// SMI sources (`SMITYPE_*`).
pub mod source {
    /// GEVENT 0..23 are sources 0..23.
    pub const GEVENTS: u8 = 24;
    pub const PSP: u8 = 33;
    pub const POWER_BUTTON_UP: u8 = 51;
    pub const SLP_TYP: u8 = 65;
    pub const SMI_COMMAND: u8 = 75;
}

pub use super::{Pm1Enable, Pm1Status};
pub use super::{SMI_TRIGGER0 as TRIGGER0, SmiMode as Mode, SmiTrigger0, smi_control as control};
register_bitfields![u16,
    /// SMIx94: which status registers hold events.
    pub SmiPointer [SCI OFFSET(0) NUMBITS(1) [], STATUS OFFSET(1) NUMBITS(5) []],
    pub Pm1Control [
        SCI_EN OFFSET(0) NUMBITS(1) [],
        SLP_TYP OFFSET(10) NUMBITS(3) [S5 = 5],
        SLP_EN OFFSET(13) NUMBITS(1) []
    ]
];

pub const SCI_STATUS: AcpiReg<u32> = smi::at(0x10);
/// SMIx80 + 4 * n, n < 5.
pub const fn status(n: usize) -> AcpiReg<u32> {
    smi::at(0x80 + 4 * n as u32)
}
const STATUS_COUNT: usize = 5;
pub const POINTER: AcpiReg<u16, SmiPointer::Register> = smi::at(0x94);

/// Byte offset of SMIxA0 + n and the bit shift of `source` in it.
#[cfg(test)]
const fn control_slot(source: u8) -> (usize, u32) {
    ((source / 16) as usize, (source % 16) as u32 * 2)
}

/// One FCH's SMI registers, PM registers and ACPI blocks: the shared
/// operations of normal mode and the SMM handler.
struct Smi<B> {
    fch: Fch<B>,
    ports: FchSmmConfig,
}

impl<B: Bus> Smi<B> {
    fn new(bus: B, config: &FchSmmConfig) -> Self {
        Self {
            fch: Fch::new(bus, Acpimmio::DIE0),
            ports: *config,
        }
    }

    fn pm1_status(&self) -> super::BusRegister<'_, B, u16, Pm1Status::Register> {
        self.fch.bus().reg(Reg::io(self.ports.pm1_event))
    }
    fn pm1_enable(&self) -> super::BusRegister<'_, B, u16, Pm1Enable::Register> {
        self.fch.bus().reg(Reg::io(self.ports.pm1_event + 2))
    }
    fn pm1_control(&self) -> super::BusRegister<'_, B, u16, Pm1Control::Register> {
        self.fch.bus().reg(Reg::io(self.ports.pm1_control))
    }
    fn gpe0_status(&self) -> super::BusRegister<'_, B, u32, ()> {
        self.fch.bus().reg(Reg::io(self.ports.gpe0))
    }

    fn configure(&self, source: u8, mode: Mode) {
        super::smi_route(&self.fch, source, mode);
    }

    /// Clear every SMI status (no event status: GEVENT SCIs stay).
    fn clear_all_status(&self) {
        let sci = self.fch.reg(SCI_STATUS);
        sci.set(sci.get());
        for n in 0..STATUS_COUNT {
            let status = self.fch.reg(status(n));
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
        self.fch
            .reg(PM_ACPI_CONFIG_HIGH)
            .modify(AcpiConfigHigh::PWRBTN_EN::SET);
        self.configure(source::POWER_BUTTON_UP, Mode::Smi);
    }

    #[cfg(feature = "smm")]
    fn disable_power_button_smi(&self) {
        self.configure(source::POWER_BUTTON_UP, Mode::Disable);
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

    fn smi(&self) -> Smi<NativeBus> {
        // SAFETY: the constructor's contract.
        Smi::new(unsafe { NativeBus::new() }, &self.config)
    }
}

impl fstart_arch::x86::cpu::amd::smm::SmiControl for FchSmi {
    type HandlerConfig = FchSmmConfig;

    fn smm_handler_config(&self) -> FchSmmConfig {
        self.config
    }

    fn disable_smi(&self) {
        self.smi()
            .fch
            .reg(TRIGGER0)
            .modify(SmiTrigger0::SMI_DISABLE::SET);
    }

    /// The SMI command port, SLP_TYP and power button SMIs, the board's
    /// GEVENT SMIs, then SMIs on.
    fn enable_smi(&self) {
        let smi = self.smi();
        smi.fch.reg(PM_ACPI_SMI_CMD).set(APM_CNT);
        smi.configure(source::SMI_COMMAND, Mode::Smi);
        // SMI on SlpTyp requires sending SMI before completion response of
        // the I/O write; the SMM handler takes over the sleep entry.
        smi.fch
            .reg(PM_PCI_CONTROL)
            .modify(PciControl::FORCE_SLPSTATE_RETRY::SET);
        smi.fch
            .reg(PM_RESET_CONTROL1)
            .modify(ResetControl1::SLP_TYPE_EN::CLEAR);
        smi.configure(source::SLP_TYP, Mode::Smi);
        (0..source::GEVENTS)
            .filter(|gevent| self.gevents & 1 << gevent != 0)
            .for_each(|gevent| smi.configure(gevent, Mode::Smi));
        // Legacy mode until the OS sends ACPI_ENABLE; meanwhile the power
        // button powers off through SMM.
        smi.clear_pm_gpe_status();
        smi.set_sci(false);
        smi.enable_power_button_smi();
        smi.clear_all_status();
        smi.fch
            .reg(TRIGGER0)
            .modify(SmiTrigger0::SMI_DISABLE::CLEAR + SmiTrigger0::EOS::SET);
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
    use crate::fch::SmmBus;
    use fstart_smm::SmmContext;

    /// The sources of one SMI on the FCH, read once by [`PendingSmi::take`];
    /// the board's SMI handler acts on the ones it owns and ends the SMI
    /// with [`PendingSmi::finish`], or puts the system to sleep.
    pub struct PendingSmi {
        fch: Smi<SmmBus>,
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
            // SAFETY: SMM owns the FCH; it decodes no config spaces.
            let fch = Smi::new(unsafe { SmmBus::without_config() }, config);
            let pointer = fch.fch.reg(POINTER).extract();
            if pointer.is_set(SmiPointer::SCI) {
                let sci = fch.fch.reg(SCI_STATUS);
                sci.set(sci.get());
            }
            let pending = pointer.read(SmiPointer::STATUS);
            let mut status = [0; STATUS_COUNT];
            for (index, status) in status.iter_mut().enumerate() {
                if pending & 1 << index != 0 {
                    *status = fch.fch.reg(super::status(index)).get();
                }
            }
            let is_pending = |source: u8| {
                let index = usize::from(source / 32);
                status[index] & 1 << (source % 32) != 0
            };
            let gevents = status[0] & ((1 << source::GEVENTS) - 1);
            let apm = is_pending(source::SMI_COMMAND).then(|| {
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
            let sleep = if is_pending(source::POWER_BUTTON_UP) && !fch.sci_enabled() {
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
                    self.fch.fch.reg(super::status(index)).set(*status);
                }
            }
            self.fch.fch.reg(TRIGGER0).modify(SmiTrigger0::EOS::SET);
        }
    }

    /// A sleep transition after [`PendingSmi::prepare_sleep`].
    pub struct Sleep {
        fch: Smi<SmmBus>,
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
            fch.fch.reg(PM_PCI_CONTROL).modify(
                PciControl::FORCE_STPCLK_RETRY::SET + PciControl::FORCE_SLPSTATE_RETRY::CLEAR,
            );
            fch.configure(source::SLP_TYP, Mode::Disable);
            fch.fch
                .reg(PM_RESET_CONTROL1)
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
        fch: Smi<SmmBus>,
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
        // SMI_REG_CONTROL0 + 4 * (n / 16), 2 bits each.
        assert_eq!(control_slot(source::SMI_COMMAND), (4, 22));
        assert_eq!(control_slot(source::SLP_TYP), (4, 2));
        assert_eq!(control_slot(source::POWER_BUTTON_UP), (3, 6));
        assert_eq!(control_slot(source::PSP), (2, 2));
        assert_eq!(control_slot(11), (0, 22));
    }

    #[test]
    fn the_psp_smm_info_names_these_smi_registers() {
        let info = crate::psp::command::SmmInfo::new(0, 0, 0, 0);
        let trigger0 = u64::from(Acpimmio::DIE0.0 + TRIGGER0.offset());
        let psp = SmiTrigger0::PSP::SET.value;
        assert_eq!((info.trigger.address, info.trigger.mask), (trigger0, !psp));
        assert_eq!(info.trigger.value, psp);
        assert_eq!(info.smi_enable.mask, SmiTrigger0::SMI_DISABLE::SET.value);
        assert_eq!(info.eos.value, SmiTrigger0::EOS::SET.value);
        let (index, shift) = control_slot(source::PSP);
        let control = u64::from(Acpimmio::DIE0.0 + control(index).offset());
        assert_eq!(info.psp_smi_control.address, control);
        assert_eq!(
            (info.psp_smi_control.mask, info.psp_smi_control.value),
            (3 << shift, (Mode::Smi as u32) << shift)
        );
    }
}
