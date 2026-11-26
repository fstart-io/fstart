// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H permanent SMI control and native APM/PM1/GPI/TCO/S5 handling.
//! No retained/S3 transition is advertised or silently accepted.

use super::{
    gpio::{self, Group},
    pcr::{self, Pcr},
    pmc::{self, AfterPowerLoss, GenPmconA, GenPmconB},
    pmio::{AcpiIo, SmiEnable, SmiStatus, UsbPower},
    spi,
};
use crate::{
    skylake::early_sa::EcamConfig,
    southbridge::{
        pmio_ich::{PM_CONTROL, PM_STATUS, TcoIo},
        smi::{APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, busmaster_disable_on_bus},
    },
};
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::{Udelay, cpu::intel::smm::SmiControl};
use fstart_core::services::ServiceError;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

// Only these groups implement SMI banks (H: B/C/D/E/I; LP: B/C/D/E). Never
// touch the reserved A/F/G/H/GPD offsets as though they were generic windows.
const SMI_GROUPS_H: &[Group] = &[Group::B, Group::C, Group::D, Group::E, Group::I];
const SMI_GROUPS_LP: &[Group] = &[Group::B, Group::C, Group::D, Group::E];
const CONFIG_VERSION: u32 = 1;
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Config {
    ecam_base: u64,
    version: u32,
    buses: u16,
    pm_base: u16,
    tco_base: u16,
    s5_delay_ms: u16,
    after_power_loss: u8,
    pch_kind: u8,
    reserved: [u8; 10],
}
const _: () = assert!(size_of::<Config>() == 32);

#[derive(Clone, Copy)]
pub struct EnableState {
    pm1: u16,
    gpe: [u32; 4],
    gpio: [u32; 5],
    usb: u32,
}

pub struct Controller {
    config: Config,
    kind: super::dmi::Kind,
    pm: AcpiIo,
    tco: TcoIo,
    pcr: Pcr,
}

/// The PMC's GEN_PMCON_A/B, through CF8 like the ICH handlers: the SMM
/// image keeps no ECAM state.
const PMC: (u8, u8, u8) = (0, 0x1f, 2);
const GEN_PMCON_A: u8 = 0xa0;
const GEN_PMCON_B: u8 = 0xa4;

fn gen_pmcon_a() -> LocalRegisterCopy<u32, GenPmconA::Register> {
    // SAFETY: the SMM handler and the installer own configuration space.
    LocalRegisterCopy::new(unsafe { fstart_pci::cf8::read32(PMC.0, PMC.1, PMC.2, GEN_PMCON_A) })
}
impl Controller {
    /// # Safety
    /// Own actual UC PCH-H/LP PCR/ECAM and decoded ACPI/TCO ports. The platform
    /// owns SMM installation and all GPIO/PM/PCI accesses, with AP/SMI/OS
    /// competitors excluded. TCOBASE must be the live SMBus function's decode.
    pub unsafe fn bind(
        kind: super::dmi::Kind,
        ecam: EcamConfig,
        tco_base: u16,
        after: AfterPowerLoss,
        s5_delay_ms: u16,
    ) -> Result<Self, ServiceError> {
        ecam.encoded_pciexbar()
            .map_err(|_| ServiceError::InvalidParam)?;
        let config = Config {
            version: CONFIG_VERSION,
            ecam_base: ecam.base,
            buses: ecam.buses,
            pm_base: pmc::ACPI_BASE,
            tco_base,
            s5_delay_ms,
            after_power_loss: after as u8,
            pch_kind: match kind {
                super::dmi::Kind::H => 0,
                super::dmi::Kind::Lp => 1,
            },
            reserved: [0; 10],
        };
        let controller = unsafe { Self::from_config(config) }?;
        if gen_pmcon_a().is_set(GenPmconA::SMI_LOCK) {
            return Err(ServiceError::HardwareError);
        }
        Ok(controller)
    }
    fn smi_groups(&self) -> &'static [Group] {
        match self.kind {
            super::dmi::Kind::H => SMI_GROUPS_H,
            super::dmi::Kind::Lp => SMI_GROUPS_LP,
        }
    }
    unsafe fn from_config(config: Config) -> Result<Self, ServiceError> {
        let kind = match config.pch_kind {
            0 => super::dmi::Kind::H,
            1 => super::dmi::Kind::Lp,
            _ => return Err(ServiceError::InvalidParam),
        };
        if config.version != CONFIG_VERSION
            || config.reserved != [0; 10]
            || config.pm_base != pmc::ACPI_BASE
            || config.tco_base == 0
            || config.tco_base & 0x1f != 0
            || config.tco_base.checked_add(0x1f).is_none()
            || config.s5_delay_ms > 1000
            || config.after_power_loss > 2
            || !matches!(config.buses, 64 | 128 | 256)
        {
            return Err(ServiceError::InvalidParam);
        }
        let pcr = unsafe { Pcr::at(pcr::DEFAULT_BASE as usize) }
            .map_err(|_| ServiceError::HardwareError)?;
        if config.tco_base != super::tco::BASE {
            return Err(ServiceError::HardwareError);
        }
        let groups = match kind {
            super::dmi::Kind::H => SMI_GROUPS_H,
            super::dmi::Kind::Lp => SMI_GROUPS_LP,
        };
        for &group in groups {
            let info = group.info_for(kind).ok_or(ServiceError::HardwareError)?;
            pcr.register::<()>(info.port, gpio::SMI_STATUS + 4 * u16::from(info.index))
                .map_err(|_| ServiceError::HardwareError)?;
            pcr.register::<()>(info.port, gpio::SMI_ENABLE + 4 * u16::from(info.index))
                .map_err(|_| ServiceError::HardwareError)?;
        }
        Ok(Self {
            config,
            kind,
            pm: AcpiIo::new(config.pm_base),
            tco: TcoIo::new(config.tco_base),
            pcr,
        })
    }
    fn gpio_enables(&self) -> [u32; 5] {
        let mut out = [0u32; 5];
        for (slot, &group) in out.iter_mut().zip(self.smi_groups().iter()) {
            let info = group
                .info_for(self.kind)
                .unwrap_or_else(|| fstart_arch::x86::boot::halt());
            *slot = self
                .pcr
                .register::<()>(info.port, gpio::SMI_ENABLE + 4 * u16::from(info.index))
                .unwrap_or_else(|_| fstart_arch::x86::boot::halt())
                .get()
                & ((1u32 << info.count) - 1);
        }
        out
    }
    fn set_gpio_enables(&self, values: [u32; 5]) {
        for (&group, value) in self.smi_groups().iter().zip(values) {
            let info = group
                .info_for(self.kind)
                .unwrap_or_else(|| fstart_arch::x86::boot::halt());
            self.pcr
                .register::<()>(info.port, gpio::SMI_ENABLE + 4 * u16::from(info.index))
                .unwrap_or_else(|_| fstart_arch::x86::boot::halt())
                .set(value & ((1u32 << info.count) - 1));
        }
    }
    fn clear_gpio_status(&self) {
        for &group in self.smi_groups() {
            let info = group
                .info_for(self.kind)
                .unwrap_or_else(|| fstart_arch::x86::boot::halt());
            let status = self
                .pcr
                .register::<()>(info.port, gpio::SMI_STATUS + 4 * u16::from(info.index))
                .unwrap_or_else(|_| fstart_arch::x86::boot::halt());
            status.set(status.get() & ((1u32 << info.count) - 1));
        }
    }
    fn clear_status(&self) {
        self.pm.smi_status().set(self.pm.smi_status().get());
        self.pm.status().set(self.pm.status().get());
        self.tco.reset_tco_status();
        for status in self.pm.gpe_status() {
            status.set(status.get());
        }
        self.clear_gpio_status();
    }
    fn set_power_fail_for_s5(&self) {
        let sleep = self.config.after_power_loss != AfterPowerLoss::On as u8;
        // Only the low byte: preserve its W0C RTC/PWR_FLR status and do not
        // touch the upper RW1C reset causes or ACPI/stretch policy locks.
        // SAFETY: the SMM handler owns configuration space.
        let low = unsafe { fstart_pci::cf8::read8(PMC.0, PMC.1, PMC.2, GEN_PMCON_B) };
        let mut value = LocalRegisterCopy::<u32, GenPmconB::Register>::new(u32::from(low));
        value.modify(GenPmconB::SLEEP_AFTER_POWER_FAIL.val(u32::from(sleep)));
        unsafe { fstart_pci::cf8::write8(PMC.0, PMC.1, PMC.2, GEN_PMCON_B, value.get() as u8) };
    }
    fn sleep(&self, sleep_type: u32) {
        self.pm.smi_enable().modify(SmiEnable::SLEEP::CLEAR);
        if !matches!(sleep_type, 5..=7) {
            self.pm.control().modify(PM_CONTROL::SLEEP_ENABLE::CLEAR);
            self.pm.smi_enable().modify(SmiEnable::SLEEP::SET);
            return;
        }
        for status in self.pm.gpe_status() {
            status.set(status.get());
        }
        if sleep_type == 5 {
            // Flush all dirty cache lines to DRAM before entering self-refresh.
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            // SAFETY: WBINVD has no memory-safety preconditions in SMM.
            unsafe {
                core::arch::asm!("wbinvd", options(nomem, nostack, preserves_flags));
            }
        }
        if sleep_type == 7 {
            for enable in self.pm.gpe_enable() {
                enable.set(0);
            }
            self.set_power_fail_for_s5();
            // SAFETY: the SMM handler owns configuration space.
            unsafe { busmaster_disable_on_bus(0) };
            let mut delay = Udelay;
            delay.delay_ms(u32::from(self.config.s5_delay_ms));
            self.pm.status().set(self.pm.status().get());
        }
        self.pm
            .control()
            .modify(PM_CONTROL::SLEEP_TYPE.val(sleep_type) + PM_CONTROL::SLEEP_ENABLE::SET);
        fstart_arch::x86::boot::halt()
    }
    fn handle(&self) {
        // Native pmc_reset_smi_status acknowledges the captured snapshot BEFORE
        // dispatch. In particular do not clear TCO again after SPI SYNC_STATUS:
        // a TCO pending during that clear must cause a subsequent SMI.
        let status = self.pm.smi_status().extract();
        let acpi = self.pm.control().is_set(PM_CONTROL::SCI_EN);
        self.pm.smi_status().set(status.get());
        // Native source-bit dispatch order: sleep(4), APM(5), PM1(8),
        // GPE(9), GPIO(10), TCO(13). Sleep transitions do not return.
        if status.is_set(SmiStatus::SLEEP) {
            self.sleep(self.pm.control().read(PM_CONTROL::SLEEP_TYPE));
        }
        if status.is_set(SmiStatus::APM) {
            let command = unsafe { fstart_core::pio::inb(APM_CNT) };
            match command {
                APM_CNT_ACPI_ENABLE => self.set_acpi_mode(true),
                APM_CNT_ACPI_DISABLE => self.set_acpi_mode(false),
                _ => {} // No SMM flash-store or untrusted save-state service.
            }
        }
        if !acpi && status.is_set(SmiStatus::PM1) {
            let pm1 = self.pm.status().extract();
            self.pm.status().set(pm1.get());
            if pm1.is_set(PM_STATUS::POWER_BUTTON) {
                self.sleep(7);
            }
        }
        if !acpi && status.is_set(SmiStatus::GPE0) {
            for register in self.pm.gpe_status() {
                register.set(register.get());
            }
        }
        if status.is_set(SmiStatus::GPIO) {
            self.clear_gpio_status();
            self.clear_gpio_status(); // native post-mainboard-handler clear
        }
        if status.is_set(SmiStatus::TCO) {
            self.tco.reset_tco_status();
            if spi::service_write_protection().is_err() {
                fstart_arch::x86::boot::halt();
            }
        }
        self.pm.smi_enable().modify(SmiEnable::EOS::SET);
    }
}
impl SmiControl for Controller {
    type EnableState = EnableState;
    type HandlerConfig = Config;
    fn smm_handler_config(&self) -> Config {
        self.config
    }
    fn set_acpi_mode(&self, enabled: bool) {
        self.pm
            .control()
            .modify(PM_CONTROL::SCI_EN.val(u32::from(enabled)));
    }
    fn quiesce_for_relocation(&self) -> EnableState {
        self.pm.smi_enable().set(0);
        let previous = EnableState {
            pm1: self.pm.enable().get(),
            gpe: self.pm.gpe_enable().map(|r| r.get()),
            gpio: self.gpio_enables(),
            usb: self.pm.usb_power().get(),
        };
        self.pm.enable().set(0);
        for enable in self.pm.gpe_enable() {
            enable.set(0);
        }
        self.set_gpio_enables([0; 5]);
        self.pm
            .usb_power()
            .modify(UsbPower::SMI::CLEAR + UsbPower::STATUS::CLEAR);
        self.clear_status();
        self.pm
            .smi_enable()
            .write(SmiEnable::GLOBAL::SET + SmiEnable::EOS::SET);
        previous
    }
    fn enable_permanent_smi(&self, previous: EnableState) {
        self.clear_status();
        if spi::service_write_protection().is_err() {
            fstart_arch::x86::boot::halt();
        }
        self.pm.enable().set(previous.pm1);
        for (register, value) in self.pm.gpe_enable().into_iter().zip(previous.gpe) {
            register.set(value);
        }
        self.set_gpio_enables(previous.gpio);
        let usb = LocalRegisterCopy::<u32, UsbPower::Register>::new(previous.usb);
        self.pm.usb_power().modify(
            UsbPower::SMI.val(usb.read(UsbPower::SMI))
                + UsbPower::WAKE.val(usb.read(UsbPower::WAKE))
                + UsbPower::STATUS::CLEAR,
        );
        self.pm.smi_enable().write(
            SmiEnable::GLOBAL::SET
                + SmiEnable::EOS::SET
                + SmiEnable::APM::SET
                + SmiEnable::SLEEP::SET
                + SmiEnable::TCO::SET,
        );
    }
    fn lock_permanent_smi(&self) -> bool {
        let mut control = gen_pmcon_a();
        control.modify(
            GenPmconA::SMI_LOCK::SET
                + GenPmconA::GLOBAL_RESET_STATUS::CLEAR
                + GenPmconA::MS4V::CLEAR,
        );
        // SAFETY: the installer owns configuration space.
        unsafe { fstart_pci::cf8::write32(PMC.0, PMC.1, PMC.2, GEN_PMCON_A, control.get()) };
        gen_pmcon_a().is_set(GenPmconA::SMI_LOCK)
            && self.pm.smi_enable().matches_all(
                SmiEnable::GLOBAL::SET
                    + SmiEnable::APM::SET
                    + SmiEnable::SLEEP::SET
                    + SmiEnable::TCO::SET,
            )
    }
}

/// Actual SMM rendezvous leader only; config was copied into protected SMRAM
/// by the installed image's same-version driver, not supplied by the OS.
#[inline(always)]
pub fn handle(_context: &mut fstart_smm::SmmContext<'_>, config: &Config) {
    let Ok(controller) = (unsafe { Controller::from_config(*config) }) else {
        fstart_arch::x86::boot::halt()
    };
    // The native feature-control lock is PACKAGE scoped: do this only on the
    // shared rendezvous leader, never in unsynchronized enter_cpu callbacks.
    if unsafe { fstart_arch::x86::cpu::intel::skylake::secure_smm_entry() }.is_err() {
        fstart_arch::x86::boot::halt();
    }
    if unsafe { fstart_arch::x86::cpu::intel::skylake::deny_smm_flash_writes() }.is_err() {
        fstart_arch::x86::boot::halt();
    }
    controller.handle();
}
