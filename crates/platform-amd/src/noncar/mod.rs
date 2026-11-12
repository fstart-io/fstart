// SPDX-License-Identifier: MIT OR Apache-2.0
//! The phases of the PSP-booted ("noncar") AMD SoCs from Family 17h on.
//! The PSP trains DRAM and starts the BSP in DRAM; one stage runs from the
//! reset vector to the payload, and the board's `main` is that stage:
//!
//! 1. [`start`]: ECAM, FCH pre-init ([`Init::fch_pre_init`]);
//!    [`Early::console`]: console, CPU identification ([`Init::identify`]),
//!    the memory map from the APOB, the cache plan, the FFS.
//! 2. The silicon phases on [`Stage`], each a step of the SoC's driver
//!    ([`Init`]): `init_fch`, `train_dxio`, `init_chipset` and
//!    `start_cpus`, which runs MP init ([`mp::start`]) and SMM with the
//!    PSP's SMM info ([`smm::install`], [`psp::notify_smm`]).
//! 3. [`Stage::scan_bus`]: PCI enumeration and resource allocation, then
//!    [`Init::after_resources`].
//! 4. [`Stage::ready_to_boot`]: the IOAPICs, then [`Init::ready_to_boot`]
//!    and the display ([`Soc::init_display`]).
//! 5. [`Stage::emit_tables`]: [`Init::end_of_post`], the ACPI tables from
//!    what the silicon and the board report ([`crate::acpi`]) and SMBIOS.
//! 6. [`Stage::boot`]: PSP LockDFReg (a failure halts), the payload's
//!    memory map and page tables, PSP BootDone and the payload.
//!
//! The SoC's driver (`fstart_driver_amd::soc`) owns the silicon; this
//! module keeps the host side: enumeration, resources, ACPI, memory map,
//! SMM and MP. A SoC module here ([`crate::zeppelin`], [`crate::picasso`])
//! adds what the flow needs to know of the SoC beyond its silicon
//! ([`Soc`]). The board's work goes between the phases, in its `main`.
pub mod acpi;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub mod boot;
pub mod cpu;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod flow;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
pub mod memory;
pub mod mp;
pub mod pci;
pub mod psp;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod smbios;
pub mod smm;

/// Devices on buses the platform set up, for CrabEFI's drivers.
#[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
pub type Peripherals = alloc::vec::Vec<fstart_payload::crabefi::peripheral::Peripheral>;

#[cfg(all(feature = "bundle", target_os = "none"))]
pub use flow::{
    Chipset, Cpus, Early, Enumerated, FchUp, Mapped, Published, Ready, Stage, Trained, start, stop,
};

use crate::acpi::SocAcpi;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_driver_amd::{df::Die, soc::Init};
pub use fstart_driver_amd::{
    fch::post,
    soc::{Ctx, Memory},
};
use fstart_pci::ecam::GlobalEcam;
use layout::{Layout, Region};

/// The most dies the flow handles: two sockets of four.
pub const MAX_DIES: usize = 8;

/// Why the flow stops before the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// A step failed.
    Failed(&'static str),
    /// The SoC's flow ends here: its later phases aren't ported yet.
    Checkpoint(&'static str),
}
impl From<&'static str> for Stop {
    fn from(what: &'static str) -> Self {
        Self::Failed(what)
    }
}

/// The fixed-address devices a SoC reports, shared by ACPI and payload
/// discovery.
pub type Devices = heapless::Vec<fstart_core::acpi::AcpiGenericDevice, 16>;

/// A noncar SoC: its silicon init ([`Init`], the SoC's driver) and what
/// the flow needs to know of it beyond the silicon: where the PSP puts
/// what, and what its tables say.
pub trait Soc: Init {
    /// The name of the CPU driver in the MP init log.
    const CPU_NAME: &'static str;
    const LAYOUT: Layout;
    /// The PCI segment ECAM decodes: what MCFG, the DSDT and the payload see.
    fn pci_root() -> fstart_core::description::PciRootInfo {
        fstart_core::description::PciRootInfo {
            segment: 0,
            ecam_base: Self::ECAM,
            bus_start: 0,
            bus_end: (Self::ECAM_BUSES - 1) as u8,
        }
    }
    /// Where the PSP writes the APOB.
    const APOB: Region;
    /// Where the PSP finds the BIOS directory.
    const BIOS_DIRECTORY: BiosDirectorySlot;
    /// TSEG size, at the top of the DRAM below 4 GiB.
    const TSEG_SIZE: u32;
    /// The most CPUs of the SoC's packages.
    const MAX_CPUS: u16;
    /// What the SMBIOS tables say of the package and its memory.
    const SMBIOS: SmbiosFacts;
    /// What the SoC's ACPI tables differ in.
    const ACPI: &'static SocAcpi;

    /// The SoC's own controllers for the tables, once the silicon init is
    /// done: the enabled fixed-address devices and further `\_SB` AML (FCH
    /// controllers and the devices behind them).
    fn acpi_devices(
        config: &Self::Config,
        state: &Self::State,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str>;
    /// The DRAM of each die, for SRAT (domain = die), if the dies are NUMA
    /// nodes: several and not interleaved.
    fn die_memory(_dies: &[Die<Self, GlobalEcam>]) -> Option<crate::acpi::DieMemory> {
        None
    }
    /// The display, at ready to boot after [`Init::ready_to_boot`]: its
    /// contributions (a framebuffer, the posted video ROM's tables) must
    /// be ready before ACPI publication and the PSP's DF lock.
    fn init_display(
        _silicon: &mut Silicon<Self>,
        _ctx: &Ctx<'_, Self>,
        _pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        Ok(())
    }

    /// The board's devices for CrabEFI to drive, after the buses are set up.
    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(_ctx: &Ctx<'_, Self>, _config: &Self::Config) -> Peripherals {
        Peripherals::new()
    }
}

/// What the silicon init hands the later phases.
pub struct Silicon<S: Soc> {
    pub cpus: mp::Cpus,
    /// Die 0's PSP mailbox, if it took the SMM info.
    pub psp: Option<psp::Psp>,
    /// Enabled fixed-address devices, shared by ACPI and payload discovery.
    pub devices: Devices,
    /// Further `\_SB` AML: FCH controllers and the devices behind them.
    pub aml: alloc::vec::Vec<u8>,
    /// Standalone device ACPI tables (for example, a posted VBIOS VFCT).
    pub tables: alloc::vec::Vec<alloc::vec::Vec<u8>>,
    pub framebuffer: Option<fstart_core::services::FramebufferInfo>,
    /// What the SoC's driver left ([`Init::State`]).
    pub state: S::State,
}

/// A board's facts: its SoC and the SoC's configuration. Host planning and
/// the board's `main` read the same values; the flow is the board's `main`.
pub trait Board: 'static {
    type Soc: Soc;
    const CONFIG: &'static <Self::Soc as Init>::Config;
}

/// The SMBIOS facts of a SoC.
#[derive(Debug, Clone, Copy)]
pub struct SmbiosFacts {
    /// Processor upgrade (socket) code, 0x06 (none) for a BGA package.
    pub socket_upgrade: u8,
    /// The largest DIMM a position takes, MiB.
    pub max_dimm_mib: u32,
    /// Memory channels per die: how the APOB numbers its DIMM seeds.
    pub channels_per_die: u8,
}

/// Declare the board's stage `main`: `fstart_platform_amd::entry!(main);`
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::__stage::stage_bin!(main: $main);
    };
}
