//! What the SP3 SoCs' silicon steps share (Naples in
//! [`crate::naples::silicon`], Rome and Milan in [`crate::iod::silicon`])
//! and what they hand on: the board's silicon
//! policy, the fabric's topology and plan, the handles and the BARs.
//!
//! A boot flow runs the steps in the order they are declared in, on the
//! BSP. A step that fails returns its name.
extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::fch::FchHw;

use fstart_core::description::PciRootInfo;
use fstart_driver_amd::apob::Apob;
use fstart_driver_amd::ccx;
use fstart_driver_amd::df::plan::{self, Plan, RootBridgePlan, Window};
use fstart_driver_amd::df::topology::MAX_ROOT_BRIDGES;
use fstart_driver_amd::df::topology::{self, Topology};
use fstart_driver_amd::df::{self as sp3_df, Fabric};
use fstart_driver_amd::df::{Version, program};
use fstart_driver_amd::dxio::DxioPolicy;
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::fch;
use fstart_driver_amd::fch::tasks::env::EnvPolicy;
use fstart_driver_amd::nbio::NbioPolicy;
use fstart_driver_amd::nbio::iohc::DramTops;

use super::Identity;
use super::hw::{Pci, PrimaryFch, Tsc};
use fstart_driver_amd::cpu::Silicon;

/// Silicon policy of a board (NATIVE_PORT_PLAN §4 steps 6–12).
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// DF resource planner: the generation's split plus bus/IO requests.
    pub df: fstart_driver_amd::df::plan::Policy,
    pub nbio: NbioPolicy,
    /// SMU PPTable inputs (Rome/Milan; Naples has no PPTable).
    pub power: fstart_driver_amd::smu::v11::pptable::PowerPolicy,
    pub ccx: ccx::init::Policy,
    /// FCH env: power-failure state after AC loss.
    pub fch_env: EnvPolicy,
    /// Per-socket AHCI policy, used consistently at reset, env and late.
    /// A secondary socket with SATA lanes must not inherit the donor's
    /// all-disabled secondary-controller policy.
    pub sata: [fstart_driver_amd::fch::huashan::sata::SataPolicy; 2],
    pub dxio: DxioPolicy,
}

/// The SMU-served CPPC PCC region (`smu::v11::cppc`).
pub struct Cppc {
    pub base: u64,
    pub smu_bar: u64,
    pub info: fstart_driver_amd::smu::v11::CppcInfo,
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
    pub generation: Silicon,
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
    pub df: Fabric<Pci>,
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
    pub cppc: Vec<Cppc>,
    /// An SMM handler is installed and locked.
    pub smm_installed: bool,
    /// What the board attaches to its engines, with the bridge DXIO mapped
    /// each PCIe link to ([`fstart_driver_amd::dxio::ports::attached`]).
    pub ports: heapless::Vec<fstart_driver_amd::dxio::ports::AttachedPort, MAX_ENGINES>,
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
        let generation = identity.silicon();
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
            df: Fabric::new(Pci::default()),
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
            cppc: Vec::new(),
            smm_installed: false,
            ports: heapless::Vec::new(),
        }
    }
}

/// A step's result with the step's name as its error.
pub(crate) trait Step<T> {
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
pub(crate) fn trained(
    result: Result<(), fstart_driver_amd::dxio::DxioError>,
) -> Result<(), &'static str> {
    result.map_err(|e| {
        let (what, a, b) = e.parts();
        fstart_log::error!("sp3: DXIO error {} ({:#x}, {:#x})", what, a, b);
        "DXIO training"
    })
}

/// The microcode patches of `blob`, the verified FFS's copy (the patch
/// loader reads them at their address).
pub(crate) fn microcode_candidates(blob: Option<&'static [u8]>) -> Vec<&'static [u8]> {
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
    if size == 0 {
        return Err("DMA layout");
    }
    let layout = core::alloc::Layout::from_size_align(size, align).step("DMA layout")?;
    // SAFETY: the size is not zero; never freed; zeroed before use.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return Err("DMA allocation");
    }
    // SAFETY: fresh allocation of `size` bytes.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr, size) })
}

/// The FCH IOAPIC (`FEC0_0000h`) gets the generation's ID (ACPI_SMM §3.2).
pub fn set_fch_ioapic_id(generation: Silicon) {
    // SAFETY: the FCH IOAPIC is decoded at its fixed address.
    let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(0xfec0_0000) };
    ioapic.set_id(fstart_driver_amd::nbio::ioapic::fch_ioapic_id(generation));
}

/// The eight PEI reset tasks on socket 0's FCH (FCH_SP3 §2.1). Returns
/// whether this is an S3 resume.
pub fn fch_reset(
    fch: &PrimaryFch,
    generation: Silicon,
    policy: &Policy,
) -> Result<bool, &'static str> {
    use fch::tasks::reset;
    reset::prelim(fch, generation);
    reset::ab_bar(fch);
    fch::spi::reset(fch, generation, &fch::spi::SpiPolicy::default());
    reset::hw_acpi(fch, generation);
    match generation {
        Silicon::Zp => fstart_driver_amd::fch::taishan::sata::reset(
            &fch.smn(),
            &mut fstart_core::services::TimerDelay(fch.timer()),
        ),
        Silicon::Ssp | Silicon::Gn => {
            fstart_driver_amd::fch::huashan::sata::reset(fch, generation, &policy.sata[0])
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
pub fn data_fabric<V: Version>(ctx: &mut Ctx) -> Result<(), &'static str> {
    let mut topology = topology::discover(&ctx.df, ctx.generation).step("DF topology")?;
    if let Some(ranges) =
        plan::plan_buses(&topology.root_bridges, &ctx.policy.df).step("DF bus plan")?
    {
        sp3_df::program::bus_maps::<V, _>(&ctx.df, &mut topology, &ranges);
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
            mmio_pairs: V::MMIO_PAIRS,
        },
        &ctx.policy.df,
    )
    .step("DF plan")?;
    // SAFETY: the BSP owns every node's DF during the silicon init.
    let nodes = unsafe { ctx.df.each(&topology.nodes) };
    program::mmio::<V, _>(&nodes, &plan.mmio);
    program::io::<V, _>(&nodes, &plan.io);
    program::vga::<V, _>(&nodes, topology.primary().fabric_id);
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
fn phys_addr_bits(generation: Silicon, smee: bool) -> u8 {
    let bits = fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff;
    let reduced = (fstart_arch::x86::cpuid(0x8000_001f).1 >> 6) & 0x3f;
    let reduce = match generation {
        Silicon::Zp => false,
        Silicon::Ssp => true,
        Silicon::Gn => smee,
    };
    (bits - if reduce { reduced } else { 0 }) as u8
}

/// FCH env (DXE) tasks on socket 0 (FCH_SP3 §2.2).
pub fn fch_env(ctx: &Ctx) {
    use fch::tasks::env;
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
/// tasks (the SoC's `fch_late` after the common ones), DF extended config
/// off, PSP LockDFReg, Milan DRTM off and TPM establishment reset, BootDone
/// last.
pub fn ready_to_boot(
    ctx: &Ctx,
    fch_late: impl FnOnce() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    use fstart_driver_amd::psp::command::{BootDone, C2pBuffer, LockDfReg};
    fch::tasks::late::hw_acpi(&ctx.fch, ctx.generation, &[]);
    fch_late()?;
    sp3_df::program::ready_to_boot(&ctx.df, &ctx.topology.nodes);
    let psp = psp(ctx)?;
    psp.send(&ctx.timer, &mut C2pBuffer::new(LockDfReg))
        .step("PSP LockDFReg")?;
    if ctx.generation == Silicon::Gn {
        use fstart_driver_amd::psp::command::{DisableDrtm, ResetTpmEstablishment};
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
) -> Result<
    fstart_driver_amd::psp::mailbox::Mailbox<fstart_driver_amd::psp::mailbox::MmioTransport>,
    &'static str,
> {
    use fstart_driver_amd::psp::mailbox::{Mailbox, MmioTransport};
    if ctx.psp_bar == 0 {
        return Err("PSP BAR");
    }
    // SAFETY: the BAR NBIO programmed; the flow owns the mailbox.
    Ok(Mailbox::new(
        unsafe { MmioTransport::new(ctx.psp_bar) },
        ctx.generation.into(),
    ))
}

#[cfg(test)]
mod tests {
    use fstart_driver_amd::df::plan::Window;

    #[test]
    fn dma_alloc_rejects_an_empty_or_misaligned_request() {
        assert_eq!(super::dma_alloc(0, 0x1000).err(), Some("DMA layout"));
        assert_eq!(super::dma_alloc(0x1000, 3).err(), Some("DMA layout"));
        assert_eq!(super::dma_alloc(0x20, 0x10).unwrap().len(), 0x20);
    }

    #[test]
    fn mcfg_per_generation() {
        let m =
            fstart_acpi::build_mcfg(&[super::super::hw::segment(super::super::Generation::Naples)]);
        assert_eq!(&m[..4], b"MCFG");
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xF000_0000
        );
        assert_eq!(m[55], 127);
        let m =
            fstart_acpi::build_mcfg(&[super::super::hw::segment(super::super::Generation::Milan)]);
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xE000_0000
        );
        assert_eq!(m[55], 255);
    }

    #[test]
    fn non_pci_bars_are_aligned_and_fit() {
        let mut pool = super::NonPci::new(Window::new(0xaf00_0000, 0x0100_0000));
        let smu = pool.alloc(1 << 20).unwrap();
        let psp = pool.alloc(1 << 20).unwrap();
        let ioapic = pool.alloc(0x1000).unwrap();
        let iommu = pool.alloc(512 << 10).unwrap();
        assert_eq!(smu, 0xaff0_0000);
        assert_eq!(psp, 0xafe0_0000);
        assert_eq!(ioapic, 0xafdf_f000);
        assert_eq!(iommu, 0xafd0_0000, "aligned below the IOAPIC page");
        assert_eq!(
            super::NonPci::new(Window::new(0x1000, 0x1000)).alloc(0x2000),
            None
        );
        assert_eq!(pool.alloc(0), None);
    }
}
