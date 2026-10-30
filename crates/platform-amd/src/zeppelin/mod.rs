// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) on the noncar flow.
pub mod acpi;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod layout;
pub mod silicon;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub use init::{Dxio, Init};
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests;

use crate::noncar::{self, Ctx, Silicon, Stop, layout::Region};
use fstart_driver_amd::cpu::{Package, Revision};
use fstart_driver_amd::{
    ccx,
    dxio::Descriptor,
    fch::{
        Fch, NativeBus,
        early::PowerFailure,
        taishan::{Devices, irq::Route},
    },
    soc::zeppelin::{self as soc, MAX_DIES, Zeppelin},
};
use silicon::Identity;

/// A board's Zeppelin configuration, by IP block.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Subsystem ID of the FCH, NBIO and PCIe functions.
    pub subsystem_id: u32,
    pub fch: FchConfig,
    pub dxio: DxioConfig,
}

/// The Taishan FCH.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// AC power-loss policy.
    pub power_failure: PowerFailure,
    /// FCH devices in use, per die.
    pub devices: [Devices; MAX_DIES],
    /// The PCI interrupt router (0xc00/0xc01) table.
    pub irq_routes: &'static [Route],
    /// GEVENT pins (bit n = GEVENTn) routed to the board's SMM handler,
    /// `fstart_driver_amd::fch::taishan::smm::SmmBoard::gevent`.
    pub smi_gevents: u32,
}
impl FchConfig {
    pub const DEFAULT: Self = Self {
        power_failure: PowerFailure::Previous,
        devices: [Devices {
            xhci: true,
            sata: true,
        }; MAX_DIES],
        irq_routes: &[],
        smi_gevents: 0,
    };
}
impl Default for FchConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The DXIO engines (PCIe, SATA lanes) of each die.
#[derive(Debug, Clone, Copy, Default)]
pub struct DxioConfig {
    pub descriptors: [&'static [Descriptor]; MAX_DIES],
}

#[cfg(feature = "host")]
impl noncar::host::HostSoc for Zeppelin {
    const POLICY: noncar::host::Policy = noncar::host::Policy {
        feature: "bundle-zeppelin",
        payloads: true,
        smm: true,
        release_only: false,
        // The PSP inflates the stage instead of copying the whole 2.3 MiB
        // reset region, and reads the flash with Fast Read at 33 MHz
        // instead of its default: about 1.4 s faster to x86 on the MJ11.
        image_config: noncar::host::ImageConfig::Vendor {
            prefix: layout::VENDOR_PREFIX,
            psp: fstart_image_build::amd::assembly::PspOptions {
                compress_reset: true,
                spi: Some((7, 1)),
            },
        },
    };
}

impl noncar::Soc for Zeppelin {
    const NAME: &'static str = "zeppelin";
    const CPU_NAME: &'static str = "AMD Zeppelin";
    const LAYOUT: noncar::layout::Layout = layout::LAYOUT;
    const ECAM: u64 = soc::ECAM;
    const ECAM_BUSES: u16 = soc::ECAM_BUSES;
    const PSP: fstart_driver_amd::psp::Variant = fstart_driver_amd::psp::Variant::Zp;
    const APOB: Region = Region::new(soc::APOB_BASE, soc::APOB_SIZE as u64);
    const BIOS_DIRECTORY: fstart_amdfw_format::efs::BiosDirectorySlot =
        fstart_amdfw_format::efs::BiosDirectorySlot::Family17hModel00;
    /// 8 MiB of TSEG.
    const TSEG_SIZE: u32 = 8 << 20;
    /// Two dies of eight SMT cores.
    const MAX_CPUS: u16 = 32;
    type Config = Config;
    type Identity = Identity;
    type State = silicon::State;

    fn fch_pre_init(
        fch: &Fch<NativeBus>,
        _: &Config,
        _: Option<ccx::tsc::Tsc>,
    ) -> Result<(), &'static str> {
        silicon::fch_pre_init(fch).map_err(|_| "FCH pre-init")
    }

    fn identify(
        fch: &Fch<NativeBus>,
        config: &Config,
        mhz: u32,
        dies: u8,
    ) -> Result<Identity, &'static str> {
        silicon::fch_early_init(fch, config);
        let (ext_eax, ext_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        let package = Package::from_cpuid(ext_ebx);
        let revision = Revision::from_signature(ext_eax)
            .ok()
            .filter(|revision| revision.zp());
        fstart_log::info!(
            "fstart: AMD Zeppelin {}, package {}, TSC {} MHz",
            revision.map_or("unknown", Revision::name),
            package.name(),
            mhz
        );
        let revision = revision
            .filter(|_| package.dies() == Some(dies) && mhz != 0)
            .ok_or("CPU identification")?;
        Ok(Identity { revision, package })
    }

    fn after_resources(_: &mut Silicon<Self>, ctx: &Ctx<'_, Self>) {
        silicon::after_resources(ctx.fch);
    }

    /// The vendor firmware's ReadyToBoot.
    /// The vendor firmware's ReadyToBoot, up to the board's own work.
    fn ready_to_boot(
        _: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        let sata = pci
            .devices()
            .find(|a| pci.device(*a).is_some_and(|d| d.read32(0) == 0x7901_1022));
        silicon::ready_to_boot(ctx.dies, ctx.fch, sata);
        Ok(())
    }

    /// The end of the vendor ReadyToBoot, after the board's work.
    fn end_of_post(_: &mut Silicon<Self>, ctx: &Ctx<'_, Self>) {
        silicon::end_of_post(ctx.dies);
    }
}
