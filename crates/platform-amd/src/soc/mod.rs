// SPDX-License-Identifier: MIT OR Apache-2.0
//! What makes a SoC. A SoC is a list of IP versions of the driver's IP
//! blocks and the constants they differ in, which the SoC's module of this
//! crate names ([`fstart_driver_amd::soc::Soc`] is what the blocks need of
//! it):
//!
//! | IP | [`crate::picasso`] | [`crate::zeppelin`] | [`crate::naples`] | [`crate::rome`] | [`crate::milan`] |
//! |----|----|----|----|----|----|
//! | CCX | `ccx::rv` | `ccx::zp` | `ccx::init`, `tables::zp` | `tables::ssp` | `tables::gn` |
//! | DF | `df::rv` | `df::zp` | `df::zp` | `df::ssp` | `df::gn` |
//! | NBIO | `nbio::rv` | `nbio::zp` | `nbio::zp` | `nbio::ssp` | `nbio::ssp` |
//! | DXIO | `dxio::rv` | `dxio::zp` | `dxio::zp::sequence` | `dxio::ssp` | `dxio::ssp` |
//! | SMU | `smu::v10` | `smu::v9` | `smu::v9` | `smu::v11` | `smu::v11` |
//! | FCH | `fch::sandstone` | `fch::taishan` | `fch::taishan`, `fch::hw` | `fch::huashan` | `fch::huashan` |
//!
//! PSP and APOB are common; Picasso adds the ACP.
//!
//! A SoC also says in which order its blocks are brought up, and what of
//! that a platform's boot flow has to wait for: [`Init`], the steps the flow
//! calls. The flow itself (console, memory map, MP, SMM, PCI, tables,
//! payload) is the platform's and the same for every SoC.
use fstart_arch::x86::{
    cpu::{amd::smm::InstalledTseg, msr::NativeMsr},
    ioapic::IoApic,
    mp::CpuDriver,
};
use fstart_core::{description::PciRootInfo, services::memory_detect::E820State};
use fstart_driver_amd::{
    apob::Apob,
    ccx::tsc::Tsc,
    df::Die,
    fch::{Acpimmio, Fch, NativeBus, gpio::Gpio},
    psp,
};
use fstart_pci::{MAX_PCI_ROOTS, PciEcam, PciRoot, ecam::GlobalEcam};

#[cfg(test)]
mod silicon_tests;

pub use fstart_driver_amd::soc::Soc;

/// DRAM as the PSP left it: the CPU's TOP_MEM/TOM2 and the APOB's map.
#[derive(Clone, Copy)]
pub struct Memory<'a> {
    pub top_mem: u64,
    pub top_mem2: Option<u64>,
    pub apob: Option<&'a Apob<'static>>,
    /// TSEG, at the top of the DRAM below 4 GiB.
    pub tseg: u64,
    pub tseg_size: u32,
    /// The e820 map for the payload.
    pub e820: &'a E820State,
}

/// What the steps of [`Init`] work on: the board's configuration, what
/// [`Init::identify`] found, the SoC's register handles, the BSP's MSRs,
/// the clock and the memory map.
pub struct Ctx<'a, S: Init> {
    pub config: &'a S::Config,
    /// What the flow's services read of the board.
    pub board: &'a crate::board::Facts,
    pub identity: S::Identity,
    pub hw: &'a S::Hw,
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

/// What [`Init::enter`] leaves: the handles, the clock if the TSC frequency
/// is known, and whether the FCH setup for the console succeeded (a
/// failure is reported once there is a console).
pub struct Entered<S: Init> {
    pub hw: S::Hw,
    pub clock: Option<Tsc>,
    pub pre_init: Result<(), &'static str>,
}

/// The root bridges of a SoC, for one PCI allocator.
pub type Roots = heapless::Vec<PciRoot, MAX_PCI_ROOTS>;

/// The cold init of a SoC's silicon: the steps a platform's boot flow
/// calls on the BSP, each once and in the order they are declared in. The
/// flow owns the console, the memory map, the MP engine, the SMM
/// installation, PCI enumeration, the tables and the payload; a SoC says
/// what its silicon needs at each point of that flow and in which order it
/// programs its IP blocks there. Its register handles are its own
/// ([`Init::Hw`]), and so is what a step leaves for the next.
///
/// A step that fails returns its name; the flow logs it and halts.
pub trait Init: Sized + 'static {
    /// The log prefix.
    const NAME: &'static str;

    /// A board's configuration of the SoC, by IP block.
    type Config: 'static;
    /// What [`Init::identify`] found.
    type Identity: Copy;
    /// The register handles of the SoC.
    type Hw;
    /// What a board decides at run time for [`Init::train_dxio`] and
    /// [`Init::init_chipset`], `()` if nothing.
    type Lanes<'a>;
    type Runtime;
    /// What each step leaves for the next.
    type FchUp;
    type Trained;
    type Chipset;
    /// The CPU driver of the MP engine: the CCX init of every thread.
    type Cpu: CpuDriver;
    /// What the silicon init leaves for the steps after MP.
    type State;

    /// Right after the reset vector: ECAM, the handles and the FCH setup
    /// the board's console needs.
    fn enter(config: &Self::Config, msr: &NativeMsr) -> Entered<Self>;
    /// After the console: identify the CPU and finish the early FCH setup.
    fn identify(
        hw: &Self::Hw,
        config: &Self::Config,
        clock: Option<Tsc>,
    ) -> Result<Self::Identity, &'static str>;
    /// The PCI segment ECAM decodes: what the allocator, MCFG, the DSDT
    /// and the payload see.
    fn pci_segment(hw: &Self::Hw) -> PciRootInfo;
    /// The PSP: its mailbox rules and APOB format.
    fn psp(hw: &Self::Hw) -> psp::Variant;

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

    /// On the BSP, before the APs are released: its own CCX init and the
    /// driver every thread runs, with the patches of `microcode`.
    fn cpu_driver(
        ctx: &Ctx<'_, Self>,
        chipset: &mut Self::Chipset,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str>;
    /// The threads MP init brings up, the BSP included.
    fn threads(ctx: &Ctx<'_, Self>, chipset: &Self::Chipset) -> Result<u16, &'static str>;
    /// Release `aps` APs into the reset vector, which the flow routed to
    /// the MP engine. The error is for the MP engine's log.
    fn release_aps(ctx: &Ctx<'_, Self>, chipset: &Self::Chipset, aps: u16) -> Result<(), u32>;
    /// Threads whose CCX init or microcode load failed.
    fn cpu_errors(cpu: &Self::Cpu) -> u32;
    /// Every thread ran its CCX init; `apic_ids` are theirs, the BSP's
    /// first.
    fn cpus_up(
        ctx: &Ctx<'_, Self>,
        chipset: Self::Chipset,
        mp: &fstart_arch::x86::mp::MpHandle,
        apic_ids: &[u32],
    ) -> Result<Self::State, &'static str>;

    /// PCI enumeration is next.
    fn before_enumeration(
        _ctx: &Ctx<'_, Self>,
        _state: &mut Self::State,
    ) -> Result<(), &'static str> {
        Ok(())
    }
    /// The root bridges: what the fabric routes to each and what that is
    /// for. The allocator and the tables both take them from here.
    fn roots(ctx: &Ctx<'_, Self>, state: &Self::State) -> Option<Roots>;
    /// After the PCI resource allocation.
    fn after_resources(ctx: &Ctx<'_, Self>, state: &mut Self::State) -> Result<(), &'static str>;
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
    /// After the last fabric write: lock what the OS must not change. A
    /// failure halts.
    fn finalize(ctx: &Ctx<'_, Self>, state: &mut Self::State) -> Result<(), &'static str>;
    /// Right before the payload, the APs parked: the PSP's BootDone. A
    /// SoC whose payload must not run without it returns the failure.
    fn boot_done(ctx: &Ctx<'_, Self>, state: &mut Self::State) -> Result<(), &'static str>;
}

/// What the SMM installation of a SoC made of dies ([`Hw`]) needs: the
/// FCH as the owner of the SMI sources, and the PSP's SMM info through
/// die 0's mailbox afterwards.
pub trait DieInit: Init<Hw = Hw<Self>> + Soc {
    /// The owner of the SMI sources.
    fn smi(ctx: &Ctx<'_, Self>, state: &Self::State) -> fstart_driver_amd::fch::smi::FchSmi;
    /// SMM is installed and locked in `tseg`: the PSP gets its SMM info.
    fn smm_installed(
        ctx: &Ctx<'_, Self>,
        state: &mut Self::State,
        tseg: &InstalledTseg,
    ) -> Result<(), &'static str>;
}

/// The register handles of a SoC whose dies each have a Data Fabric node
/// with one root complex ([`Die`]): Picasso, Zeppelin.
pub struct Hw<S: Soc> {
    /// Die 0's FCH.
    pub fch: Fch<NativeBus>,
    /// The dies, die 0 first.
    pub dies: heapless::Vec<Die<S, GlobalEcam>, 8>,
}

impl<S: Soc> Hw<S> {
    /// ECAM at `ecam` with `buses` buses first, everything else uses it;
    /// then the ACPI MMIO, the clock and the SoC's FCH setup for the
    /// console, `pre_init`.
    pub fn enter<I: Init<Hw = Self>>(
        msr: &NativeMsr,
        ecam: u64,
        buses: u16,
        pre_init: impl FnOnce(&Fch<NativeBus>, Option<Tsc>) -> Result<(), &'static str>,
    ) -> Entered<I> {
        // SAFETY: BSP only, single-threaded cold boot; the ECAM comes up
        // before the first config space access.
        let fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
        fstart_driver_amd::fch::post(&fch, 0x30);
        fstart_driver_amd::ccx::msr::enable_ecam(msr, ecam, buses);
        fstart_pci::ecam::init(ecam as usize);
        // SAFETY: exclusive FCH programming before the console.
        unsafe { fstart_driver_amd::fch::enable_acpimmio() };
        let mhz = fstart_arch::x86::cpu::amd::msr::tsc_mhz(msr).unwrap_or(0);
        let clock = core::num::NonZeroU32::new(mhz).map(Tsc::new);
        let pre_init = pre_init(&fch, clock);
        // SAFETY: BSP-only silicon init serializes all die register accesses.
        let dies = unsafe { fstart_driver_amd::df::dies(GlobalEcam) }.collect();
        Entered {
            hw: Self { fch, dies },
            clock,
            pre_init,
        }
    }

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

    /// The LPC bridge, for decodes and Super I/O routing of a console
    /// behind it.
    pub fn lpc(&self) -> fstart_driver_amd::fch::lpc::Lpc<&NativeBus> {
        // SAFETY: exclusive LPC decode programming on the BSP.
        unsafe { fstart_driver_amd::fch::lpc::Lpc::new(self.fch.bus()) }
    }

    /// The threads the SMU releases, the BSP included.
    pub fn threads(&self) -> Result<u16, &'static str> {
        fstart_driver_amd::smu::threads::thread_count(
            &self.dies,
            fstart_driver_amd::smu::threads::Policy::DEFAULT,
        )
        .map(|threads| threads as u16)
        .map_err(|_| "CPU topology")
    }

    /// The SMU releases `aps` APs.
    pub fn release_aps(&self, aps: u16, clock: Tsc) -> Result<(), u32> {
        use fstart_driver_amd::smu::threads::{self, Policy};
        // SAFETY: the flow routed the reset vector to the MP engine.
        unsafe {
            threads::launch(&self.dies, Policy::DEFAULT, usize::from(aps), &mut {
                clock
            })
        }
        .map_err(|error| {
            if let threads::Error::ApCount { started, requested } = error {
                fstart_log::error!("smu: started {} of {} APs", started, requested);
            }
            1
        })
    }

    /// Each die's root complex as a PCI root bridge in `segment`, with the
    /// die's own MMIO `non_pci` (base, size).
    pub fn roots(
        &self,
        segment: &PciRootInfo,
        non_pci: impl Fn(u8) -> (u64, u64),
    ) -> Option<Roots> {
        let ecam = (segment.ecam_base, segment.ecam_size());
        self.dies
            .iter()
            .map(|die| {
                let routes = fstart_driver_amd::df::map::root_complex(die)?;
                fstart_driver_amd::df::root::root(&routes, ecam, non_pci(die.index()))
            })
            .collect()
    }

    /// The FCH's IOAPIC (GSI 0-23, ID 0) in virtual-wire mode to the BSP
    /// `bsp_apic`, then one per die's NBIO at `ioapic(die)` (ID 1 + die),
    /// all masked.
    pub fn init_ioapics(&self, bsp_apic: u8, ioapic: impl Fn(u8) -> u32) {
        self.fch.enable_hpet_ioapic();
        // SAFETY: decoded IOAPIC MMIO, BSP only.
        let fch_ioapic = unsafe { IoApic::new(fstart_driver_amd::fch::IOAPIC as usize) };
        fch_ioapic.set_id(0);
        fch_ioapic.mask_all();
        fch_ioapic.route_i8259(bsp_apic);
        for die in &self.dies {
            // SAFETY: as above, mapped by the silicon init.
            let nbio = unsafe { IoApic::new(ioapic(die.index()) as usize) };
            nbio.set_id(1 + die.index());
            nbio.mask_all();
        }
    }
}
