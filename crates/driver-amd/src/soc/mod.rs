// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs. A SoC is a list of IP versions and the constants its IP
//! blocks differ in; the IP blocks themselves are the crate's modules, each
//! with its common code and the versions where the code differs:
//!
//! | IP | [`picasso`] | [`zeppelin`] |
//! |----|-------------|--------------|
//! | [`crate::ccx`] | `ccx::picasso` (AmdCcxZenRv) | `ccx::zeppelin` (AmdCcxZenZp) |
//! | [`crate::df`] | `df::picasso` (AmdFabricRv) | `df::zeppelin` (AmdFabricZp) |
//! | [`crate::nbio`] | `nbio::picasso` (AmdNbioBaseRV) | `nbio::zeppelin` (AmdNbioBaseZP) |
//! | [`crate::dxio`] | `dxio::picasso` | `dxio::zeppelin` |
//! | [`crate::smu`] | `smu::v10` (AmdNbioSmuV10) | `smu::v9` (AmdNbioSmuV9) |
//! | [`crate::fch`] | `fch::sandstone` (FchSandstone) | `fch::taishan` (FchTaishan) |
//! | [`crate::psp`], [`crate::apob`] | common | common |
//! | [`crate::acp`] | ACP 3.x | - |
//!
//! The names in parentheses are the AGESA modules of the version.
//!
//! A SoC also says in which order its blocks are brought up, and what of
//! that a platform's boot flow has to wait for: [`Init`], the steps the flow
//! calls. The flow itself (console, memory map, MP, SMM, PCI, tables,
//! payload) is the platform's and the same for every SoC.
use crate::{
    apob::Apob,
    ccx::{Thread, microcode::Family, tsc::Tsc},
    df::Die,
    fch::{
        Acpimmio, Fch, NativeBus,
        gpio::{Bank, Gpio},
        smi::FchSmi,
    },
    psp,
    smu::Mailbox,
};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_pci::{PciEcam, ecam::GlobalEcam};

#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

/// What the IP blocks of a SoC differ in.
pub trait Soc: 'static {
    /// Dies of the largest package.
    const MAX_DIES: usize;
    /// The SMU mailbox of every die.
    const SMU: Mailbox;
    /// The SMU message of the DXIO firmware ([`crate::dxio`]).
    const DXIO_SERVICE: u32;
    /// The FCH GPIO bank.
    const GPIO_BANK: &'static Bank;
    /// MMIO bases of the FCH I2C controllers, numbered like the vendor
    /// AmdI2CMasterPei instances ([`crate::fch::i2c`]).
    const I2C: &'static [usize];
    /// Fabric ID of the IOMS (the PCIe root complex) of `die`.
    fn ioms_fabric_id(die: u8) -> u8;
}

/// DRAM as the PSP left it: the CPU's TOP_MEM/TOM2 and the APOB's map.
#[derive(Clone, Copy)]
pub struct Memory<'a> {
    pub top_mem: u64,
    pub top_mem2: Option<u64>,
    pub apob: Option<&'a Apob<'a>>,
    /// TSEG, at the top of the DRAM below 4 GiB.
    pub tseg: u64,
    pub tseg_size: u32,
    /// The e820 map for the payload.
    pub e820: &'a E820State,
}

/// What the steps of [`Init`] work on: the board's configuration, what
/// [`Init::identify`] found, the dies, die 0's FCH, the BSP's MSRs, the
/// clock and the memory map.
pub struct Ctx<'a, S: Init> {
    pub config: &'static S::Config,
    pub identity: S::Identity,
    pub dies: &'a [Die<S, GlobalEcam>],
    pub fch: &'a Fch<NativeBus>,
    pub msr: &'a NativeMsr,
    pub clock: Tsc,
    pub memory: Memory<'a>,
}
impl<S: Init> Clone for Ctx<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Init> Copy for Ctx<'_, S> {}
impl<S: Init> Ctx<'_, S> {
    /// The FCH of die 0, for pad routing and the like.
    pub fn fch(&self) -> Fch<NativeBus> {
        // SAFETY: board work runs on the BSP between the flow's own FCH
        // accesses, with the ACPI MMIO and ECAM decoded.
        Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0)
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&self) -> Gpio<NativeBus> {
        Gpio::new(self.fch(), S::GPIO_BANK)
    }
}

/// The cold init of a SoC's silicon: the steps a platform's boot flow
/// calls on the BSP, each once and in the order they are declared in. The
/// flow owns the console, the memory map, MP, SMM, PCI enumeration, the
/// tables and the payload; a SoC says what its silicon needs at each point
/// of that flow and in which order it programs its IP blocks there.
///
/// A step that fails returns its name; the flow logs it and halts.
pub trait Init: Soc + Family + Send + Sync + Sized {
    /// The log prefix.
    const NAME: &'static str;
    /// PCIe ECAM and its bus count.
    const ECAM: u64;
    const ECAM_BUSES: u16;
    /// The PSP: its mailbox rules and APOB format.
    const PSP: psp::Variant;

    /// A board's configuration of the SoC, by IP block.
    type Config: 'static;
    /// What [`Init::identify`] found.
    type Identity: Copy;
    /// What a board decides at run time for [`Init::train_dxio`] and
    /// [`Init::init_chipset`], `()` if nothing.
    type Lanes<'a>;
    type Runtime;
    /// What each step leaves for the next.
    type FchUp;
    type Trained;
    type Chipset;
    /// What every CPU's [`Init::cpu_init`] needs.
    type CpuPolicy: Send + Sync;
    /// What the silicon init leaves for the steps after MP.
    type State;

    /// FCH setup before the console: the decodes the board's console
    /// needs. `clock` is `None` if the TSC frequency is unknown.
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
    /// With the memory map: up to the FCH's buses, which the board's
    /// devices on them wait for.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<Self::FchUp, &'static str>;
    /// Up to the trained links of the board's `lanes`.
    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        fch_up: Self::FchUp,
        lanes: Self::Lanes<'_>,
    ) -> Result<Self::Trained, &'static str>;
    /// The rest of the silicon init before MP.
    fn init_chipset(
        ctx: &Ctx<'_, Self>,
        trained: Self::Trained,
        runtime: Self::Runtime,
    ) -> Result<Self::Chipset, &'static str>;
    /// On the BSP, before the APs are released.
    fn cpu_policy(
        ctx: &Ctx<'_, Self>,
        chipset: &Self::Chipset,
    ) -> Result<Self::CpuPolicy, &'static str>;
    /// The CCX init of `thread`, on that thread; whether it succeeded.
    fn cpu_init(policy: &Self::CpuPolicy, msr: &NativeMsr, thread: Thread) -> bool;
    /// Every CPU ran [`Init::cpu_init`].
    fn cpus_up(ctx: &Ctx<'_, Self>, chipset: Self::Chipset) -> Result<Self::State, &'static str>;
    /// The FCH as the owner of the SMI sources, for the SMM installation.
    fn smi(ctx: &Ctx<'_, Self>) -> FchSmi;
    /// SMM is installed and the PSP has its SMM info; PCI enumeration is
    /// next.
    fn before_enumeration(
        _ctx: &Ctx<'_, Self>,
        _state: &mut Self::State,
    ) -> Result<(), &'static str> {
        Ok(())
    }
    /// After the PCI resource allocation.
    fn after_resources(ctx: &Ctx<'_, Self>, state: &mut Self::State);
    /// Ready to boot, before the tables: the devices `pci` enumerated are
    /// where they stay.
    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        state: &mut Self::State,
        pci: &PciEcam,
    ) -> Result<(), &'static str>;
    /// The last step before the tables, after the board's own
    /// ready-to-boot work.
    fn end_of_post(_ctx: &Ctx<'_, Self>, _state: &mut Self::State) {}

    /// Die 0's PSP BAR, for the flow's PSP mailbox.
    fn psp_bar(state: &Self::State) -> u32;
    /// The NBIO IOAPIC of `die`.
    fn ioapic(state: &Self::State, die: u8) -> u32;
    /// The MMIO of `die` that PCI resources must avoid, base and size.
    fn nonpci_mmio(state: &Self::State, die: u8) -> (u64, u64);
}
