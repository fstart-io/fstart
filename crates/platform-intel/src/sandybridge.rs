//! Sandy Bridge / Ivy Bridge with a 6/7 Series (Cougar Point / Panther
//! Point) PCH: the chipset policy the shared fixed Intel flow drives
//! (coreboot `northbridge/intel/sandybridge`, `southbridge/intel/bd82x6x`,
//! `cpu/intel/model_206ax`).
//!
//! The memory controller and graphics are in the CPU package, so like the
//! Ironlake pairing this platform owns its CPU model: model 206AX/306AX.

pub use fstart_driver_intel::bd82x6x::{
    EhciController, GpiRoute, GpioPin, HdaConfig, HdaVerbTable, IntelBd82x6xConfig,
    LpcGenericIoDecode, PchFunction, SataConfig, SataMode, UsbPort,
};
pub use fstart_driver_intel::sandybridge::{
    CpuCstate, IntelSandybridgeConfig, Model206axConfig, PanelPort, PsiState, SandybridgeDevices,
    SandybridgeIgdConfig, Vr12Phases,
};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};

pub const SANDYBRIDGE_ECAM_BASE: u64 = fstart_driver_intel::sandybridge::DEFAULT_ECAM_BASE;
pub const SANDYBRIDGE_ECAM_BUSES: u16 = fstart_driver_intel::sandybridge::DEFAULT_ECAM_BUSES;
pub const BD82X6X_RCBA: u64 = fstart_driver_intel::bd82x6x::DEFAULT_RCBA;
pub const BD82X6X_PMBASE: u16 = fstart_driver_intel::bd82x6x::DEFAULT_PMBASE;
pub const BD82X6X_GPIOBASE: u16 = fstart_driver_intel::bd82x6x::DEFAULT_GPIOBASE;

/// Board policy for a Sandy Bridge / Ivy Bridge machine.
#[derive(Debug, Clone, Copy)]
pub struct SandybridgeBd82x6xConfig {
    northbridge: IntelSandybridgeConfig,
    southbridge: IntelBd82x6xConfig,
    max_cpus: u16,
    /// What the board attaches to PCIe root port `n` (00:1c.n).
    pcie_attached: [Option<&'static fstart_core::smbios::Attached<'static>>; 8],
    pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
}

impl SandybridgeBd82x6xConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            northbridge: IntelSandybridgeConfig::new(),
            southbridge: IntelBd82x6xConfig::new(),
            max_cpus: 8,
            pcie_attached: [None; 8],
            pci_subsystem_ids: &[],
        }
    }

    /// coreboot `SYSTEM_TYPE_LAPTOP`: the mobile ACPI PM profile.
    #[must_use]
    pub const fn laptop(mut self) -> Self {
        self.southbridge.laptop = true;
        self
    }

    /// coreboot `spd_addresses`: channel 0 slot 0/1, channel 1 slot 0/1;
    /// zero means absent.
    #[must_use]
    pub const fn spd_addresses(mut self, addresses: [u8; 4]) -> Self {
        self.northbridge.spd_addresses = addresses;
        self
    }

    /// DDR3 clock cap (devicetree `max_mem_clock_mhz`); zero uses the fuses.
    #[must_use]
    pub const fn max_mem_clock_mhz(mut self, mhz: u16) -> Self {
        self.northbridge.max_mem_clock_mhz = mhz;
        self
    }

    /// IGD stolen memory, 32 MiB steps (coreboot `gfx_uma_size`).
    #[must_use]
    pub const fn gfx_uma_mib(mut self, mib: u32) -> Self {
        if mib == 0 || !mib.is_multiple_of(32) || mib > 512 {
            panic!("Sandy Bridge IGD UMA is 32..512 MiB in 32 MiB steps");
        }
        self.northbridge.gfx_uma_index = (mib / 32 - 1) as u8;
        self
    }

    /// The PEG ports, IGD and devices 4 and 7 the devicetree enables.
    #[must_use]
    pub const fn northbridge_devices(mut self, devices: SandybridgeDevices) -> Self {
        self.northbridge.devices = devices;
        self
    }

    #[must_use]
    pub const fn igd(mut self, igd: SandybridgeIgdConfig) -> Self {
        self.northbridge.igd = igd;
        self
    }

    /// coreboot `cpu_intel_model_206ax_config`, with the CMOS
    /// `hyper_threading` choice the PCH latches through its soft reset.
    #[must_use]
    pub const fn cpu(mut self, cpu: Model206axConfig) -> Self {
        self.northbridge.cpu = cpu;
        self.southbridge.hyper_threading = cpu.hyper_threading;
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

    /// The slot or onboard device the board attaches to PCIe root port
    /// `index` (00:1c.`index`).
    #[must_use]
    pub const fn pcie_attached(
        mut self,
        index: usize,
        attached: &'static fstart_core::smbios::Attached<'static>,
    ) -> Self {
        if index >= self.pcie_attached.len() {
            panic!("PCH PCIe port index out of range");
        }
        self.pcie_attached[index] = Some(attached);
        // A soldered-down device has no slot number or power limit.
        self.southbridge.pcie_onboard[index] =
            matches!(attached, fstart_core::smbios::Attached::Onboard(_));
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
        if sata.ports & !0x3f != 0
            || sata.hotplug_ports & !sata.ports != 0
            || sata.interface_speed > 3
        {
            panic!("SATA has ports 0..5, hot-plug on enabled ports and speeds 0..3");
        }
        self.southbridge.sata = Some(sata);
        self
    }

    /// coreboot `usb_port_config`.
    #[must_use]
    pub const fn usb_ports(mut self, ports: [UsbPort; 14]) -> Self {
        let mut index = 0;
        while index < 14 {
            if let Some(pin) = ports[index].oc_pin
                && pin > 7
            {
                panic!("USB over-current pins are 0..7");
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

    /// coreboot `pcie_hotplug_map`.
    #[must_use]
    pub const fn pcie_hotplug(mut self, port: usize) -> Self {
        if port >= 8 {
            panic!("the PCH has root ports 0..7");
        }
        self.southbridge.pcie_hotplug[port] = true;
        self
    }

    /// coreboot `pcie_port_coalesce`: zero-based linear root port functions.
    #[must_use]
    pub const fn pcie_port_coalesce(mut self) -> Self {
        self.southbridge.pcie_port_coalesce = true;
        self
    }

    /// Devicetree `off` functions.
    #[must_use]
    pub const fn disable<const N: usize>(mut self, functions: [PchFunction; N]) -> Self {
        let mut index = 0;
        while index < N {
            if let PchFunction::PcieRootPort(port) = functions[index]
                && port > 7
            {
                panic!("the PCH has root ports 0..7");
            }
            self.southbridge.disabled = self.southbridge.disabled.push(functions[index]);
            index += 1;
        }
        self
    }

    /// coreboot `spi_uvscc`/`spi_lvscc`.
    #[must_use]
    pub const fn spi_vscc(mut self, upper: u32, lower: u32) -> Self {
        self.southbridge.spi_uvscc = upper;
        self.southbridge.spi_lvscc = lower;
        self
    }

    #[must_use]
    pub const fn docking_supported(mut self) -> Self {
        self.southbridge.docking_supported = true;
        self
    }

    /// coreboot `SERIRQ_CONTINUOUS_MODE`.
    #[must_use]
    pub const fn serirq_continuous(mut self) -> Self {
        self.southbridge.serirq_continuous = true;
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

    /// The EHCI controller wired to the board's debug connector (coreboot
    /// `USBDEBUG_HCD_INDEX`). Without one, EHCI2 is preferred.
    #[must_use]
    pub const fn ehci_debug_controller(mut self, controller: EhciController) -> Self {
        self.southbridge.ehci_debug_controller = Some(controller);
        self
    }

    #[must_use]
    pub const fn build(self) -> SandybridgeBd82x6xPlatform {
        let mut northbridge = self.northbridge;
        northbridge.rcba = self.southbridge.rcba;
        northbridge.pmbase = self.southbridge.pmbase;
        northbridge.hda_enabled = !const_contains(&self.southbridge, PchFunction::Hda);
        SandybridgeBd82x6xPlatform {
            northbridge,
            southbridge: self.southbridge,
            max_cpus: self.max_cpus,
            pcie_attached: self.pcie_attached,
            pci_subsystem_ids: self.pci_subsystem_ids,
        }
    }
}

/// Whether the board disabled `function`, in const context.
const fn const_contains(config: &IntelBd82x6xConfig, function: PchFunction) -> bool {
    let mut index = 0;
    while index < config.disabled.len() {
        let disabled = config.disabled.get(index);
        if disabled.devfn() == function.devfn() {
            return true;
        }
        index += 1;
    }
    false
}

impl Default for SandybridgeBd82x6xConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Built configuration, a board static.
#[derive(Debug, Clone, Copy)]
pub struct SandybridgeBd82x6xPlatform {
    pub northbridge: IntelSandybridgeConfig,
    pub southbridge: IntelBd82x6xConfig,
    pub max_cpus: u16,
    /// What the board attaches to PCIe root port `n` (00:1c.n).
    pub pcie_attached: [Option<&'static fstart_core::smbios::Attached<'static>>; 8],
    pub pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
}

/// The Sandy Bridge / Ivy Bridge + 6/7 Series pairing.
pub struct SandybridgeBd82x6x;

impl fstart_acpi::PcNamespace for SandybridgeBd82x6x {
    const LPC: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::lpc::<
        fstart_driver_intel::sandybridge::IntelSandybridge,
        fstart_driver_intel::bd82x6x::IntelBd82x6x,
    >();
    const IGD: fstart_acpi::AcpiPath =
        fstart_driver_intel::namespace::igd::<fstart_driver_intel::sandybridge::IntelSandybridge>();
}

impl crate::IntelPlatform for SandybridgeBd82x6x {
    type Config = SandybridgeBd82x6xPlatform;
    const NAME: &'static str = "sandybridge/bd82x6x";
    /// coreboot `DCACHE_RAM_BASE` / `DCACHE_RAM_SIZE`.
    const CAR_BASE: u64 = 0xfefe_0000;
    const CAR_SIZE: u64 = 0x20000;
    /// Native raminit keeps its SPD set and training state on the stack.
    const BOOTBLOCK_STACK: u64 = 0x4000;
    /// Sandy Bridge D2/J1/Q0 and Ivy Bridge E1/L1.
    const MICROCODE_SIGNATURES: &'static [&'static str] = &["06-2a-07", "06-3a-09"];
}

impl crate::IntelPlatformConfig for SandybridgeBd82x6xPlatform {
    fn describe_options(&self, each: &mut dyn FnMut(fstart_options::Entry)) {
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        fstart_options::describe(
            fstart_driver_intel::sandybridge::IntelSandybridge::KNOBS,
            &fstart_driver_intel::sandybridge::IntelSandybridge::board_options(&self.northbridge),
            each,
        );
        fstart_options::describe(
            fstart_driver_intel::bd82x6x::IntelBd82x6x::KNOBS,
            &fstart_driver_intel::bd82x6x::IntelBd82x6x::board_options(&self.southbridge),
            each,
        );
    }
    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }
    fn pcie_attached(&self) -> &[Option<&'static fstart_core::smbios::Attached<'static>>] {
        &self.pcie_attached
    }
    fn pci_subsystem_ids(&self) -> &'static [crate::facts::PciSubsystemId] {
        self.pci_subsystem_ids
    }
    fn ehci_debug(&self) -> Option<fstart_driver_usbdebug::Config> {
        self.southbridge.ehci_debug(
            self.northbridge.ecam_base as usize,
            self.northbridge.ecam_buses,
        )
    }
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    use fstart_driver_intel::bd82x6x::IntelBd82x6x;
    use fstart_driver_intel::sandybridge::IntelSandybridge;

    impl IntelEarlyPlatform for SandybridgeBd82x6x {
        fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
            static WORKSPACE: crate::memory_cache::TrainingWorkspace<
                { fstart_driver_intel::sandybridge::raminit::TRAINING_RECORD_SIZE },
            > = crate::memory_cache::TrainingWorkspace::new();
            WORKSPACE.take()
        }
        /// The OS graphics driver restores the panel on resume (coreboot
        /// skips the modeset on the S3 path).
        const RESUME_DISPLAY_INIT: bool = false;
        type Northbridge = IntelSandybridge;
        type Southbridge = IntelBd82x6x;
        type Cpu = fstart_arch::x86::cpu::intel::model_206ax::Model206axCpuDriver;
        fn cpu_driver(
            microcode: Option<&'static [u8]>,
            northbridge: &Self::Northbridge,
            _southbridge: &Self::Southbridge,
        ) -> Self::Cpu {
            use fstart_driver_intel::IntelNorthbridgeDriver;
            Self::Cpu::new(northbridge.config().cpu, microcode)
        }
    }

    impl IntelChipsetConfig for SandybridgeBd82x6xPlatform {
        type Northbridge = IntelSandybridge;
        type Southbridge = IntelBd82x6x;
        fn northbridge(&'static self) -> &'static IntelSandybridgeConfig {
            &self.northbridge
        }
        fn southbridge(&'static self) -> &'static IntelBd82x6xConfig {
            &self.southbridge
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_shares_pch_resources_with_the_system_agent() {
        const CONFIG: SandybridgeBd82x6xPlatform = SandybridgeBd82x6xConfig::new()
            .spd_addresses([0x50, 0, 0x51, 0])
            .gfx_uma_mib(32)
            .laptop()
            .disable([PchFunction::Hda])
            .build();
        assert_eq!(CONFIG.northbridge.ecam_base, SANDYBRIDGE_ECAM_BASE);
        assert_eq!(CONFIG.northbridge.rcba, CONFIG.southbridge.rcba);
        assert_eq!(CONFIG.northbridge.pmbase, CONFIG.southbridge.pmbase);
        assert_eq!(CONFIG.northbridge.gfx_uma_index, 0);
        const { assert!(!CONFIG.northbridge.hda_enabled) };
        const { assert!(CONFIG.southbridge.laptop) };
    }

    #[test]
    fn ehci_debug_fits_below_the_64_bus_ecam() {
        use crate::IntelPlatformConfig;
        const CONFIG: SandybridgeBd82x6xPlatform = SandybridgeBd82x6xConfig::new()
            .ehci_debug_controller(EhciController::Ehci2)
            .build();
        let debug = CONFIG.ehci_debug().unwrap();
        assert_eq!((debug.device, debug.function), (0x1a, 0));
    }

    #[test]
    #[should_panic(expected = "32 MiB steps")]
    fn uma_size_is_checked() {
        let _ = SandybridgeBd82x6xConfig::new().gfx_uma_mib(48);
    }

    #[test]
    #[should_panic(expected = "LPC generic decodes overlap")]
    fn rejects_overlapping_decodes() {
        let _ = SandybridgeBd82x6xConfig::new().lpc_generic_io([
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
