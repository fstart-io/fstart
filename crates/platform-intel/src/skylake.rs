// SPDX-License-Identifier: MIT OR Apache-2.0
//! Skylake/Kaby Lake with a Sunrise Point-H PCH: the board's policy bound
//! into the system agent and PCH configurations the Intel flow drives.
//!
//! The CPU and its system agent are one die, so there is no CPU-package
//! parameter; the CPU policy lives in the system agent's configuration.

use crate::{IntelPlatform, IntelPlatformConfig};
pub use fstart_arch::x86::cpu::intel::skylake::{AcpiTimer, Policy as CpuPolicy};
use fstart_driver_intel::{skylake::driver as nb, sunrise_point::driver as sb};

/// Board policy for a Skylake/Kaby Lake system with a Sunrise Point-H PCH.
#[derive(Debug, Clone, Copy)]
pub struct SkylakeSunrisePointConfig {
    pub northbridge: &'static nb::Config,
    pub southbridge: &'static sb::Config,
    pub max_cpus: u16,
    /// Board subsystem IDs for bus-0 functions.
    pub pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
    /// PCIe bridges declared hotplug-capable with their bus/memory/I/O reservations.
    pub pci_hotplug_ports: &'static [crate::facts::PciHotplugPort],
}

impl SkylakeSunrisePointConfig {
    #[must_use]
    pub const fn new(
        northbridge: &'static nb::Config,
        southbridge: &'static sb::Config,
        max_cpus: u16,
        pci_subsystem_ids: &'static [crate::facts::PciSubsystemId],
    ) -> Self {
        let [[a, b], [c, d]] = northbridge.spd_addresses;
        assert!(
            a != 0 || b != 0 || c != 0 || d != 0,
            "the board must wire its DIMMs: northbridge.spd_addresses"
        );
        Self {
            northbridge,
            southbridge,
            max_cpus,
            pci_subsystem_ids,
            pci_hotplug_ports: &[],
        }
    }

    /// Declare PCIe hotplug ports (for example, Thunderbolt or ExpressCard root
    /// ports) with their bus, MMIO, prefetchable MMIO, and I/O reservations.
    #[must_use]
    pub const fn with_pci_hotplug_ports(
        mut self,
        ports: &'static [crate::facts::PciHotplugPort],
    ) -> Self {
        self.pci_hotplug_ports = ports;
        self
    }
}

/// The Skylake/Kaby Lake + Sunrise Point-H platform.
pub struct SkylakeSunrisePoint;

impl IntelPlatform for SkylakeSunrisePoint {
    type Config = SkylakeSunrisePointConfig;
    const NAME: &'static str = "skylake/sunrise-point-h";
    /// coreboot's DCACHE_RAM_BASE/SIZE for Skylake.
    const CAR_BASE: u64 = 0xfef0_0000;
    const CAR_SIZE: u64 = 0x4_0000;
    /// The native DDR4 training's call chain runs deeper than 16 KiB.
    const BOOTBLOCK_STACK: u64 = 0x1_0000;
    const FIT: bool = true;
    const MICROCODE_SIGNATURES: &'static [&'static str] = &["06-5e-03", "06-9e-09", "06-9e-0a"];
}

impl IntelPlatformConfig for SkylakeSunrisePointConfig {
    fn describe_options(&self, _each: &mut dyn FnMut(fstart_options::Entry)) {}

    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }

    fn pci_subsystem_ids(&self) -> &'static [crate::facts::PciSubsystemId] {
        self.pci_subsystem_ids
    }

    fn pci_hotplug_ports(&self) -> &'static [crate::facts::PciHotplugPort] {
        self.pci_hotplug_ports
    }
}

impl fstart_acpi::PcNamespace for SkylakeSunrisePoint {
    const LPC: fstart_acpi::AcpiPath =
        fstart_driver_intel::namespace::lpc::<nb::IntelSkylake, sb::IntelSunrisePoint>();
    const IGD: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::igd::<nb::IntelSkylake>();
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{IntelChipsetConfig, IntelEarlyPlatform};
    use fstart_core::services::ServiceError;

    impl IntelEarlyPlatform for SkylakeSunrisePoint {
        const RESUME_DISPLAY_INIT: bool = false;
        const CACHE_BIOS_WINDOW: bool = true;
        type Northbridge = nb::IntelSkylake;
        type Southbridge = sb::IntelSunrisePoint;
        type Cpu = fstart_arch::x86::cpu::intel::skylake::SkylakeCpuDriver;

        fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
            use fstart_driver_intel::skylake::training_cache::RECORD_SIZE;
            static WORKSPACE: crate::memory_cache::TrainingWorkspace<RECORD_SIZE> =
                crate::memory_cache::TrainingWorkspace::new();
            WORKSPACE.take()
        }

        /// The memory controller also needs the boot mode and HSIO state the
        /// PCH captured in its early init.
        fn train_memory(
            northbridge: &mut Self::Northbridge,
            southbridge: &mut Self::Southbridge,
            cached: Option<&[u8]>,
            capture: &mut [u8],
        ) -> Result<Option<usize>, ServiceError> {
            use fstart_core::services::MemoryInit;
            use fstart_driver_intel::IntelSouthbridgeDriver;
            let pre_memory = southbridge
                .pre_memory_context()
                .ok_or(ServiceError::NotInitialized)?;
            northbridge.set_pre_memory(pre_memory);
            let smbus = southbridge
                .smbus_mut()
                .ok_or(ServiceError::NotInitialized)?;
            northbridge.bring_up(fstart_driver_intel::SpdTraining {
                smbus,
                cached,
                capture,
            })
        }

        /// The 100 Series PCH's SPI controller is its own PCI function.
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe fn cache_flash(
            extent: fstart_driver_intel::southbridge::spi::FlashExtent,
        ) -> Result<fstart_driver_intel::southbridge::spi::CacheSpi, ServiceError> {
            // SAFETY: the caller's contract.
            unsafe { fstart_driver_intel::southbridge::spi::CacheSpi::new_pch(extent) }
        }

        /// The PCH's devices need their assigned BARs, and its HDA the
        /// system agent's display engine for the display codec.
        fn init_devices(
            northbridge: &mut Self::Northbridge,
            southbridge: &mut Self::Southbridge,
            pci: &fstart_pci::PciEcam,
        ) -> Result<(), ServiceError> {
            use fstart_driver_intel::IntelNorthbridgeDriver;
            northbridge.post_dram_init()?;
            northbridge.bind_scanout(pci);
            southbridge.post_pci_init(pci)?;
            let display_audio = northbridge.display_audio()?;
            southbridge.initialize_devices(
                display_audio
                    .as_ref()
                    .map(|(graphics, mhz96, tmode2t)| (graphics, *mhz96, *tmode2t)),
            )
        }

        fn cpu_driver(
            microcode: Option<&'static [u8]>,
            northbridge: &Self::Northbridge,
            _southbridge: &Self::Southbridge,
        ) -> Self::Cpu {
            use fstart_driver_intel::IntelNorthbridgeDriver;
            Self::Cpu::new(northbridge.config().cpu, microcode)
        }
    }

    impl IntelChipsetConfig for SkylakeSunrisePointConfig {
        type Northbridge = nb::IntelSkylake;
        type Southbridge = sb::IntelSunrisePoint;
        fn northbridge(&'static self) -> &'static nb::Config {
            self.northbridge
        }
        fn southbridge(&'static self) -> &'static sb::Config {
            self.southbridge
        }
    }
}
