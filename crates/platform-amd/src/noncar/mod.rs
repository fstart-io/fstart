// SPDX-License-Identifier: MIT OR Apache-2.0
//! The phases of the PSP-booted ("noncar") AMD SoCs from Family 17h on.
//! The PSP trains DRAM and starts the BSP in DRAM; one stage runs from the
//! reset vector to the payload, and the board's `main` is that stage:
//!
//! 1. [`Reset::enter`]: ECAM, FCH pre-init ([`Soc::fch_pre_init`]);
//!    [`Reset::console`]: console, CPU identification ([`Soc::identify`]),
//!    the memory map from the APOB, the cache plan, the FFS.
//! 2. The SoC module's own silicon init phases, which take the [`Stage`].
//!    MP init ([`mp::start`]) and SMM with the PSP's SMM info
//!    ([`smm::install`], [`psp::notify_smm`]) are part of it where the SoC
//!    does them before PCI enumeration.
//! 3. [`Stage::enumerate`]: PCI enumeration and resource allocation, then
//!    [`Soc::after_resources`].
//! 4. [`Stage::ready_to_boot`]: the IOAPICs, then [`Soc::ready_to_boot`].
//! 5. [`Stage::boot`]: [`Soc::end_of_post`], the ACPI tables from what the
//!    silicon and the board report ([`crate::acpi`]), PSP LockDFReg (a failure halts), the
//!    payload's memory map and page tables, PSP BootDone and the payload.
//!
//! The SoC modules own the silicon; this module keeps the host side:
//! enumeration, resources, ACPI, memory map, SMM and MP. The board's work
//! goes between the phases, in its `main`.
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
pub mod smm;

/// Devices on buses the platform set up, for CrabEFI's drivers.
#[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
pub type Peripherals = alloc::vec::Vec<fstart_stage::crabefi::peripheral::Peripheral>;

#[cfg(all(feature = "bundle", target_os = "none"))]
pub use flow::{Enumerated, Reset, Stage, stop};

use crate::acpi::SocAcpi;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    apob::Apob,
    ccx::{microcode::Family, tsc::Tsc},
    df::Die,
    fch::{Acpimmio, Bus, Fch, NativeBus, gpio::Gpio},
};
use fstart_pci::ecam::GlobalEcam;
use layout::{Layout, Region};
use tock_registers::interfaces::Writeable;

/// A POST code to the port 80 snoop (a BMC, a debug card).
pub fn post(fch: &Fch<impl Bus>, code: u8) {
    fch.bus().reg(fstart_driver_amd::fch::POST_CODE).set(code);
}

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

/// A noncar SoC: its constants and the steps the shared phases call, each
/// once, on the BSP. The silicon init itself is the SoC module's own phases,
/// which the board's `main` calls.
pub trait Soc: fstart_driver_amd::soc::Soc + Family + Send + Sync + Sized {
    /// The log prefix.
    const NAME: &'static str;
    /// The name of the CPU driver in the MP init log.
    const CPU_NAME: &'static str;
    const LAYOUT: Layout;
    /// PCIe ECAM and its bus count.
    const ECAM: u64;
    const ECAM_BUSES: u16;
    /// The PSP: its mailbox rules and APOB format.
    const PSP: fstart_driver_amd::psp::Variant;
    /// Where the PSP writes the APOB.
    const APOB: Region;
    /// Where the PSP finds the BIOS directory.
    const BIOS_DIRECTORY: BiosDirectorySlot;
    /// TSEG size, at the top of the DRAM below 4 GiB.
    const TSEG_SIZE: u32;
    /// The most CPUs of the SoC's packages.
    const MAX_CPUS: u16;

    /// A board's configuration of the SoC, by IP block.
    type Config: 'static;
    /// What [`Soc::identify`] found.
    type Identity: Copy;
    /// What the silicon init leaves for the later phases.
    type State;

    /// FCH setup before the console: the decodes the board's
    /// console needs. `clock` is `None` if the TSC frequency is unknown.
    fn fch_pre_init(
        fch: &Fch<NativeBus>,
        config: &Self::Config,
        clock: Option<Tsc>,
    ) -> Result<(), &'static str>;
    /// After the console: identify the CPU (TSC at `mhz`, `dies` dies) and
    /// finish the early FCH setup.
    fn identify(
        fch: &Fch<NativeBus>,
        config: &Self::Config,
        mhz: u32,
        dies: u8,
    ) -> Result<Self::Identity, &'static str>;
    /// After the PCI resource allocation.
    fn after_resources(silicon: &mut Silicon<Self>, ctx: &Ctx<'_, Self>);
    /// Ready to boot, before ACPI publication and the PSP's DF lock.
    /// Device contributions such as the posted video ROM must be ready here.
    fn ready_to_boot(
        silicon: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), Stop>;

    /// The SoC's last step before the tables, after the board's own
    /// ready-to-boot work.
    fn end_of_post(_silicon: &mut Silicon<Self>, _ctx: &Ctx<'_, Self>) {}

    /// The board's devices for CrabEFI to drive, after the buses are set up.
    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(_ctx: &Ctx<'_, Self>, _config: &Self::Config) -> Peripherals {
        Peripherals::new()
    }
}

/// The memory map from the APOB and the CPU's TOP_MEM/TOM2.
pub struct Memory<'a> {
    pub top_mem: u64,
    pub top_mem2: Option<u64>,
    pub apob: Option<&'a Apob<'a>>,
    pub tseg: u64,
    /// The e820 map for the payload.
    pub e820: &'a E820State,
}

/// What the silicon init hands the later phases.
pub struct Silicon<S: Soc> {
    pub cpus: mp::Cpus,
    /// Die 0's PSP mailbox, if it took the SMM info.
    pub psp: Option<psp::Psp>,
    /// The NBIO IOAPIC of each die.
    pub ioapics: [u32; MAX_DIES],
    /// The MMIO of each die PCI resources must avoid.
    pub reserved_mmio: [Region; MAX_DIES],
    /// What the SoC's ACPI tables differ in.
    pub acpi: &'static SocAcpi,
    /// Enabled fixed-address devices, shared by ACPI and payload discovery.
    pub devices: heapless::Vec<fstart_core::acpi::AcpiGenericDevice, 16>,
    /// Further `\_SB` AML: FCH controllers and the devices behind them.
    pub aml: alloc::vec::Vec<u8>,
    /// Standalone device ACPI tables (for example, a posted VBIOS VFCT).
    pub tables: alloc::vec::Vec<alloc::vec::Vec<u8>>,
    pub framebuffer: Option<fstart_core::services::FramebufferInfo>,
    pub state: S::State,
}

/// What the phases work on: the dies, die 0's FCH and the clock.
pub struct Ctx<'a, S: Soc> {
    pub dies: &'a [Die<S, GlobalEcam>],
    pub fch: &'a Fch<NativeBus>,
    pub msr: &'a NativeMsr,
    pub clock: Tsc,
}
impl<S: Soc> Clone for Ctx<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Soc> Copy for Ctx<'_, S> {}
impl<S: Soc> Ctx<'_, S> {
    /// The FCH of die 0, for pad routing and the like.
    pub fn fch(&self) -> Fch<NativeBus> {
        // SAFETY: board work runs on the BSP between the platform's own FCH
        // accesses, with the ACPI MMIO and ECAM decoded.
        Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0)
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&self) -> Gpio<NativeBus> {
        Gpio::new(self.fch(), S::GPIO_BANK)
    }
}

/// A board's facts: its SoC and the SoC's configuration. Host planning and
/// the board's `main` read the same values; the flow is the board's `main`.
pub trait Board: 'static {
    type Soc: Soc;
    const CONFIG: &'static <Self::Soc as Soc>::Config;
}

/// Declare the board's stage `main`: `fstart_platform_amd::entry!(main);`
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::__stage::stage_bin!(main: $main);
    };
}
