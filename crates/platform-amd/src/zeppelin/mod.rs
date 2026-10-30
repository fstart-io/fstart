// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) on the noncar flow.
pub mod acpi;
pub mod layout;
pub mod silicon;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests;

use crate::noncar::{self, Board, Ctx, Silicon, Stop, Tp1, layout::Region, mp, psp, smm};
use fstart_driver_amd::cpu::{Package, Revision};
use fstart_driver_amd::{
    ccx,
    dxio::{self, Descriptor},
    fch::{
        Fch, NativeBus,
        early::PowerFailure,
        smi::FchSmi,
        taishan::{self, Devices, irq::Route},
    },
    soc::zeppelin::{self as soc, MAX_DIES, Zeppelin},
};
use silicon::{Dma, Identity, SmuPages, SmuPolicy};

/// A board's Zeppelin configuration, by IP block.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Subsystem ID of the FCH, NBIO and PCIe functions.
    pub subsystem_id: u32,
    pub ccx: CcxConfig,
    pub fch: FchConfig,
    pub dxio: DxioConfig,
}

/// The cores.
#[derive(Debug, Clone, Copy)]
pub struct CcxConfig {
    /// SYSCFG.SMEE on every thread, as the vendor firmware sets it: the OS
    /// may use SME (`mem_encrypt=on`) and SEV. Off by default: the PSP's SEV
    /// firmware keeps its state in flash through the SMM handler's P2C SPI
    /// services (PSP_ZP §3.5), which fstart doesn't implement, so Linux's
    /// SEV setup times out (5 s) and disables the PSP driver.
    pub memory_encryption: bool,
}
impl CcxConfig {
    pub const DEFAULT: Self = Self {
        memory_encryption: false,
    };
}
impl Default for CcxConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
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
    /// SP4 and SP4r2 are BGA packages; 2 channels per die, RDIMMs and
    /// LRDIMMs up to 64 GiB.
    const SMBIOS: noncar::SmbiosFacts = noncar::SmbiosFacts {
        socket_upgrade: 0x06,
        max_dimm_mib: 64 * 1024,
        channels_per_die: 2,
    };
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

    /// The silicon, MP init, SMM with the PSP's SMM info, the PCI interrupt
    /// router.
    fn silicon_init<B: Board<Soc = Self>>(tp1: Tp1<'_, Self>) -> Result<Silicon<Self>, Stop> {
        /// DRAM the SMU and the DXIO firmware use with DMA: firmware BSS in
        /// the reserved early region.
        static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
        static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
        static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();

        let Tp1 {
            ctx,
            config,
            identity,
            memory,
            microcode,
        } = tp1;
        let Ctx {
            dies,
            fch,
            msr,
            mut clock,
        } = ctx;
        // SAFETY: the DMA memory is reserved firmware BSS below 4 GiB, handed
        // out once here.
        let dma = unsafe {
            Dma {
                smu_pages: &mut *core::ptr::addr_of_mut!(SMU_PAGES),
                smu_policy: &mut *core::ptr::addr_of_mut!(SMU_POLICY),
                dxio: &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS),
            }
        };
        let mut config = *config;
        let state = silicon::pre_mp(
            dies,
            fch,
            msr,
            &mut config,
            identity,
            (memory.top_mem, memory.top_mem2),
            memory.apob,
            dma,
            &mut clock,
            |config| B::fch_reset_done(&mut ctx.board(), config),
            || B::dxio_training_done(&mut ctx.board()),
        )?;

        let policy = state.cpu_policy();
        let cpus = mp::start::<Self, _>(
            dies,
            microcode,
            move |msr, thread| ccx::zeppelin::cpu_init(msr, &policy, thread),
            clock,
        )?;
        silicon::cpus_up(dies, &mut clock);
        // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
        // decoded since the FCH init.
        let smi = unsafe { FchSmi::new(taishan::ACPI_PORTS, config.fch.smi_gevents) };
        let tseg = smm::install(Self::NAME, &cpus.mp, &smi, memory.tseg, Self::TSEG_SIZE)?;
        // The PSP gets the SMM info now, LockDFReg at ready-to-boot and
        // BootDone right before the payload.
        let psp = psp::notify_smm(state.psp_bars[0], Self::PSP, &tseg, &clock);
        silicon::post(fch, 0x42);
        // PCI: the interrupt router first.
        silicon::route_interrupts(fch, &config)?;

        Ok(Silicon {
            cpus,
            psp,
            ioapics: core::array::from_fn(|die| state.ioapics.get(die).copied().unwrap_or(0)),
            // The non-PCI MMIO pool.
            reserved_mmio: core::array::from_fn(|die| {
                state.nonpci.get(die).map_or(Region::new(0, 0), |&base| {
                    Region::new(base.into(), soc::NONPCI_MMIO_SIZE.into())
                })
            }),
            acpi: &acpi::ACPI,
            devices: heapless::Vec::new(),
            aml: Default::default(),
            tables: Default::default(),
            framebuffer: None,
            state,
        })
    }

    fn after_resources<B: Board<Soc = Self>>(_: &mut Silicon<Self>, ctx: &Ctx<'_, Self>) {
        silicon::after_resources(ctx.fch);
    }

    /// The dies of an SP4 as AmdFabricZpDxe reports them for SRAT.
    fn die_memory(
        dies: &[fstart_driver_amd::df::Die<Self, fstart_pci::ecam::GlobalEcam>],
    ) -> Option<crate::acpi::DieMemory> {
        if dies.len() < 2 {
            return None;
        }
        let memory = fstart_driver_amd::df::zeppelin::die_memory(&dies[0].df())?;
        Some(
            memory
                .iter()
                .map(|m| crate::numa::MemoryAffinity {
                    base: m.base,
                    end: m.end,
                    domain: m.die.into(),
                })
                .collect(),
        )
    }

    /// The vendor firmware's ReadyToBoot.
    fn ready_to_boot<B: Board<Soc = Self>>(
        _: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        let sata = pci
            .devices()
            .find(|a| pci.device(*a).is_some_and(|d| d.read32(0) == 0x7901_1022));
        silicon::ready_to_boot(ctx.dies, ctx.fch, sata, || {
            B::ready_to_boot(&mut ctx.board())
        });
        Ok(())
    }
}
