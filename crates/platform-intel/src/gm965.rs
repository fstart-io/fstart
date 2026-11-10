//! GM965/ICH8 platform defaults and fixed handwritten flow.

use fstart_driver_intel::gm965;
pub use fstart_driver_intel::gm965::{Gm965IgdConfig, IntelGm965Config};
use fstart_driver_intel::ich8;
pub use fstart_driver_intel::ich8::{
    HdaConfig, HdaVerbTable, IdeConfig, IntelIch8Config, IoTrapAccess, IoTrapConfig,
    LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode, LpcGenericIoDecode, LpcParallelDecode,
    LpcSerialDecode, PinColor, PinConfig, PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc,
    SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
pub use fstart_driver_intel::southbridge::pirq::PirqRouting;
pub use gpio::GpiRoute;

pub const GM965_MCHBAR: u64 = 0xFED1_4000;
pub const GM965_DMIBAR: u64 = 0xFED1_8000;
pub const GM965_EPBAR: u64 = 0xFED1_9000;
pub const GM965_ECAM_BASE: u64 = 0xE000_0000;
pub const GM965_ECAM_BUSES: u16 = 64;
pub const ICH8_RCBA: u64 = 0xFED1_C000;
pub const ICH8_PMBASE: u32 = 0x0500;
pub const ICH8_SMBUS_BASE: u16 = 0x0400;

/// Closed GM965/ICH8 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy)]
pub struct Gm965Ich8Config {
    pub igd: gm965::Gm965IgdConfig,
    pub enable_peg: bool,
    southbridge: IntelIch8Config,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl Gm965Ich8Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            igd: gm965::Gm965IgdConfig::new(),
            enable_peg: false,
            southbridge: IntelIch8Config::new(),
            max_cpus: 1,
        }
    }

    #[must_use]
    pub const fn pirq(mut self, routing: PirqRouting) -> Self {
        self.southbridge.pirq = routing;
        self
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }

    #[must_use]
    pub const fn northbridge_config(self) -> gm965::IntelGm965Config {
        let mut config = gm965::IntelGm965Config::new();
        config.mchbar = GM965_MCHBAR;
        config.dmibar = GM965_DMIBAR;
        config.epbar = GM965_EPBAR;
        config.ecam_base = GM965_ECAM_BASE;
        config.ecam_buses = GM965_ECAM_BUSES;
        config.enable_peg = self.enable_peg;
        config.igd = self.igd;
        config
    }

    #[must_use]
    pub const fn southbridge_config(self) -> ich8::IntelIch8Config {
        let mut config = self.southbridge;
        config.rcba = ICH8_RCBA;
        config.dmibar = GM965_DMIBAR;
        config.smbus_base = ICH8_SMBUS_BASE;
        config
    }

    /// Integrated graphics and display policy.
    #[must_use]
    pub const fn igd(mut self, igd: gm965::Gm965IgdConfig) -> Self {
        self.igd = igd;
        self
    }

    #[must_use]
    pub const fn pcie_port(mut self, port_index: usize, enabled: bool) -> Self {
        if port_index >= self.southbridge.pcie_ports.len() {
            panic!("ICH8 PCIe port index out of range");
        }
        self.southbridge.pcie_ports[port_index] = enabled;
        self
    }

    #[must_use]
    pub const fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.southbridge.lpc_decode.fixed_io = fixed_io;
        self
    }

    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(mut self, ranges: [LpcGenericIoDecode; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.lpc_decode.generic_io =
                self.southbridge.lpc_decode.generic_io.push(ranges[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.southbridge.gpe0_en = value;
        self
    }

    #[must_use]
    pub const fn gpi_routing(mut self, routing: [GpiRoute; 16]) -> Self {
        self.southbridge.gpi_routing = routing;
        self
    }

    #[must_use]
    pub const fn ide(mut self, ide: IdeConfig) -> Self {
        self.southbridge.ide = Some(ide);
        self
    }

    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.southbridge.sata = Some(sata);
        self
    }

    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.southbridge.usb = Some(usb);
        self
    }

    #[must_use]
    pub const fn io_traps<const N: usize>(mut self, traps: [IoTrapConfig; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.io_traps = self.southbridge.io_traps.push(traps[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.gpio.pins = self.southbridge.gpio.pins.push(pins[idx]);
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

        let mut hda = match self.southbridge.hda {
            Some(hda) => hda,
            None => HdaConfig::new(),
        };
        hda = hda.verb(table);
        self.southbridge.hda = Some(hda);
        self
    }

    #[must_use]
    pub const fn build(self) -> Gm965Ich8Platform {
        if self.southbridge.lpc_decode.fixed_io.com_a as u8
            == self.southbridge.lpc_decode.fixed_io.com_b as u8
        {
            panic!("ICH8 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.southbridge.sata
            && sata.ports == 0
        {
            panic!("ICH8 SATA enabled with no ports");
        }

        LpcGenericIoDecode::validate_all(&self.southbridge.lpc_decode.generic_io);
        validate_io_traps(&self.southbridge.io_traps);
        if self.max_cpus == 0 {
            panic!("GM965 max_cpus must be non-zero");
        }

        Gm965Ich8Platform {
            northbridge: self.northbridge_config(),
            southbridge: self.southbridge_config(),
            max_cpus: self.max_cpus,
        }
    }
}

const fn validate_io_traps(traps: &fstart_core::ConstVec<IoTrapConfig, 4>) {
    if konst::iter::eval!(
        0..traps.len(),
        any(|idx| { !ich8::valid_io_trap(traps.get(idx)) })
    ) {
        panic!("ICH8 I/O trap is invalid");
    }
}

/// Built GM965/ICH8 policy: the derived driver configs the fixed flow binds.
///
/// Produced by [`Gm965Ich8Config::build`]; boards point their
/// `IntelBoard::CONFIG` at a `static` of this type.
#[derive(Debug, Clone, Copy)]
pub struct Gm965Ich8Platform {
    pub northbridge: IntelGm965Config,
    pub southbridge: IntelIch8Config,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl Default for Gm965Ich8Config {
    fn default() -> Self {
        Self::new()
    }
}

/// GM965/ICH8 identity shared by host planning and the runtime flow.
pub struct Gm965Ich8<C>(core::marker::PhantomData<C>);

/// The GM965/ICH8 devices boards attach to, from the names the drivers give them.
#[cfg(feature = "namespace")]
impl<C> fstart_acpi::PcNamespace for Gm965Ich8<C> {
    const LPC: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::lpc::<
        fstart_driver_intel::gm965::IntelGm965,
        fstart_driver_intel::ich8::IntelIch8,
    >();
    const IGD: fstart_acpi::AcpiPath =
        fstart_driver_intel::namespace::igd::<fstart_driver_intel::gm965::IntelGm965>();
}

impl<C: crate::legacy_cpu::LegacyCpu> crate::IntelPlatform for Gm965Ich8<C> {
    type Config = Gm965Ich8Platform;
    const NAME: &'static str = "gm965/ich8";
    const CAR_BASE: u64 = C::CAR_BASE;
    const CAR_SIZE: u64 = C::CAR_SIZE;
    const MICROCODE_SIGNATURES: &'static [&'static str] = C::MICROCODE_SIGNATURES;
}

impl crate::IntelPlatformConfig for Gm965Ich8Platform {
    fn describe_options(&self, each: &mut dyn FnMut(fstart_options::Entry)) {
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        fstart_options::describe(
            fstart_driver_intel::gm965::IntelGm965::KNOBS,
            &fstart_driver_intel::gm965::IntelGm965::board_options(&self.northbridge),
            each,
        );
        fstart_options::describe(
            fstart_driver_intel::ich8::IntelIch8::KNOBS,
            &fstart_driver_intel::ich8::IntelIch8::board_options(&self.southbridge),
            each,
        );
    }
    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    use fstart_driver_intel::gm965::IntelGm965;
    use fstart_driver_intel::ich8::IntelIch8;

    impl<C: crate::legacy_cpu::LegacyCpu> IntelEarlyPlatform for Gm965Ich8<C> {
        #[cfg(feature = "memory-cache")]
        fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
            static WORKSPACE: crate::memory_cache::TrainingWorkspace<768> =
                crate::memory_cache::TrainingWorkspace::new();
            WORKSPACE.take()
        }
        #[cfg(feature = "acpi")]
        const RESUME_DISPLAY_INIT: bool = false;
        type Northbridge = IntelGm965;
        type Southbridge = IntelIch8;
        #[cfg(feature = "mp")]
        type Cpu = C::Driver;
        #[cfg(feature = "mp")]
        fn cpu_driver(
            microcode: Option<&'static [u8]>,
            _northbridge: &Self::Northbridge,
            _southbridge: &Self::Southbridge,
        ) -> Self::Cpu {
            C::cpu_driver(
                ICH8_PMBASE,
                microcode,
                crate::legacy_cpu::CpuPowerPolicy::default(),
            )
        }
    }

    impl IntelChipsetConfig for Gm965Ich8Platform {
        type Northbridge = IntelGm965;
        type Southbridge = IntelIch8;
        fn northbridge(&'static self) -> &'static IntelGm965Config {
            &self.northbridge
        }
        fn southbridge(&'static self) -> &'static IntelIch8Config {
            &self.southbridge
        }
    }
}
