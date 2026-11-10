//! Intel 5 Series / 3400 Series chipset (Ibex Peak) PCH.
//!
//! A port of coreboot `southbridge/intel/ibexpeak` plus the
//! `southbridge/intel/common` pieces it selects. The PCH pairs with the
//! Ironlake uncore of Arrandale/Clarkdale CPUs over DMI; its register model
//! is the ICH9/ICH10 one with the rate-matching USB hubs, the ME interface
//! and the PCH display link added, so the shared ICH helpers
//! ([`gpio_ich`](crate::southbridge::gpio_ich), [`pmio_ich`],
//! [`smbus`](crate::southbridge::smbus), [`hda`](crate::southbridge::hda),
//! [`smi`](crate::southbridge::smi)) carry over unchanged.
//!
//! Board wiring (GPIOs, USB port currents, decodes, codec verbs, the
//! disabled functions) is configuration; nothing here is board-specific.

#[cfg(feature = "acpi")]
mod acpi;
mod early;
pub mod me;
mod ramstage;
pub mod regs;

pub use crate::southbridge::gpio_ich::{GpiRoute, GpioConfig, GpioPin};
pub use crate::southbridge::hda::{HdaConfig, HdaVerbTable};
pub use crate::southbridge::lpc::LpcGenericIoDecode;
pub use fstart_driver_usbdebug::Config as EhciDebugConfig;

use crate::southbridge::gpio_ich::IchGpio;
use crate::southbridge::pmio_ich::PmIo;
use crate::southbridge::smbus::I801SmBus;
use fstart_core::ConstVec;
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::{PciAddress, PciFixedBar, PciFixedBarType, PciFixedBars, ecam};
use regs::{IbexPeakLpcConfig, RcbaRegs};

/// Fixed southbridge resources (coreboot `pch.h` and Kconfig defaults).
pub const DEFAULT_RCBA: u64 = 0xfed1_c000;
pub const DEFAULT_PMBASE: u16 = 0x0500;
pub const DEFAULT_GPIOBASE: u16 = 0x0480;
pub const DEFAULT_SMBUS_BASE: u16 = 0x0400;
pub const DEFAULT_HECIBAR: u32 = 0xfed1_7000;
/// coreboot `CONFIG_EHCI_BAR` for this PCH.
pub const DEFAULT_EHCI_DEBUG_BAR: u32 = 0xfef0_0000;
/// coreboot `SMBUS_SLAVE_ADDR`.
pub const SMBUS_SLAVE_ADDR: u8 = 0x24;
pub(crate) const HPET_BASE: usize = 0xfed0_0000;
pub(crate) const IOAPIC_BASE: usize = 0xfec0_0000;

/// USB port drive strength, an index into the EDS current table
/// (coreboot `enum current_lookup_idx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbCurrent {
    F57,
    F5F,
    I753,
    I75F,
    I14B,
    I74B,
    I557,
    I757,
    I55F,
    I54B,
}

impl UsbCurrent {
    /// The USBIRx current setting (coreboot `early_usb_init()` table).
    #[must_use]
    pub const fn setting(self) -> u32 {
        match self {
            Self::F57 => 0xf57,
            Self::F5F => 0xf5f,
            Self::I753 => 0x753,
            Self::I75F => 0x75f,
            Self::I14B => 0x14b,
            Self::I74B => 0x74b,
            Self::I557 => 0x557,
            Self::I757 => 0x757,
            Self::I55F => 0x55f,
            Self::I54B => 0x54b,
        }
    }
}

/// One of the 14 USB 2.0 ports (coreboot `struct southbridge_usb_port`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbPort {
    pub enabled: bool,
    pub current: UsbCurrent,
    /// Over-current pin; ports 0..7 report on OC0..3, ports 8..13 on OC4..7.
    pub oc_pin: Option<u8>,
}

impl UsbPort {
    #[must_use]
    pub const fn new(current: UsbCurrent, oc_pin: u8) -> Self {
        Self {
            enabled: true,
            current,
            oc_pin: Some(oc_pin),
        }
    }

    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            current: UsbCurrent::F57,
            oc_pin: None,
        }
    }
}

pub use crate::southbridge::options::SataMode;

/// SATA 1 policy (coreboot `sata_port_map`, `sata_interface_speed_support`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SataConfig {
    pub mode: SataMode,
    /// Implemented ports, bits 0..5.
    pub ports: u8,
    /// Maximum interface speed: 0 keeps the chip default, 1 = 1.5 Gb/s,
    /// 2 = 3 Gb/s, 3 = 6 Gb/s.
    pub interface_speed: u8,
}

/// PCH PCI functions a board can disable (coreboot devicetree `off`),
/// hidden through the function-disable registers (`pch_disable_devfn()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PchFunction {
    Mei1,
    Mei2,
    Ider,
    Kt,
    Gbe,
    Ehci1,
    Ehci2,
    Hda,
    /// PCIe root port 1..8 as its function number 0..7.
    PcieRootPort(u8),
    Lpc,
    Sata1,
    Smbus,
    Sata2,
    Thermal,
}

impl PchFunction {
    /// Bus-0 device and function.
    #[must_use]
    pub const fn devfn(self) -> (u8, u8) {
        use regs::dev;
        match self {
            Self::Mei1 => dev::MEI1,
            Self::Mei2 => dev::MEI2,
            Self::Ider => dev::IDER,
            Self::Kt => dev::KT,
            Self::Gbe => dev::GBE,
            Self::Ehci1 => dev::EHCI1,
            Self::Ehci2 => dev::EHCI2,
            Self::Hda => dev::HDA,
            Self::PcieRootPort(port) => (dev::PCIE, port),
            Self::Lpc => dev::LPC,
            Self::Sata1 => dev::SATA1,
            Self::Smbus => dev::SMBUS,
            Self::Sata2 => dev::SATA2,
            Self::Thermal => dev::THERMAL,
        }
    }
}

/// Ibex Peak policy: chipset defaults plus the board's wiring.
#[derive(Debug, Clone, Copy)]
pub struct IntelIbexPeakConfig {
    pub rcba: u64,
    pub pmbase: u16,
    pub gpiobase: u16,
    pub smbus_base: u16,
    /// Uncore DMIBAR, programmed by the northbridge. The ME UMA handshake
    /// reprograms both ends of the DMI virtual channels.
    pub dmibar: u64,
    /// Desktop (Clarkdale/Lynnfield) rather than mobile CIR programming
    /// (coreboot `IRONLAKE_DESKTOP` passed to `pch_setup_cir()`).
    pub desktop: bool,
    /// GPIO pad configuration (coreboot `mainboard_gpio_map`).
    pub gpio: GpioConfig,
    pub gpi_routing: [GpiRoute; 16],
    /// GPE0_EN (PMBASE+0x28).
    pub gpe0_en: u32,
    pub alt_gp_smi_en: u16,
    /// LPC generic I/O decodes (GEN1..4_DEC).
    pub lpc_generic_io: ConstVec<LpcGenericIoDecode, 4>,
    pub sata: Option<SataConfig>,
    /// The 14 USB ports' enable, drive current and over-current pin.
    pub usb_ports: [UsbPort; 14],
    pub hda: Option<HdaConfig>,
    /// PCIe root ports with an ExpressCard-style hot-plug slot.
    pub pcie_hotplug: [bool; 8],
    /// Devicetree-disabled functions.
    pub disabled: ConstVec<PchFunction, 24>,
    /// Docking station support (FADT `DCK_CAP`).
    pub docking_supported: bool,
    /// Mobile ACPI PM profile.
    pub laptop: bool,
    /// After G3: 0 = stay off, 1 = power on, 2 = previous state.
    pub power_on_after_fail: u8,
    /// Unmask NMI sources.
    pub nmi: bool,
    /// Hide the MEI when the ME reports an error (coreboot
    /// `HIDE_MEI_ON_ERROR`).
    pub hide_mei_on_error: bool,
    /// GPIO that gates the DIMM reset across S3 (coreboot
    /// `DRAM_RESET_GATE_GPIO`), driven low by SMM on S3 entry.
    pub dram_reset_gate_gpio: Option<u8>,
    /// Board EHCI debug wiring; the platform reserves its BAR only when selected.
    pub ehci_debug_bar: Option<EhciDebugConfig>,
}

impl IntelIbexPeakConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rcba: DEFAULT_RCBA,
            pmbase: DEFAULT_PMBASE,
            gpiobase: DEFAULT_GPIOBASE,
            smbus_base: DEFAULT_SMBUS_BASE,
            dmibar: 0xfed1_8000,
            desktop: false,
            gpio: GpioConfig::new(),
            gpi_routing: [GpiRoute::Disabled; 16],
            gpe0_en: 0,
            alt_gp_smi_en: 0,
            lpc_generic_io: ConstVec::new(LpcGenericIoDecode { base: 0, size: 0 }),
            sata: None,
            usb_ports: [UsbPort::disabled(); 14],
            hda: None,
            pcie_hotplug: [false; 8],
            disabled: ConstVec::new(PchFunction::Lpc),
            docking_supported: false,
            laptop: false,
            power_on_after_fail: 0,
            nmi: false,
            hide_mei_on_error: false,
            dram_reset_gate_gpio: Some(60),
            ehci_debug_bar: None,
        }
    }

    /// Whether the board left `function` enabled.
    #[must_use]
    pub fn enabled(&self, function: PchFunction) -> bool {
        !self.disabled.as_slice().contains(&function)
    }
}

impl Default for IntelIbexPeakConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Intel 5 Series PCH driver.
pub struct IntelIbexPeak {
    config: &'static IntelIbexPeakConfig,
    /// The user options over the board config.
    options: crate::southbridge::options::Options,
    smbus: Option<I801SmBus>,
    /// Advertise `_S3_`; set once training and stage caches persisted.
    s3_enabled: bool,
    /// This boot resumes from S3.
    resume: bool,
}

// SAFETY: chipset init runs on the BSP before concurrency exists.
unsafe impl Send for IntelIbexPeak {}
// SAFETY: config, optional bus state and fixed I/O bases only.
unsafe impl Sync for IntelIbexPeak {}

impl IntelIbexPeak {
    pub(crate) fn lpc(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, regs::dev::LPC.0, regs::dev::LPC.1)
    }

    pub(crate) fn lpc_regs(&self) -> &'static IbexPeakLpcConfig {
        // SAFETY: the PCH LPC bridge is the fixed function 00:1f.0 and ECAM
        // is enabled by the northbridge before any southbridge access.
        unsafe { self.lpc().regs::<IbexPeakLpcConfig>() }
    }

    pub(crate) fn rcba(&self) -> &'static RcbaRegs {
        // SAFETY: RCBA is enabled at this validated base in pre-console init.
        unsafe { &*(self.config.rcba as usize as *const RcbaRegs) }
    }

    /// Raw RCBA access for undocumented chipset-initialization values.
    pub(crate) fn rcba_raw(&self) -> RcbaWindow {
        RcbaWindow(self.config.rcba as usize)
    }

    pub(crate) fn pm(&self) -> PmIo {
        PmIo::new(self.config.pmbase)
    }

    /// The GPIO controller, for board code to take the pins it wires.
    #[must_use]
    pub const fn gpio(&self) -> IchGpio {
        IchGpio::new(self.config.gpiobase)
    }

    /// SMI routing view of this PCH for SMM installation.
    #[must_use]
    pub const fn smi(&self) -> crate::southbridge::smi::IchSmi {
        let smi = crate::southbridge::smi::IchSmi::new(
            self.config.pmbase,
            crate::southbridge::smi::ICH8_GPE0,
        )
        .with_sleep_smi(self.config.gpiobase)
        .with_power_on_after_fail(self.options.power_on_after_fail == 1);
        match self.config.dram_reset_gate_gpio {
            Some(gpio) => smi.with_dram_reset_gate(gpio),
            None => smi,
        }
    }

    #[must_use]
    pub const fn config_ref(&self) -> &'static IntelIbexPeakConfig {
        self.config
    }

    /// The board's SATA config in the user's mode.
    pub(crate) fn sata(&self) -> Option<SataConfig> {
        self.config.sata.map(|sata| SataConfig {
            mode: self.options.sata_mode.unwrap_or(sata.mode),
            ..sata
        })
    }
}

impl IntelIbexPeakConfig {
    /// The board's values of the user options.
    pub const fn options(&self) -> crate::southbridge::options::Options {
        let mut options = crate::southbridge::options::Options::from_config(
            self.power_on_after_fail,
            match self.sata {
                Some(sata) => Some(sata.mode),
                None => None,
            },
        );
        options.nmi = self.nmi;
        options
    }
}

/// The RCBA window as raw MMIO for undocumented chipset-initialization
/// registers coreboot programs as value tables (also the uncore raminit's
/// `RCBA8/32` pokes).
#[derive(Clone, Copy)]
pub(crate) struct RcbaWindow(pub(crate) usize);

impl crate::MmioBar for RcbaWindow {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x4000)
    }
}

impl crate::southbridge::smi::SmiControl for IntelIbexPeak {
    type EnableState = crate::southbridge::smi::SmiEnableState;
    type HandlerConfig = crate::southbridge::smi::IchSmmConfig;

    fn smm_handler_config(&self) -> Self::HandlerConfig {
        self.smi().smm_handler_config()
    }
    fn set_acpi_mode(&self, enabled: bool) {
        self.smi().set_acpi_mode(enabled);
    }
    fn quiesce_for_relocation(&self) -> Self::EnableState {
        self.smi().quiesce_for_relocation()
    }
    fn enable_permanent_smi(&self, previous: Self::EnableState) {
        self.smi().enable_permanent_smi(previous);
    }
    fn lock_permanent_smi(&self) -> bool {
        self.smi().lock_permanent_smi()
    }
}

/// Reject board policy the PCH cannot express before touching hardware.
fn validate(config: &IntelIbexPeakConfig) -> Result<(), ServiceError> {
    let aligned =
        |base: u64, size: u64| base != 0 && base <= u64::from(u32::MAX) && base % size == 0;
    if !aligned(config.rcba, 0x4000)
        || !aligned(config.dmibar, 0x1000)
        || config.pmbase == 0
        || config.pmbase & 0x7f != 0
        || config.gpiobase == 0
        || config.gpiobase & 0x7f != 0
        || config.smbus_base == 0
        || config.smbus_base & 0x1f != 0
        || config.power_on_after_fail > 2
        || config
            .sata
            .is_some_and(|sata| sata.ports & !0x3f != 0 || sata.interface_speed > 3)
        || config
            .usb_ports
            .iter()
            .any(|port| port.oc_pin.is_some_and(|pin| pin > 7))
        || config.dram_reset_gate_gpio.is_some_and(|gpio| gpio > 75)
        || config
            .disabled
            .as_slice()
            .iter()
            .any(|function| matches!(function, PchFunction::PcieRootPort(port) if *port > 7))
        || config
            .pcie_hotplug
            .iter()
            .enumerate()
            .any(|(port, hotplug)| {
                *hotplug && !config.enabled(PchFunction::PcieRootPort(port as u8))
            })
    {
        return Err(ServiceError::InvalidParam);
    }
    let decodes = config.lpc_generic_io.as_slice();
    if decodes.iter().any(|window| window.encode().is_none())
        || decodes
            .iter()
            .enumerate()
            .any(|(i, a)| decodes[i + 1..].iter().any(|b| a.overlaps(*b)))
    {
        return Err(ServiceError::InvalidParam);
    }
    if let Some(debug) = config.ehci_debug_bar {
        <fstart_driver_usbdebug::EhciDebug as fstart_core::services::ConsoleDevice>::new(debug)?;
        let bar = debug.fixed_bar();
        let function = match (
            bar.address.bus(),
            bar.address.device(),
            bar.address.function(),
        ) {
            (0, 0x1d, 0) => PchFunction::Ehci1,
            (0, 0x1a, 0) => PchFunction::Ehci2,
            _ => return Err(ServiceError::InvalidParam),
        };
        if !config.enabled(function)
            || bar.base == 0
            || bar.base & 0xfff != 0
            || [
                (config.rcba, 0x4000),
                (config.dmibar, 0x1000),
                (HPET_BASE as u64, 0x400),
                (IOAPIC_BASE as u64, 0x1000),
                (0xfee0_0000, 0x1000),
                (u64::from(DEFAULT_HECIBAR), 0x1000),
                (debug.ecam_base as u64, 256 << 20),
                (0xff00_0000, 16 << 20),
            ]
            .iter()
            .any(|&(base, size)| bar.base < base + size && base < bar.base + bar.size)
        {
            return Err(ServiceError::InvalidParam);
        }
    }
    Ok(())
}

impl crate::IntelSouthbridgeDriver for IntelIbexPeak {
    type Config = IntelIbexPeakConfig;
    const ACPI_LPC_NAME: &'static str = "LPCB";
    type Options = crate::southbridge::options::Options;
    const KNOBS: &'static [fstart_options::Knob<Self::Options>] = &[
        crate::southbridge::options::POWER_ON_AFTER_FAIL_KEEP,
        crate::southbridge::options::SATA_MODE,
        crate::southbridge::options::NMI,
    ];

    fn board_options(config: &Self::Config) -> Self::Options {
        config.options()
    }

    fn options(&self) -> Self::Options {
        self.options
    }

    fn set_options(&mut self, options: Self::Options) {
        self.options = options;
    }

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        validate(config)?;
        Ok(Self {
            config,
            options: Self::board_options(config),
            smbus: None,
            s3_enabled: false,
            resume: false,
        })
    }

    #[cfg(feature = "acpi")]
    fn config(&self) -> &'static Self::Config {
        self.config
    }

    /// coreboot `bootblock_early_southbridge_init()` and
    /// `ibexpeak_setup_bars()`, plus the USB rate-matching hub switch the
    /// EHCI debug console's function number depends on. Every write is
    /// idempotent: the DRAM mainstage repeats this phase.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.bootblock_init();
        self.setup_bars();
        self.default_disable();
        self.early_usb_init();
        Ok(())
    }

    /// SMBus for the SPDs and board devices, in both stages.
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.smbus = Some(I801SmBus::enable_on_i801(
            0,
            regs::dev::SMBUS.0,
            regs::dev::SMBUS.1,
            self.config.smbus_base,
        ));
        Ok(())
    }

    /// coreboot `early_pch_init()` remainder and `early_thermal_init()`:
    /// raminit-time setup that the mainstage must not repeat.
    fn pre_raminit_init(&mut self) -> Result<(), ServiceError> {
        self.gpio().setup(&self.config.gpio);
        self.setup_cir();
        self.configure_default_intmap();
        self.early_thermal_init();
        Ok(())
    }

    fn detect_s3_resume(&self) -> bool {
        self.detect_s3()
    }

    fn set_s3_enabled(&mut self, enabled: bool) {
        self.s3_enabled = enabled;
    }

    fn set_resume(&mut self, resume: bool) {
        self.resume = resume;
    }

    fn training_identity(&self) -> Option<[u8; 5]> {
        // SAFETY: fixed LPC function; the overlay is its type-0 header.
        let header = unsafe { self.lpc().regs::<fstart_pci::PciType0Config>() };
        Some(crate::southbridge::lpc::training_identity(header))
    }

    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        self.smbus.as_mut().map(|bus| bus as &mut dyn SmBus)
    }

    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        bars.push(PciFixedBar {
            address: PciAddress::new(0, 0, regs::dev::SMBUS.0, regs::dev::SMBUS.1),
            register: 0x20,
            kind: PciFixedBarType::Io,
            base: u64::from(self.config.smbus_base),
            size: 0x20,
            prefetchable: false,
        })
        .expect("Ibex Peak fixed PCI resource capacity");
        bars
    }

    /// coreboot `pch_enable()` for every function and the LPC and SATA
    /// `.enable` hooks: device enables before resource allocation.
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        self.enable_functions();
        if let Some(sata) = self.sata() {
            self.sata_enable(&sata);
        }
        Ok(())
    }

    /// Device `.init` in devicetree order, then `lpc_init()`.
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.me_init();
        if self.config.enabled(PchFunction::Ehci2) {
            self.ehci_init(regs::dev::EHCI2);
        }
        if let Some(hda) = self.config.hda.as_ref() {
            self.hda_init(hda);
        }
        if self.config.enabled(PchFunction::Ehci1) {
            self.ehci_init(regs::dev::EHCI1);
        }
        self.pci_bridge_init();
        self.lpc_init();
        if let Some(sata) = self.sata() {
            self.sata_init(&sata);
        }
        self.smbus_init();
        if self.config.enabled(PchFunction::Thermal) {
            self.thermal_init();
        }
        self.pirq_init();
        fstart_log::info!("ibexpeak: ramstage init complete");
        Ok(())
    }

    /// coreboot `lpc_final()`: SPI opcode menu, then the chipset lockdown
    /// the SMM `APM_CNT_FINALIZE` handler performs.
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        self.spi_finalize();
        self.finalize();
        Ok(())
    }

    fn program_subsystem_id(&self, device: ecam::EcamDevice, id: u32) -> bool {
        // EHCI subsystem IDs are write-protected behind ACCESS_CNTL.
        if (device.dev(), device.func()) == regs::dev::EHCI1
            || (device.dev(), device.func()) == regs::dev::EHCI2
        {
            return ramstage::with_ehci_unlocked(device, || device.set_subsystem(id));
        }
        device.set_subsystem(id)
    }
}

impl SmBus for IntelIbexPeak {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .read_byte(addr, cmd)
    }
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .write_byte(addr, cmd, value)
    }
    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .read_word(addr, cmd)
    }
    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .write_word(addr, cmd, value)
    }
    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .block_read(addr, cmd, buf)
    }
    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .block_write(addr, cmd, data)
    }
    fn i2c_eeprom_read(
        &mut self,
        addr: u8,
        offset: u8,
        buf: &mut [u8],
    ) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .i2c_eeprom_read(addr, offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;

    #[test]
    fn rejects_policy_the_pch_cannot_express() {
        static GOOD: IntelIbexPeakConfig = IntelIbexPeakConfig::new();
        assert!(IntelIbexPeak::new_from_config(&GOOD).is_ok());
        static BAD_OC: IntelIbexPeakConfig = IntelIbexPeakConfig {
            usb_ports: [UsbPort {
                enabled: true,
                current: UsbCurrent::I74B,
                oc_pin: Some(8),
            }; 14],
            ..IntelIbexPeakConfig::new()
        };
        assert!(IntelIbexPeak::new_from_config(&BAD_OC).is_err());
        static HOTPLUG_ON_DISABLED: IntelIbexPeakConfig = IntelIbexPeakConfig {
            pcie_hotplug: [false, false, true, false, false, false, false, false],
            disabled: ConstVec::new(PchFunction::Lpc).push(PchFunction::PcieRootPort(2)),
            ..IntelIbexPeakConfig::new()
        };
        assert!(IntelIbexPeak::new_from_config(&HOTPLUG_ON_DISABLED).is_err());
    }

    #[test]
    fn debug_wiring_alone_does_not_reserve_an_uninitialized_ehci_bar() {
        static CONFIG: IntelIbexPeakConfig = IntelIbexPeakConfig {
            ehci_debug_bar: Some(EhciDebugConfig {
                ecam_base: 0xe000_0000,
                bus: 0,
                device: 0x1a,
                function: 0,
                bar: DEFAULT_EHCI_DEBUG_BAR,
            }),
            ..IntelIbexPeakConfig::new()
        };
        let driver = IntelIbexPeak::new_from_config(&CONFIG).unwrap();
        let bars = driver.fixed_pci_bars();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].address, PciAddress::new(0, 0, 0x1f, 3));
        assert_eq!(bars[0].register, 0x20);
        assert_eq!(bars[0].base, u64::from(CONFIG.smbus_base));
    }

    #[test]
    fn smm_receives_the_configured_power_on_after_failure_policy() {
        use crate::southbridge::smi::{ICH8_GPE0, IchSmi, SmiControl};
        static CONFIGS: [IntelIbexPeakConfig; 3] = [
            IntelIbexPeakConfig::new(),
            IntelIbexPeakConfig {
                power_on_after_fail: 1,
                ..IntelIbexPeakConfig::new()
            },
            IntelIbexPeakConfig {
                power_on_after_fail: 2,
                ..IntelIbexPeakConfig::new()
            },
        ];
        for config in &CONFIGS {
            let pch = IntelIbexPeak::new_from_config(config).unwrap();
            let expected = IchSmi::new(config.pmbase, ICH8_GPE0)
                .with_sleep_smi(config.gpiobase)
                .with_power_on_after_fail(config.power_on_after_fail == 1);
            let expected = match config.dram_reset_gate_gpio {
                Some(gpio) => expected.with_dram_reset_gate(gpio),
                None => expected,
            };
            assert_eq!(pch.smm_handler_config(), expected.smm_handler_config());
        }
        // The user's choice, not the board's, reaches SMM.
        let mut pch = IntelIbexPeak::new_from_config(&CONFIGS[0]).unwrap();
        let board = pch.smm_handler_config();
        let mut options = pch.options();
        options.power_on_after_fail = 1;
        pch.set_options(options);
        assert_ne!(pch.smm_handler_config(), board);
        assert_eq!(
            pch.smm_handler_config(),
            IntelIbexPeak::new_from_config(&CONFIGS[1])
                .unwrap()
                .smm_handler_config()
        );
    }

    #[test]
    fn usb_current_table_matches_coreboot() {
        assert_eq!(UsbCurrent::I557.setting(), 0x557);
        assert_eq!(UsbCurrent::I74B.setting(), 0x74b);
        assert_eq!(UsbCurrent::I14B.setting(), 0x14b);
    }
}
