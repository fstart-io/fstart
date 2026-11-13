//! The south cluster: the PCU (LPC, PMC, SMBus, SPI, GPIO, legacy block)
//! and the PCI functions behind it (coreboot bootblock, `southcluster.c`,
//! `ramstage.c` and the device drivers).

use super::devices::lpe::LpeConfig;
use super::devices::pcie::{PcieConfig, PcieState};
use super::devices::sata::SataConfig;
use super::devices::scc::SccConfig;
use super::devices::usb::UsbConfig;
use super::devices::{self, AcpiModeDevice, Function, Functions};
use super::gpio::{self, GpioConfig};
use super::lpc::{self, SpiLockdown};
use super::pmc::{self, PowerState};
use super::smi::BaytrailSmi;
use super::{SMBUS_BASE, iosf};
use crate::southbridge::pirq::PirqRouting;
use crate::southbridge::pmio_ich as pmio;
use crate::southbridge::smbus::I801SmBus;
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam::GlobalEcam;
use fstart_pci::{PciAddress, PciFixedBar, PciFixedBarType, PciFixedBars};
use tock_registers::interfaces::{ReadWriteable, Writeable};

/// South cluster policy (coreboot `soc_intel_baytrail_config` and the
/// devicetree's south cluster functions).
#[derive(Debug, Clone, Copy)]
pub struct SouthConfig {
    /// The PCU's legacy COM1 at 0x3f8.
    pub com1: bool,
    pub gpio: GpioConfig,
    /// Per-device INTx to PIRQ routing.
    pub pirq: PirqRouting,
    /// PIC IRQ of each PIRQ in legacy mode; 0 leaves it unrouted.
    pub pic_routes: [u8; 8],
    /// Functions the board does not use.
    pub disabled: Functions,
    pub sata: SataConfig,
    pub usb: UsbConfig,
    pub pcie: PcieConfig,
    /// PCIe devices may wake the system from sleep.
    pub pcie_wake: bool,
    pub scc: SccConfig,
    /// LPSS functions in ACPI mode.
    pub lpss_acpi_mode: bool,
    pub lpe: LpeConfig,
    /// Disable SLP_X stretching after suspend well power loss.
    pub disable_slp_x_stretch_sus_fail: bool,
    /// SPI opcode menu locked at the end of the boot.
    pub spi_lockdown: Option<SpiLockdown>,
}

/// Bay Trail per-device routing: every device's INTA-D on PIRQA-D.
pub const DEFAULT_PIRQ: PirqRouting = super::irq::IDENTITY_ROUTING;

impl SouthConfig {
    pub const fn new() -> Self {
        Self {
            com1: true,
            gpio: GpioConfig::new(),
            pirq: DEFAULT_PIRQ,
            pic_routes: [0; 8],
            disabled: Functions::NONE,
            sata: SataConfig {
                port_map: 0x3,
                ahci: true,
                ide_legacy_combined: false,
            },
            usb: UsbConfig::new(),
            pcie: PcieConfig::new(),
            pcie_wake: false,
            scc: SccConfig {
                sdcard_cap: None,
                acpi_mode: false,
            },
            lpss_acpi_mode: false,
            lpe: LpeConfig {
                codec_clock: None,
                acpi_mode: false,
            },
            disable_slp_x_stretch_sus_fail: false,
            spi_lockdown: None,
        }
    }

    pub const fn enabled(&self, function: Function) -> bool {
        !self.disabled.contains(function)
    }
}

impl Default for SouthConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// ACPI-mode functions, for the DSDT.
pub type AcpiModeDevices = heapless::Vec<AcpiModeDevice, 20>;

/// The south cluster driver.
pub struct BaytrailSouth {
    config: &'static SouthConfig,
    smbus: Option<I801SmBus>,
    resume: bool,
    pcie: PcieState,
    acpi_devices: AcpiModeDevices,
}

impl BaytrailSouth {
    pub fn config_ref(&self) -> &'static SouthConfig {
        self.config
    }

    /// Functions firmware switched to ACPI mode.
    pub fn acpi_devices(&self) -> &[AcpiModeDevice] {
        &self.acpi_devices
    }

    /// PIRQ PIC routes, per-device routes, SCI on IRQ 9, the 8259s, the
    /// RTC and SLP_X stretching (coreboot `sc_init`).
    fn sc_init(&self) {
        let ilb = lpc::ilb();
        for (pirq, irq) in ilb.pirq.iter().zip(self.config.pic_routes) {
            pirq.set(irq);
        }
        for (slot, ir) in ilb.ir.iter().enumerate() {
            ir.set(self.config.pirq.ir_value(slot as u8));
        }
        lpc::route_sci_irq9();
        // The MADT describes the IOAPIC by this ID.
        // SAFETY: the IOAPIC is decoded and only firmware touches it now.
        unsafe { fstart_arch::x86::ioapic::IoApic::new(super::IOAPIC_BASE as usize) }
            .set_id(super::IOAPIC_ID);
        fstart_arch::x86::legacy_pc::setup_i8259();
        fstart_driver_rtc::init_clock("baytrail", PowerState::read().rtc_failed());
        let pmcon1 = &pmc::regs().gen_pmcon1;
        if self.config.disable_slp_x_stretch_sus_fail {
            fstart_log::debug!("baytrail: SLP_X stretching off");
            pmcon1.modify(pmc::GEN_PMCON1::DIS_SLP_X_STRCH_SUS_UP::SET);
        } else {
            pmcon1.modify(pmc::GEN_PMCON1::DIS_SLP_X_STRCH_SUS_UP::CLEAR);
        }
        if self.resume && lpc::com1_enabled() {
            com1_configure_resume();
        }
    }
}

/// The UART loses power in suspend; leave it in 115200 8N1 for an OS
/// console that does not reprogram it on resume.
fn com1_configure_resume() {
    const PORT: u16 = 0x3f8;
    // SAFETY: COM1 is decoded by the PCU UART.
    unsafe {
        use fstart_core::pio::outb;
        outb(PORT + 1, 0x00);
        outb(PORT + 2, 0x01);
        outb(PORT + 4, 0x03);
        outb(PORT + 3, 0x83);
        outb(PORT, 1);
        outb(PORT + 1, 0);
        outb(PORT + 3, 0x03);
    }
}

impl crate::IntelSouthbridgeDriver for BaytrailSouth {
    type Config = SouthConfig;
    const ACPI_LPC_NAME: &'static str = "LPCB";

    /// Bay Trail has no chipset options yet: the board's policy is all in
    /// [`SouthConfig`].
    type Options = ();
    const KNOBS: &'static [fstart_options::Knob<Self::Options>] = &[];

    fn options(&self) -> Self::Options {}

    fn set_options(&mut self, _options: Self::Options) {}

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        Ok(Self {
            config,
            smbus: None,
            resume: false,
            pcie: PcieState::default(),
            acpi_devices: AcpiModeDevices::new(),
        })
    }

    #[cfg(feature = "acpi")]
    fn config(&self) -> &'static Self::Config {
        self.config
    }

    /// Decode the fixed blocks, halt the TCO watchdog, bring out COM1 and
    /// cache the boot flash (coreboot bootblock).
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        lpc::program_bases(&GlobalEcam);
        lpc::tco_disable();
        if self.config.com1 {
            lpc::enable_com1(&GlobalEcam);
        }
        lpc::spi_init();
        Ok(())
    }

    /// The SMBus host and its pads.
    fn early_init(&mut self) -> Result<(), ServiceError> {
        if self.smbus.is_none() {
            let (dev, func) = Function::Smbus.devfn();
            self.smbus = Some(I801SmBus::enable_on_i801(0, dev, func, SMBUS_BASE));
            gpio::Bank::Score.select_function(gpio::score_pad::PCU_SMB_CLK, 1);
            gpio::Bank::Score.select_function(gpio::score_pad::PCU_SMB_DATA, 1);
        }
        Ok(())
    }

    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        let (dev, func) = Function::Smbus.devfn();
        bars.push(PciFixedBar {
            address: PciAddress::new(0, 0, dev, func),
            register: 0x20,
            kind: PciFixedBarType::Io,
            base: u64::from(SMBUS_BASE),
            size: 0x20,
            prefetchable: false,
        })
        .expect("Bay Trail fixed PCI resource list capacity");
        bars
    }

    /// Log the boot's power state and clear a stale sleep type (coreboot
    /// `fill_power_state`/`chipset_prev_sleep_state`). Native raminit has no
    /// S3 path, so this never reports a resume and the tables never offer
    /// S3.
    fn detect_s3_resume(&self) -> bool {
        let ps = PowerState::read();
        fstart_log::debug!(
            "baytrail: pm1_sts {:#06x} pm1_en {:#06x} pm1_cnt {:#010x}",
            ps.pm1_sts,
            ps.pm1_en,
            ps.pm1_cnt
        );
        fstart_log::debug!(
            "baytrail: gpe0_sts {:#010x} gpe0_en {:#010x} tco_sts {:#010x}",
            ps.gpe0_sts,
            ps.gpe0_en,
            ps.tco_sts
        );
        fstart_log::debug!(
            "baytrail: prsts {:#010x} gen_pmcon1 {:#010x} gen_pmcon2 {:#010x}",
            ps.prsts,
            ps.gen_pmcon1,
            ps.gen_pmcon2
        );
        if ps.pm1_sts & pmio::WAK_STS != 0 {
            let slp_typ = (ps.pm1_cnt & pmio::SLP_TYP_MASK) >> pmio::SLP_TYP_SHIFT;
            if slp_typ == 5 && !ps.power_failed() {
                fstart_log::warn!("baytrail: S3 wake without S3 support, booting cold");
            }
            pmc::pm().write32(pmio::PM1_CNT, ps.pm1_cnt & !pmio::SLP_TYP_MASK);
        }
        false
    }

    fn set_resume(&mut self, resume: bool) {
        self.resume = resume;
    }

    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        self.smbus.as_mut().map(|bus| bus as &mut dyn SmBus)
    }

    /// Chip init and the device enables before enumeration (coreboot
    /// `baytrail_init_pre_device` and the south cluster `enable` hooks):
    /// ModPHY, GPIOs, SCC sideband, then each function on or off.
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the driver hooks run in the BSP's single-threaded flow.
        let iosf = unsafe { iosf::ecam() };
        devices::modphy::init(&iosf);
        self.config.gpio.apply();
        devices::scc::init_sideband(&iosf);
        for function in Function::ALL {
            match function {
                Function::Sata if self.config.enabled(function) => {
                    devices::sata::enable(&self.config.sata);
                }
                // The ports are handled together below.
                Function::PciePort(_) => {}
                _ => devices::set_enabled(function, self.config.enabled(function)),
            }
        }
        let mut pcie = self.config.pcie;
        for n in 1..=4u8 {
            pcie.ports[usize::from(n - 1)] &= self.config.enabled(Function::PciePort(n));
        }
        self.pcie = devices::pcie::enable(&pcie);
        devices::pcie::wait_for_links(&self.pcie);
        Ok(())
    }

    /// Device init after PCI allocation.
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the driver hooks run in the BSP's single-threaded flow.
        let iosf = unsafe { iosf::ecam() };
        self.sc_init();
        let config = self.config;
        let mut acpi_devices = AcpiModeDevices::new();
        let mut record = |device| {
            let _ = acpi_devices.push(device);
        };
        devices::scc::init(&config.scc, &iosf, &mut record);
        devices::lpe::init(&config.lpe, &iosf, &mut record);
        devices::lpss::init(&iosf, config.lpss_acpi_mode, &mut record);
        devices::hda::init(&iosf);
        if config.enabled(Function::Sata) {
            devices::sata::init(&config.sata);
        }
        devices::usb::xhci_init(&config.usb, &iosf, self.resume)?;
        devices::usb::ehci_init(&config.usb, &iosf, self.resume)?;
        devices::pcie::init(&config.pcie, &self.pcie);
        self.acpi_devices = acpi_devices;
        Ok(())
    }

    /// Perf/power settings and the locks (coreboot `perf_power` and
    /// `finalize_chipset`, before the payload or the OS wake vector).
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the driver hooks run in the BSP's single-threaded flow.
        let iosf = unsafe { iosf::ecam() };
        fstart_log::debug!("baytrail: perf/power settings");
        for rmw in devices::perf_power::SETTINGS {
            rmw.apply(&iosf);
        }
        lpc::lock_spi(self.config.spi_lockdown.as_ref());
        let regs = pmc::regs();
        regs.gen_pmcon2
            .modify(pmc::GEN_PMCON2::SLPSX_STR_POL_LOCK::SET + pmc::GEN_PMCON2::SMI_LOCK::SET);
        regs.etr.modify(pmc::ETR::CF9LOCK::SET);
        Ok(())
    }
}

impl BaytrailSouth {
    fn smi(&self) -> BaytrailSmi {
        BaytrailSmi::new(self.config.gpio.smi_sci_route(), self.config.pcie_wake)
    }
}

impl crate::southbridge::smi::SmiControl for BaytrailSouth {
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

impl SmBus for BaytrailSouth {
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
