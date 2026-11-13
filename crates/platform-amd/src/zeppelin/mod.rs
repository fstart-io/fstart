// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Zeppelin (EPYC Embedded 3000) on the noncar flow: the reset image's
//! layout, the host plan and the tables of the SoC whose silicon init is
//! `crate::soc::zeppelin`.
pub mod acpi;
pub mod layout;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests;

use crate::noncar::{self, Devices, layout::Region};
use crate::soc::zeppelin::{self as soc, Up, Zeppelin};
pub use crate::soc::zeppelin::{CcxConfig, Config, DxioConfig, FchConfig};
use fstart_driver_amd::fch::taishan;

#[cfg(feature = "host")]
impl noncar::host::HostSoc for Zeppelin {
    const POLICY: noncar::host::Policy = noncar::host::Policy {
        feature: "bundle",
        payloads: true,
        smm: true,
        release_only: false,
        // The PSP firmware packed from the SoC's and the board's blob
        // tables, with the reset image at the top of the flash. The PSP
        // inflates the stage instead of copying the whole 2.3 MiB reset
        // region, and reads the flash with Fast Read at 33 MHz (the SoC
        // table's EFS SPI settings): about 1.4 s faster to x86 on the MJ11.
        image_config: noncar::host::ImageConfig::Packed {
            reset_offset: Some(layout::RESET_FLASH_OFFSET),
            psp: fstart_image_build::amd::assembly::PspOptions {
                compress_reset: true,
                spi: None,
            },
        },
    };
}

impl noncar::DieSoc for Zeppelin {
    const LAYOUT: noncar::layout::Layout = layout::LAYOUT;
    const APOB: Region = Region::new(soc::APOB_BASE, soc::APOB_SIZE as u64);
    const BIOS_DIRECTORY: fstart_amdfw_format::efs::BiosDirectorySlot =
        fstart_amdfw_format::efs::BiosDirectorySlot::Family17hModel00;
    /// Two dies of eight SMT cores.
    const MAX_CPUS: u16 = 32;
    /// The FCH GPIO controller (AMDI0030) on the IRQ the board routes it
    /// to, as the vendor DSDT has it: three banks.
    fn acpi_devices(
        config: &Config,
        _: &Up,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str> {
        let aml = config
            .fch
            .irq_routes
            .iter()
            .find(|route| route.source == taishan::irq::GPIO)
            .and_then(|route| fstart_driver_amd::fch::aml::gpio(route.apic, 0x300).ok())
            .unwrap_or_default();
        Ok((Devices::new(), aml))
    }

    /// SP4 and SP4r2 are BGA packages; 2 channels per die, RDIMMs and
    /// LRDIMMs up to 64 GiB.
    const SMBIOS: crate::smbios::SocSmbios = crate::smbios::SocSmbios {
        socket_upgrade: 0x06,
        max_dimm_mib: 64 * 1024,
        channels_per_die: 2,
    };
    const ACPI: &'static crate::acpi::SocAcpi = &acpi::ACPI;

    fn descriptors(config: &Self::Config) -> &[&'static [fstart_driver_amd::dxio::Descriptor]] {
        &config.dxio.descriptors
    }

    fn ioapic(up: &Up, die: u8) -> u32 {
        Zeppelin::ioapic(&up.silicon, die)
    }

    /// The dies of an SP4 as AmdFabricZpDxe reports them for SRAT.
    fn die_memory(
        dies: &[fstart_driver_amd::df::Die<Self, fstart_pci::ecam::GlobalEcam>],
    ) -> Option<noncar::DieMemory> {
        if dies.len() < 2 {
            return None;
        }
        let memory = fstart_driver_amd::df::zp::die_memory(&dies[0].df())?;
        Some(
            memory
                .iter()
                .map(|m| fstart_acpi::numa::MemoryAffinity {
                    base: m.base,
                    end: m.end,
                    domain: m.die.into(),
                })
                .collect(),
        )
    }
}
