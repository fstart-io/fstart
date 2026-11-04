//! Bay Trail SoC platform: the board's policy built into the north and
//! south cluster configurations the fixed Intel flow binds.
//!
//! The SoC integrates the CPU, so unlike the legacy chipset pairs there is
//! no CPU-package parameter: Silvermont is the CPU.

pub use fstart_driver_intel::baytrail::devices::gfx::{GfxConfig, PanelPipe};
pub use fstart_driver_intel::baytrail::devices::lpe::{CodecClock, LpeConfig};
pub use fstart_driver_intel::baytrail::devices::pcie::PcieConfig;
pub use fstart_driver_intel::baytrail::devices::sata::SataConfig;
pub use fstart_driver_intel::baytrail::devices::scc::SccConfig;
pub use fstart_driver_intel::baytrail::devices::usb::{Usb2Phy, UsbConfig};
pub use fstart_driver_intel::baytrail::devices::{Function, Functions};
pub use fstart_driver_intel::baytrail::gpio::{self, GpioConfig, Pad, Pull, Strength};
pub use fstart_driver_intel::baytrail::lpc::SpiLockdown;
pub use fstart_driver_intel::baytrail::north::SpdSource;
pub use fstart_driver_intel::baytrail::punit::VrPs2;
pub use fstart_driver_intel::baytrail::{NorthConfig, SouthConfig};
pub use fstart_driver_intel::southbridge::pirq::PirqRouting;

/// Board policy for a Bay Trail SoC (coreboot's `soc/intel/baytrail`
/// chip config and the devicetree's function switches).
#[derive(Debug, Clone, Copy)]
pub struct BaytrailConfig {
    north: NorthConfig,
    south: SouthConfig,
    max_cpus: u16,
}

impl BaytrailConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            north: NorthConfig::new(),
            south: SouthConfig::new(),
            max_cpus: 4,
        }
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }

    /// Where raminit finds the SPD.
    #[must_use]
    pub const fn spd(mut self, spd: SpdSource) -> Self {
        self.north.spd = spd;
        self
    }

    #[must_use]
    pub const fn vr_ps2(mut self, ps2: VrPs2) -> Self {
        self.north.vr_ps2 = ps2;
        self
    }

    /// Keep the DDR3 refresh rate at 1x (`disable_ddr_2x_refresh_rate`).
    #[must_use]
    pub const fn ddr_1x_refresh(mut self) -> Self {
        self.north.ddr_2x_refresh = false;
        self
    }

    #[must_use]
    pub const fn gfx(mut self, gfx: GfxConfig) -> Self {
        self.north.gfx = gfx;
        self
    }

    /// Turn a south cluster function off. Turning the TXE off also skips
    /// its UMA handshake: a TXE hidden after DRAM Init Done locks up.
    #[must_use]
    pub const fn disable(mut self, function: Function) -> Self {
        self.south.disabled = self.south.disabled.with(function);
        if matches!(function, Function::Txe) {
            self.north.txe = false;
        }
        self
    }

    /// Turn several south cluster functions off.
    #[must_use]
    pub const fn disable_all<const N: usize>(mut self, functions: [Function; N]) -> Self {
        let mut i = 0;
        while i < N {
            self = self.disable(functions[i]);
            i += 1;
        }
        self
    }

    /// Bring out the PCU's legacy COM1 (on by default).
    #[must_use]
    pub const fn com1(mut self, enabled: bool) -> Self {
        self.south.com1 = enabled;
        self
    }

    #[must_use]
    pub const fn gpio(mut self, gpio: GpioConfig) -> Self {
        self.south.gpio = gpio;
        self
    }

    #[must_use]
    pub const fn pirq(mut self, routing: PirqRouting) -> Self {
        self.south.pirq = routing;
        self
    }

    /// PIC IRQ per PIRQ in legacy mode; 0 leaves a PIRQ unrouted.
    #[must_use]
    pub const fn pic_routes(mut self, routes: [u8; 8]) -> Self {
        self.south.pic_routes = routes;
        self
    }

    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.south.sata = sata;
        self
    }

    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.south.usb = usb;
        self
    }

    #[must_use]
    pub const fn pcie(mut self, pcie: PcieConfig) -> Self {
        self.south.pcie = pcie;
        self
    }

    #[must_use]
    pub const fn pcie_wake(mut self, wake: bool) -> Self {
        self.south.pcie_wake = wake;
        self
    }

    #[must_use]
    pub const fn scc(mut self, scc: SccConfig) -> Self {
        self.south.scc = scc;
        self
    }

    #[must_use]
    pub const fn lpss_acpi_mode(mut self, acpi: bool) -> Self {
        self.south.lpss_acpi_mode = acpi;
        self
    }

    #[must_use]
    pub const fn lpe(mut self, lpe: LpeConfig) -> Self {
        self.south.lpe = lpe;
        self
    }

    #[must_use]
    pub const fn disable_slp_x_stretch_sus_fail(mut self, disable: bool) -> Self {
        self.south.disable_slp_x_stretch_sus_fail = disable;
        self
    }

    #[must_use]
    pub const fn spi_lockdown(mut self, lockdown: SpiLockdown) -> Self {
        self.south.spi_lockdown = Some(lockdown);
        self
    }

    #[must_use]
    pub const fn build(self) -> BaytrailPlatform {
        if self.max_cpus == 0 {
            panic!("Bay Trail max_cpus must be non-zero");
        }
        if self.south.sata.port_map & !0x3 != 0 {
            panic!("Bay Trail SATA has two ports");
        }
        if self.south.disabled.hides_function_zero_of_enabled() {
            panic!("Bay Trail: disabling function 0 needs its sibling functions disabled too");
        }
        if self.south.disabled.contains(Function::Txe) == self.north.txe {
            panic!("the TXE handshake must follow the TXE function switch");
        }
        BaytrailPlatform {
            north: self.north,
            south: self.south,
            max_cpus: self.max_cpus,
        }
    }
}

impl Default for BaytrailConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Built Bay Trail policy: the cluster configs the fixed flow binds.
#[derive(Debug, Clone, Copy)]
pub struct BaytrailPlatform {
    pub north: NorthConfig,
    pub south: SouthConfig,
    pub max_cpus: u16,
}

/// Platform-owned ACPI namespace context.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BaytrailAcpiContext;

impl BaytrailAcpiContext {
    #[must_use]
    pub const fn sb_scope(self) -> &'static str {
        "\\_SB_"
    }

    #[must_use]
    pub const fn lpc_scope(self) -> &'static str {
        "\\_SB_.PCI0.LPCB"
    }
}

/// The Bay Trail SoC.
pub struct Baytrail;

impl crate::IntelPlatform for Baytrail {
    type Config = BaytrailPlatform;
    const NAME: &'static str = "baytrail";
    /// coreboot's DCACHE_RAM_BASE with its CAR and MRC areas combined.
    const CAR_BASE: u64 = 0xfe00_0000;
    const CAR_SIZE: u64 = 0x1_0000;
    const MICROCODE_SIGNATURES: &'static [&'static str] = &["06-37-08", "06-37-09"];
}

impl crate::IntelPlatformConfig for BaytrailPlatform {
    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    use fstart_driver_intel::baytrail::{BaytrailNorth, BaytrailSouth};

    impl IntelEarlyPlatform for Baytrail {
        #[cfg(feature = "acpi")]
        const RESUME_DISPLAY_INIT: bool = false;
        type Northbridge = BaytrailNorth;
        type Southbridge = BaytrailSouth;
        #[cfg(feature = "mp")]
        type Cpu = fstart_arch::x86::cpu::intel::silvermont::SilvermontCpuDriver;
        #[cfg(feature = "mp")]
        fn cpu_driver(microcode: Option<&'static [u8]>) -> Self::Cpu {
            Self::Cpu::new(microcode)
        }
        /// Cores from the topology leaf: Silvermont has no hyper-threading
        /// and CPUID leaf 1 counts addressable IDs, not cores.
        #[cfg(feature = "mp")]
        fn logical_cpus() -> u16 {
            (0..8)
                .map(|level| fstart_arch::x86::cpuid_count(0xb, level))
                .find(|&(_, _, ecx, _)| (ecx >> 8) & 0xff == 2)
                .map_or(1, |(_, ebx, _, _)| (ebx & 0xffff) as u16)
                .max(1)
        }
        #[cfg(feature = "acpi")]
        type AcpiContext = BaytrailAcpiContext;
    }

    impl IntelChipsetConfig for BaytrailPlatform {
        type Northbridge = BaytrailNorth;
        type Southbridge = BaytrailSouth;
        fn northbridge(&'static self) -> &'static NorthConfig {
            &self.north
        }
        fn southbridge(&'static self) -> &'static SouthConfig {
            &self.south
        }
    }
}
