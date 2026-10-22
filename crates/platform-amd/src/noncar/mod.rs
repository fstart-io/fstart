// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared flow of the PSP-booted ("noncar") AMD SoCs from Family 17h on.
//! The PSP trains DRAM
//! and starts the BSP in DRAM; one stage runs from the reset vector to the
//! payload:
//!
//! 1. Entry: ECAM, FCH pre-init ([`Soc::fch_pre_init`]), console, CPU
//!    identification ([`Soc::identify`]), the memory map from the APOB,
//!    the cache plan.
//! 2. TP1, [`Soc::silicon_init`]: nearly all silicon init. MP init
//!    ([`mp::start`]) and SMM with the PSP's SMM info ([`smm::install`],
//!    [`psp::notify_smm`]) are part of it where the SoC does them before
//!    PCI enumeration.
//! 3. PCI enumeration and resource allocation, then TP2,
//!    [`Soc::after_resources`], and the board's [`Board::mainboard_final`].
//! 4. The IOAPICs and the ACPI tables from what the silicon reports
//!    ([`crate::acpi`]).
//! 5. TP3, [`Soc::ready_to_boot`], then PSP LockDFReg, the payload's
//!    memory map and page tables, PSP BootDone and the payload.
//!
//! The SoC's hooks are openSIL's timepoints; the flow keeps the host side:
//! enumeration, resources, ACPI, memory map, SMM and MP.
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

#[cfg(all(feature = "bundle", target_os = "none"))]
pub use flow::Program;

use crate::acpi::{BoardAcpi, SocAcpi};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    apob::Apob,
    ccx::{microcode::Family, tsc::Tsc},
    df::Die,
    fch::{Acpimmio, Bus, Fch, NativeBus, gpio::Gpio, lpc::Lpc},
};
use fstart_driver_uart::ns16550::Ns16550Config;
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

/// A noncar SoC: its constants and its phases of the silicon init. The flow
/// calls the phases in order, each once, on the BSP.
pub trait Soc: fstart_driver_amd::soc::Soc + Family + Send + Sync + Sized {
    /// The log prefix.
    const NAME: &'static str;
    /// The name of the CPU driver in the MP init log.
    const CPU_NAME: &'static str;
    const LAYOUT: Layout;
    /// PCIe ECAM and its bus count.
    const ECAM: u64;
    const ECAM_BUSES: u16;
    /// Where the PSP writes the APOB.
    const APOB: Region;
    /// TSEG size, at the top of the DRAM below 4 GiB.
    const TSEG_SIZE: u32;
    /// The most CPUs of the SoC's packages.
    const MAX_CPUS: u16;

    /// A board's configuration of the SoC, by IP block.
    type Config: 'static;
    /// What [`Soc::identify`] found.
    type Identity: Copy;
    /// What TP1 leaves for the later phases.
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
    /// TP1: the silicon init before PCI enumeration.
    fn silicon_init<B: Board<Soc = Self>>(tp1: Tp1<'_, Self>) -> Result<Silicon<Self>, Stop>;
    /// TP2: after the PCI resource allocation.
    fn after_resources<B: Board<Soc = Self>>(silicon: &mut Silicon<Self>, ctx: &Ctx<'_, Self>);
    /// TP3: ready to boot, before the PSP locks the DF.
    fn ready_to_boot<B: Board<Soc = Self>>(
        silicon: &mut Silicon<Self>,
        ctx: &Ctx<'_, Self>,
        pci: &fstart_pci::PciEcam,
    );
}

/// What the flow found before TP1.
pub struct Tp1<'a, S: Soc> {
    pub ctx: Ctx<'a, S>,
    pub config: &'a S::Config,
    pub identity: S::Identity,
    pub memory: &'a Memory<'a>,
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

/// What TP1 hands the flow.
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
    /// What the board hooks get.
    pub fn board(&self) -> BoardCtx<'_, S> {
        BoardCtx {
            dies: self.dies,
            clock: self.clock,
        }
    }
}

/// What the board hooks get: the dies and the clock.
pub struct BoardCtx<'a, S: Soc> {
    pub dies: &'a [Die<S, GlobalEcam>],
    pub clock: Tsc,
}
impl<S: Soc> BoardCtx<'_, S> {
    /// The FCH of die 0, for pad routing and the like.
    pub fn fch(&mut self) -> Fch<NativeBus> {
        // SAFETY: board hooks run on the BSP between the platform's own FCH
        // accesses, with the ACPI MMIO and ECAM decoded; the borrow of the
        // context keeps them sequential.
        Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0)
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&mut self) -> Gpio<NativeBus> {
        Gpio::new(self.fch(), S::GPIO_BANK)
    }
}

/// A board: its SoC, configuration and hooks. The flow owns the order of
/// the calls.
pub trait Board: 'static {
    type Soc: Soc;
    const CONFIG: &'static <Self::Soc as Soc>::Config;

    /// LPC decodes and Super I/O routing for the console, before any
    /// output.
    fn early_init(_lpc: &Lpc<impl Bus>) {}
    /// The board's console UART.
    fn console() -> Ns16550Config;
    /// After the FCH early and reset init: SMBus, I2C and GPIO work (the
    /// vendor board PEIMs).
    fn fch_reset_done(_ctx: &mut BoardCtx<'_, Self::Soc>) {}
    /// After DXIO link training, before PCI enumeration.
    fn dxio_training_done(_ctx: &mut BoardCtx<'_, Self::Soc>) {}
    /// After PCI enumeration.
    fn mainboard_final(_ctx: &mut BoardCtx<'_, Self::Soc>) {}
    /// Right before the payload.
    fn ready_to_boot(_ctx: &mut BoardCtx<'_, Self::Soc>) {}
    /// What the board adds to the ACPI tables.
    fn acpi() -> BoardAcpi {
        BoardAcpi::default()
    }
}

/// Declare the board binary: `fstart_platform_amd::stage_bin!(Board);`
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        $crate::__stage::stage_bin!(program: $crate::noncar::Program<$board>);
    };
}
