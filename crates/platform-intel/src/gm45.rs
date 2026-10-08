//! GM45/ICH9 chipset policy for the shared fixed Intel flow.

use fstart_core::ConstVec;
pub use fstart_driver_intel::gm45::{Gm45GraphicsPolicy, IntelGm45Config};
use fstart_driver_intel::ich9;
pub use fstart_driver_intel::ich9::{
    HdaConfig, HdaVerbTable, IntelIch9Config, IoTrapAccess, IoTrapConfig, LpcDecodeConfig,
    LpcFixedIoDecode, LpcFloppyDecode, LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode,
    PciePowerLimit, PinColor, PinConfig, PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc,
    SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
pub use fstart_driver_intel::southbridge::pirq::PirqRouting;

pub const GM45_MCHBAR: u64 = 0xFED1_4000;
pub const GM45_DMIBAR: u64 = 0xFED1_8000;
pub const GM45_EPBAR: u64 = 0xFED1_9000;
pub const GM45_ECAM_BASE: u64 = 0xE000_0000;
pub const GM45_ECAM_BUSES: u16 = 64;
pub const ICH9_RCBA: u64 = 0xFED1_C000;
/// ICH9 SPIBAR is at RCBA + 0x3800, not a separately programmed BAR.
pub const ICH9_SPIBAR: u64 = ICH9_RCBA + 0x3800;
pub const ICH9_PMBASE: u16 = ich9::ich9::DEFAULT_PMBASE;
pub const ICH9_SMBUS_BASE: u16 = ich9::ich9::DEFAULT_SMBUS_BASE;

/// GM45/ICH9 chipset policy; board wiring and attached-device settings are
/// supplied by the board rather than inferred from the chipset pair.
#[derive(Debug, Clone, Copy)]
pub struct Gm45Ich9Config {
    pub graphics: Gm45GraphicsPolicy,
    /// Channel 0 slot 0/1, then channel 1 slot 0/1; zero means absent.
    pub spd_addresses: [u8; 4],
    pub pcie_ports: [bool; 6],
    pub pcie_slots: [bool; 6],
    pub pcie_power_limits: [PciePowerLimit; 6],
    pub pcie_hotplug: [bool; 6],
    /// Board-verified RCBA interrupt routing; required by the ICH9 driver.
    pub pirq: Option<PirqRouting>,
    pub pirq_routing: [u8; 8],
    pub lpc_decode: LpcDecodeConfig,
    pub gpe0_en: u32,
    pub gpi_routing: [u8; 16],
    pub alt_gp_smi_en: u16,
    pub c4_on_c3: bool,
    pub c5_enable: bool,
    pub c6_enable: bool,
    /// ICH9 throttle duty encoding (coreboot THTL_75_0 is 2).
    pub throttle_duty: u8,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub ehci_debug_bar: Option<fstart_pci::PciFixedBar>,
    pub hda: Option<HdaConfig>,
    pub io_traps: ConstVec<IoTrapConfig, 4>,
    pub gpio: gpio::GpioConfig,
    /// Maximum logical CPU count (BSP + APs) populated by the board.
    pub max_cpus: u16,
}

impl Gm45Ich9Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            graphics: Gm45GraphicsPolicy::Integrated,
            spd_addresses: [0; 4],
            pcie_ports: [false; 6],
            pcie_slots: [false; 6],
            pcie_power_limits: [PciePowerLimit { value: 0, scale: 0 }; 6],
            pcie_hotplug: [false; 6],
            pirq: None,
            pirq_routing: [0x80; 8],
            lpc_decode: LpcDecodeConfig::new(),
            gpe0_en: 0,
            gpi_routing: [0; 16],
            alt_gp_smi_en: 0,
            c4_on_c3: true,
            c5_enable: false,
            c6_enable: false,
            throttle_duty: 0,
            sata: None,
            usb: None,
            ehci_debug_bar: None,
            hda: None,
            io_traps: ConstVec::new(empty_io_trap()),
            gpio: gpio::GpioConfig::new(),
            max_cpus: 1,
        }
    }

    #[must_use]
    pub const fn northbridge_config(self) -> IntelGm45Config {
        let mut config = IntelGm45Config::new();
        config.mchbar = GM45_MCHBAR;
        config.dmibar = GM45_DMIBAR;
        config.epbar = GM45_EPBAR;
        config.ecam_base = GM45_ECAM_BASE;
        config.ecam_buses = GM45_ECAM_BUSES;
        config.pmbase = ICH9_PMBASE;
        config.smbus_base = ICH9_SMBUS_BASE;
        config.graphics = self.graphics;
        config.spd_addresses = self.spd_addresses;
        config
    }

    #[must_use]
    pub const fn southbridge_config(self) -> IntelIch9Config {
        let mut config = IntelIch9Config::new();
        config.rcba = ICH9_RCBA;
        config.dmibar = GM45_DMIBAR;
        config.smbus_base = ICH9_SMBUS_BASE;
        config.pirq = self.pirq;
        config.pirq_routing = self.pirq_routing;
        config.pcie_ports = self.pcie_ports;
        config.pcie_slots = self.pcie_slots;
        config.pcie_power_limits = self.pcie_power_limits;
        config.pcie_hotplug = self.pcie_hotplug;
        config.lpc_decode = self.lpc_decode;
        config.gpe0_en = self.gpe0_en;
        config.gpi_routing = self.gpi_routing;
        config.alt_gp_smi_en = self.alt_gp_smi_en;
        config.c4_on_c3 = self.c4_on_c3;
        config.c5_enable = self.c5_enable;
        config.c6_enable = self.c6_enable;
        config.throttle_duty = self.throttle_duty;
        config.sata = self.sata;
        config.usb = self.usb;
        config.ehci_debug_bar = self.ehci_debug_bar;
        config.hda = self.hda;
        config.io_traps = self.io_traps;
        config.gpio = self.gpio;
        config
    }

    #[must_use]
    pub const fn ehci_debug_bar(mut self, bar: fstart_pci::PciFixedBar) -> Self {
        self.ehci_debug_bar = Some(bar);
        self
    }

    #[must_use]
    pub const fn graphics(mut self, graphics: Gm45GraphicsPolicy) -> Self {
        self.graphics = graphics;
        self
    }

    #[must_use]
    pub const fn spd_addresses(mut self, addresses: [u8; 4]) -> Self {
        self.spd_addresses = addresses;
        self
    }

    #[must_use]
    pub const fn pirq(mut self, routing: PirqRouting) -> Self {
        self.pirq = Some(routing);
        self
    }

    #[must_use]
    pub const fn pirq_routing(mut self, routing: [u8; 8]) -> Self {
        self.pirq_routing = routing;
        self
    }

    #[must_use]
    pub const fn pcie_port(mut self, index: usize, enabled: bool) -> Self {
        if index >= self.pcie_ports.len() {
            panic!("ICH9 PCIe port index out of range");
        }
        self.pcie_ports[index] = enabled;
        self
    }

    #[must_use]
    pub const fn pcie_slot(
        mut self,
        index: usize,
        power_limit: PciePowerLimit,
        hotplug: bool,
    ) -> Self {
        if index >= self.pcie_slots.len() {
            panic!("ICH9 PCIe slot index out of range");
        }
        self.pcie_slots[index] = true;
        self.pcie_power_limits[index] = power_limit;
        self.pcie_hotplug[index] = hotplug;
        self
    }

    #[must_use]
    pub const fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.lpc_decode.fixed_io = fixed_io;
        self
    }

    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(mut self, ranges: [LpcGenericIoDecode; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.lpc_decode.generic_io = self.lpc_decode.generic_io.push(ranges[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.gpe0_en = value;
        self
    }

    #[must_use]
    pub const fn gpi_routing(mut self, routing: [u8; 16]) -> Self {
        self.gpi_routing = routing;
        self
    }

    #[must_use]
    pub const fn alt_gp_smi_en(mut self, value: u16) -> Self {
        self.alt_gp_smi_en = value;
        self
    }

    /// Board power-state policy; C6 requires C5 support on ICH9.
    #[must_use]
    pub const fn c_states(mut self, c4_on_c3: bool, c5: bool, c6: bool) -> Self {
        if c6 && !c5 {
            panic!("ICH9 C6 requires C5");
        }
        self.c4_on_c3 = c4_on_c3;
        self.c5_enable = c5;
        self.c6_enable = c6;
        self
    }

    #[must_use]
    pub const fn throttle_duty(mut self, duty: u8) -> Self {
        if duty > 7 {
            panic!("ICH9 throttle duty is a 3-bit value");
        }
        self.throttle_duty = duty;
        self
    }

    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.sata = Some(sata);
        self
    }

    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.usb = Some(usb);
        self
    }

    #[must_use]
    pub const fn hda(mut self, hda: HdaConfig) -> Self {
        self.hda = Some(hda);
        self
    }

    #[must_use]
    pub const fn io_traps<const N: usize>(mut self, traps: [IoTrapConfig; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.io_traps = self.io_traps.push(traps[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.gpio.pins = self.gpio.pins.push(pins[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn hda_verb<const P: usize, const E: usize>(
        mut self,
        vendor_id: u32,
        subsystem_id: u32,
        pins: [PinConfig; P],
        extra_verbs: [u32; E],
    ) -> Self {
        let mut table = HdaVerbTable::new(vendor_id, subsystem_id);
        let mut idx = 0;
        while idx < P {
            table = table.pin(pins[idx]);
            idx += 1;
        }
        idx = 0;
        while idx < E {
            table = table.extra_verb(extra_verbs[idx]);
            idx += 1;
        }
        let mut hda = match self.hda {
            Some(hda) => hda,
            None => HdaConfig::new(),
        };
        hda = hda.verb(table);
        self.hda = Some(hda);
        self
    }

    /// Checks decode policy at build time; ICH9 validates the required PIRQ
    /// map against enabled devices when binding the runtime driver.
    #[must_use]
    pub const fn build(self) -> Gm45Ich9Platform {
        if self.lpc_decode.fixed_io.com_a as u8 == self.lpc_decode.fixed_io.com_b as u8 {
            panic!("ICH9 COMA and COMB decode the same port");
        }
        validate_lpc_generic_io_decodes(&self.lpc_decode.generic_io);
        validate_io_traps(&self.io_traps);
        if self.max_cpus == 0 {
            panic!("GM45 max_cpus must be non-zero");
        }
        Gm45Ich9Platform {
            northbridge: self.northbridge_config(),
            southbridge: self.southbridge_config(),
            max_cpus: self.max_cpus,
        }
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }
}

const fn empty_io_trap() -> IoTrapConfig {
    IoTrapConfig {
        index: 0,
        base: 0,
        size: 0,
        access: IoTrapAccess::Any,
    }
}

const fn validate_lpc_generic_io_decodes(decodes: &ConstVec<LpcGenericIoDecode, 4>) {
    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { !ich9::valid_lpc_generic_io(decodes.get(idx)) })
    ) {
        panic!("ICH9 LPC generic I/O decode is invalid");
    }
    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { lpc_generic_io_overlaps_later(decodes, idx) })
    ) {
        panic!("ICH9 LPC generic I/O decodes overlap");
    }
}

const fn lpc_generic_io_overlaps_later(
    decodes: &ConstVec<LpcGenericIoDecode, 4>,
    idx: usize,
) -> bool {
    let decode = decodes.get(idx);
    konst::iter::eval!(
        idx + 1..decodes.len(),
        any(|next_idx| { ich9::lpc_generic_io_overlaps(decode, decodes.get(next_idx)) })
    )
}

const fn validate_io_traps(traps: &ConstVec<IoTrapConfig, 4>) {
    if konst::iter::eval!(
        0..traps.len(),
        any(|idx| { !ich9::valid_io_trap(traps.get(idx)) })
    ) {
        panic!("ICH9 I/O trap is invalid");
    }
}

/// Built config stored as a board static for the fixed Intel flow.
#[derive(Debug, Clone, Copy)]
pub struct Gm45Ich9Platform {
    pub northbridge: IntelGm45Config,
    pub southbridge: IntelIch9Config,
    pub max_cpus: u16,
}

impl Default for Gm45Ich9Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform-owned ACPI namespace context for GM45/ICH9 flows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Gm45Ich9AcpiContext;

impl Gm45Ich9AcpiContext {
    #[must_use]
    pub const fn sb_scope(self) -> &'static str {
        "\\_SB_"
    }
    #[must_use]
    pub const fn lpc_scope(self) -> &'static str {
        "\\_SB_.PCI0.LPCB"
    }
    #[must_use]
    pub const fn gpe_scope(self) -> &'static str {
        "\\_GPE"
    }
}

/// Cantiga chipset pairing; CPU-package policy is independent of the MCH.
pub struct Gm45Ich9<C = crate::legacy_cpu::Bga956>(core::marker::PhantomData<C>);

impl<C: crate::legacy_cpu::LegacyCpu> crate::IntelPlatform for Gm45Ich9<C> {
    type Config = Gm45Ich9Platform;
    const NAME: &'static str = "gm45/ich9";
    const CAR_BASE: u64 = <crate::legacy_cpu::SocketPCore2 as crate::legacy_cpu::LegacyCpu>::CAR_BASE;
    const CAR_SIZE: u64 = <crate::legacy_cpu::SocketPCore2 as crate::legacy_cpu::LegacyCpu>::CAR_SIZE;
    const MICROCODE_SIGNATURES: &'static [&'static str] =
        <crate::legacy_cpu::SocketPCore2 as crate::legacy_cpu::LegacyCpu>::MICROCODE_SIGNATURES;
}

impl crate::IntelPlatformConfig for Gm45Ich9Platform {
    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    #[cfg(feature = "mp")]
    use fstart_arch::x86::cpu::intel::core2_cpu::Core2CpuDriver;
    use fstart_driver_intel::gm45::IntelGm45;
    use fstart_driver_intel::ich9::IntelIch9;

    impl IntelEarlyPlatform for Gm45Ich9 {
        #[cfg(feature = "acpi")]
        const RESUME_DISPLAY_INIT: bool = false;
        type Northbridge = IntelGm45;
        type Southbridge = IntelIch9;
        #[cfg(feature = "mp")]
        type Cpu = Core2CpuDriver;
        #[cfg(feature = "mp")]
        fn cpu_driver(microcode: Option<&'static [u8]>) -> Self::Cpu {
            Core2CpuDriver::new(ICH9_PMBASE as u32, microcode)
        }
        #[cfg(feature = "acpi")]
        type AcpiContext = Gm45Ich9AcpiContext;
    }

    impl IntelChipsetConfig for Gm45Ich9Platform {
        type Northbridge = IntelGm45;
        type Southbridge = IntelIch9;
        fn northbridge(&'static self) -> &'static IntelGm45Config {
            &self.northbridge
        }
        fn southbridge(&'static self) -> &'static IntelIch9Config {
            &self.southbridge
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm45_ich9_config_preserves_board_policy_and_requires_pirq() {
        const CONFIG: Gm45Ich9Platform = Gm45Ich9Config::new()
            .spd_addresses([0x50, 0, 0x52, 0])
            .pcie_port(0, true)
            .c_states(true, true, true)
            .throttle_duty(2)
            .build();
        assert_eq!(CONFIG.northbridge.spd_addresses, [0x50, 0, 0x52, 0]);
        assert_eq!(CONFIG.northbridge.pmbase, ICH9_PMBASE);
        assert_eq!(CONFIG.southbridge.smbus_base, ICH9_SMBUS_BASE);
        assert_eq!(CONFIG.southbridge.rcba + 0x3800, ICH9_SPIBAR);
        assert!(CONFIG.southbridge.pcie_ports[0]);
        assert!(CONFIG.southbridge.c4_on_c3);
        assert!(CONFIG.southbridge.c5_enable);
        assert!(CONFIG.southbridge.c6_enable);
        assert_eq!(CONFIG.southbridge.throttle_duty, 2);
        assert!(CONFIG.southbridge.pirq.is_none());
    }

    #[test]
    #[should_panic(expected = "ICH9 C6 requires C5")]
    fn gm45_ich9_rejects_c6_without_c5() {
        let _ = Gm45Ich9Config::new().c_states(true, false, true);
    }

    #[test]
    #[should_panic(expected = "ICH9 LPC generic I/O decode is invalid")]
    fn gm45_ich9_rejects_invalid_lpc_decode() {
        let _ = Gm45Ich9Config::new()
            .lpc_generic_io([LpcGenericIoDecode {
                base: 0x123,
                size: 4,
            }])
            .build();
    }

    #[test]
    #[should_panic(expected = "ICH9 LPC generic I/O decodes overlap")]
    fn gm45_ich9_rejects_overlapping_lpc_decodes() {
        let _ = Gm45Ich9Config::new()
            .lpc_generic_io([
                LpcGenericIoDecode {
                    base: 0x200,
                    size: 0x20,
                },
                LpcGenericIoDecode {
                    base: 0x210,
                    size: 0x10,
                },
            ])
            .build();
    }

    #[test]
    #[should_panic(expected = "ICH9 I/O trap is invalid")]
    fn gm45_ich9_rejects_invalid_io_trap() {
        let _ = Gm45Ich9Config::new()
            .io_traps([IoTrapConfig {
                index: 0,
                base: 0x201,
                size: 4,
                access: IoTrapAccess::Any,
            }])
            .build();
    }
}
