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
use crate::psp::{SmiRegisters, SmmRegister};
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

/// The SMI registers the PSP uses from its side of SMM: it raises SMIs with
/// SMIx98[25] and expects SMIs enabled, EOS set and its own source routed to
/// SMI. Its source isn't routed here, so the PSP can't raise SMIs.
pub fn psp_registers() -> SmiRegisters {
    let address = |offset: u32| u64::from(Acpimmio::DIE0.0 + offset);
    let trigger0 = address(TRIGGER0.offset());
    let bit = |field: tock_registers::fields::FieldValue<u32, SmiTrigger0::Register>| field.value;
    let (index, shift) = control_slot(source::PSP);
    SmiRegisters {
        trigger: SmmRegister {
            address: trigger0,
            mask: bit(SmiTrigger0::PSP::SET),
            value: bit(SmiTrigger0::PSP::SET),
        },
        smi_enable: SmmRegister {
            address: trigger0,
            mask: bit(SmiTrigger0::SMI_DISABLE::SET),
            value: 0,
        },
        eos: SmmRegister {
            address: trigger0,
            mask: bit(SmiTrigger0::EOS::SET),
            value: bit(SmiTrigger0::EOS::SET),
        },
        psp_smi_enable: SmmRegister {
            address: address(control(index).offset()),
            mask: 3 << shift,
            value: (Mode::Smi as u32) << shift,
        },
    }
}

/// Configuration the installer writes into SMRAM for [`FchSmmHandler`]: the
/// ACPI blocks of the FADT.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FchSmmConfig {
    pm1_event: u16,
    pm1_control: u16,
    gpe0: u16,
}

#[cfg(feature = "smm")]
pub use handler::{FchSmmHandler, FchSmmHooks};

#[cfg(feature = "smm")]
mod handler {
    use super::*;
    use crate::fch::SmmBus;
    use core::marker::PhantomData;
    use fstart_smm::{SmmContext, SmmHandler};

    /// SoC and board parts of the FCH SMM handler.
    pub trait FchSmmHooks {
        /// Before the system enters S3, S4 or S5.
        /// # Safety
        /// Called only by [`FchSmmHandler`] while it owns the SMM rendezvous.
        unsafe fn sleep_entry(_slp_typ: u8) {}
        /// GEVENT SMIs, bit n = GEVENTn.
        /// # Safety
        /// As for [`Self::sleep_entry`].
        unsafe fn gevent(_status: u32) {}
        /// Any SMI command, after the FCH handled its own.
        /// # Safety
        /// As for [`Self::sleep_entry`].
        unsafe fn apmc(_command: u8) {}
    }

    /// The FCH SMI dispatcher.
    pub struct FchSmmHandler<H>(PhantomData<H>);

    impl<H: FchSmmHooks> SmmHandler for FchSmmHandler<H> {
        type Config = FchSmmConfig;

        /// Inlined into the board SMM entry: the installed blob is a raw
        /// copy with no dynamic loader.
        #[inline(always)]
        unsafe fn handle(_ctx: &mut SmmContext<'_>, config: &FchSmmConfig) {
            // SAFETY: SMM owns the FCH; it decodes no config spaces.
            let fch = Smi::new(unsafe { SmmBus::without_config() }, config);
            let pointer = fch.fch.reg(POINTER).extract();
            if pointer.is_set(SmiPointer::SCI) {
                let sci = fch.fch.reg(SCI_STATUS);
                sci.set(sci.get());
            }
            let pending = pointer.read(SmiPointer::STATUS);
            for index in 0..STATUS_COUNT {
                if pending & 1 << index == 0 {
                    continue;
                }
                let register = fch.fch.reg(status(index));
                let status = register.get();
                let base = index as u8 * 32;
                let pending = |source: u8| {
                    (base..base + 32).contains(&source) && status & 1 << (source - base) != 0
                };
                if index == 0 && status & ((1 << source::GEVENTS) - 1) != 0 {
                    unsafe { H::gevent(status & ((1 << source::GEVENTS) - 1)) };
                }
                if pending(source::POWER_BUTTON_UP) {
                    unsafe { power_button::<H>(&fch) };
                }
                if pending(source::SLP_TYP) {
                    unsafe { sleep::<H>(&fch) };
                }
                if pending(source::SMI_COMMAND) {
                    unsafe { apmc::<H>(&fch) };
                }
                register.set(status);
            }
            fch.fch.reg(TRIGGER0).modify(SmiTrigger0::EOS::SET);
        }
    }

    /// An SMI from the SMI command port.
    #[inline(always)]
    unsafe fn apmc<H: FchSmmHooks>(fch: &Smi<SmmBus>) {
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
        unsafe { H::apmc(command) };
    }

    /// Power button: S5 unless the OS owns ACPI.
    #[inline(always)]
    unsafe fn power_button<H: FchSmmHooks>(fch: &Smi<SmmBus>) {
        if fch.sci_enabled() {
            return;
        }
        fch.clear_pm_gpe_status();
        fch.pm1_control().modify(Pm1Control::SLP_TYP::S5);
        unsafe { sleep::<H>(fch) };
    }

    /// SLP_TYP for S3..S5, entered the way the vendor
    /// FchTaishanSmmInit powers off: the FCH sleeps on the OS's SLP_TYP once
    /// SlpTypeEn is set and the SLP_TYP SMI is off. S0 and S1 requests are
    /// dropped.
    #[inline(always)]
    unsafe fn sleep<H: FchSmmHooks>(fch: &Smi<SmmBus>) {
        let control = fch.pm1_control();
        let slp_typ = control.read(Pm1Control::SLP_TYP) as u8;
        if slp_typ < 3 {
            return;
        }
        unsafe {
            fstart_arch::x86::writeback_invalidate_caches();
            fch.clear_all_status();
            H::sleep_entry(slp_typ);
        }
        fch.fch
            .reg(PM_PCI_CONTROL)
            .modify(PciControl::FORCE_STPCLK_RETRY::SET + PciControl::FORCE_SLPSTATE_RETRY::CLEAR);
        fch.configure(source::SLP_TYP, Mode::Disable);
        fch.fch
            .reg(PM_RESET_CONTROL1)
            .modify(ResetControl1::SLP_TYPE_EN::SET);
        control.modify(Pm1Control::SLP_TYP.val(slp_typ.into()) + Pm1Control::SLP_EN::CLEAR);
        control.modify(Pm1Control::SLP_EN::SET);
        loop {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
        }
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
    fn psp_registers_match_the_vendor_smm_info() {
        let registers = psp_registers();
        assert_eq!(registers.trigger.address, 0xfed8_0298);
        assert_eq!(registers.trigger.mask, 1 << 25);
        assert_eq!(registers.smi_enable.mask, 1 << 31);
        assert_eq!(registers.eos.value, 1 << 28);
        assert_eq!(registers.psp_smi_enable.address, 0xfed8_02a8);
        assert_eq!(
            (
                registers.psp_smi_enable.mask,
                registers.psp_smi_enable.value
            ),
            (0xc, 0x4)
        );
    }
}
