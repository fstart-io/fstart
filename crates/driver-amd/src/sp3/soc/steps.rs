//! The SP3 silicon steps (Naples in [`super::naples`], Rome and Milan in
//! [`super::rome_milan`]) and what they hand on: the board's silicon
//! policy, the fabric's topology and plan, the handles and the BARs.
//!
//! A boot flow runs the steps in the order they are declared in, on the
//! BSP. A step that fails returns its name.
extern crate alloc;

use alloc::vec::Vec;

use crate::apob::Apob;
use crate::df::plan::{self, Plan, RootBridgePlan, Window};
use crate::df::{Version, program};
use crate::dxio::wire::MAX_ENGINES;
use crate::nbio::iohc::DramTops;
use crate::sp3::ccx;
use crate::sp3::df::topology::{self, Topology};
use crate::sp3::df::{self as sp3_df, Df};
use crate::sp3::dxio::DxioPolicy;
use crate::sp3::dxio::lanes::Engine;
use crate::sp3::fch;
use crate::sp3::fch::common::env::EnvPolicy;
use crate::sp3::nbio::{Nbio, NbioPolicy};
use crate::sp3::topology::MAX_ROOT_BRIDGES;
use fstart_arch::x86::mp::MpHandle;
use fstart_arch::x86::psp_reset::ApRoute;
use fstart_core::description::PciRootInfo;

use super::hw::{Pci, PrimaryFch, Tsc};
use super::{Generation, Identity};

/// Silicon policy of a board (NATIVE_PORT_PLAN §4 steps 6–12).
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// DF resource planner: the generation's split plus bus/IO requests.
    pub df: crate::df::plan::Policy,
    pub nbio: NbioPolicy,
    /// SMU PPTable inputs (Rome/Milan; Naples has no PPTable).
    #[cfg(any(feature = "rome", feature = "milan"))]
    pub power: crate::smu::v11::pptable::PowerPolicy,
    pub ccx: ccx::Policy,
    /// FCH env: power-failure state after AC loss.
    pub fch_env: EnvPolicy,
    /// Per-socket AHCI policy, used consistently at reset, env and late.
    /// A secondary socket with SATA lanes must not inherit the donor's
    /// all-disabled secondary-controller policy.
    #[cfg(any(feature = "rome", feature = "milan"))]
    pub sata: [crate::sp3::fch::huashan::sata::SataPolicy; 2],
    pub dxio: DxioPolicy,
}

/// The SMU-served CPPC PCC region (`smu::v11::cppc`).
#[cfg(any(feature = "rome", feature = "milan"))]
pub struct Cppc {
    pub base: u64,
    pub smu_bar: u64,
    pub info: crate::smu::v11::CppcInfo,
    pub lowest_mhz: u32,
    pub nominal_mhz: u32,
    /// APIC ID of each local PCC slot (this socket's PPTable CPPC map).
    pub slots: Vec<u32>,
}

/// NUMA domains as ACPI needs them.
pub struct NumaPlan {
    /// Socket of each proximity domain.
    pub domains: Vec<u8>,
    /// DRAM `(base, end, domain)`.
    pub memory: Vec<(u64, u64, u32)>,
    /// Domain of each thread, in [`Ctx::apic_ids`] order.
    pub cpus: Vec<u32>,
    /// Domain of each root bridge, in topology order.
    pub root_bridges: Vec<u32>,
}

/// Bump allocator over a root bridge's non-PCI MMIO pool (below 4 GiB).
/// Allocations are naturally aligned (every NBIO BAR is aligned to its
/// size) and taken from the top down, keeping the pool base free for the
/// larger ones that follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonPci {
    base: u64,
    top: u64,
}

impl NonPci {
    pub const fn new(pool: Window) -> Self {
        Self {
            base: pool.base,
            top: pool.base + pool.size,
        }
    }

    /// The non-PCI pool below 4 GiB of `rb`.
    pub const fn of(rb: &RootBridgePlan) -> Self {
        Self::new(rb.below_pools.non_pci)
    }

    /// `size` bytes aligned to `size` (rounded up to a power of two).
    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        let align = size.checked_next_power_of_two()?;
        let base = self.top.checked_sub(size)? & !(align - 1);
        (base >= self.base && size != 0).then(|| {
            self.top = base;
            base
        })
    }
}

/// What the steps work on and hand on.
pub struct Ctx {
    pub generation: Generation,
    pub identity: Identity,
    pub timer: Tsc,
    pub pci: Pci,
    /// The PCI segment, as the allocator, ACPI and the payload see it.
    pub pci_root: PciRootInfo,
    pub fch: PrimaryFch,
    pub resume: bool,
    pub policy: Policy,
    pub apob: Apob<'static>,
    pub tops: DramTops,
    pub df: Df<Pci>,
    pub topology: Topology,
    pub plan: Plan,
    /// Non-PCI MMIO of each root bridge, physical order.
    pub non_pci: heapless::Vec<NonPci, MAX_ROOT_BRIDGES>,
    pub phys_addr_bits: u8,
    /// Socket 0's (Naples: die 0's) PSP BAR.
    pub psp_bar: u64,
    /// APIC IDs of every enabled thread, BSP first (MADT order).
    pub apic_ids: Vec<u32>,
    /// CPPC through the SMU, when the SMU reports it enabled.
    #[cfg(any(feature = "rome", feature = "milan"))]
    pub cppc: Vec<Cppc>,
    /// An SMM handler is installed and locked.
    pub smm_installed: bool,
}

impl Ctx {
    /// Before the first step: the identified CPU, the handles, the board's
    /// policy and DRAM as the PSP left it. The fabric is not discovered
    /// yet.
    pub fn new(
        identity: Identity,
        timer: Tsc,
        pci_root: PciRootInfo,
        fch: PrimaryFch,
        policy: Policy,
        apob: Apob<'static>,
        tops: DramTops,
    ) -> Self {
        let generation = identity.generation();
        Self {
            generation,
            identity,
            timer,
            pci: Pci::default(),
            pci_root,
            fch,
            resume: false,
            policy,
            apob,
            tops,
            df: Df::new(Pci::default()),
            topology: Topology {
                generation,
                sockets: 0,
                nodes: heapless::Vec::new(),
                root_bridges: heapless::Vec::new(),
            },
            plan: Plan {
                mmio: heapless::Vec::new(),
                io: heapless::Vec::new(),
                root_bridges: heapless::Vec::new(),
                above_ecam_too_small: false,
                below_ecam_too_small: false,
                above_ecam: 0,
            },
            non_pci: heapless::Vec::new(),
            phys_addr_bits: 0,
            psp_bar: 0,
            apic_ids: Vec::new(),
            #[cfg(any(feature = "rome", feature = "milan"))]
            cppc: Vec::new(),
            smm_installed: false,
        }
    }
}

/// The silicon steps of one family, in flow order: [`super::naples`] (DF2,
/// an NBIO and SMU V9 per die) and [`super::rome_milan`] (DF3, SMU V11 per
/// IOD).
pub trait Steps: Sized {
    /// The DF version's rule layout.
    type DfLayout: Version;
    fn df_init(ctx: &mut Ctx) -> Result<(), &'static str>;
    fn nbio_early(ctx: &mut Ctx) -> Result<Self, &'static str>;
    fn smu_init(ctx: &mut Ctx, si: &mut Self) -> Result<(), &'static str>;
    fn fch_secondaries(ctx: &mut Ctx, si: &Self) -> Result<(), &'static str>;
    fn dxio(
        ctx: &mut Ctx,
        si: &Self,
        engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
    ) -> Result<(), &'static str>;
    fn fch_env(ctx: &Ctx, si: &Self) -> Result<(), &'static str>;
    /// CCX on the BSP, the release of every thread into the flow's AP
    /// route (`route_aps` arms and parks it), CCX on every thread. The
    /// microcode patches are those of `microcode`.
    fn mp(
        ctx: &mut Ctx,
        si: &Self,
        max_cpus: u16,
        microcode: Option<&'static [u8]>,
        route_aps: &dyn Fn(ApRoute),
    ) -> Result<MpHandle, &'static str>;
    fn pci_pre(ctx: &mut Ctx, si: &mut Self) -> Result<(), &'static str>;
    fn pci_post(ctx: &Ctx, si: &Self) -> Result<(), &'static str>;
    fn fch_late(ctx: &Ctx, si: &Self) -> Result<(), &'static str>;
    fn usb_buses(ctx: &Ctx, si: &Self) -> Result<[u8; 8], &'static str>;
    fn usb_bus_count(ctx: &Ctx) -> u8;
    /// NUMA domains for SRAT/SLIT/MSCT and `_PXM`; `None` with one domain.
    fn numa(ctx: &Ctx, si: &Self) -> Option<NumaPlan>;
    /// Every present NBIO, physical order.
    fn nbios(&self) -> &[Nbio<Pci>];
}

/// A step's result with the step's name as its error.
pub(super) trait Step<T> {
    fn step(self, name: &'static str) -> Result<T, &'static str>;
}
impl<T, E> Step<T> for Result<T, E> {
    fn step(self, name: &'static str) -> Result<T, &'static str> {
        self.map_err(|_| name)
    }
}
impl<T> Step<T> for Option<T> {
    fn step(self, name: &'static str) -> Result<T, &'static str> {
        self.ok_or(name)
    }
}

/// DXIO training, with the error's details logged.
pub(super) fn trained(result: Result<(), crate::dxio::DxioError>) -> Result<(), &'static str> {
    result.map_err(|e| {
        let (what, a, b) = e.parts();
        fstart_log::error!("sp3: DXIO error {} ({:#x}, {:#x})", what, a, b);
        "DXIO training"
    })
}

/// The microcode patches of `blob`, the verified FFS's copy (the patch
/// loader reads them at their address).
pub(super) fn microcode_candidates(blob: Option<&'static [u8]>) -> Vec<&'static [u8]> {
    blob.map(|blob| {
        fstart_amdfw_format::microcode::patches(blob)
            .map_while(Result::ok)
            .collect()
    })
    .unwrap_or_default()
}

/// Physically contiguous, identity-mapped DRAM from the stage heap (inside
/// the reserved early region, so it stays the firmware's after handoff):
/// DMA buffers and tables the SMU/PSP keep using.
pub fn dma_alloc(size: usize, align: usize) -> Result<&'static mut [u8], &'static str> {
    let layout = core::alloc::Layout::from_size_align(size, align).step("DMA layout")?;
    // SAFETY: nonzero size; never freed; zeroed before use.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return Err("DMA allocation");
    }
    // SAFETY: fresh allocation of `size` bytes.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr, size) })
}

/// The FCH IOAPIC (`FEC0_0000h`) gets the generation's ID (ACPI_SMM §3.2).
pub fn set_fch_ioapic_id(generation: Generation) {
    // SAFETY: the FCH IOAPIC is decoded at its fixed address.
    let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(0xfec0_0000) };
    ioapic.set_id(crate::sp3::nbio::ioapic::fch_ioapic_id(generation));
}

/// The eight PEI reset tasks on socket 0's FCH (FCH_SP3 §2.1). Returns
/// whether this is an S3 resume.
#[cfg_attr(not(any(feature = "rome", feature = "milan")), allow(unused_variables))]
pub fn fch_reset(
    fch: &PrimaryFch,
    generation: Generation,
    policy: &Policy,
) -> Result<bool, &'static str> {
    use fch::common::reset;
    reset::prelim(fch, generation);
    reset::ab_bar(fch);
    fch::spi::reset(fch, generation, &fch::spi::SpiPolicy::default());
    reset::hw_acpi(fch, generation);
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => fch::taishan::sata::reset(fch, true),
        #[cfg(any(feature = "rome", feature = "milan"))]
        Generation::Rome | Generation::Milan => {
            fch::huashan::sata::reset(fch, generation, &policy.sata[0])
        }
        #[allow(unreachable_patterns)]
        _ => return Err("FCH SATA reset"),
    }
    fch::lpc::reset(fch, generation);
    reset::espi(fch, false);
    Ok(reset::resume_sleep_type(fch) == 3)
}

/// DF topology, board bus counts, the MMIO/IO planner and its maps
/// (SOC_DF §3.3–3.4). Runs before NBIO: every NBIO access goes through a
/// root bus the DF routes.
pub fn data_fabric<S: Steps>(ctx: &mut Ctx) -> Result<(), &'static str> {
    let mut topology = topology::discover(&ctx.df, ctx.generation).step("DF topology")?;
    if let Some(ranges) =
        plan::plan_buses(&topology.root_bridges, &ctx.policy.df).step("DF bus plan")?
    {
        sp3_df::program::bus_maps::<S::DfLayout, _>(&ctx.df, &mut topology, &ranges);
    }
    ctx.phys_addr_bits = phys_addr_bits(ctx.generation, ctx.policy.ccx.smee);
    let plan = plan::plan(
        &plan::Input {
            tom: ctx.tops.tom,
            tom2: ctx.tops.tom2.unwrap_or(0),
            ecam_base: ctx.pci_root.ecam_base,
            ecam_size: ctx.pci_root.ecam_size(),
            phys_addr_bits: ctx.phys_addr_bits,
            root_bridges: &topology.root_bridges,
            mmio_pairs: <S::DfLayout as Version>::MMIO_PAIRS,
        },
        &ctx.policy.df,
    )
    .step("DF plan")?;
    // SAFETY: the BSP owns every node's DF during the silicon init.
    let nodes = unsafe { ctx.df.each(&topology.nodes) };
    program::mmio::<S::DfLayout, _>(&nodes, &plan.mmio);
    program::io::<S::DfLayout, _>(&nodes, &plan.io);
    program::vga::<S::DfLayout, _>(&nodes, topology.primary().fabric_id);
    for rb in &plan.root_bridges {
        fstart_log::info!(
            "df: rb {}.{} buses {:#x}-{:#x} mmio {:#x}+{:#x}",
            rb.socket,
            rb.index,
            rb.bus.base,
            rb.bus.limit,
            rb.below.base,
            rb.below.size
        );
    }
    ctx.non_pci = plan.root_bridges.iter().map(NonPci::of).collect();
    ctx.topology = topology;
    ctx.plan = plan;
    Ok(())
}

/// CPUID 8000_0008 EAX\[7:0\], minus the memory-encryption reduction
/// (8000_001F EBX\[11:6\]) where it applies: Rome always, Milan with SMEE
/// (SOC_DF §3.4).
fn phys_addr_bits(generation: Generation, smee: bool) -> u8 {
    let bits = fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff;
    let reduced = (fstart_arch::x86::cpuid(0x8000_001f).1 >> 6) & 0x3f;
    let reduce = match generation {
        Generation::Naples => false,
        Generation::Rome => true,
        Generation::Milan => smee,
    };
    (bits - if reduce { reduced } else { 0 }) as u8
}

/// FCH env (DXE) tasks on socket 0 (FCH_SP3 §2.2).
pub fn fch_env(ctx: &Ctx) {
    use fch::common::env;
    let (fch, generation) = (&ctx.fch, ctx.generation);
    env::hw_acpi_pre(fch, generation);
    env::hw_acpi(fch, generation, &ctx.policy.fch_env);
    fch::lpc::env(fch, generation);
    env::sd(fch, generation);
    fch::alink::clock_gating(
        fch,
        generation,
        &fch::alink::AbPolicy::default_for(generation),
    );
}

/// ReadyToBoot (SOC_DF §3.7, PSP_APOB §3.4, FCH_SP3 §2.3): FCH late
/// tasks, DF extended config off, PSP LockDFReg, Milan DRTM off and TPM
/// establishment reset, BootDone last.
pub fn ready_to_boot<S: Steps>(ctx: &Ctx, si: &S) -> Result<(), &'static str> {
    use crate::psp::command::{BootDone, C2pBuffer, LockDfReg};
    fch::common::late::hw_acpi(&ctx.fch, ctx.generation, &[]);
    S::fch_late(ctx, si)?;
    sp3_df::program::ready_to_boot(&ctx.df, &ctx.topology.nodes);
    let psp = psp(ctx)?;
    psp.send(&ctx.timer, &mut C2pBuffer::new(LockDfReg))
        .step("PSP LockDFReg")?;
    #[cfg(feature = "milan")]
    if ctx.generation == Generation::Milan {
        use crate::psp::command::{DisableDrtm, ResetTpmEstablishment};
        psp.send(&ctx.timer, &mut C2pBuffer::new(DisableDrtm::new()))
            .step("PSP DisableDrtm")?;
        // SAFETY: the TPM locality-0 page; all-ones means no TPM.
        let tpm = unsafe { fstart_core::mmio::read32(0xfed4_0000 as *const u32) };
        if tpm != u32::MAX {
            psp.send(&ctx.timer, &mut C2pBuffer::new(ResetTpmEstablishment))
                .step("PSP ResetTpmEstablishment")?;
        }
    }
    psp.send(&ctx.timer, &mut C2pBuffer::new(BootDone))
        .step("PSP BootDone")?;
    Ok(())
}

/// Socket 0's C2P mailbox through its PSP BAR.
pub fn psp(
    ctx: &Ctx,
) -> Result<crate::psp::mailbox::Mailbox<crate::psp::mailbox::MmioTransport>, &'static str> {
    use crate::psp::mailbox::{Mailbox, MmioTransport};
    if ctx.psp_bar == 0 {
        return Err("PSP BAR");
    }
    // SAFETY: the BAR NBIO programmed; the flow owns the mailbox.
    Ok(Mailbox::new(
        unsafe { MmioTransport::new(ctx.psp_bar) },
        ctx.generation.into(),
    ))
}
