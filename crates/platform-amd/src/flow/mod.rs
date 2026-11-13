// SPDX-License-Identifier: MIT OR Apache-2.0
//! The boot flow of every PSP-booted AMD SoC. The PSP trains DRAM and
//! starts the BSP in DRAM; one stage runs from the reset vector to the
//! payload, and the board's `main` is that stage. It drives the phases
//! and does its own work between them; each phase consumes the previous
//! one ([`Stage`]'s second parameter), so they run once and in order:
//!
//! 1. [`enter`]: ECAM, the SoC's handles and its FCH setup for the console
//!    ([`Init::enter`]); [`Early::console`]: console, BIST, CPU
//!    identification ([`Init::identify`]), then the memory map from the
//!    APOB, the cache plan and the FFS ([`Soc::map`]).
//! 2. The silicon phases, each a step of the SoC's driver ([`Init`]):
//!    `init_fch`, `train_dxio`, `init_chipset` and `start_cpus`, which
//!    runs MP init ([`crate::mp::start`]) with the SoC's CPU driver and
//!    installs SMM ([`Soc::install_smm`]).
//! 3. [`Stage::scan_bus`]: PCI enumeration and resource allocation below
//!    the root bridges the SoC reports ([`Init::roots`]), then
//!    [`Init::after_resources`].
//! 4. [`Stage::ready_to_boot`]: [`Init::ready_to_boot`] and the display
//!    ([`Soc::init_display`]).
//! 5. [`Stage::emit_tables`]: [`Init::end_of_post`], then the SoC's ACPI
//!    and SMBIOS tables with the board's part ([`Soc::tables`]).
//! 6. [`Stage::boot`]: the SoC's locks ([`Init::finalize`], a failure
//!    halts), the payload's page tables, the PSP's BootDone
//!    ([`Init::boot_done`]) and the payload ([`Soc::launch`]).
//!
//! The SoC's driver (`fstart_driver_amd::soc`) owns the silicon. What the
//! flow needs to know of a SoC beyond its silicon is [`Soc`]: where its
//! memory map and boot media come from, its SMM handler and its tables.
#[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
mod stage;
#[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
pub use stage::{
    Chipset, Console, Cpus, Early, Enumerated, FchUp, Published, Ready, Stage, Trained, enter, stop,
};

use fstart_core::services::memory_detect::E820State;
pub use fstart_driver_amd::soc::{Ctx, Memory};
use fstart_driver_amd::{apob::Apob, soc::Init};

/// Devices on buses the platform set up, for CrabEFI's drivers.
#[cfg(all(
    any(feature = "bundle", feature = "bundle-stage"),
    target_os = "none",
    feature = "payload-uefi-basic"
))]
pub type Peripherals = alloc::vec::Vec<fstart_payload::crabefi::peripheral::Peripheral>;

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

/// RAM a step takes from the OS after the memory map was built.
pub type Reserved = heapless::Vec<core::ops::Range<u64>, 4>;

/// DRAM as [`Soc::map`] found and cached it, and the boot media.
pub struct Mapped<S: Soc> {
    pub top_mem: u64,
    pub top_mem2: Option<u64>,
    pub apob: Option<Apob<'static>>,
    /// TSEG, [`Soc::TSEG_SIZE`] at the top of the DRAM below 4 GiB.
    pub tseg: u64,
    pub e820: E820State,
    /// The mounted FFS with the payload.
    pub ffs: core::ops::Range<u64>,
    pub flow: S::Flow,
}

/// A SoC on the flow: its silicon init ([`Init`], the SoC's driver) and
/// what the flow needs of it beyond the silicon.
pub trait Soc: Init {
    /// TSEG size.
    const TSEG_SIZE: u32;
    /// What the SoC's services here keep between the phases.
    type Flow;
    /// The board's part of the tables: its SMBIOS identity and its ACPI.
    type Smbios<'a>;
    type Acpi;

    /// The most CPUs of a board with `config`.
    fn max_cpus(config: &Self::Config) -> u16;
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    /// With the console: the APOB, the memory map with TSEG, the cache
    /// plan and the verified FFS, mounted before the silicon init (MP init
    /// loads the CPUs' microcode from it). The SoC may adjust `config`
    /// from what the boot media holds.
    fn map(
        hw: &Self::Hw,
        config: &mut Self::Config,
        msr: &fstart_arch::x86::cpu::msr::NativeMsr,
        clock: fstart_driver_amd::ccx::tsc::Tsc,
    ) -> Result<Mapped<Self>, &'static str>;
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    /// Install the board's SMM handler on every CPU `mp` brought up and
    /// give the PSP its SMM info.
    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut Self::State,
        mp: &fstart_arch::x86::mp::MpHandle,
        flow: &Self::Flow,
    ) -> Result<Reserved, &'static str>;
    /// The SoC's own controllers for the tables, once the silicon init is
    /// done: the enabled fixed-address devices and further `\_SB` AML (FCH
    /// controllers and the devices behind them).
    fn acpi_devices(
        _config: &Self::Config,
        _state: &Self::State,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str> {
        Ok((Devices::new(), alloc::vec::Vec::new()))
    }
    /// The display, at ready to boot after the SoC's own ready-to-boot:
    /// its contributions (a framebuffer, the posted video ROM's tables)
    /// must be ready before ACPI publication and the PSP's DF lock.
    fn init_display(
        _silicon: &mut Silicon<Self>,
        _ctx: &Ctx<'_, Self>,
        _pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop> {
        Ok(())
    }
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    /// Install ACPI with the board's `acpi` and SMBIOS with its `smbios`,
    /// from what the silicon reports and `pci` enumerated.
    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Silicon<Self>,
        pci: &fstart_pci::PciEcam,
        flow: &Self::Flow,
        smbios: &Self::Smbios<'_>,
        acpi: Self::Acpi,
    ) -> Tables;
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    /// Page tables for what the payload reaches beyond the reset entry's
    /// identity map.
    fn map_payload_ram(
        _e820: &E820State,
        _pci: &fstart_pci::PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// The board's devices for CrabEFI to drive, after the buses are set up.
    #[cfg(all(
        any(feature = "bundle", feature = "bundle-stage"),
        target_os = "none",
        feature = "payload-uefi-basic"
    ))]
    fn peripherals(_ctx: &Ctx<'_, Self>, _config: &Self::Config) -> Peripherals {
        Peripherals::new()
    }
    /// Start the payload with `handoff`.
    #[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
    fn launch(handoff: fstart_payload::Handoff<'_>, _ctx: &Ctx<'_, Self>, _flow: &Self::Flow) -> ! {
        fstart_payload::boot(handoff, ())
    }
}

/// Where the installed tables are, and how the OS turns the system off.
pub struct Tables {
    pub rsdp: Option<u64>,
    pub smbios: Option<u64>,
    pub s5: fstart_core::description::AcpiSleepControl,
}

/// What the silicon init hands the later phases.
pub struct Silicon<S: Soc> {
    pub cpus: crate::mp::Cpus,
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
