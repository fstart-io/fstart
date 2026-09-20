//! i945/ICH7 platform defaults and fixed handwritten flow.

use fstart_driver_intel::i945;
pub use fstart_driver_intel::i945::{I945IgdConfig, I945Variant, IntelI945Config};
use fstart_driver_intel::ich7;
pub use fstart_driver_intel::ich7::{
    HdaConfig, HdaVerbTable, IntelIch7Config, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PinColor, PinConfig, PinConn,
    PinConnector, PinDevice, PinGeoLoc, PinLoc, SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::southbridge::gpio_ich as gpio;

pub const I945_MCHBAR: u64 = 0xFED1_4000;
pub const I945_DMIBAR: u64 = 0xFED1_8000;
pub const I945_EPBAR: u64 = 0xFED1_9000;
pub const I945_ECAM_BASE: u64 = 0xF000_0000;
pub const I945_ECAM_BUSES: u16 = 64;
pub const ICH7_RCBA: u64 = 0xFED1_C000;
pub const ICH7_PMBASE: u32 = 0x0500;
pub const ICH7_SMBUS_BASE: u16 = 0x0400;
/// Socket 441 CAR window (matches coreboot `DCACHE_RAM_BASE/SIZE`).
pub const I945_CAR_BASE: u64 = 0xFEFC_0000;
pub const I945_CAR_SIZE: u64 = 0x8000;

/// Closed i945/ICH7 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy)]
pub struct I945Ich7Config {
    pub variant: I945Variant,
    pub gfx_gms: u8,
    pub pci_mmio_size: u32,
    pub pcie_ports: [bool; 4],
    pub pirq_routing: [u8; 8],
    pub gpi_routing: [u8; 16],
    /// Internal LAN function present (`FD_INTLAN` when false).
    pub lan: bool,
    /// AC97 audio/modem functions present (`FD_ACAUD`/`FD_ACMOD` when false).
    pub ac97_audio: bool,
    pub ac97_modem: bool,
    pub lpc_decode: LpcDecodeConfig,
    pub gpe0_en: u32,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub hda: Option<HdaConfig>,
    pub gpio: gpio::GpioConfig,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
    /// Integrated graphics configuration.
    pub igd: i945::I945IgdConfig,
}

impl I945Ich7Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            variant: I945Variant::DesktopGc,
            gfx_gms: 4,
            pci_mmio_size: 768,
            pcie_ports: [false; 4],
            pirq_routing: [0; 8],
            gpi_routing: [0; 16],
            lan: true,
            ac97_audio: true,
            ac97_modem: true,
            lpc_decode: LpcDecodeConfig::new(),
            gpe0_en: 0,
            sata: None,
            usb: None,
            hda: None,
            gpio: gpio::GpioConfig::new(),
            max_cpus: 1,
            igd: i945::I945IgdConfig::new(),
        }
    }

    #[must_use]
    pub const fn northbridge_config(self) -> i945::IntelI945Config {
        let mut config = i945::IntelI945Config::new();
        config.mchbar = I945_MCHBAR;
        config.dmibar = I945_DMIBAR;
        config.epbar = I945_EPBAR;
        config.ecam_base = I945_ECAM_BASE;
        config.ecam_buses = I945_ECAM_BUSES;
        config.rcba = ICH7_RCBA;
        config.variant = self.variant;
        config.gfx_gms = self.gfx_gms;
        config.pci_mmio_size = self.pci_mmio_size;
        config.smbus_base = ICH7_SMBUS_BASE;
        config.igd = self.igd;
        config
    }

    #[must_use]
    pub const fn igd(mut self, igd: i945::I945IgdConfig) -> Self {
        self.igd = igd;
        self
    }

    #[must_use]
    pub const fn southbridge_config(self) -> ich7::IntelIch7Config {
        let mut config = ich7::IntelIch7Config::new();
        config.rcba = ICH7_RCBA;
        config.pirq_routing = self.pirq_routing;
        config.gpi_routing = self.gpi_routing;
        config.pcie_ports = self.pcie_ports;
        config.lan = self.lan;
        config.ac97_audio = self.ac97_audio;
        config.ac97_modem = self.ac97_modem;
        config.gpe0_en = self.gpe0_en;
        config.lpc_decode = self.lpc_decode;
        config.hda = self.hda;
        config.sata = self.sata;
        config.usb = self.usb;
        config.smbus_base = ICH7_SMBUS_BASE;
        config.gpio = self.gpio;
        config
    }

    /// Date the RTC is reset to after a power loss; pass the board's SMBIOS
    /// build date so both come from one constant.
    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }
    #[must_use]
    pub const fn variant(mut self, variant: I945Variant) -> Self {
        self.variant = variant;
        self
    }
    #[must_use]
    pub const fn gfx_gms(mut self, gms: u8) -> Self {
        self.gfx_gms = gms;
        self
    }
    #[must_use]
    pub const fn pci_mmio_size(mut self, size_mb: u32) -> Self {
        self.pci_mmio_size = size_mb;
        self
    }
    #[must_use]
    pub const fn pcie_port(mut self, port_index: usize, enabled: bool) -> Self {
        if port_index >= self.pcie_ports.len() {
            panic!("ICH7 PCIe port index out of range");
        }
        self.pcie_ports[port_index] = enabled;
        self
    }
    #[must_use]
    pub const fn pirq_routing(mut self, routing: [u8; 8]) -> Self {
        self.pirq_routing = routing;
        self
    }
    #[must_use]
    pub const fn gpi_routing(mut self, routing: [u8; 16]) -> Self {
        self.gpi_routing = routing;
        self
    }
    #[must_use]
    pub const fn lan(mut self, present: bool) -> Self {
        self.lan = present;
        self
    }
    #[must_use]
    pub const fn ac97_audio(mut self, present: bool) -> Self {
        self.ac97_audio = present;
        self
    }
    #[must_use]
    pub const fn ac97_modem(mut self, present: bool) -> Self {
        self.ac97_modem = present;
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
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.gpio.pins = self.gpio.pins.push(pins[idx]);
            idx += 1;
        }
        self
    }
    #[must_use]
    pub const fn build(self) -> I945Ich7Platform {
        if self.lpc_decode.fixed_io.com_a as u8 == self.lpc_decode.fixed_io.com_b as u8 {
            panic!("ICH7 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.sata
            && !ich7::valid_sata_ports(sata.ports)
        {
            panic!("ICH7 SATA enabled with invalid ports");
        }
        validate_lpc_generic_io_decodes(&self.lpc_decode.generic_io);
        validate_pirq_routing(&self.pirq_routing);
        if !ich7::valid_gpe0_en(self.gpe0_en) {
            panic!("ICH7 GPE0 enable has reserved bits set");
        }
        if self.gfx_gms > 7 {
            panic!("i945 GMS must be 0..=7");
        }
        if self.max_cpus == 0 {
            panic!("i945 max_cpus must be non-zero");
        }
        I945Ich7Platform {
            northbridge: self.northbridge_config(),
            southbridge: self.southbridge_config(),
            max_cpus: self.max_cpus,
        }
    }
}

const fn validate_lpc_generic_io_decodes(decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>) {
    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { !ich7::valid_lpc_generic_io(decodes.get(idx)) })
    ) {
        panic!("ICH7 LPC generic I/O decode is invalid");
    }

    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { lpc_generic_io_overlaps_later(decodes, idx) })
    ) {
        panic!("ICH7 LPC generic I/O decodes overlap");
    }
}

const fn lpc_generic_io_overlaps_later(
    decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>,
    idx: usize,
) -> bool {
    let decode = decodes.get(idx);
    konst::iter::eval!(
        idx + 1..decodes.len(),
        any(|next_idx| { ich7::lpc_generic_io_overlaps(decode, decodes.get(next_idx)) })
    )
}

const fn validate_pirq_routing(routing: &[u8; 8]) {
    if konst::iter::eval!(
        0..routing.len(),
        any(|idx| { !ich7::valid_pirq_route(routing[idx]) })
    ) {
        panic!("ICH7 PIRQ route is invalid");
    }
}

/// Built i945/ICH7 policy: the derived driver configs the fixed flow binds.
///
/// Produced by [`I945Ich7Config::build`]; boards point their
/// `IntelBoard::CONFIG` at a `static` of this type.
#[derive(Debug, Clone, Copy)]
pub struct I945Ich7Platform {
    pub northbridge: IntelI945Config,
    pub southbridge: IntelIch7Config,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl Default for I945Ich7Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform-owned ACPI namespace context for i945/ICH7 flows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct I945Ich7AcpiContext;

impl I945Ich7AcpiContext {
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

#[cfg(feature = "stage")]
pub use stage::I945Ich7;

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    #[cfg(feature = "mp")]
    use fstart_arch::x86::cpu::intel::pineview::PineviewCpuDriver;
    use fstart_driver_intel::i945::IntelI945;
    use fstart_driver_intel::ich7::IntelIch7;

    /// i945/ICH7 chipset pair for the shared Intel flow.
    pub struct I945Ich7;

    impl IntelEarlyPlatform for I945Ich7 {
        const NAME: &'static str = "i945/ich7";
        #[cfg(feature = "acpi")]
        const RESUME_DISPLAY_INIT: bool = false;
        type Config = I945Ich7Platform;
        type Northbridge = IntelI945;
        type Southbridge = IntelIch7;
        #[cfg(feature = "mp")]
        type Cpu = PineviewCpuDriver;
        #[cfg(feature = "mp")]
        fn cpu_driver(microcode: Option<&'static [u8]>) -> Self::Cpu {
            PineviewCpuDriver::new(ICH7_PMBASE, microcode)
        }
        #[cfg(feature = "acpi")]
        type AcpiContext = I945Ich7AcpiContext;
    }

    impl IntelChipsetConfig for I945Ich7Platform {
        type Northbridge = IntelI945;
        type Southbridge = IntelIch7;
        fn northbridge(&'static self) -> &'static IntelI945Config {
            &self.northbridge
        }
        fn southbridge(&'static self) -> &'static IntelIch7Config {
            &self.southbridge
        }
        fn max_cpus(&self) -> u16 {
            self.max_cpus
        }
    }
}
