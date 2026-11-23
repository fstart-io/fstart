// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Sunrise Point-H PCH. The bootblock's cold path programs the decodes
//! and runs the pre-memory sequence; once DRAM is up (DISB) the later stages
//! only check those decodes. Peripheral recipes are separate PCH modules.
mod acpi;
mod media;
mod smm;
use super::{
    auxiliary, early, finalize, gpio, hda, hsio, interconnect, irq, lan, lpc, me, pcie,
    pcr::{self, Pcr},
    pmc::{self, PmcRegisters},
    pmio, power, sata, smbus as native_smbus, smi, spi, tco, thermal, usb,
};
use crate::{
    IntelSouthbridgeDriver,
    heci::Budgets,
    skylake::{cse, early_sa, sa_graphics_pm::Graphics},
    southbridge::{pmio_ich::TcoIo, smbus::I801SmBus},
};
use fstart_arch::x86::Udelay;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_rtc as rtc;
use fstart_pci::{
    EcamDevice, PciAddress, PciEcam, PciFixedBar, PciFixedBarType, PciFixedBars, PciRootInfo,
    PciWindow,
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub ecam: early_sa::EcamConfig,
    pub decodes: lpc::DecodeConfig,
    pub smbus_base: u16,
    pub heci1_base: u32,
    pub dci_enabled: bool,
    pub intrusion_smi: bool,
    pub legacy: lpc::LegacyPolicy,
    pub interrupt_devices: &'static [u8],
    pub power: power::Policy,
    pub me: me::Policy,
    pub hda: hda::Policy,
    pub sata: sata::Policy,
    pub disable_spi_write_status: bool,
    pub spi_visible_to_os: bool,
    /// Whether the OS may flash the BIOS region in band after handoff.
    pub bios_writes: spi::BiosWrites,
    /// Native PCH-H host mode; device/disabled-controller modes are not yet
    /// offered by this owner rather than silently leaving an enabled device.
    pub usb: &'static usb::Config,
    pub pcie: &'static pcie::Config,
    /// Native PCH-H PCI thermal mode; this is not an EC/fan policy.
    pub thermal: thermal::Config,
    pub lan: lan::Config,
    pub auxiliary: &'static auxiliary::Config,
    /// Low-latency policy must match the NB; PIT availability follows payload.
    pub interconnect: interconnect::Policy,
    pub s5_delay_ms: u16,
    /// Selects Mobile vs Desktop FADT PM profile.
    pub laptop: bool,
    /// Board GPIO pad table applied at the start of ramstage `chip_init`,
    /// before the PCH's own pin and CLKREQ programming.
    pub pads: &'static [gpio::PadConfig],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Phase,
    Retained,
    Decode,
    MissingSmm,
    ColdResetRequired,
    Early(early::Error),
    Lpc(lpc::Error),
    Legacy(lpc::InitError),
    Pcr(pcr::Error),
    Pmc(pmc::DecodeError),
    Hsio(hsio::Error),
    Cse(cse::Error),
    Me(me::Error),
    Irq(irq::Error),
    Power(power::Error),
    Spi(spi::Error),
    Tco(tco::Error),
    Finalize(finalize::Error),
    Smi(ServiceError),
    Hda(hda::Error),
    Sata(sata::Error),
    Usb(usb::Error),
    Pcie(pcie::Error),
    Thermal(thermal::Error),
    Lan(lan::Error),
    Auxiliary(auxiliary::Error),
    Interconnect(interconnect::Error),
    Smbus(native_smbus::Error),
    Gpio(gpio::Error),
}
macro_rules! from_error { ($($ty:ty => $variant:ident),* $(,)?) => { $(impl From<$ty> for Error { fn from(error:$ty)->Self { Self::$variant(error) } })* }; }
from_error! { early::Error=>Early, lpc::Error=>Lpc, lpc::InitError=>Legacy, pcr::Error=>Pcr,
pmc::DecodeError=>Pmc, hsio::Error=>Hsio, cse::Error=>Cse, me::Error=>Me,
irq::Error=>Irq, power::Error=>Power, spi::Error=>Spi, tco::Error=>Tco, finalize::Error=>Finalize, hda::Error=>Hda, sata::Error=>Sata, usb::Error=>Usb, pcie::Error=>Pcie, thermal::Error=>Thermal, lan::Error=>Lan, auxiliary::Error=>Auxiliary, interconnect::Error=>Interconnect, native_smbus::Error=>Smbus, gpio::Error=>Gpio }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    ColdNew,
    ColdDecoded,
    ColdReady,
    LateImported,
    Devices,
    Enumerated,
    Initialized,
    Finalized,
    Running,
    Failed,
}

pub struct IntelSunrisePoint {
    config: &'static Config,
    phase: Phase,
    pcr: Pcr,
    lpc: EcamDevice,
    pmc: EcamDevice,
    p2sb: EcamDevice,
    spi: EcamDevice,
    smbus: EcamDevice,
    heci: EcamDevice,
    bus: Option<I801SmBus>,
    pre_memory: Option<early::PreMemory>,
    me: me::NativeMe,
    smi: Option<smi::Controller>,
    hda_bar: Option<PciWindow>,
    hda_outcome: Option<hda::Outcome>,
    sata_early: Option<sata::Early>,
    sata_bar: Option<PciWindow>,
    sata_outcome: Option<sata::Outcome>,
    usb_early: Option<usb::Prepared>,
    usb_bar: Option<PciWindow>,
    usb_outcome: Option<usb::Outcome>,
    pcie_ports: Option<pcie::Ports>,
    thermal_bar: Option<PciWindow>,
    thermal_outcome: Option<thermal::Outcome>,
    lan_early: Option<lan::Prepared>,
    lan_bar: Option<PciWindow>,
    lan_outcome: Option<lan::Outcome>,
    auxiliary: Option<auxiliary::Prepared>,
    interconnect_prepared: Option<interconnect::Prepared>,
    interconnect_outcome: Option<interconnect::Outcome>,
    failure: Option<Error>,
    acpi: Option<acpi::Snapshot>,
    #[cfg(not(target_os = "none"))]
    host_baseline: bool,
}

// SAFETY: firmware runs the chipset drivers on the BSP only; the bounds are
// for the shared services (SmBus) the flow hands out.
unsafe impl Send for IntelSunrisePoint {}
unsafe impl Sync for IntelSunrisePoint {}

/// The CSE's MBP and HSIO messages, in the ramstage only.
fn mbp_scratch() -> Option<&'static mut [u8; cse::MBP_BUFFER_BYTES]> {
    static TAKEN: fstart_core::once::TakeOnce = fstart_core::once::TakeOnce::new();
    static mut MBP: [u8; cse::MBP_BUFFER_BYTES] = [0; cse::MBP_BUFFER_BYTES];
    // SAFETY: the claim hands out the buffer once.
    TAKEN
        .take()
        .then(|| unsafe { &mut *core::ptr::addr_of_mut!(MBP) })
}
impl IntelSunrisePoint {
    /// Pure policy validation shared by host planning and actual owners.
    pub fn validate_config(config: &Config) -> Result<(), Error> {
        config.ecam.encoded_pciexbar().map_err(|_| Error::Policy)?;
        config.legacy.interrupts.validate()?;
        config.sata.validate()?;
        config.usb.validate()?;
        config.pcie.validate()?;
        config.thermal.validate()?;
        config.auxiliary.validate(config.interrupt_devices)?;
        config.lan.validate(config.pcie.lan_clkreq)?;
        if !config
            .interrupt_devices
            .contains(&fstart_pci::devfn(0x14, 2))
            || config.lan.enabled
                != config
                    .interrupt_devices
                    .contains(&fstart_pci::devfn(0x1f, 6))
        {
            return Err(Error::Policy);
        }
        config
            .pcie
            .validate_interrupt_devices(config.interrupt_devices)?;
        if !config
            .interrupt_devices
            .contains(&fstart_pci::devfn(0x14, 0))
            || config
                .interrupt_devices
                .contains(&fstart_pci::devfn(0x14, 1))
        {
            return Err(Error::Policy);
        }
        if config.sata.enabled()
            != config
                .interrupt_devices
                .contains(&fstart_pci::devfn(0x17, 0))
        {
            return Err(Error::Policy);
        }
        if config.smbus_base == 0
            || config.smbus_base & 31 != 0
            || config.smbus_base.checked_add(31).is_none()
            || config.heci1_base == 0
            || config.heci1_base & 0xfff != 0
            || config.s5_delay_ms > 1000
            || config.hda.enabled()
                != config
                    .interrupt_devices
                    .contains(&fstart_pci::devfn(0x1f, 3))
        {
            return Err(Error::Policy);
        }
        for (base, size) in [(pmc::ACPI_BASE, 0x100u32), (tco::BASE, 0x20)] {
            if u32::from(config.smbus_base) < u32::from(base) + size
                && u32::from(base) < u32::from(config.smbus_base) + 32
            {
                return Err(Error::Policy);
            }
        }
        Ok(())
    }
    /// # Safety
    /// The BSP owns the PCH: its PCR, ECAM, PWRM, SPI, HECI1 and legacy
    /// ports. The system agent enabled ECAM before `pre_console_init`.
    pub unsafe fn new(config: &'static Config) -> Result<Self, Error> {
        Self::validate_config(config)?;
        let root = PciRootInfo {
            segment: 0,
            ecam_base: config.ecam.base,
            bus_start: 0,
            bus_end: (config.ecam.buses - 1) as u8,
        };
        let bytes = usize::from(config.ecam.buses) << 20;
        let [
            Some(lpc),
            Some(pmc),
            Some(p2sb),
            Some(spi),
            Some(smbus),
            Some(heci),
        ] = [
            (0x1f, 0),
            (0x1f, 2),
            (0x1f, 1),
            (0x1f, 5),
            (0x1f, 4),
            (0x16, 0),
        ]
        .map(|(dev, func)| EcamDevice::from_root(root, bytes, PciAddress::new(0, 0, dev, func)))
        else {
            return Err(Error::Policy);
        };
        Ok(Self {
            config,
            phase: Phase::ColdNew,
            pcr: unsafe { Pcr::at(pcr::DEFAULT_BASE as usize) }?,
            lpc,
            pmc,
            p2sb,
            spi,
            smbus,
            heci,
            bus: None,
            pre_memory: None,
            me: me::NativeMe::new(config.me),
            smi: None,
            hda_bar: None,
            hda_outcome: None,
            sata_early: None,
            sata_bar: None,
            sata_outcome: None,
            usb_early: None,
            usb_bar: None,
            usb_outcome: None,
            pcie_ports: None,
            thermal_bar: None,
            thermal_outcome: None,
            lan_early: None,
            lan_bar: None,
            lan_outcome: None,
            auxiliary: None,
            interconnect_prepared: None,
            interconnect_outcome: None,
            acpi: None,
            failure: None,
            #[cfg(not(target_os = "none"))]
            host_baseline: false,
        })
    }

    /// Populate synthetic host-side state so `check-acpi` can assemble the
    /// PCH ACPI tables without touching live PCI/MMIO/PCR hardware.
    #[cfg(not(target_os = "none"))]
    pub fn set_host_acpi_baseline(&mut self) {
        self.host_baseline = true;
        self.acpi = Some(acpi::Snapshot::host_baseline(self.config));
    }

    /// Observed pre-memory state from this PCH's early init, not a fresh
    /// classification after wake status has been cleared.
    pub fn pre_memory_context(&self) -> Option<early::PreMemory> {
        self.pre_memory
    }

    pub fn interconnect_outcome(&self) -> Option<interconnect::Outcome> {
        self.interconnect_outcome
    }
    pub fn auxiliary_outcome(&self) -> Option<auxiliary::Outcome> {
        self.auxiliary.as_ref().map(auxiliary::Prepared::outcome)
    }
    pub fn thermal_outcome(&self) -> Option<thermal::Outcome> {
        self.thermal_outcome
    }
    pub fn lan_outcome(&self) -> Option<lan::Outcome> {
        self.lan_outcome
    }
    pub fn pcie_outcome(&self) -> Option<pcie::Outcome> {
        self.pcie_ports.as_ref().map(pcie::Ports::outcome)
    }
    pub fn usb_outcome(&self) -> Option<usb::Outcome> {
        self.usb_outcome
    }
    pub fn sata_outcome(&self) -> Option<sata::Outcome> {
        self.sata_outcome
    }
    pub fn hda_outcome(&self) -> Option<hda::Outcome> {
        self.hda_outcome
    }
    pub fn failure(&self) -> Option<Error> {
        self.failure
    }
    fn finish(&mut self, result: Result<(), Error>, next: Phase) -> Result<(), ServiceError> {
        match result {
            Ok(()) => {
                self.phase = next;
                Ok(())
            }
            Err(error) => {
                // A later rejected call must not erase the causal failure.
                self.failure.get_or_insert(error);
                self.phase = Phase::Failed;
                Err(ServiceError::HardwareError)
            }
        }
    }
    fn reject(&mut self) -> Result<(), ServiceError> {
        self.finish(Err(Error::Phase), Phase::Failed)
    }
    fn family(&self) -> Result<(), Error> {
        if fstart_pci::ecam::base() != self.config.ecam.base as usize {
            return Err(Error::Decode);
        }
        if hsio::Silicon::read(&self.lpc)?.kind() != super::dmi::Kind::H {
            return Err(Error::Policy);
        }
        let root = EcamDevice::from_root(
            PciRootInfo {
                segment: 0,
                ecam_base: self.config.ecam.base,
                bus_start: 0,
                bus_end: (self.config.ecam.buses - 1) as u8,
            },
            usize::from(self.config.ecam.buses) << 20,
            PciAddress::new(0, 0, 0, 0),
        )
        .ok_or(Error::Policy)?;
        early_sa::require_ecam(&root, self.config.ecam).map_err(|_| Error::Decode)
    }
    fn existing(&self) -> Result<(), Error> {
        self.family()?;
        pmc::require_decodes(&self.pmc)?;
        if self.p2sb.memory_bar(0x10) != Some(u64::from(pcr::DEFAULT_BASE))
            || self.p2sb.read16(4) & 2 == 0
            || self.spi.memory_bar(0x10) != Some(early::SPI_BASE as u64)
            || self.spi.read16(4) & 2 == 0
            || self.smbus.vendor_id() != 0x8086
            || self.smbus.read32(0x20) != (u32::from(self.config.smbus_base) | 1)
            || self.smbus.read8(0x40) & 1 == 0
            || self.smbus.read16(4) & 1 == 0
            || self.heci.memory_bar(0x10) != Some(u64::from(self.config.heci1_base))
            || self.heci.read16(4) & 6 != 6
        {
            return Err(Error::Decode);
        }
        tco::validate_existing(&self.smbus, &self.pcr)?;
        if !TcoIo::new(tco::BASE).is_halted() {
            return Err(Error::Decode);
        }
        Ok(())
    }
}
impl IntelSunrisePoint {
    /// After PCI allocation: take the BARs it assigned to the devices
    /// `initialize_devices` programs, and set up legacy LPC.
    pub fn post_pci_init(&mut self, pci: &PciEcam) -> Result<(), ServiceError> {
        if self.phase != Phase::Devices {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            auxiliary::finish(self.auxiliary.as_ref().ok_or(Error::Phase)?, pci)?;
            pcie::finish(
                self.config.pcie,
                self.pcie_ports.as_ref().ok_or(Error::Phase)?,
                &self.pcr,
                pci,
            )?;
            if self.config.hda.enabled() {
                self.hda_bar = Some(
                    pci.assigned_memory_bar(PciAddress::new(0, 0, 0x1f, 3), 0x10)
                        .ok_or(Error::Hda(hda::Error::Resource))?,
                );
            }
            if matches!(self.sata_early.as_ref(), Some(sata::Early::Ahci(_))) {
                self.sata_bar = Some(
                    pci.assigned_memory_bar(PciAddress::new(0, 0, 0x17, 0), 0x24)
                        .ok_or(Error::Sata(sata::Error::Resource))?,
                );
            }
            self.usb_bar = Some(
                pci.assigned_memory_bar(PciAddress::new(0, 0, 0x14, 0), 0x10)
                    .ok_or(Error::Usb(usb::Error::Resource))?,
            );
            self.thermal_bar = Some(
                pci.assigned_memory_bar(PciAddress::new(0, 0, 0x14, 2), 0x10)
                    .ok_or(Error::Thermal(thermal::Error::Resource))?,
            );
            if self.lan_early.as_ref().ok_or(Error::Phase)?.enabled() {
                self.lan_bar = Some(
                    pci.assigned_memory_bar(PciAddress::new(0, 0, 0x1f, 6), 0x10)
                        .ok_or(Error::Lan(lan::Error::Resource))?,
                );
            }
            // Native LPC/eSPI initialization runs AFTER the canonical probe;
            // it already enables master/slave bus mastering. No second rearm
            // transaction or unused interface snapshot is needed.
            unsafe {
                lpc::initialize_legacy(&self.lpc, &self.pcr, pci, self.config.legacy, &mut Udelay)
            }?;
            unsafe {
                native_smbus::finish(&self.smbus, pci, self.bus.as_ref().ok_or(Error::Phase)?)?;
                self.interconnect_outcome = Some(interconnect::finish(
                    self.interconnect_prepared.take().ok_or(Error::Phase)?,
                    &self.pcr,
                )?);
            }
            let lost = unsafe { self.pmc.register::<u32, pmc::GenPmconB::Register>(0xa4) }
                .is_set(pmc::GenPmconB::RTC_BATTERY_DEAD);
            rtc::init_clock("sunrise-point", lost);
            {
                let excluded = self.auxiliary.as_ref().ok_or(Error::Phase)?.excluded();
                self.acpi = Some(acpi::Snapshot::capture(
                    pci,
                    self.pcie_ports.as_ref().ok_or(Error::Phase)?,
                    &excluded,
                )?);
            }
            Ok(())
        })();
        self.finish(
            result.inspect_err(|error| {
                fstart_log::error!("PCH post-PCI: {:?}", fstart_log::DebugValue(error));
            }),
            Phase::Enumerated,
        )
    }

    /// Complete native device init with the system agent's scoped IGD engine.
    pub fn initialize_devices(
        &mut self,
        display_audio: Option<(&Graphics<'_>, bool, bool)>,
    ) -> Result<(), ServiceError> {
        if self.phase != Phase::Enumerated {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            unsafe {
                self.thermal_outcome = Some(thermal::initialize(
                    self.config.thermal,
                    &self.pcr,
                    self.thermal_bar.ok_or(Error::Phase)?,
                )?);
                self.lan_outcome = Some(lan::finish(
                    self.lan_early.take().ok_or(Error::Phase)?,
                    self.lan_bar,
                    self.config.power.wol_override,
                )?);
                self.sata_outcome = Some(sata::finish(
                    self.sata_early.take().ok_or(Error::Phase)?,
                    self.sata_bar,
                )?);
                self.hda_outcome = Some(hda::initialize(
                    &self.pcr,
                    self.config.hda,
                    self.hda_bar,
                    display_audio,
                    &mut Udelay,
                )?);
                self.usb_outcome = Some(usb::initialize(
                    self.usb_early.take().ok_or(Error::Phase)?,
                    self.config.usb,
                    &self.pcr,
                    &self.p2sb,
                    self.usb_bar.ok_or(Error::Phase)?,
                    &mut Udelay,
                )?);
                power::configure_native(
                    PmcRegisters::at(pmc::PWRM_BASE),
                    &self.pmc,
                    &self.lpc,
                    &self.pcr,
                    self.config.power,
                )?;
                self.smi = Some(
                    smi::Controller::bind(
                        self.config.ecam,
                        tco::BASE,
                        self.config.power.after_power_loss,
                        self.config.s5_delay_ms,
                    )
                    .map_err(Error::Smi)?,
                );
            }
            Ok(())
        })();
        self.finish(
            result.inspect_err(|error| {
                fstart_log::error!("PCH device init: {:?}", fstart_log::DebugValue(error));
            }),
            Phase::Initialized,
        )
    }
}

impl IntelSouthbridgeDriver for IntelSunrisePoint {
    type Config = Config;
    type Options = ();
    const KNOBS: &'static [fstart_options::Knob<()>] = &[];
    fn board_options(_: &Config) {}
    fn options(&self) {}
    fn set_options(&mut self, _: ()) {}
    fn training_identity(&self) -> Option<[u8; 5]> {
        // SAFETY: fixed LPC/eSPI function; the overlay is its type-0 header.
        let header = unsafe { self.lpc.regs::<fstart_pci::overlay::PciType0Config>() };
        Some(crate::southbridge::lpc::training_identity(header))
    }
    const ACPI_LPC_NAME: &'static str = "LPCB";
    fn new_from_config(config: &'static Config) -> Result<Self, ServiceError> {
        // SAFETY: the Intel flow constructs one southbridge per stage, on
        // the BSP.
        unsafe { Self::new(config) }.map_err(|_| ServiceError::InvalidParam)
    }
    fn config(&self) -> &'static Config {
        self.config
    }
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        if self.phase == Phase::ColdNew && self.existing().is_ok() {
            // A stage after the bootblock, which programmed these decodes and
            // trained DRAM in this boot.
            self.phase = Phase::Running;
            let trained = unsafe { self.pmc.register::<u32, pmc::GenPmconA::Register>(0xa0) }
                .is_set(pmc::GenPmconA::DISB);
            let result = if trained {
                self.bus = Some(I801SmBus::new(self.config.smbus_base));
                Ok(())
            } else {
                Err(Error::Decode)
            };
            return self.finish(result, Phase::LateImported);
        }
        if self.phase != Phase::ColdNew {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            fstart_log::debug!("PCH pre-console: identify silicon and verify ECAM");
            self.family()?;
            fstart_log::debug!("PCH pre-console: P2SB decode");
            pcr::enable_p2sb(&self.p2sb, pcr::DEFAULT_BASE)?;
            fstart_log::debug!("PCH pre-console: PMC decodes");
            pmc::enable_decodes(&self.pmc, &self.pcr)?;
            fstart_log::debug!("PCH pre-console: capture wake/reset state");
            let pmc = unsafe { PmcRegisters::at(pmc::PWRM_BASE) };
            let saved = early::capture_power_state(pmc)?;
            if saved.mode() == pmc::MemoryBootMode::Warm {
                // A hard reset, such as the OS's reboot through CF9, keeps
                // DRAM in self-refresh and the memory controller programmed.
                // There is no self-refresh exit path yet, and cold training
                // needs the controller at its reset state, so power-cycle
                // through S5 into a cold boot. Clear DISB first (native
                // pmc_clear_disb) so that boot is classified cold.
                pmc::clear_dram_initialized(&self.pmc)?;
                fstart_arch::x86::boot::full_reset();
            }
            if saved.mode() != pmc::MemoryBootMode::Cold {
                fstart_log::error!("PCH pre-console: retained boot state");
                return Err(Error::Retained);
            }
            // Saved rejection precedes SPI write-enable and watchdog clearing.
            fstart_log::debug!("PCH pre-console: LPC decodes");
            lpc::configure_decodes(&self.lpc, &self.pcr, &self.config.decodes)?;
            unsafe {
                fstart_log::debug!("PCH pre-console: SPI decode");
                spi::initialize_cold(&self.spi, saved)?;
                fstart_log::debug!("PCH pre-console: TCO watchdog");
                tco::configure_cold(&self.smbus, &self.pcr, saved, self.config.intrusion_smi)?;
            }
            fstart_log::debug!("PCH pre-console: SMBus decode");
            self.bus = Some(I801SmBus::enable_on_i801(
                fstart_pci::devfn(0x1f, 4),
                self.config.smbus_base,
            ));
            fstart_log::debug!("PCH pre-console: HECI BAR");
            unsafe {
                cse::assign_bar(&self.heci, self.config.heci1_base)?;
            }
            fstart_log::debug!("PCH pre-console: verify decodes");
            self.existing()
        })();
        self.finish(result, Phase::ColdDecoded)
    }
    fn early_init(&mut self) -> Result<(), ServiceError> {
        if self.phase == Phase::LateImported {
            return Ok(());
        }
        if self.phase != Phase::ColdDecoded {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            self.existing()?;
            let mut cse = unsafe { cse::MmioCse::bind(self.heci, Budgets::SOURCE) }?;
            cse.initialize(&mut Udelay)?;
            let observed = unsafe {
                early::pre_memory(
                    &self.pcr,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    &mut cse,
                    self.config.dci_enabled,
                    &mut Udelay,
                )
            }?;
            self.pre_memory = Some(observed);
            Ok(())
        })();
        self.finish(result, Phase::ColdReady)
    }
    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        if !matches!(
            self.phase,
            Phase::ColdReady
                | Phase::LateImported
                | Phase::Devices
                | Phase::Enumerated
                | Phase::Initialized
        ) {
            return None;
        }
        self.bus.as_mut().map(|bus| bus as &mut dyn SmBus)
    }
    /// Before PCI enumeration: board GPIO pads, HSIO, ME, root ports and function visibility.
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        if self.phase != Phase::LateImported {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            self.existing()?;
            if !self.config.pads.is_empty() {
                gpio::Gpio {
                    pcr: &self.pcr,
                    program_itss_polarity: true,
                }
                .configure_pads(self.config.pads)?;
            }
            let pcie_prepared = unsafe {
                pcie::prepare(
                    self.config.pcie,
                    &self.pcr,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    self.config.ecam.buses,
                )
            }?;
            let scratch = mbp_scratch().ok_or(Error::Phase)?;
            let (request, response) = scratch.split_at_mut(2576);
            let mut cse = unsafe { cse::MmioCse::bind(self.heci, Budgets::SOURCE) }?;
            match hsio::post_memory(
                &self.lpc,
                &mut cse,
                request,
                &mut response[..20],
                &mut Udelay,
            )? {
                hsio::SyncOutcome::InSync => {}
                hsio::SyncOutcome::ColdResetRequired => return Err(Error::ColdResetRequired),
            }
            #[allow(clippy::drop_non_drop)] // the HECI binding ends here
            drop(cse);
            unsafe {
                hda::prepare(&self.pcr, PmcRegisters::at(pmc::PWRM_BASE), self.config.hda)?;
                self.sata_early = Some(sata::prepare(
                    &self.pcr,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    self.config.sata,
                    &mut Udelay,
                )?);
                self.auxiliary = Some(auxiliary::prepare(
                    self.config.auxiliary,
                    &self.pcr,
                    PmcRegisters::at(pmc::PWRM_BASE),
                )?);
                self.usb_early = Some(usb::prepare(&self.pcr, &mut Udelay)?);
                self.me.initialize(&self.pcr, scratch, &mut Udelay)?;
                interconnect::before_root_ports(
                    &self.pcr,
                    &self.p2sb,
                    pcie_prepared.silicon,
                    self.config.interconnect,
                )?;
                let mut fabric =
                    pcie::Fabric::new(&self.pcr, &self.p2sb, pcie_prepared.silicon, &mut Udelay)?;
                fabric.grants()?;
                let discovered = pcie::discover(pcie_prepared, &mut fabric, &mut Udelay)?;
                let mut interrupt_devices =
                    discovered.interrupt_devices(self.config.interrupt_devices)?;
                if self.auxiliary.as_ref().ok_or(Error::Phase)?.outcome().ish
                    != super::ish::Outcome::Pci
                {
                    // A requested but actually fused/disabled ISH is not an
                    // ECAM interrupt target. Do not write absent config space.
                    interrupt_devices.retain(|devfn| *devfn != 0x98);
                }
                irq::configure_native(
                    &self.pcr,
                    &interrupt_devices,
                    self.config.legacy.interrupts,
                )?;
                self.pcie_ports = Some(pcie::initialize(
                    discovered,
                    &mut fabric,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    &mut Udelay,
                )?);
                self.lan_early = Some(lan::prepare(
                    self.config.lan,
                    self.config.pcie.lan_clkreq,
                    &self.pcr,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    &self.spi,
                )?);
                self.interconnect_prepared = Some(interconnect::after_root_ports(
                    &self.pcr,
                    fabric.silicon,
                    self.config.pcie.vtd_policy,
                    self.config.interconnect,
                )?);
                native_smbus::prepare(
                    &self.smbus,
                    &self.pcr,
                    self.bus.as_ref().ok_or(Error::Phase)?,
                )?;
                spi::configure_power(&self.spi, &self.pcr)?;
                lpc::configure_power(&self.pcr)?;
                interconnect::housekeeping(&self.pcr, self.config.interconnect)?;
            }
            Ok(())
        })();
        self.finish(
            result.inspect_err(|error| {
                fstart_log::error!("PCH pre-PCI: {:?}", fstart_log::DebugValue(error));
            }),
            Phase::Devices,
        )
    }
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        // Native shared-engine init must consume the NB borrow, not rediscover
        // or remap IGD independently through the old no-context entry point.
        self.reject()
    }
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        if self.phase != Phase::Initialized {
            return self.reject();
        }
        self.phase = Phase::Running;
        let result = (|| {
            if !unsafe { self.pmc.register::<u32, pmc::GenPmconA::Register>(0xa0) }
                .is_set(pmc::GenPmconA::SMI_LOCK)
                || !pmio::AcpiIo::new(pmc::ACPI_BASE).smi_enable().matches_all(
                    pmio::SmiEnable::GLOBAL::SET
                        + pmio::SmiEnable::APM::SET
                        + pmio::SmiEnable::SLEEP::SET
                        + pmio::SmiEnable::TCO::SET,
                )
            {
                return Err(Error::MissingSmm);
            }
            if self.config.hda.enabled() {
                // common/block/hda/hda.c final: request bus mastering only
                // after native initialization and actual SMM installation.
                // This does not start a CORB/RIRB stream or prove DMA safety.
                let device = EcamDevice::new(0, 0x1f, 3);
                if device.vendor_id() != 0x8086 {
                    return Err(Error::Decode);
                }
                let command =
                    unsafe { device.register::<u16, fstart_pci::PCI_COMMAND_BITS::Register>(4) };
                command.modify(fstart_pci::PCI_COMMAND_BITS::BUS_MASTER::SET);
                if !command.is_set(fstart_pci::PCI_COMMAND_BITS::BUS_MASTER) {
                    return Err(Error::Decode);
                }
            }
            // Native pch_finalize_script locks TCO before its CSE ready hook.
            // Permanent SMM and TCO_EN have already been installed above.
            tco::lockdown(&self.smbus, &self.pcr)?;
            self.me.ready_to_boot(&self.pcr, &mut Udelay)?;
            if !self.config.spi_visible_to_os {
                spi::hide_from_os(&self.spi)?;
            }
            unsafe {
                spi::lockdown(
                    &self.spi,
                    self.config.disable_spi_write_status,
                    self.config.bios_writes,
                )?;
            }
            self.me.finish(&self.pcr, &mut Udelay)?;
            unsafe {
                finalize::lock_native(
                    &self.pmc,
                    &self.p2sb,
                    PmcRegisters::at(pmc::PWRM_BASE),
                    true,
                )?;
            }
            // Native pm1_enable_pwrbtn_smi (skylake/pmc.c), last thing before
            // the payload: with SCI_EN clear a press raises a PM1 SMI and the
            // handler enters S5. Drop the press that powered the board on.
            let acpi = pmio::AcpiIo::new(pmc::ACPI_BASE);
            acpi.status()
                .write(crate::southbridge::pmio_ich::PM_STATUS::POWER_BUTTON::SET);
            acpi.enable().modify(pmio::Enable::POWER_BUTTON::SET);
            Ok(())
        })();
        self.finish(
            result.inspect_err(|error| {
                fstart_log::error!("PCH finalize: {:?}", fstart_log::DebugValue(error));
            }),
            Phase::Finalized,
        )
    }
    fn excluded_pci_functions(&self) -> fstart_pci::PciExcludedFunctions {
        self.auxiliary
            .as_ref()
            .map(|auxiliary| auxiliary.excluded())
            .unwrap_or_default()
    }
    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        for (device, register, kind, base, size) in [
            (
                self.p2sb,
                0x10,
                PciFixedBarType::Memory64,
                u64::from(pcr::DEFAULT_BASE),
                pcr::WINDOW_SIZE as u64,
            ),
            (
                self.spi,
                0x10,
                PciFixedBarType::Memory32,
                early::SPI_BASE as u64,
                0x1000,
            ),
            (
                self.smbus,
                0x20,
                PciFixedBarType::Io,
                u64::from(self.config.smbus_base),
                0x20,
            ),
            (
                self.heci,
                0x10,
                if self.heci.read32(0x10) & 6 == 4 {
                    PciFixedBarType::Memory64
                } else {
                    PciFixedBarType::Memory32
                },
                u64::from(self.config.heci1_base),
                0x1000,
            ),
        ] {
            // HECI1 can already be hidden by the explicit native BIOS-path policy.
            if device.is_present() {
                bars.push(PciFixedBar {
                    address: device.address(),
                    register,
                    kind,
                    base,
                    size,
                    prefetchable: false,
                })
                .expect("four fixed PCH BARs");
            }
        }
        if let Some(aux) = &self.auxiliary {
            for bar in aux.fixed() {
                bars.push(*bar)
                    .expect("validated native fixed BAR capacity");
            }
        }
        bars
    }
}

/// The SMBus, once `pre_console_init` set it up.
impl SmBus for IntelSunrisePoint {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .read_byte(addr, cmd)
    }
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .write_byte(addr, cmd, value)
    }
    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .read_word(addr, cmd)
    }
    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .write_word(addr, cmd, value)
    }
    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .block_read(addr, cmd, buf)
    }
    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        self.bus
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
        self.bus
            .as_mut()
            .ok_or(ServiceError::NotInitialized)?
            .i2c_eeprom_read(addr, offset, buf)
    }
}
