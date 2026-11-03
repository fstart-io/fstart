//! Intel 6-series (Cougar Point) and 7-series (Panther Point) PCH
//! (coreboot `southbridge/intel/bd82x6x`).
//!
//! - [`early`]: bootblock decode, GPIO and SPI setup, the CPU flex-ratio/SMT
//!   soft reset, pre-DRAM USB, IOBP, DMI and thermal programming.
//! - [`devices`]: function enable/hide and PCIe root-port coalescing before
//!   enumeration; PCIe, SATA, USB and HD Audio init after it.
//! - [`lpc`]: the LPC bridge's ramstage power-management init.
//! - [`acpi`]: FADT/MADT facts and the PCH namespace.
//!
//! The finalize locks run from the ramstage like the other fstart ICH
//! drivers do, rather than from an SMI.

mod acpi;
mod devices;
mod early;
mod lpc;
pub mod me;
pub mod regs;

use crate::southbridge::gpio_ich::IchGpio;
use crate::southbridge::pmio_ich::{self as pmio, PmIo};
use crate::southbridge::smbus::I801SmBus;
use crate::southbridge::spibar::{SpiRegs, SwseqConfig};
use fstart_core::ConstVec;
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam::EcamDevice;
use fstart_pci::{PciAddress, PciFixedBar, PciFixedBarType, PciFixedBars};
use me::{Me, MeBiosPath};
use regs::{FD2, GCS, GEN_PMCON_1, GEN_PMCON_LOCK, LpcConfig, Rcba, dev};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub use crate::southbridge::ehci_debug::EhciController;
pub use crate::southbridge::gpio_ich::{GpiRoute, GpioConfig, GpioPin};
pub use crate::southbridge::hda::{HdaConfig, HdaVerbTable};
pub use crate::southbridge::lpc::LpcGenericIoDecode;
pub use crate::southbridge::options::SataMode;

/// Fixed southbridge resources (coreboot `pch.h` and Kconfig defaults).
pub const DEFAULT_PMBASE: u16 = 0x0500;
pub const DEFAULT_GPIOBASE: u16 = 0x0480;
pub const DEFAULT_SMBUS_BASE: u16 = 0x0400;
pub const DEFAULT_RCBA: u64 = 0xfed1_c000;
pub const HPET_BASE: u64 = 0xfed0_0000;
pub const IOAPIC_BASE: u64 = 0xfec0_0000;
/// The I/O APIC's ID: programmed by `lpc_init`, described by MADT and DMAR.
pub const IOAPIC_ID: u8 = 2;
/// coreboot `PCH_IOAPIC_PCI_BUS/SLOT` and `PCH_HPET_PCI_BUS/SLOT`: the
/// requester IDs the DMAR scopes name.
pub const IOAPIC_BUS: u8 = 250;
pub const IOAPIC_DEV: u8 = 31;
pub const HPET_BUS: u8 = 250;
pub const HPET_DEV: u8 = 15;
const PCH_TYPE_CPT: u8 = 0x1c;
const PCH_TYPE_PPT: u8 = 0x1e;
const PCH_STEP_B0: u8 = 2;
const PCH_STEP_B1: u8 = 3;
/// UM77 has four root ports.
const PCI_DID_UM77: u16 = 0x1e58;
/// The mobile LPC device IDs (coreboot `pch_is_mobile`, from `pci_ids.h`).
const MOBILE_LPC_IDS: [u16; 13] = [
    0x1c43, 0x1c47, 0x1c49, 0x1c4b, 0x1c4f, // 6-series QM67/UM67/HM65/HM67/QS67
    0x1e55, 0x1e56, 0x1e57, 0x1e58, 0x1e59, 0x1e5d, 0x1e5e, 0x1e5f, // 7-series
];
/// SATA IOBP Gen3 transmitter registers.
const SATA_IOBP_SP0G3IR: u32 = 0xea00_0151;
const SATA_IOBP_SP1G3IR: u32 = 0xea00_0051;

/// SATA 1 policy (coreboot `sata_*` chip registers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SataConfig {
    pub mode: SataMode,
    /// Implemented ports, bits 0..5.
    pub ports: u8,
    /// Hot-plug capable ports, a subset of `ports`.
    pub hotplug_ports: u8,
    /// Gen3 transmitter settings of ports 0 and 1; zero keeps the default.
    pub port0_gen3_tx: u32,
    pub port1_gen3_tx: u32,
    /// Maximum interface speed: 0 keeps the chip default, 1 = 1.5 Gb/s,
    /// 2 = 3 Gb/s, 3 = 6 Gb/s.
    pub interface_speed: u8,
}

impl SataConfig {
    /// AHCI on `ports` with the chip's default speed and transmitters.
    #[must_use]
    pub const fn ahci(ports: u8) -> Self {
        Self {
            mode: SataMode::Ahci,
            ports,
            hotplug_ports: 0,
            port0_gen3_tx: 0,
            port1_gen3_tx: 0,
            interface_speed: 0,
        }
    }
}

/// One USB 2.0 port (coreboot `struct southbridge_usb_port`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbPort {
    pub enabled: bool,
    /// Index into the transmitter gain table, or a raw `USBIRx[11:0]` value
    /// when beyond it.
    pub current: u16,
    /// Over-current pin; ports 0..7 report on OC0..3, ports 8..13 on OC4..7.
    pub oc_pin: Option<u8>,
}

impl UsbPort {
    pub const DISABLED: Self = Self {
        enabled: false,
        current: 0,
        oc_pin: None,
    };

    #[must_use]
    pub const fn new(current: u16, oc_pin: Option<u8>) -> Self {
        Self {
            enabled: true,
            current,
            oc_pin,
        }
    }
}

/// PCH PCI functions a board can disable (coreboot devicetree `off`),
/// hidden through the function-disable registers (`pch_hide_devfn()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PchFunction {
    /// Panther Point only.
    Xhci,
    Mei1,
    Mei2,
    Ider,
    Kt,
    Gbe,
    Ehci1,
    Ehci2,
    Hda,
    /// PCIe root port 1..8 as its physical port number 0..7.
    PcieRootPort(u8),
    PciBridge,
    Sata1,
    Smbus,
    Sata2,
    Thermal,
}

impl PchFunction {
    /// Bus-0 device and function.
    #[must_use]
    pub const fn devfn(self) -> u8 {
        match self {
            Self::Xhci => dev::XHCI,
            Self::Mei1 => dev::MEI1,
            Self::Mei2 => dev::MEI2,
            Self::Ider => dev::IDER,
            Self::Kt => dev::KT,
            Self::Gbe => dev::GBE,
            Self::Ehci1 => dev::EHCI1,
            Self::Ehci2 => dev::EHCI2,
            Self::Hda => dev::HDA,
            Self::PcieRootPort(port) => fstart_pci::devfn(dev::PCIE, port),
            Self::PciBridge => dev::PCI_BRIDGE,
            Self::Sata1 => dev::SATA1,
            Self::Smbus => dev::SMBUS,
            Self::Sata2 => dev::SATA2,
            Self::Thermal => dev::THERMAL,
        }
    }
}

impl From<EhciController> for PchFunction {
    fn from(controller: EhciController) -> Self {
        match controller {
            EhciController::Ehci1 => Self::Ehci1,
            EhciController::Ehci2 => Self::Ehci2,
        }
    }
}

/// 6/7-series PCH policy (coreboot `southbridge_intel_bd82x6x_config`
/// plus the board's CMOS defaults): chipset defaults plus the board's
/// wiring.
#[derive(Debug, Clone, Copy)]
pub struct IntelBd82x6xConfig {
    pub rcba: u64,
    pub pmbase: u16,
    pub gpiobase: u16,
    pub smbus_base: u16,
    /// GPIO pad configuration (coreboot `mainboard_gpio_map`).
    pub gpio: GpioConfig,
    pub gpi_routing: [GpiRoute; 16],
    /// GPE0_EN (PMBASE+0x28).
    pub gpe0_en: u32,
    pub alt_gp_smi_en: u16,
    /// LPC generic I/O decodes (GEN1..4_DEC).
    pub lpc_generic_io: ConstVec<LpcGenericIoDecode, 4>,
    pub sata: Option<SataConfig>,
    /// Renumber enabled root ports linearly from function 0.
    pub pcie_port_coalesce: bool,
    /// Per-port ASPM override (0 keeps the port's own).
    pub pcie_aspm: [u8; 8],
    /// Root ports with a hot-plug slot, by physical port.
    pub pcie_hotplug: [bool; 8],
    /// The port's device is soldered down (devicetree child on the
    /// mainboard): no slot number or power limit.
    pub pcie_onboard: [bool; 8],
    /// Docking station support (FADT `DCK_CAP`).
    pub docking_supported: bool,
    pub xhci_switchable_ports: u32,
    pub superspeed_capable_ports: u32,
    pub usb_ports: [UsbPort; 14],
    pub spi_uvscc: u32,
    pub spi_lvscc: u32,
    pub spi_ops: SwseqConfig,
    /// Devicetree-disabled functions.
    pub disabled: ConstVec<PchFunction, 24>,
    /// After G3: 0 = stay off, 1 = power on, 2 = previous state.
    pub power_on_after_fail: u8,
    /// Unmask NMI sources.
    pub nmi: bool,
    /// Mobile ACPI PM profile.
    pub laptop: bool,
    pub hda: Option<HdaConfig>,
    /// The EHCI controller wired to the board's debug connector, if not the
    /// default ([`EhciController::select`]).
    pub ehci_debug_controller: Option<EhciController>,
    /// Serial IRQ stays in continuous mode (coreboot
    /// `SERIRQ_CONTINUOUS_MODE`).
    pub serirq_continuous: bool,
    /// Hyper-threading enabled (CMOS `hyper_threading`).
    pub hyper_threading: bool,
    /// GPIO that gates the DIMM reset across S3 (coreboot
    /// `DRAM_RESET_GATE_GPIO`), driven low by SMM on S3 entry.
    pub dram_reset_gate_gpio: Option<u8>,
}

impl IntelBd82x6xConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rcba: DEFAULT_RCBA,
            pmbase: DEFAULT_PMBASE,
            gpiobase: DEFAULT_GPIOBASE,
            smbus_base: DEFAULT_SMBUS_BASE,
            gpio: GpioConfig::new(),
            gpi_routing: [GpiRoute::Disabled; 16],
            gpe0_en: 0,
            alt_gp_smi_en: 0,
            lpc_generic_io: ConstVec::new(LpcGenericIoDecode { base: 0, size: 0 }),
            sata: None,
            pcie_port_coalesce: false,
            pcie_aspm: [0; 8],
            pcie_hotplug: [false; 8],
            pcie_onboard: [false; 8],
            docking_supported: false,
            xhci_switchable_ports: 0,
            superspeed_capable_ports: 0,
            usb_ports: [UsbPort::DISABLED; 14],
            spi_uvscc: 0,
            spi_lvscc: 0,
            spi_ops: SwseqConfig::DEFAULT,
            disabled: ConstVec::new(PchFunction::Xhci),
            power_on_after_fail: 1,
            nmi: false,
            laptop: false,
            hda: None,
            ehci_debug_controller: None,
            serirq_continuous: false,
            hyper_threading: true,
            dram_reset_gate_gpio: None,
        }
    }

    /// Whether the board left `function` enabled.
    #[must_use]
    pub fn enabled(&self, function: PchFunction) -> bool {
        !self.disabled.as_slice().contains(&function)
    }

    /// The EHCI debug console if the platform selects USB debug, with
    /// the northbridge's ECAM window.
    pub fn ehci_debug(
        &self,
        ecam_base: usize,
        ecam_buses: u16,
    ) -> Option<fstart_driver_usbdebug::Config> {
        let function = EhciController::select(self.ehci_debug_controller, |controller| {
            self.enabled(controller.into())
        })?;
        crate::southbridge::ehci_debug::config_with_ecam_size(
            PchFunction::from(function).devfn(),
            ecam_base,
            u64::from(ecam_buses) << 20,
            &[(self.rcba, 0x4000)],
        )
    }

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

impl Default for IntelBd82x6xConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The 6/7-series PCH.
pub struct IntelBd82x6x {
    config: &'static IntelBd82x6xConfig,
    /// The user options over the board config.
    options: crate::southbridge::options::Options,
    smbus: Option<I801SmBus>,
    /// Advertise `_S3_`; set once training and stage caches persisted.
    s3_enabled: bool,
    /// This boot resumes from S3.
    resume: bool,
    /// Root ports left enabled after the presence check, by physical port.
    pcie_enabled: [bool; 8],
    /// Hotplug policy by function number after coalescing.
    pcie_hotplug: [bool; 8],
    /// The function number of each enabled physical root port after
    /// coalescing (RPFN); `None` for a disabled one.
    pcie_function: [Option<u8>; 8],
}

// SAFETY: firmware initializes the PCH on the BSP before concurrency exists.
unsafe impl Send for IntelBd82x6x {}
// SAFETY: only configuration, fixed I/O bases and boot-time state.
unsafe impl Sync for IntelBd82x6x {}

impl IntelBd82x6x {
    /// The GPIO controller, for board code to take the pins it wires.
    #[must_use]
    pub const fn gpio(&self) -> IchGpio {
        IchGpio::new(self.config.gpiobase)
    }

    fn lpc() -> EcamDevice {
        EcamDevice::from_devfn(0, dev::LPC)
    }

    fn lpc_regs(&self) -> &'static LpcConfig {
        // SAFETY: the LPC bridge is the fixed 00:1f.0 and ECAM is enabled.
        unsafe { Self::lpc().regs::<LpcConfig>() }
    }

    fn rcba(&self) -> &'static Rcba {
        // SAFETY: RCBA is enabled at the validated base in pre_console_init.
        unsafe { &*(self.config.rcba as usize as *const Rcba) }
    }

    fn spi(&self) -> &'static SpiRegs {
        &self.rcba().spi
    }

    fn pm(&self) -> PmIo {
        PmIo::new(self.config.pmbase)
    }

    /// SMI routing view of this PCH for SMM installation; SMM gates the DRAM
    /// reset before S3 like coreboot's bd82x6x handler.
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
    pub const fn config_ref(&self) -> &'static IntelBd82x6xConfig {
        self.config
    }

    /// The board's SATA config in the user's mode.
    fn sata(&self) -> Option<SataConfig> {
        self.config.sata.map(|sata| SataConfig {
            mode: self.options.sata_mode.unwrap_or(sata.mode),
            ..sata
        })
    }

    fn device_id(&self) -> u16 {
        self.lpc_regs().device_id.get()
    }

    /// coreboot `pch_silicon_type`.
    fn pch_type(&self) -> u8 {
        (self.device_id() >> 8) as u8
    }

    /// coreboot `pch_silicon_revision`.
    fn pch_revision(&self) -> u8 {
        self.lpc_regs().revision_id.get()
    }

    /// coreboot `pch_silicon_supported`.
    fn silicon_supported(&self, kind: u8, revision: u8) -> bool {
        match (kind, self.pch_type()) {
            (PCH_TYPE_CPT, PCH_TYPE_CPT) => self.pch_revision() >= revision,
            (PCH_TYPE_CPT, PCH_TYPE_PPT) => true,
            (PCH_TYPE_PPT, PCH_TYPE_PPT) => self.pch_revision() >= revision,
            _ => false,
        }
    }

    fn is_mobile(&self) -> bool {
        MOBILE_LPC_IDS.contains(&self.device_id())
    }

    fn root_ports(&self) -> u8 {
        if self.device_id() == PCI_DID_UM77 {
            4
        } else {
            8
        }
    }

    /// The CPU's platform type (coreboot `get_platform_type`: CAPID0 bits
    /// of the host bridge device ID), which the CLKRUN policy follows.
    fn mobile_platform(&self) -> bool {
        EcamDevice::new(0, 0, 0).read16(2) & 0xc == 0x4
    }

    fn wait_iobp(&self) -> bool {
        let done =
            fstart_arch::x86::wait_us(10_000, || !self.rcba().iobps.is_set(regs::IOBPS::BUSY));
        if !done {
            fstart_log::error!("bd82x6x: IOBP timeout");
        }
        done
    }

    /// coreboot `pch_iobp_update` (`common.c`).
    fn iobp_update(&self, address: u32, and: u32, or: u32) {
        let r = self.rcba();
        // CPT B0+ and PPT use one read/write opcode; CPT A-steps read with
        // bit 8 also set.
        let (read, write) = if self.silicon_supported(PCH_TYPE_CPT, PCH_STEP_B0) {
            (0x600, 0x600)
        } else {
            (0x700, 0x600)
        };
        r.iobpiri.set(address);
        r.iobps.set(read);
        if !self.wait_iobp() {
            return;
        }
        let data = r.iobpd.get();
        if !self.wait_iobp() {
            return;
        }
        if r.iobps.read(regs::IOBPS::STATUS) != 0 {
            fstart_log::error!("bd82x6x: IOBP read {:#x} failed", address);
            return;
        }
        r.iobps.set(write);
        if !self.wait_iobp() {
            return;
        }
        r.iobpd.set((data & and) | or);
        self.wait_iobp();
    }

    fn me(&self) -> Me {
        Me::new()
    }

    /// coreboot `intel_me_init` for the ramstage: report the ME and hide
    /// it on S3 resume, where the OS gets no MEI.
    fn me_init(&self) {
        let me = self.me();
        match me.bios_path(self.resume) {
            MeBiosPath::S3Wake => {
                me.hide(|| self.rcba().fd2.modify(FD2::MEI1::SET));
            }
            MeBiosPath::Normal => {
                me.extend_valid();
                if let Some(mei) = me.mei() {
                    if me.device_id() == Some(me::DEVID_ME_8) {
                        let _ = mei.read_mbp();
                    } else {
                        let _ = mei.log_firmware_version();
                    }
                }
            }
            path => fstart_log::info!("bd82x6x: ME BIOS path {}", path as u8),
        }
    }

    /// coreboot `intel_pch_finalize_smm` + `intel_me_finalize_smm`.
    fn lockdown(&self) {
        let r = self.rcba();
        let me = self.me();
        me.finalize();
        if me.device_id().is_some() {
            r.fd2.modify(FD2::MEI1::SET);
        }
        self.spi().lock();
        r.cir0.modify(regs::CIR0::TC_LOCKDOWN::SET);
        r.gcs.modify(GCS::BILD::SET);
        r.fdsw.set(r.fdsw.get() | 1 << 7);
        let lpc = self.lpc_regs();
        lpc.gen_pmcon_1.modify(GEN_PMCON_1::SMI_LOCK::SET);
        lpc.gen_pmcon_lock
            .modify(GEN_PMCON_LOCK::ACPI_BASE_LOCK::SET + GEN_PMCON_LOCK::SLP_STR_POL_LOCK::SET);
        lpc.etr3
            .modify(regs::ETR3::CF9GR::CLEAR + regs::ETR3::CF9LOCK::SET);
        // R/WO registers.
        r.lcap.set(r.lcap.get());
        let hda = EcamDevice::from_devfn(0, dev::HDA);
        if hda.is_present() {
            hda.write32(0x74, hda.read32(0x74));
        }
        self.pm().tco().lock();
        fstart_log::info!("bd82x6x: chipset locked down");
    }
}

impl crate::southbridge::smi::SmiControl for IntelBd82x6x {
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
fn validate(config: &IntelBd82x6xConfig) -> Result<(), ServiceError> {
    let aligned = |base: u64, size: u64| {
        base != 0 && base <= u64::from(u32::MAX) && base.is_multiple_of(size)
    };
    if !aligned(config.rcba, 0x4000)
        || config.pmbase == 0
        || config.pmbase & 0x7f != 0
        || config.gpiobase == 0
        || config.gpiobase & 0x7f != 0
        || config.smbus_base == 0
        || config.smbus_base & 0x1f != 0
        || config.power_on_after_fail > 2
        || config.sata.is_some_and(|sata| {
            sata.ports & !0x3f != 0
                || sata.hotplug_ports & !sata.ports != 0
                || sata.interface_speed > 3
        })
        || (config.sata.is_some() && !config.enabled(PchFunction::Sata1))
        || config
            .usb_ports
            .iter()
            .any(|port| port.oc_pin.is_some_and(|pin| pin > 7))
        || config.pcie_aspm.iter().any(|aspm| *aspm > 3)
        || config.dram_reset_gate_gpio.is_some_and(|gpio| gpio > 75)
        || config
            .ehci_debug_controller
            .is_some_and(|controller| !config.enabled(controller.into()))
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
    Ok(())
}

impl crate::IntelSouthbridgeDriver for IntelBd82x6x {
    const ACPI_LPC_NAME: &'static str = "LPCB";
    type Config = IntelBd82x6xConfig;
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
            pcie_enabled: core::array::from_fn(|port| {
                config.enabled(PchFunction::PcieRootPort(port as u8))
            }),
            pcie_hotplug: config.pcie_hotplug,
            pcie_function: core::array::from_fn(|port| Some(port as u8)),
        })
    }

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    /// coreboot bootblock and `early_pch_init()` up to the GPIOs: SPI
    /// caching, LPC decode, BARs, port 80, SPI speed, upper CMOS and early
    /// USB/RMH routing (which puts EHCI at .0 before the debug console probes
    /// it). Every write is idempotent: the DRAM mainstage repeats this phase.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the bootblock maps ECAM before southbridge init.
        let flash = unsafe { crate::southbridge::lpc::flash_config() };
        flash.enable_prefetching_and_caching();
        // Cache commits precede device finalization. Establish the same write
        // policy coreboot's spi_init() does before the first flash operation.
        flash.prepare_firmware_writes();
        self.early_pch_init();
        self.rcba().gcs.modify(GCS::PCI_POST::CLEAR);
        self.set_spi_speed();
        self.rcba().rc.set(1 << 2);
        self.early_usb_init();
        Ok(())
    }

    fn bootblock_pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_init()?;
        self.cpu_soft_reset();
        Ok(())
    }

    /// coreboot romstage before raminit: GbE strap and SMBus.
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.enable_gbe();
        self.smbus = Some(I801SmBus::enable_on_i801(
            dev::SMBUS,
            self.config.smbus_base,
        ));
        Ok(())
    }

    /// coreboot `early_pch_init()`'s `setup_pch_gpios()`: bootblock only,
    /// before DRAM training; the mainstage must not drive the pads again.
    fn pre_raminit_init(&mut self) -> Result<(), ServiceError> {
        self.gpio().setup(&self.config.gpio);
        Ok(())
    }

    fn detect_s3_resume(&self) -> bool {
        IntelBd82x6x::detect_s3_resume(self)
    }

    fn set_s3_enabled(&mut self, enabled: bool) {
        self.s3_enabled = enabled;
    }

    fn set_resume(&mut self, resume: bool) {
        self.resume = resume;
    }

    fn root_port_function(&self, port: u8) -> Option<u8> {
        *self.pcie_function.get(usize::from(port))?
    }

    fn training_identity(&self) -> Option<[u8; 5]> {
        // SAFETY: the LPC bridge is the fixed 00:1f.0.
        let header = unsafe { Self::lpc().regs::<fstart_pci::PciType0Config>() };
        Some(crate::southbridge::lpc::training_identity(header))
    }

    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        self.smbus.as_mut().map(|bus| bus as &mut dyn SmBus)
    }

    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        bars.push(PciFixedBar {
            address: {
                let (dev, func) = fstart_pci::devfn_parts(dev::SMBUS);
                PciAddress::new(0, 0, dev, func)
            },
            register: 0x20,
            kind: PciFixedBarType::Io,
            base: u64::from(self.config.smbus_base),
            size: 0x20,
            prefetchable: false,
        })
        .expect("PCH fixed PCI resource list capacity");
        bars
    }

    /// coreboot `early_pch_init_native` and the PCH half of
    /// `early_init_dmi` before the system agent trains the link.
    fn prepare_dram_link_init(&mut self) -> Result<(), ServiceError> {
        self.early_pch_init_native();
        self.dmi_pre();
        Ok(())
    }

    /// The PCH virtual channels after the system agent's, then
    /// `early_thermal_init`.
    fn finish_dram_link_init(&mut self) -> Result<(), ServiceError> {
        self.dmi_post()?;
        self.early_thermal_init();
        Ok(())
    }

    /// coreboot romstage after raminit: `early_pch_reset_pmcon`, the default
    /// interrupt map and `southbridge_rcba_config`.
    fn prepare_early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        let pmcon3 = &self.lpc_regs().gen_pmcon_3;
        pmcon3.modify(regs::GEN_PMCON_3::RTC_BATTERY_DEAD::CLEAR);
        self.configure_default_intmap();
        self.rcba().fd.set(1 << 0 | 1 << 26);
        Ok(())
    }

    /// coreboot `pch_enable()` for every function: device enables, root
    /// port coalescing and the SATA map before resource allocation.
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        self.enable_devices();
        Ok(())
    }

    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.lpc_init();
        self.pcie_init();
        self.usb_init();
        if let Some(sata) = self.sata() {
            self.sata_init(&sata);
        }
        if self.config.enabled(PchFunction::Hda) {
            self.hda_init();
        }
        if self.config.enabled(PchFunction::Mei1) {
            self.me_init();
        }
        self.write_interrupt_lines();
        fstart_log::info!("bd82x6x: ramstage init complete");
        Ok(())
    }

    /// coreboot `lpc_final` and the `APM_CNT_FINALIZE` handlers, run from
    /// the ramstage before the payload. Like the Ibex Peak binding, this
    /// development configuration keeps firmware SPI writes open for the
    /// training cache, the flash log and payload writes.
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        self.spi().finalize_ops(&self.config.spi_ops);
        // SAFETY: fixed LPC function; ECAM is up.
        unsafe { crate::southbridge::lpc::flash_config() }.prepare_firmware_writes();
        self.lockdown();
        Ok(())
    }
}

impl SmBus for IntelBd82x6x {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
            .read_byte(addr, cmd)
    }

    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
            .write_byte(addr, cmd, value)
    }

    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
            .read_word(addr, cmd)
    }

    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
            .write_word(addr, cmd, value)
    }

    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
            .block_read(addr, cmd, buf)
    }

    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        self.smbus
            .as_mut()
            .ok_or(ServiceError::HardwareError)?
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
            .ok_or(ServiceError::HardwareError)?
            .i2c_eeprom_read(addr, offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;

    #[test]
    fn rejects_policy_the_pch_cannot_express() {
        static GOOD: IntelBd82x6xConfig = IntelBd82x6xConfig::new();
        assert!(IntelBd82x6x::new_from_config(&GOOD).is_ok());
        static BAD_OC: IntelBd82x6xConfig = IntelBd82x6xConfig {
            usb_ports: [UsbPort::new(0, Some(8)); 14],
            ..IntelBd82x6xConfig::new()
        };
        assert!(IntelBd82x6x::new_from_config(&BAD_OC).is_err());
        static HOTPLUG_ON_DISABLED: IntelBd82x6xConfig = IntelBd82x6xConfig {
            pcie_hotplug: [false, false, true, false, false, false, false, false],
            disabled: ConstVec::new(PchFunction::Xhci).push(PchFunction::PcieRootPort(2)),
            ..IntelBd82x6xConfig::new()
        };
        assert!(IntelBd82x6x::new_from_config(&HOTPLUG_ON_DISABLED).is_err());
        static SATA_DISABLED: IntelBd82x6xConfig = IntelBd82x6xConfig {
            sata: Some(SataConfig::ahci(1)),
            disabled: ConstVec::new(PchFunction::Xhci).push(PchFunction::Sata1),
            ..IntelBd82x6xConfig::new()
        };
        assert!(IntelBd82x6x::new_from_config(&SATA_DISABLED).is_err());
    }

    #[test]
    fn ehci_debug_maps_the_selected_controller() {
        let mut config = IntelBd82x6xConfig::new();
        let debug =
            |config: &IntelBd82x6xConfig| config.ehci_debug(0xf000_0000, 64).map(|d| d.address());
        assert_eq!(debug(&config), Some(PciAddress::new(0, 0, 0x1a, 0)));
        config.ehci_debug_controller = Some(EhciController::Ehci1);
        assert_eq!(debug(&config), Some(PciAddress::new(0, 0, 0x1d, 0)));
        config.disabled = config.disabled.push(PchFunction::Ehci1);
        assert_eq!(debug(&config), None);
    }

    #[test]
    fn smm_receives_the_users_power_on_after_failure_policy() {
        use crate::southbridge::smi::SmiControl;
        static CONFIG: IntelBd82x6xConfig = IntelBd82x6xConfig {
            power_on_after_fail: 0,
            dram_reset_gate_gpio: Some(10),
            ..IntelBd82x6xConfig::new()
        };
        let mut pch = IntelBd82x6x::new_from_config(&CONFIG).unwrap();
        let board = pch.smm_handler_config();
        let mut options = pch.options();
        options.power_on_after_fail = 1;
        pch.set_options(options);
        assert_ne!(pch.smm_handler_config(), board);
    }
}
