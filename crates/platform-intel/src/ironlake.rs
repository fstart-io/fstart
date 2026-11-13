//! Arrandale/Clarkdale (Ironlake uncore) with a 5 Series (Ibex Peak) PCH:
//! the chipset policy the shared fixed Intel flow drives.
//!
//! The memory controller and graphics are in the CPU package, so unlike the
//! legacy GMCH pairings this platform owns its CPU model: model 2065x.

pub use fstart_driver_intel::ibexpeak::{
    EhciDebugConfig, GpiRoute, GpioPin, HdaConfig, HdaVerbTable, IntelIbexPeakConfig,
    LpcGenericIoDecode, PchFunction, SataConfig, SataMode, UsbCurrent, UsbPort,
};
pub use fstart_driver_intel::ironlake::{
    IntelIronlakeConfig, IronlakeChipset, IronlakeIgdConfig, PanelPort,
};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};

pub const IRONLAKE_MCHBAR: u64 = fstart_driver_intel::ironlake::DEFAULT_MCHBAR;
pub const IRONLAKE_DMIBAR: u64 = fstart_driver_intel::ironlake::DEFAULT_DMIBAR;
pub const IRONLAKE_ECAM_BASE: u64 = fstart_driver_intel::ironlake::DEFAULT_ECAM_BASE;
pub const IBEXPEAK_RCBA: u64 = fstart_driver_intel::ibexpeak::DEFAULT_RCBA;
pub const IBEXPEAK_PMBASE: u16 = fstart_driver_intel::ibexpeak::DEFAULT_PMBASE;
pub const IBEXPEAK_GPIOBASE: u16 = fstart_driver_intel::ibexpeak::DEFAULT_GPIOBASE;
pub const IBEXPEAK_EHCI_DEBUG_BAR: u32 = fstart_driver_intel::ibexpeak::DEFAULT_EHCI_DEBUG_BAR;

/// Board policy for an Ironlake/Ibex Peak machine.
#[derive(Debug, Clone, Copy)]
pub struct IronlakeIbexPeakConfig {
    northbridge: IntelIronlakeConfig,
    southbridge: IntelIbexPeakConfig,
    max_cpus: u16,
    pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
}

impl IronlakeIbexPeakConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            northbridge: IntelIronlakeConfig::new(),
            southbridge: IntelIbexPeakConfig::new(),
            max_cpus: 4,
            pci_subsystem_ids: &[],
        }
    }

    /// coreboot `SYSTEM_TYPE_LAPTOP`: the mobile ACPI PM profile.
    #[must_use]
    pub const fn laptop(mut self) -> Self {
        self.southbridge.laptop = true;
        self
    }

    /// Clarkdale/Lynnfield desktop CIR programming and DID override.
    #[must_use]
    pub const fn desktop(mut self) -> Self {
        self.northbridge.chipset = IronlakeChipset::Desktop;
        self.southbridge.desktop = true;
        self
    }

    /// coreboot `mainboard_get_spd_map()`: channel 0 slot 0/1, channel 1
    /// slot 0/1; zero means absent. Only one DIMM per channel is currently
    /// supported, so the second-slot entries (1 and 3) must remain zero.
    #[must_use]
    pub const fn spd_addresses(mut self, addresses: [u8; 4]) -> Self {
        if addresses[1] != 0 || addresses[3] != 0 {
            panic!("Ironlake raminit supports one DIMM per channel");
        }
        self.northbridge.spd_addresses = addresses;
        self
    }

    /// IGD stolen memory (coreboot `gfx_uma_size`).
    #[must_use]
    pub const fn gfx_uma_size(mut self, selector: u8) -> Self {
        if selector > 8 {
            panic!("Ironlake gfx_uma_size selects 0..8");
        }
        self.northbridge.gfx_uma_size = selector;
        self
    }

    #[must_use]
    pub const fn peg(mut self, enabled: bool) -> Self {
        self.northbridge.peg = enabled;
        self
    }

    #[must_use]
    pub const fn igd(mut self, igd: IronlakeIgdConfig) -> Self {
        self.northbridge.igd = igd;
        self.northbridge.igd_enabled = true;
        self
    }

    /// TCC activation offset in degrees below TjMax (coreboot
    /// `cpu_intel_model_2065x_config.tcc_offset`); 0 keeps the default.
    #[must_use]
    pub const fn tcc_offset(mut self, degrees: u8) -> Self {
        if degrees > 15 {
            panic!("the TCC offset is a 4-bit value");
        }
        self.northbridge.tcc_offset = degrees;
        self
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        if max_cpus == 0 {
            panic!("max_cpus must be non-zero");
        }
        self.max_cpus = max_cpus;
        self
    }

    #[must_use]
    pub const fn pci_subsystem_ids(mut self, ids: &'static [crate::facts::PciSubsystemId]) -> Self {
        self.pci_subsystem_ids = ids;
        self
    }

    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut index = 0;
        while index < N {
            self.southbridge.gpio.pins = self.southbridge.gpio.pins.push(pins[index]);
            index += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpi_routing(mut self, routing: [GpiRoute; 16]) -> Self {
        self.southbridge.gpi_routing = routing;
        self
    }

    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.southbridge.gpe0_en = value;
        self
    }

    #[must_use]
    pub const fn alt_gp_smi_en(mut self, value: u16) -> Self {
        self.southbridge.alt_gp_smi_en = value;
        self
    }

    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(
        mut self,
        windows: [LpcGenericIoDecode; N],
    ) -> Self {
        if N > 4 {
            panic!("the PCH has four generic LPC decodes");
        }
        let mut index = 0;
        while index < N {
            if windows[index].encode().is_none() {
                panic!("LPC generic decode must be a naturally aligned power of two");
            }
            let mut other = 0;
            while other < index {
                if windows[index].overlaps(windows[other]) {
                    panic!("LPC generic decodes overlap");
                }
                other += 1;
            }
            self.southbridge.lpc_generic_io = self.southbridge.lpc_generic_io.push(windows[index]);
            index += 1;
        }
        self
    }

    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        if sata.ports & !0x3f != 0 || sata.interface_speed > 3 {
            panic!("SATA has ports 0..5 and speeds 0..3");
        }
        self.southbridge.sata = Some(sata);
        self
    }

    /// coreboot `mainboard_usb_ports`.
    #[must_use]
    pub const fn usb_ports(mut self, ports: [UsbPort; 14]) -> Self {
        let mut index = 0;
        while index < 14 {
            if let Some(pin) = ports[index].oc_pin {
                if pin > 7 {
                    panic!("USB over-current pins are 0..7");
                }
            }
            index += 1;
        }
        self.southbridge.usb_ports = ports;
        self
    }

    /// One codec verb table (coreboot `cim_verb_data`), matched by vendor ID.
    #[must_use]
    pub const fn hda_verb<const P: usize, const E: usize>(
        mut self,
        vendor_id: u32,
        subsystem_id: u32,
        pins: [hda::PinConfig; P],
        extra_verbs: [u32; E],
    ) -> Self {
        let mut table = HdaVerbTable::new(vendor_id, subsystem_id);
        let mut index = 0;
        while index < P {
            table = table.pin(pins[index]);
            index += 1;
        }
        index = 0;
        while index < E {
            table = table.extra_verb(extra_verbs[index]);
            index += 1;
        }
        let hda = match self.southbridge.hda {
            Some(hda) => hda,
            None => HdaConfig::new(),
        };
        self.southbridge.hda = Some(hda.verb(table));
        self
    }

    #[must_use]
    pub const fn pcie_hotplug(mut self, port: usize) -> Self {
        if port >= 8 {
            panic!("the PCH has root ports 0..7");
        }
        self.southbridge.pcie_hotplug[port] = true;
        self
    }

    /// Devicetree `off` functions.
    #[must_use]
    pub const fn disable<const N: usize>(mut self, functions: [PchFunction; N]) -> Self {
        let mut index = 0;
        while index < N {
            if let PchFunction::PcieRootPort(port) = functions[index] {
                if port > 7 {
                    panic!("the PCH has root ports 0..7");
                }
            }
            self.southbridge.disabled = self.southbridge.disabled.push(functions[index]);
            index += 1;
        }
        self
    }

    #[must_use]
    pub const fn docking_supported(mut self) -> Self {
        self.southbridge.docking_supported = true;
        self
    }

    /// After G3: 0 = stay off, 1 = power on, 2 = previous state.
    #[must_use]
    pub const fn power_on_after_fail(mut self, state: u8) -> Self {
        if state > 2 {
            panic!("power_on_after_fail is off/on/keep");
        }
        self.southbridge.power_on_after_fail = state;
        self
    }

    #[must_use]
    pub const fn nmi(mut self, enabled: bool) -> Self {
        self.southbridge.nmi = enabled;
        self
    }

    /// coreboot `DRAM_RESET_GATE_GPIO`.
    #[must_use]
    pub const fn dram_reset_gate_gpio(mut self, gpio: u8) -> Self {
        if gpio > 75 {
            panic!("the PCH has GPIOs 0..75");
        }
        self.southbridge.dram_reset_gate_gpio = Some(gpio);
        self
    }

    #[must_use]
    pub const fn ehci_debug_bar(mut self, bar: EhciDebugConfig) -> Self {
        self.southbridge.ehci_debug_bar = Some(bar);
        self
    }

    #[must_use]
    pub const fn build(self) -> IronlakeIbexPeakPlatform {
        crate::require_spd_addresses(self.northbridge.spd_addresses);
        let mut southbridge = self.southbridge;
        southbridge.rcba = IBEXPEAK_RCBA;
        southbridge.dmibar = self.northbridge.dmibar;
        let mut northbridge = self.northbridge;
        northbridge.rcba = southbridge.rcba;
        northbridge.pmbase = southbridge.pmbase;
        northbridge.gpiobase = southbridge.gpiobase;
        northbridge.chipset = if southbridge.desktop {
            IronlakeChipset::Desktop
        } else {
            IronlakeChipset::Mobile
        };
        IronlakeIbexPeakPlatform {
            northbridge,
            southbridge,
            max_cpus: self.max_cpus,
            pci_subsystem_ids: self.pci_subsystem_ids,
        }
    }
}

impl Default for IronlakeIbexPeakConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Built configuration, a board static.
#[derive(Debug, Clone, Copy)]
pub struct IronlakeIbexPeakPlatform {
    pub northbridge: IntelIronlakeConfig,
    pub southbridge: IntelIbexPeakConfig,
    pub max_cpus: u16,
    pub pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
}

/// The Arrandale/Clarkdale + 5 Series pairing.
pub struct IronlakeIbexPeak;

#[cfg(feature = "namespace")]
impl fstart_acpi::PcNamespace for IronlakeIbexPeak {
    const LPC: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::lpc::<
        fstart_driver_intel::ironlake::IntelIronlake,
        fstart_driver_intel::ibexpeak::IntelIbexPeak,
    >();
    const IGD: fstart_acpi::AcpiPath =
        fstart_driver_intel::namespace::igd::<fstart_driver_intel::ironlake::IntelIronlake>();
}

impl crate::IntelPlatform for IronlakeIbexPeak {
    type Config = IronlakeIbexPeakPlatform;
    const NAME: &'static str = "ironlake/ibexpeak";
    /// coreboot `DCACHE_RAM_BASE`/`DCACHE_RAM_SIZE` for Ironlake.
    const CAR_BASE: u64 = 0xfefc_0000;
    const CAR_SIZE: u64 = 0x10000;
    const BOOTBLOCK_STACK: u64 = 0x4000;
    const MICROCODE_SIGNATURES: &'static [&'static str] = &["06-25-02", "06-25-05"];
}

impl crate::IntelPlatformConfig for IronlakeIbexPeakPlatform {
    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }
    fn pci_subsystem_ids(&self) -> &'static [crate::facts::PciSubsystemId] {
        self.pci_subsystem_ids
    }
    fn ehci_debug(&self) -> Option<fstart_driver_usbdebug::Config> {
        self.southbridge.ehci_debug_bar
    }
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    use fstart_driver_intel::ibexpeak::IntelIbexPeak;
    use fstart_driver_intel::ironlake::IntelIronlake;

    impl IntelEarlyPlatform for IronlakeIbexPeak {
        #[cfg(feature = "memory-cache")]
        fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
            static WORKSPACE: crate::memory_cache::TrainingWorkspace<2048> =
                crate::memory_cache::TrainingWorkspace::new();
            WORKSPACE.take()
        }
        /// The OS display driver restores the screen; resume skips the
        /// modeset like coreboot (`gma_func0_init()` on S3).
        #[cfg(feature = "acpi")]
        const RESUME_DISPLAY_INIT: bool = false;
        type Northbridge = IntelIronlake;
        type Southbridge = IntelIbexPeak;
        #[cfg(feature = "mp")]
        type Cpu = fstart_arch::x86::cpu::intel::model_2065x::Model2065xCpuDriver;
        #[cfg(feature = "mp")]
        fn cpu_driver(
            microcode: Option<&'static [u8]>,
            northbridge: &Self::Northbridge,
            _southbridge: &Self::Southbridge,
        ) -> Self::Cpu {
            use fstart_driver_intel::IntelNorthbridgeDriver;
            Self::Cpu::new(
                microcode,
                fstart_arch::x86::cpu::intel::model_2065x::Model2065xPolicy {
                    tcc_offset: northbridge.config().tcc_offset,
                },
            )
        }
    }

    impl IntelChipsetConfig for IronlakeIbexPeakPlatform {
        type Northbridge = IntelIronlake;
        type Southbridge = IntelIbexPeak;
        fn northbridge(&'static self) -> &'static IntelIronlakeConfig {
            &self.northbridge
        }
        fn southbridge(&'static self) -> &'static IntelIbexPeakConfig {
            &self.southbridge
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_shares_pch_resources_with_the_uncore() {
        const CONFIG: IronlakeIbexPeakPlatform = IronlakeIbexPeakConfig::new()
            .laptop()
            .spd_addresses([0x50, 0, 0x51, 0])
            .lpc_generic_io([
                LpcGenericIoDecode {
                    base: 0x1600,
                    size: 0x80,
                },
                LpcGenericIoDecode {
                    base: 0x15e0,
                    size: 0x10,
                },
            ])
            .build();
        assert_eq!(CONFIG.northbridge.rcba, CONFIG.southbridge.rcba);
        assert_eq!(CONFIG.southbridge.dmibar, IRONLAKE_DMIBAR);
        assert_eq!(CONFIG.northbridge.spd_addresses, [0x50, 0, 0x51, 0]);
        assert!(CONFIG.southbridge.laptop);
    }

    #[test]
    #[should_panic(expected = "one DIMM per channel")]
    fn rejects_channel_zero_second_slot() {
        let _ = IronlakeIbexPeakConfig::new().spd_addresses([0x50, 0x52, 0x51, 0]);
    }

    #[test]
    #[should_panic(expected = "one DIMM per channel")]
    fn rejects_channel_one_second_slot() {
        let _ = IronlakeIbexPeakConfig::new().spd_addresses([0x50, 0, 0x51, 0x53]);
    }

    #[test]
    #[should_panic(expected = "LPC generic decodes overlap")]
    fn rejects_overlapping_decodes() {
        let _ = IronlakeIbexPeakConfig::new().lpc_generic_io([
            LpcGenericIoDecode {
                base: 0x200,
                size: 0x20,
            },
            LpcGenericIoDecode {
                base: 0x210,
                size: 0x10,
            },
        ]);
    }
}
