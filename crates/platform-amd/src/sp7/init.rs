// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 / WH silicon bring-up on the shared AMD boot flow.
//!
//! | POST | Step |
//! |---|---|
//! | 20 | DFv4.5: sockets and IODs from the APOB, the ABL's bus maps, MMIO and I/O planned and written to every node, PIE hardware asserts |
//! | 30 | WH NBIO on every root bridge (early bus, VDM bus ranges, base/NBIF tables, 512 KiB IOMMU + L1/L2, IOAPIC), PSP BAR, Qilian FCH + secondary FCHs (2D: PSP recovery firmware, halt; 2E: a firmware warm reset refused; 2F: a firmware warm reset, `sp7::reset`) |
//! | 38 | WH MPIO v2 per IOD of every socket: ASK/DELI PCIe link training & PERST release |
//! | 3a | Qilian SMU USB init of the die-0 FCHs (socket 0 and 1), only with `Config::usb` |
//! | 50 | Zen 6 / Zen 6c MP release through each IOD, microcode, SMU v14 |
//! | 58 | SP7 SMM in 16 MiB TSEG (`sp7::smm`), PSP registration, RAS/EINJ |
//! | 60 | PCI enumeration, the board's soldered devices checked (required halt, expected warn) |
//! | 70 | ACPI (`SRAT`/`SLIT`/`MSCT`/`HMAT`/`IVRS`/`HEST`/`BERT`/`EINJ`/`WDAT`/`HSMP`/`CEDT`/`WSMT`), SMBIOS |

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use super::fabric::{self, MAX_NODES, MAX_ROOT_BRIDGES};
use super::numa::numa_ccd;
use super::{
    Bars, Config, DEFAULT_ECAM, FCH_IOAPIC_ID, IOAPIC_ID_BASE, SocketTopology, Sp7, TSEG_SIZE,
    Variant, place_bars,
};
use crate::flow::{Ctx, Mapped, Silicon as Up, Tables};
use crate::layout::Region;
use crate::nonpci::NonPci;
use crate::soc::{Entered, Fabric, Init};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::ioapic::IoApic;
use fstart_arch::x86::mp::{CpuIdentity, MpHandle};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::apob::{
    ApobError, sysmap,
    v2::{ApobV2, CoreMapV2},
};
use fstart_driver_amd::ccx::{self, mp::Fam17hCpu, tsc::Tsc, wh as cpu};
use fstart_driver_amd::df::dram::Nps;
use fstart_driver_amd::df::plan::RootBridgePlan;
use fstart_driver_amd::df::{BusRange, RootBridge, plan, wh as df_wh};
use fstart_driver_amd::dxio::wh as dxio_wh;
use fstart_driver_amd::fch::{FchHw, Native, qilian};
use fstart_driver_amd::nbio::{self, iohc, wh as nbio_wh};
use fstart_driver_amd::psp::{self, C2pTransport};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::{self, v14 as smu_v14};
use fstart_pci::{ConfigRegionAccess, PciAddress, PciEcam, ecam::GlobalEcam};
use heapless::Vec;

/// POST `code` and log the step (generic so non-SP7 boards do not link strings).
fn step<V: Variant>(code: u8, name: &str) {
    fstart_driver_amd::fch::post_native(code);
    fstart_log::info!("{}: [{:#x}] {}", V::NAME, code, name);
}

/// The handles of the SP7 flow.
pub struct Hw {
    pub fch: Native<Tsc>,
    pub clock: Tsc,
    pub revision: cpu::Revision,
    /// The ECAM window the CPU decodes (`MMIO_CFG_BASE`, the ABL's or the
    /// board's, [`ccx::msr::adopt_ecam`]).
    pub ecam: ccx::msr::EcamWindow,
    /// The last reset and the warm-reset guard, read before the takeover
    /// ([`super::reset::Boot`]).
    pub boot: super::reset::Boot,
    /// The validated APOB V2 in DRAM, from `flow::Soc::map` on: the one
    /// copy every step and the tables read (`Mapped::apob` is the RS/SP3
    /// format's, `None` here).
    pub apob: Option<ApobV2<'static>>,
}

impl Hw {
    /// The APOB V2 `map` validated. `#[inline]`: its string is in the
    /// SoC-generic `map` too, and `Hw` is not generic over the SoC.
    #[inline]
    pub(super) fn apob(&self) -> Result<ApobV2<'static>, &'static str> {
        self.apob.ok_or("APOB V2")
    }
}

/// PSP ACPI EINJ mailbox and command buffer discovered after `RasEinj::ENABLE`.
#[derive(Debug, Clone, Copy)]
struct Einj {
    mailbox: u64,
    buffer: u64,
    /// The PSP offers EINJv2 (`WhBuffer` `+0Ch` bit 31).
    v2: bool,
}

/// What the silicon steps hand on.
pub struct State {
    topology: df_wh::Topology,
    devices: &'static df_wh::Devices,
    /// The IOMs of the vendor's topology map (socket 0 die 0, harvested
    /// ones left out, [`df_wh::Node::ioms`]).
    ioms: Vec<u8, 8>,
    /// Live DF nodes `(socket, die)`, socket-major.
    nodes: Vec<(u8, u8), MAX_NODES>,
    /// Root bridges in system order ([`fabric`]).
    roots: Vec<RootBridge, MAX_ROOT_BRIDGES>,
    plan: fabric::Plan,
    /// By system root bridge index.
    bars: [Bars; MAX_ROOT_BRIDGES],
    /// What is left of each root bridge's non-PCI pool after its NBIO BARs,
    /// by system root bridge index: the CXL steps take from it.
    non_pci: [NonPci; MAX_ROOT_BRIDGES],
    iommus: [Option<nbio::iommu::Iommu>; MAX_ROOT_BRIDGES],
    /// Verified `(socket, die, acpi_mmio_base)` of relocated secondary FCHs.
    secondary_fchs: Vec<(u8, u8, u32), { MAX_NODES - 1 }>,
    /// `(socket, first lane, root port, link up)` of MPIO and ABL-trained ports.
    lanes: alloc::vec::Vec<(u8, u8, PciAddress, bool)>,
    /// `(socket, binding)` of every verified root port.
    bindings: alloc::vec::Vec<(u8, dxio_wh::Binding)>,
    /// `(socket, die, core map)` of every IOD with cores.
    ccds: alloc::vec::Vec<(u8, u8, CoreMapV2)>,
    launch: alloc::vec::Vec<cpu::Launch>,
    /// CPUID `0x8000_0026` socket shift of the APIC IDs.
    socket_shift: u8,
    apob_nps: Option<Nps>,
    ecc_capable: bool,
    psp_bar: u32,
    psp: Option<psp::boot::Psp>,
    einj: Option<Einj>,
    /// What the CXL steps found ([`super::cxl`]).
    cxl: super::cxl::Cxl,
    /// The last boot's platform errors for the BERT ([`super::ras`]).
    platform_errors: alloc::vec::Vec<alloc::vec::Vec<fstart_acpi::cper::Entry>>,
    /// The SMU feature words `SmuInitialization` sent ([`super::smu`]).
    smu_features: Option<fstart_driver_amd::smu::v14::features::Controls>,
}

impl State {
    pub(crate) fn sockets(&self) -> u8 {
        self.topology.sockets
    }

    /// Live DF nodes `(socket, die)`, socket-major.
    pub(crate) fn nodes(&self) -> &[(u8, u8)] {
        &self.nodes
    }

    /// The IODs' DF device table.
    pub(crate) fn devices(&self) -> &'static df_wh::Devices {
        self.devices
    }

    /// The IOMs of the vendor's topology map ([`df_wh::Node::ioms`]).
    pub(crate) fn ioms(&self) -> &[u8] {
        &self.ioms
    }

    /// Root bridges in system order.
    pub(crate) fn roots(&self) -> &[RootBridge] {
        &self.roots
    }

    /// System index of root bridge `index` on `socket`.
    pub(crate) fn system_index(&self, socket: u8, index: u8) -> Option<usize> {
        self.roots
            .iter()
            .position(|rb| (rb.socket, rb.index) == (socket, index))
    }

    pub(crate) fn bars(&self, socket: u8, index: u8) -> Bars {
        self.system_index(socket, index)
            .map(|i| self.bars[i])
            .unwrap_or_default()
    }

    /// The non-PCI pool of root bridge `index` on `socket` after its NBIO
    /// BARs ([`fabric::place_bars`]).
    pub(crate) fn non_pci(&self, socket: u8, index: u8) -> Option<NonPci> {
        Some(self.non_pci[self.system_index(socket, index)?])
    }

    pub(crate) fn iommu(&self, socket: u8, index: u8) -> Option<nbio::iommu::Iommu> {
        self.iommus[self.system_index(socket, index)?]
    }

    /// The APIC mode ([`cpu::apic_mode`]) of the board's policy, with the
    /// IOMMUs up and the launch list's highest APIC ID (the BSP's is 0):
    /// x2APIC by default, xAPIC without an IOMMU.
    /// The platform errors of the last boot ([`super::ras::previous_boot`]).
    pub(crate) fn platform_errors(&self) -> &[alloc::vec::Vec<fstart_acpi::cper::Entry>] {
        &self.platform_errors
    }

    /// `EnableIommuMmio` at AmdNbioDxe's entry: the decode of every IOMMU
    /// `init_fch` placed, then what IVRS and the APIC mode read of it;
    /// with `vf` its SR-IOV BARs from the root bridge's non-PCI pool.
    fn enable_iommus<V: Variant>(&mut self, vf: bool) {
        use nbio_wh::bars::{IOMMU_VF_BAR_SIZE, IOMMU_VF_CONTROL_BAR_SIZE};
        let part = super::nbio::part();
        for i in 0..self.roots.len() {
            let (socket, index) = (self.roots[i].socket, self.roots[i].index);
            let root = fabric::root_bus(self.roots[i].segment, self.roots[i].bus_base);
            let Some(base) = self.bars[i].iommu else {
                continue;
            };
            let Some(nb) = super::nbio::root_bridge(part, index, root) else {
                continue;
            };
            if nb.enable_iommu_mmio().is_err() {
                fstart_log::warn!("{}: S{} RB{} IOMMU MMIO off", V::NAME, socket, index);
                self.bars[i].iommu = None;
                continue;
            }
            // SAFETY: the IOMMU's BAR was verified below 4 GiB in
            // `init_fch` and its decode is on; identity-mapped uncached.
            self.iommus[i] = unsafe { nb.iommu(base as u32) };
            if vf {
                let pool = &mut self.non_pci[i];
                let vf_bar = pool.alloc_bottom(IOMMU_VF_BAR_SIZE, IOMMU_VF_BAR_SIZE);
                let control = pool.alloc_bottom(IOMMU_VF_CONTROL_BAR_SIZE, 1 << 20);
                let set =
                    vf_bar.is_some_and(|vf_bar| nb.set_iommu_vf_bars(vf_bar, control).is_ok());
                if !set {
                    fstart_log::warn!(
                        "{}: S{} RB{} IOMMU VF BARs not placed",
                        V::NAME,
                        socket,
                        index
                    );
                }
            }
        }
    }

    fn apic_mode(&self, config: &Config) -> Option<ccx::x2apic::ApicMode> {
        let iommu = self.iommus.iter().any(Option::is_some);
        let max = self.launch.iter().map(|ap| ap.apic_id).max().unwrap_or(0);
        cpu::apic_mode(config.ccx.apic, iommu, max)
    }

    pub(crate) fn secondary_fchs(&self) -> &[(u8, u8, u32)] {
        &self.secondary_fchs
    }

    /// The bit of the socket in an APIC ID.
    pub(crate) fn socket_shift(&self) -> u8 {
        self.socket_shift
    }

    pub(crate) fn apob_nps(&self) -> Option<Nps> {
        self.apob_nps
    }

    pub(crate) fn ecc_capable(&self) -> bool {
        self.ecc_capable
    }

    /// `(socket, binding)` of every verified root port.
    pub(crate) fn bindings(&self) -> &[(u8, dxio_wh::Binding)] {
        &self.bindings
    }

    /// The DF plan of the root bridges' windows.
    pub(crate) fn plan(&self) -> &fabric::Plan {
        &self.plan
    }

    /// The plan before enumeration, for the windows the CXL 1.1 devices
    /// take off their root bridges ([`super::cxl_memory`]).
    pub(super) fn plan_mut(&mut self) -> &mut fabric::Plan {
        &mut self.plan
    }

    /// The PSP's mailbox session, while it lasts.
    pub(super) fn psp_mut(&mut self) -> Option<&mut psp::boot::Psp> {
        self.psp.as_mut()
    }

    /// The board lane of the root port `port` on `socket`.
    pub(crate) fn lane_of(&self, socket: u8, port: PciAddress) -> Option<u8> {
        self.lanes
            .iter()
            .find(|&&(s, _, p, _)| (s, p) == (socket, port))
            .map(|&(_, lane, _, _)| lane)
    }

    /// What the CXL steps found.
    pub(crate) fn cxl(&self) -> &super::cxl::Cxl {
        &self.cxl
    }

    /// The SMU feature words of `SmuInitialization`, if the SMU answered.
    pub(crate) fn smu_features(&self) -> Option<fstart_driver_amd::smu::v14::features::Controls> {
        self.smu_features
    }

    /// `(socket, socket CCD)` for every logical CPU in MP order (BSP first,
    /// then APs in launch order) for SRAT/SLIT NUMA domain assignment, with
    /// the vendor's CCD remapping ([`numa_ccd`]).
    pub(crate) fn thread_ccds(&self) -> alloc::vec::Vec<(u8, u8)> {
        let place = |socket: u8, die: u8, ccd: u8| {
            let die_ccds = self
                .ccds
                .iter()
                .find(|(s, d, _)| (*s, *d) == (socket, die))
                .map_or(0, |(_, _, map)| {
                    map.ccds.iter().fold(0u8, |m, c| m | 1 << (c.id % 4))
                });
            (socket, numa_ccd(die, ccd, die_ccds))
        };
        let bsp = self
            .ccds
            .first()
            .and_then(|(s, d, map)| map.ccds.first().map(|ccd| place(*s, *d, ccd.id)));
        bsp.into_iter()
            .chain(
                self.launch
                    .iter()
                    .map(|ap| place(ap.socket, ap.die, ap.phys_ccd)),
            )
            .collect()
    }

    /// Root port BDF of `lane` on `socket`, if its link is up.
    pub(crate) fn port_on(&self, socket: u8, lane: u8) -> Option<PciAddress> {
        self.lanes
            .iter()
            .find(|&&(s, l, _, up)| (s, l) == (socket, lane) && up)
            .map(|&(_, _, port, _)| port)
    }

    /// Root bus (segment and bus) that reaches the SMN of IOD `(socket,
    /// die)` (the vendor's GnbHandle of `iod == socket * 2 + die`, through
    /// `(seg << 28) | (bus << 20)`): its FCH root bridge, else its lowest
    /// routed one.
    pub(crate) fn node_root(&self, socket: u8, die: u8) -> Option<PciAddress> {
        self.roots
            .iter()
            .filter(|rb| rb.socket == socket && fabric::rb_location(rb.index).0 == die)
            .min_by_key(|rb| rb.index)
            .map(|rb| fabric::root_bus(rb.segment, rb.bus_base))
    }

    /// The root bus of root bridge `index` on `socket`.
    pub(crate) fn rb_root(&self, socket: u8, index: u8) -> Option<PciAddress> {
        self.roots
            .iter()
            .find(|rb| (rb.socket, rb.index) == (socket, index))
            .map(|rb| fabric::root_bus(rb.segment, rb.bus_base))
    }

    /// SMU v14 mailbox handle of IOD `(socket, die)`.
    pub(super) fn smu_on(&self, socket: u8, die: u8, clock: Tsc) -> smu::Smu<GlobalEcam, Tsc> {
        let root = self
            .node_root(socket, die)
            .unwrap_or_else(|| fabric::root_bus(0, 0));
        // SAFETY: BSP owns SMN on the IOD's root bus during cold init.
        smu::Smu::new(
            unsafe { Smn::on_root(GlobalEcam, root) },
            smu_v14::MAILBOX,
            clock,
            smu_v14::ID_LIMIT,
        )
    }

    pub(super) fn smu(&self, clock: Tsc) -> smu::Smu<GlobalEcam, Tsc> {
        self.smu_on(0, 0, clock)
    }
}

/// WH NBIO IOAPIC ID of the system's `index`-th root bridge.
pub(super) const fn ioapic_id(index: usize) -> Option<u8> {
    nbio_wh::ioapic_id(IOAPIC_ID_BASE, index)
}

/// DF node `(socket, die)` answers at `00:(0x18 + socket * 2 + die).0` with AMD's vendor ID.
fn node_present(socket: u8, die: u8) -> bool {
    df_wh::node_address(socket, die, 0).is_some_and(|addr| {
        // SAFETY: PCI bus 0 ECAM is active; reading Vendor/Device ID is side-effect free.
        let id = unsafe { GlobalEcam.read(addr, 0) };
        id & 0xffff == 0x1022
    })
}

/// Rewrite the bus maps when the board asks for other bus counts or the
/// ABL's `routes` reach past the ECAM window (`FabricRcInitWh`, 2P_COMPLETE
/// §2.10): planned by [`df_wh::buses::plan`] in the APOB's `9/0x2C`
/// root bridge order, written to every node ([`df_wh::program_buses`]) and
/// read back. `Ok(None)`: the ABL's maps stay. Runs before NBIO, MPIO and
/// the SMU address any IOD by bus, after the MMIO and I/O maps' fabric
/// IDs are known: no reset.
fn bus_plan<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    routes: &[df_wh::BusRoute],
    ios: &[df_wh::Ios],
    nodes: &[(u8, u8)],
    devices: &df_wh::Devices,
    request: Option<&super::BusRequest>,
) -> Result<Option<Vec<df_wh::BusRoute, { df_wh::CONFIG_PAIRS as usize }>>, &'static str> {
    let apob = ctx.hw.apob()?;
    let order = apob.root_bridge_order().ok();
    let (rbs, fch) = fabric::bus_requests(
        routes,
        ios,
        nodes,
        devices.ios.count,
        order.as_deref(),
        request,
    )
    .ok_or("DFv4.5 bus plan input")?;
    let capacity = ctx.hw.ecam.buses();
    let Some(plan) = df_wh::buses::plan(&rbs, fch, capacity).map_err(|e| e.why())? else {
        return Ok(None);
    };
    fstart_log::info!("{}: Current/Target PCI Bus Assignment", V::NAME);
    for assign in &plan {
        let live = rbs
            .iter()
            .find(|r| r.fabric_id == assign.fabric_id)
            .and_then(|r| r.live);
        let global = assign.global_base();
        fstart_log::info!(
            "{}: S{} IOD{} RB{}: {:?} -> {:#x}-{:#x}",
            V::NAME,
            assign.socket,
            assign.die,
            assign.rb,
            live,
            global,
            global + u16::from(assign.buses.limit - assign.buses.base)
        );
    }
    let mut handles: Vec<df_wh::Node<GlobalEcam>, MAX_NODES> = Vec::new();
    for &(socket, die) in nodes {
        // SAFETY: live DF node, BSP owns its indirect registers; nothing
        // addresses an IOD by bus yet.
        let node = unsafe { df_wh::Node::new(GlobalEcam, socket, die) }.ok_or("DF node")?;
        let _ = handles.push(node);
    }
    df_wh::program_buses(&handles, devices, &plan).ok_or("DFv4.5 bus plan")?;
    let written = handles[0].bus_routes().map_err(|_| "DFv4.5 bus maps")?;
    if written.len() != plan.len() {
        return Err("DFv4.5 bus plan");
    }
    Ok(Some(written))
}

/// One-shot guards enforcing exclusive mutable access to static boot buffers.
static PSP_STORAGE_CLAIMED: AtomicBool = AtomicBool::new(false);
static mut PSP_STORAGE: psp::Storage = psp::Storage::new();

/// Open the PSP mailbox at `bar` at most once per boot (generic over `V` so
/// error strings stay scoped to SP7 boards).
fn psp_session<V: Variant>(bar: u32) -> Result<psp::boot::Psp, &'static str> {
    if PSP_STORAGE_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err("PSP re-entry");
    }
    // SAFETY: mapped below 4 GiB by `init_fch`; BSP owns the normal-mode mailbox.
    let mailbox = unsafe { psp::Mailbox::mmio(bar.into(), psp::Variant::Wh) };
    // SAFETY: guarded by `PSP_STORAGE_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(PSP_STORAGE) };
    psp::Session::new(mailbox, storage).map_err(|_| {
        fstart_log::error!("{}: PSP session failed", V::NAME);
        "PSP"
    })
}

/// SMM report shared between the SMM handler and normal mode.
static mut SMM_REPORT: super::smm::Report = super::smm::Report::NONE;

impl<V: Variant> Init for Sp7<V> {
    const NAME: &'static str = V::NAME;

    type Config = Config;
    type Identity = cpu::Revision;
    type Hw = Hw;
    /// Static board PCIe lane table.
    type Lanes<'a> = &'a [SocketTopology<'a>];
    type Runtime = ();
    type FchUp = State;
    type Trained = State;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, cpu::Threads>;
    type State = State;

    fn enter(config: &Config, msr: &NativeMsr) -> Entered<Self> {
        fstart_driver_amd::fch::post_native(0x10);
        // The ABL's window if it enabled one, else the board's APCB value.
        let ecam = ccx::msr::adopt_ecam(msr, config.ecam.unwrap_or(DEFAULT_ECAM));
        fstart_pci::ecam::init_segments(ecam.base as usize, ecam.segments.into());
        // SAFETY: exclusive FCH programming before console.
        unsafe { fstart_driver_amd::fch::acpimmio::enable_acpimmio() };
        let clock = Tsc::measured(msr);
        // SAFETY: BSP only, ACPI MMIO and ECAM decoded.
        let fch = unsafe { Native::new(Tsc::native(msr)) };
        let boot = super::reset::Boot::read(&fch);
        qilian::take_over(&fch);
        let signature = CpuIdentity::current().signature;
        let revision = cpu::Revision::from_signature(signature)
            .filter(|_| V::SIGNATURES.contains(&(signature & !0xf)));
        let fallback = cpu::Revision {
            signature,
            model: cpu::MODEL_MIN,
            stepping: 0,
        };
        Entered {
            hw: Hw {
                fch,
                clock: clock.unwrap_or_else(|| Tsc::native(msr)),
                revision: revision.unwrap_or(fallback),
                ecam,
                boot,
                apob: None,
            },
            clock,
            pre_init: revision.map(drop).ok_or("WH CPU revision"),
        }
    }

    fn identify(hw: &Hw, _: &Config, _: Option<Tsc>) -> Result<cpu::Revision, &'static str> {
        fstart_log::info!(
            "{}: WH model {:#x} stepping {:#x} ({:#x}), TSC {} MHz",
            V::NAME,
            hw.revision.model,
            hw.revision.stepping,
            hw.revision.signature,
            hw.clock.mhz()
        );
        fstart_log::info!(
            "{}: ECAM {:#x}, {} segment(s)",
            V::NAME,
            hw.ecam.base,
            hw.ecam.segments
        );
        super::reset::log::<V>(&hw.boot);
        Ok(hw.revision)
    }

    fn pci_segment(hw: &Hw) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: hw.ecam.base,
            bus_start: 0,
            bus_end: 0xff,
        }
    }

    fn pci_segments(hw: &Hw) -> crate::soc::Segments {
        crate::soc::segments(Self::pci_segment(hw), hw.ecam.segments.into())
    }

    fn psp(_: &Hw) -> psp::Variant {
        psp::Variant::Wh
    }

    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State, &'static str> {
        let config = ctx.config;
        let fch = &ctx.hw.fch;
        step::<V>(0x20, "DFv4.5");
        let apob = ctx.hw.apob()?;
        super::reset::log_apob::<V>(&apob);
        if !node_present(0, 0) {
            return Err("DF node0 absent");
        }
        // SAFETY: BSP owns DF indirect registers on node (0, 0) during cold init.
        let node0 = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }.ok_or("DF node0")?;
        // Sockets and IODs come from the APOB (0xC/0x22); F4x180[8], which
        // the vendor never reads, only cross-checks.
        let counts = apob.fabric_topology().ok();
        let topology = df_wh::Topology::new(
            counts.map_or(1, |c| c.cpu_sockets),
            counts.map(|c| c.cpu_nodes),
            node0.total_dies(),
        )
        .ok_or("DFv4.5 topology")?;
        if (topology.sockets > 1) != node0.other_socket_present() {
            fstart_log::warn!(
                "{}: APOB {} socket(s), DF other socket present {}",
                V::NAME,
                topology.sockets,
                node0.other_socket_present()
            );
            if counts.is_none() {
                return Err("DFv4.5 socket count");
            }
        }
        // fstart reads a fabric ID's socket at bit `SOCKET_SHIFT`, as AMD's
        // code does; Dell's reads F4x1B4/F4x1B8 (2P_COMPLETE §2.15).
        let ids = node0.socket_id_layout();
        if topology.sockets > 1 && !ids.is_wh() {
            fstart_log::warn!(
                "{}: DF fabric-ID socket layout (shift {}, mask {:#x}) is not the one assumed",
                V::NAME,
                ids.socket_shift,
                ids.socket_mask
            );
        }
        let nodes: Vec<(u8, u8), MAX_NODES> = topology.nodes().collect();
        if let Some(&(socket, die)) = nodes.iter().find(|&&(s, d)| !node_present(s, d)) {
            fstart_log::error!("{}: DF node {}.{} absent", V::NAME, socket, die);
            return Err("DF node absent");
        }
        // One CPUID decode for the DF, NBIO and MPIO choices.
        let wh_part = super::nbio::part();
        let devices = wh_part.devices();
        fstart_log::info!(
            "{}: {} socket(s) x {} IOD(s), DF device table {:#x}",
            V::NAME,
            topology.sockets,
            topology.dies_per_socket,
            devices.table
        );
        // Socket 0's IOS name every socket's root bridges.
        let mut ios: Vec<df_wh::Ios, 8> = Vec::new();
        for &(_, die) in nodes.iter().filter(|&&(socket, _)| socket == 0) {
            // SAFETY: live DF node, BSP owns its indirect registers.
            let node = unsafe { df_wh::Node::new(GlobalEcam, 0, die) }.ok_or("DF node")?;
            let mut found = node.ios(devices);
            if found.is_empty() {
                fstart_log::warn!("{}: IOD{} names no IOS, assuming fabric IDs", V::NAME, die);
                found = fabric::assumed_ios(devices, die);
            }
            // The ABL's IOHC harvest (APOB 0xA/0x78), socket 0's for every socket.
            fabric::drop_harvested(&mut found, apob.iohc_harvest(die).unwrap_or(0));
            ios.extend_from_slice(&found).map_err(|_| "DFv4.5 IOS")?;
        }
        // The IOMs the vendor's fabric topology map lists for socket 0's
        // die 0 (the harvest of its IOHCs left out), the list its MMIO-gap
        // patch writes on every node and its NUMA code reads the DRAM map of.
        let ioms = {
            // SAFETY: live DF node, BSP owns its indirect registers.
            let node = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }.ok_or("DF node")?;
            node.ioms(devices, apob.iohc_harvest(0).unwrap_or(0))
        };
        // The ABL's bus maps are used as they are; every node must have them.
        let routes = node0.bus_routes().map_err(|_| "DFv4.5 bus maps")?;
        for &(socket, die) in nodes.iter().skip(1) {
            // SAFETY: live DF node, BSP owns its indirect registers.
            let node = unsafe { df_wh::Node::new(GlobalEcam, socket, die) }.ok_or("DF node")?;
            let other = node.bus_routes().map_err(|_| "DF bus maps")?;
            if other.len() != routes.len() || !routes.iter().all(|route| other.contains(route)) {
                return Err("DF inconsistent IOD bus maps");
            }
        }
        // A board's bus request, or an ABL map the ECAM does not decode
        // whole, is planned and written now, before anything addresses an
        // IOD by bus (FabricRcInitWh; no reset needed this early).
        let routes = bus_plan::<V>(ctx, &routes, &ios, &nodes, devices, ctx.config.bus_request)?
            .unwrap_or(routes);
        let ecam = ctx.hw.ecam;
        let roots = fabric::resolve_roots(&routes, &ios, &nodes, ecam.segments.into())
            .map_err(|e| e.why())?;
        let tops = ccx::msr::memory_limits(ctx.msr).map_err(|_| "TOP_MEM")?;
        let plan: fabric::Plan = plan::plan_for(
            &plan::Input {
                tom: tops.0,
                tom2: tops.1.unwrap_or(0),
                ecam_base: ecam.base,
                ecam_size: ecam.size(),
                phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
                root_bridges: &roots,
                mmio_pairs: df_wh::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .map_err(|_| "DF plan")?;

        // Every node routes MMIO and I/O alike (the vendor writes each pair
        // to every socket and die); then the PIE's latched hardware asserts
        // are reported and cleared (Df4WhPieRasInit, TP1).
        for &(socket, die) in &nodes {
            // SAFETY: BSP owns DF routing tables on live node (socket, die).
            let node = unsafe { df_wh::Node::new(GlobalEcam, socket, die) }.ok_or("DF node")?;
            node.program(&plan.mmio, &plan.io).ok_or("DF plan pairs")?;
            // B0: the IOMs decode the MMIO space without gaps.
            if devices.table == df_wh::devices::B0.table {
                node.patch_mmio_gaps(&ioms, tops.1.unwrap_or(0));
            }
            for hw_assert in node.clear_hw_asserts::<8>() {
                fstart_log::warn!(
                    "{}: DF {}.{} instance {:#x} hardware assert {:#x}",
                    V::NAME,
                    socket,
                    die,
                    hw_assert.instance,
                    hw_assert.status
                );
            }
        }

        step::<V>(0x30, "WH NBIO (IOAPIC, PSP BAR) & FCH");
        let nbio_part = wh_part;
        super::nbio::base_config::<V>(&plan.root_bridges, nbio_part, &config.nbio)?;
        // The IOMMU fuses stick within an IOD (`iommu_init`).
        let mut iommu_fuses = (u8::MAX, u8::MAX, nbio_wh::Fuses::default());
        let vdm = if nbio_part.a0() {
            nbio_wh::VDM_TABLE_A0
        } else {
            nbio_wh::VDM_TABLE
        };
        // The VDM table is 8-bit (InitVdmNodeBusRangesWh): segments are not
        // in it (2P_COMPLETE §2.8, open question 4).
        let buses: Vec<BusRange, MAX_ROOT_BRIDGES> =
            plan.root_bridges.iter().map(|rb| rb.bus).collect();
        let mut bars = [Bars::default(); MAX_ROOT_BRIDGES];
        let mut non_pci = [NonPci::default(); MAX_ROOT_BRIDGES];
        let iommus = [None; MAX_ROOT_BRIDGES];
        let mut secondary_fchs: Vec<(u8, u8, u32), { MAX_NODES - 1 }> = Vec::new();
        let mut psp_bar: Option<u32> = None;
        for (i, rb) in plan.root_bridges.iter().enumerate() {
            let (die, rb_on_iod) = fabric::rb_location(rb.index);
            let mut pool = NonPci::of(rb);
            let (mut rb_bars, maybe_psp) =
                place_bars(rb.socket, rb.index, &mut pool).ok_or("NBIO BAR placement")?;
            non_pci[i] = pool;
            let nb = super::nbio::root_bridge(
                nbio_part,
                rb.index,
                fabric::root_bus(rb.segment, rb.bus.base),
            )
            .ok_or("WH NBIO handle")?;
            // The socket's SMU window for HSMP where nothing decoded it
            // (`NonPciBarInitEx`): IOHC 0 of the socket.
            if config.devices.hsmp && (die, rb_on_iod) == (0, 0) && nb.smu_bar().is_none() {
                let size = nbio::iohc::SMU_BAR_SIZE;
                let placed = non_pci[i]
                    .alloc_bottom(size, size)
                    .filter(|base| base + size <= 1 << 32)
                    .is_some_and(|base| nb.set_smu_bar(base).is_ok());
                if !placed {
                    fstart_log::warn!("{}: S{} SMU BAR not placed", V::NAME, rb.socket);
                }
            }
            if (iommu_fuses.0, iommu_fuses.1) != (rb.socket, die) {
                iommu_fuses = (rb.socket, die, nbio_wh::Fuses::default());
            }
            let fuses = &mut iommu_fuses.2;
            // Every IOHC learns every root bridge's buses (InitVdmNodeBusRangesWh).
            // Fatal, where an IOMMU or IOAPIC that does not answer is only
            // left out: it fails only when the plan has more root bridges
            // than the table entries, a topology the vendor cannot boot
            // either, and an IOHC without the other nodes' ranges routes
            // their vendor-defined messages as its own.
            if !nb.set_vdm_bus_ranges(vdm, &buses, i) {
                return Err("NBIO VDM bus ranges");
            }
            if let Some(iommu_base) = rb_bars.iommu {
                // Its decode goes on at AmdNbioDxe's entry
                // ([`State::enable_iommus`]).
                let mapped = nb.map_iommu(iommu_base as u32);
                nb.iommu_init(nbio_part, &config.nbio, fuses);
                if !mapped {
                    rb_bars.iommu = None;
                    fstart_log::warn!(
                        "{}: S{} RB{} IOMMU BAR unavailable/unverified",
                        V::NAME,
                        rb.socket,
                        rb.index
                    );
                }
            } else {
                nb.iommu_init(nbio_part, &config.nbio, fuses);
            }
            // No IOHC TOM: the ABL owns it on Venice ([`super::nbio`]).
            if let Some(ioapic_base) = rb_bars.ioapic {
                let id = ioapic_id(i).ok_or("NBIO IOAPIC ID")?;
                if nb.init_ioapic(ioapic_base, id) {
                    // SAFETY: IOAPIC MMIO was just verified via SMN readback on this root bridge.
                    let ioapic = unsafe { IoApic::new(ioapic_base as usize) };
                    // All-ones MMIO reads 256 redirection entries; the ID
                    // cannot tell, `0xFF` is the 16th root bridge's.
                    if ioapic.redirection_entries() <= nbio::ioapic::NBIO_IOAPIC_PINS {
                        ioapic.set_id(id);
                        ioapic.mask_all();
                    } else {
                        rb_bars.ioapic = None;
                        fstart_log::warn!(
                            "{}: S{} RB{} IOAPIC MMIO read all-ones",
                            V::NAME,
                            rb.socket,
                            rb.index
                        );
                    }
                } else {
                    rb_bars.ioapic = None;
                    fstart_log::warn!(
                        "{}: S{} RB{} IOAPIC BAR rejected/locked elsewhere",
                        V::NAME,
                        rb.socket,
                        rb.index
                    );
                }
            }
            // Program the PSP BAR on (0, 0)'s FCH root bridge and verify C2P responds.
            if let Some(pbar) = maybe_psp {
                iohc::map_psp(&nb, pbar).map_err(|_| "PSP BAR")?;
                psp_bar = Some(pbar);
            }
            // Relocate the secondary Qilian FCH's ACPI MMIO on every other
            // IOD's FCH root bridge (`SMN 0x02D0_13D4`). One that fails is
            // left out (no init, no PERST release) and the boot goes on
            // ([`super::fch`]'s policy).
            if let Some(sec_fch) = rb_bars.secondary_fch {
                let relocated = df_wh::node_index(rb.socket, die)
                    .filter(|&fch| qilian::init_secondary_fch(nb.smn(), fch, sec_fch));
                match relocated {
                    Some(fch_index) => {
                        // One per IOD but (0, 0): `MAX_NODES - 1` at most.
                        let _ = secondary_fchs.push((rb.socket, die, sec_fch));
                        fstart_log::info!(
                            "{}: secondary Qilian FCH{} ACPI MMIO {:#x}",
                            V::NAME,
                            fch_index,
                            sec_fch
                        );
                    }
                    None => {
                        rb_bars.secondary_fch = None;
                        fstart_log::error!(
                            "{}: S{} IOD{} secondary FCH ACPI MMIO relocation failed: left out",
                            V::NAME,
                            rb.socket,
                            die
                        );
                    }
                }
            }
            bars[i] = rb_bars;
            fstart_log::info!(
                "{}: S{} RB{} (iod {} rb {}) bus {:x}:{:#x}-{:#x} mmio {:#x}+{:#x} non_pci {:#x}+{:#x}",
                V::NAME,
                rb.socket,
                rb.index,
                die,
                rb_on_iod,
                rb.segment,
                rb.bus.base,
                rb.bus.limit,
                rb.below.base,
                rb.below.size,
                rb.below_pools.non_pci.base,
                rb.below_pools.non_pci.size
            );
        }

        let psp_bar = psp_bar.ok_or("PSP BAR")?;
        // SAFETY: PSP BAR just programmed on IOHC and identity-mapped uncached.
        let c2p = unsafe { psp::MmioTransport::new(psp_bar.into(), psp::Variant::Wh) };
        fstart_log::info!(
            "{}: PSP BAR {:#x}, C2P {:#x}",
            V::NAME,
            psp_bar,
            c2p.read(psp::mailbox::C2pReg::Cmd)
        );
        if c2p.read(psp::mailbox::C2pReg::Cmd) == u32::MAX {
            return Err("PSP BAR");
        }
        // The recovery firmware halts; a ResetRequired on an A/B flash is
        // a guarded warm reset.
        super::reset::psp(ctx, c2p)?;
        let psp = psp_session::<V>(psp_bar)?;

        // FCH IOAPIC + HPET + board GPIO pads.
        fstart_driver_amd::fch::acpimmio::enable_hpet_ioapic(fch.acpi());
        // SAFETY: fixed FCH IOAPIC MMIO at 0xFEC0_0000, BSP only.
        let fch_ioapic = unsafe { IoApic::new(fstart_driver_amd::fch::IOAPIC as usize) };
        fch_ioapic.set_id(FCH_IOAPIC_ID);
        fch_ioapic.mask_all();
        let bsp_apic = (fstart_arch::x86::cpuid(1).1 >> 24) as u8;
        fch_ioapic.route_i8259(bsp_apic);

        fstart_driver_amd::fch::gpio::configure_all(fch.acpi(), config.gpio);
        fstart_driver_amd::fch::acpimmio::acpi_ports(
            fch.acpi(),
            super::acpi::soc(V::PMBASE, false).ports,
        );

        // Per-(socket, die) core maps from the APOB V2 validated in `map()`.
        let mut ccds = alloc::vec::Vec::new();
        for &(socket, die) in &nodes {
            match apob.core_topology(socket, die) {
                Ok(map) => {
                    fstart_log::info!(
                        "{}: S{} IOD{} topology: {} CCDs, {} threads",
                        V::NAME,
                        socket,
                        die,
                        map.ccds.len(),
                        map.threads()
                    );
                    ccds.push((socket, die, map));
                }
                Err(ApobError::NotFound { .. }) if (socket, die) != (0, 0) => {
                    fstart_log::warn!(
                        "{}: S{} IOD{} has no APOB V2 3/3 record (fused off)",
                        V::NAME,
                        socket,
                        die
                    );
                }
                Err(_) => return Err("APOB V2 core topology"),
            }
        }
        let apob_nps = apob.actual_nps().ok();
        let ecc_capable = apob.ecc_capable().unwrap_or(false);

        let state = State {
            topology,
            devices,
            ioms,
            nodes,
            roots,
            plan,
            bars,
            non_pci,
            iommus,
            secondary_fchs,
            lanes: alloc::vec::Vec::new(),
            bindings: alloc::vec::Vec::new(),
            ccds,
            launch: alloc::vec::Vec::new(),
            socket_shift: 0,
            apob_nps,
            ecc_capable,
            psp_bar,
            psp: Some(psp),
            einj: None,
            cxl: super::cxl::Cxl::default(),
            platform_errors: alloc::vec::Vec::new(),
            smu_features: None,
        };

        // Probe SMU v14 mailbox on die 0.
        step::<V>(0x31, "SMU v14");
        let smu = state.smu(ctx.clock);
        // openSIL's rule: a non-zero reply. No Venice module sends 02h
        // (v13's GetSmuVersion), so it is not asked.
        match smu_v14::msg::test(&smu) {
            Ok(reply) => fstart_log::info!("{}: SMU v14 test reply {:#x}", V::NAME, reply),
            Err(_) => fstart_log::warn!("{}: SMU v14 test failed", V::NAME),
        }
        super::reset::nvm_self_refresh(ctx, &state);
        super::fch::primary(ctx, &state);
        super::fch::secondaries::<V>(&state)?;

        Ok(state)
    }

    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        mut state: State,
        lanes: Self::Lanes<'_>,
    ) -> Result<State, &'static str> {
        step::<V>(0x38, "WH MPIO v2");
        let trained = super::mpio::train(ctx, &state, lanes)?;
        state.lanes = trained.lanes;
        state.bindings = trained.bindings;
        super::nbio::after_training(ctx, &state);
        state.cxl = super::cxl::after_training(ctx, &state);
        super::cxl_memory::carve_rch::<V>(&mut state);
        Ok(state)
    }

    fn init_chipset(ctx: &Ctx<'_, Self>, mut state: State, (): ()) -> Result<State, &'static str> {
        // AmdNbioDxe's entry: every socket's SMU (`super::smu`), the
        // IOMMUs' MMIO and VF BARs; the FCH I2C controllers.
        state.smu_features = super::smu::initialize(ctx, &state);
        state.enable_iommus::<V>(ctx.config.devices.iommu_vf);
        super::fch::i2c::<V>(ctx);
        if ctx.config.usb.iter().all(Option::is_none) {
            return Ok(state);
        }
        step::<V>(0x3a, "Qilian USB");
        super::fch::usb(ctx, &state, &ctx.config.usb);
        Ok(state)
    }

    fn cpu_driver(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        step::<V>(0x50, "Zen 6 MP");
        state.launch.clear();
        let shifts = cpu::TopologyShifts::current().ok_or("WH CPUID 8000_0026")?;
        state.socket_shift = shifts.socket_shift;
        // The APIC ID's CCD field counts a socket's CCDs: earlier dies of the
        // same socket only.
        let mut prior_ccds = 0u32;
        let mut socket = 0;
        for (s, die, map) in &state.ccds {
            if *s != socket {
                (socket, prior_ccds) = (*s, 0);
            }
            let die_aps = cpu::launch_order::<{ cpu::MAX_THREADS_PER_DIE }>(
                *s,
                *die,
                map,
                (*s, *die) == (0, 0),
                shifts,
                prior_ccds,
            )
            .map_err(|_| "WH CPU topology")?;
            state.launch.extend_from_slice(&die_aps);
            prior_ccds += map.ccds.len() as u32;
        }
        fstart_log::info!(
            "{}: {} ({:#x}), {} threads ({} APs across {} dies)",
            V::NAME,
            ctx.hw.revision.name(),
            ctx.hw.revision.signature,
            1 + state.launch.len(),
            state.launch.len(),
            state.ccds.len()
        );
        let signature = CpuIdentity::current().signature;
        let apob = ctx.hw.apob()?;
        let loader = match apob.asp_auth_mode() {
            Ok(1..) => {
                fstart_log::info!("{}: ASP-authenticated microcode", V::NAME);
                cpu::Microcode::Asp
            }
            Ok(0) | Err(ApobError::NotFound { .. }) => {
                let patch = microcode.and_then(|bytes| {
                    ccx::microcode::Bundle::<Self>::parse(bytes)
                        .ok()?
                        .select(signature)
                        .ok()
                });
                match &patch {
                    Some(p) => {
                        fstart_log::info!("{}: microcode patch {:#x}", V::NAME, p.revision())
                    }
                    None => fstart_log::warn!("{}: no microcode patch for this CPU", V::NAME),
                }
                cpu::Microcode::Host(patch)
            }
            Err(_) => return Err("malformed APOB ASP auth mode"),
        };
        let apic = state
            .apic_mode(ctx.config)
            .ok_or("xAPIC with APIC IDs above 254")?;
        if ctx.hw.revision.logical_id().is_none() {
            fstart_log::warn!("{}: no logical ID, revision-specific MSRs skipped", V::NAME);
        }
        let brand = ccx::brand::read(&state.smu(ctx.clock), smu_v14::READ_BRAND_STRING);
        fstart_log::info!("{}: {}", V::NAME, ccx::brand::as_str(&brand));
        Ok(Fam17hCpu::multi_socket(
            V::NAME,
            cpu::Threads {
                revision: ctx.hw.revision,
                policy: cpu_policy(ctx.config.ccx),
                microcode: loader,
                apic,
                brand: Some(brand),
            },
            cpu::thread_init,
        ))
    }

    fn threads(_: &Ctx<'_, Self>, state: &State) -> Result<u16, &'static str> {
        u16::try_from(1 + state.launch.len()).map_err(|_| "WH thread count")
    }

    /// The LAPICs run in the mode the CPU driver's table is run with
    /// ([`State::apic_mode`]).
    fn x2apic(ctx: &Ctx<'_, Self>, state: &State) -> bool {
        state.apic_mode(ctx.config) == Some(ccx::x2apic::ApicMode::X2Apic)
    }

    /// Release one AP at a time through its owning IOD's SMN `ThreadEnable`
    /// register pair, waiting up to 50 ms for each AP to check in.
    fn release_aps(ctx: &Ctx<'_, Self>, state: &State, aps: u16) -> Result<(), u32> {
        if usize::from(aps) != state.launch.len() {
            return Err(0);
        }
        for (i, &ap) in state.launch.iter().enumerate() {
            let index = i as u32 + 1;
            let Some(root) = state.node_root(ap.socket, ap.die) else {
                fstart_log::error!(
                    "{}: no root bus for AP {} on S{} IOD{}",
                    V::NAME,
                    index,
                    ap.socket,
                    ap.die
                );
                return Err(index);
            };
            // SAFETY: BSP owns SMN on the AP's IOD's root bus during MP bring-up.
            let smn = unsafe { Smn::on_root(GlobalEcam, root) };
            if !cpu::release(&smn, ctx.hw.revision, ap) {
                return Err(index);
            }
            if !fstart_arch::x86::wait_us(50_000, || {
                u32::from(fstart_arch::x86::mp::ap_checkins()) >= index
            }) {
                fstart_log::error!(
                    "{}: AP {} on IOD{} CCD {} bit {} did not check in",
                    V::NAME,
                    index,
                    ap.die,
                    ap.phys_ccd,
                    ap.bit
                );
                return Err(index);
            }
        }
        Ok(())
    }

    fn cpu_errors(cpu: &Self::Cpu) -> u32 {
        cpu.errors()
    }

    fn cpus_up(
        ctx: &Ctx<'_, Self>,
        mut state: State,
        mp: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<State, &'static str> {
        let expected = 1 + state.launch.len();
        if apic_ids.len() != expected {
            fstart_log::error!(
                "{}: checked-in CPU count {} != launch plan {}",
                V::NAME,
                apic_ids.len(),
                expected
            );
            return Err("WH AP check-in count");
        }
        let bsp_apic = cpu::Thread::current(ctx.msr).apic_id;
        if apic_ids[0] != bsp_apic {
            fstart_log::error!(
                "{}: BSP APIC ID mismatch ({:#x} != {:#x})",
                V::NAME,
                apic_ids[0],
                bsp_apic
            );
            return Err("WH BSP APIC ID");
        }
        // APs are released one at a time, each checking in before the next
        // (`release_aps`): AP `i` reports the APIC ID its launch slot
        // predicts. The MADT, SRAT and SMM all key on these.
        for (ap, &id) in state.launch.iter().zip(&apic_ids[1..]) {
            if ap.apic_id != id {
                fstart_log::error!(
                    "{}: APIC ID {:#x}, expected {:#x} (IOD{} CCD {} bit {})",
                    V::NAME,
                    id,
                    ap.apic_id,
                    ap.die,
                    ap.phys_ccd,
                    ap.bit
                );
                return Err("WH APIC IDs");
            }
        }
        // CxlManagerOnMpServices: TOM2 below the CXL windows until a pool
        // is enabled.
        state.cxl.memory.tom2 = super::cxl_memory::lower_tom2::<V>(ctx, mp);
        Ok(state)
    }

    fn fabric<'a>(hw_ctx: &Ctx<'_, Self>, state: &'a State) -> Fabric<'a> {
        Fabric {
            segment: Self::pci_segment(hw_ctx.hw),
            segments: hw_ctx.hw.ecam.segments.into(),
            bridges: &state.plan.root_bridges,
        }
    }

    fn display_port(ctx: &Ctx<'_, Self>, state: &State, _: &PciEcam) -> Option<PciAddress> {
        state.port_on(0, ctx.config.display?)
    }

    fn route_legacy_vga(_: &Ctx<'_, Self>, state: &State, root: &RootBridgePlan) {
        // Die 0 of each socket routes the VGA window (FabricEnableVgaMmio).
        for &(socket, die) in state.nodes.iter().filter(|&&(_, die)| die == 0) {
            // SAFETY: BSP owns the DF VGA routing register of this live node.
            if let Some(node) = unsafe { df_wh::Node::new(GlobalEcam, socket, die) } {
                node.route_legacy_vga(root.fabric_id);
            }
        }
    }

    /// The CXL steps after enumeration, then NBIO's (RAS, NBIF requester and
    /// HW-init locks, [`super::nbio::after_enumeration`]).
    fn after_resources(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        let mut cxl = core::mem::take(&mut state.cxl);
        super::cxl::after_resources(ctx, state, &mut cxl);
        super::cxl_memory::after_resources(ctx, state, &mut cxl);
        state.cxl = cxl;
        // Before NBIO's RAS setup clears the IOHCs' sync-flood status.
        state.platform_errors = super::ras::previous_boot(ctx, state);
        super::nbio::after_enumeration(ctx, state);
        Ok(())
    }

    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        let mpio = &ctx.config.mpio;
        let absent = |soldered: &&crate::links::Soldered| {
            !state
                .port_on(soldered.socket, soldered.lane)
                .is_some_and(|port| soldered.behind.found(pci, port))
        };
        for soldered in mpio.expected.iter().filter(absent) {
            fstart_log::warn!(
                "{}: S{} lane {}: soldered device not found behind its root port",
                V::NAME,
                soldered.socket,
                soldered.lane
            );
        }
        if let Some(soldered) = mpio.required.iter().find(absent) {
            fstart_log::error!(
                "{}: S{} lane {}: required soldered device absent behind its root port",
                V::NAME,
                soldered.socket,
                soldered.lane
            );
            return Err("WH MPIO endpoints");
        }
        // FabricReadyToBoot: CF8 extended configuration off on every node.
        for &(socket, die) in &state.nodes {
            // SAFETY: BSP owns the DF of this live node; APs are parked.
            if let Some(node) = unsafe { df_wh::Node::new(GlobalEcam, socket, die) } {
                node.ready_to_boot();
            }
        }
        // The AER routing before SMU message 9 announces it.
        super::ras::aer_paths(ctx, state);
        super::nbio::ready_to_boot(ctx, state);
        super::reset::ready_to_boot::<V>(ctx.hw);
        // FchInitPreOsHwAcpiQl: level GEVENT SCIs, the late PMxC0 write
        // (the reset code read the status at entry).
        qilian::pm::pre_os(ctx.hw.fch.acpi(), ctx.config.devices.keep_s5_reset_status);
        Ok(())
    }

    fn finalize(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        qilian::acpi_mode(&ctx.hw.fch, &super::acpi::soc(V::PMBASE, false).ports);
        let psp = state.psp.as_mut().ok_or("PSP session")?;
        psp::boot::lock(psp, &ctx.clock).map_err(|(what, error)| {
            let (why, value) = error.parts();
            fstart_log::error!("{}: PSP {}: {} {:#x}", V::NAME, what, why, value);
            "PSP lock"
        })?;
        Ok(())
    }

    fn boot_done(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        // FchSmmInit's SmmReadyToBoot: LockFCHReg (C2P 30h) from SMM
        // (`PcdFchOemSecureEnable` 1 in both donors; LockGpio 3Ah stays
        // off, `PcdFchOemGpioFencing` 0 in both). The handler reports it.
        fstart_driver_amd::fch::smi::software_smi(
            ctx.hw.fch.io(),
            fstart_driver_amd::fch::smi::sw::FCH_LOCK,
        );
        let report = &raw const SMM_REPORT;
        // SAFETY: reserved DRAM report written only by the SP7 SMM handler.
        let fch_lock = unsafe { fstart_core::mmio::read32(&raw const (*report).fch_lock) };
        if fch_lock != 0 {
            fstart_log::warn!("{}: PSP LockFCHReg status {:#x}", V::NAME, fch_lock);
        }
        let psp = state.psp.as_mut().ok_or("PSP session")?;
        // SAFETY: APs are parked and SMM is locked.
        unsafe { psp::boot::ready_to_boot(psp, &ctx.clock, crate::tpm::present()) }.map_err(
            |(what, error)| {
                let (why, value) = error.parts();
                fstart_log::error!("{}: PSP {}: {} {:#x}", V::NAME, what, why, value);
                "PSP BootDone"
            },
        )?;
        super::reset::boot_done(ctx.hw);
        Ok(())
    }
}

/// The board's CCX policy as the BSP settles it for every thread: the CPU
/// watchdog stepped below the DF's (`CcxEnableWdt`, Dell `0xf4d440`: the
/// BSP reads `F3x44` through the DF and shortens the CPU timeout until it
/// is below; none left turns it off), DF node (0, 0)'s broadcast `F3x44`
/// decoded as DFv4.5 does ([`df_wh::watchdog_ns`]); SME-MK only where
/// CPUID `8000_0023h` has it (`CcxSmeeInit`).
fn cpu_policy(mut policy: ccx::wh::Policy) -> ccx::wh::Policy {
    policy.hmkee &= ccx::security::hmkee_supported(fstart_arch::x86::cpuid(0x8000_0023).0);
    // SAFETY: BSP reads the DF of the boot node.
    let df = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }
        .and_then(|node| df_wh::watchdog_ns(node.read(df_wh::GLOBAL_CONTROL)));
    policy.wdt = policy.wdt.and_then(|w| ccx::pm::wdt_against(w, df, false));
    policy
}

impl<V: Variant> crate::flow::Soc for Sp7<V> {
    const TSEG_SIZE: u32 = TSEG_SIZE;
    type Flow = ();

    /// The reset image both ISH slots name. The EFS and its `$BL2`s sit in
    /// another 16 MiB bank than the reset image (dual origin), so ROM2's
    /// one page cannot show both: through ROM3 at its hardware base when
    /// mapped and checked against ROM2, else through SMN ([`super::flash`]).
    fn reset_image(hw: &Hw, (): &()) -> Option<&'static [u8]> {
        super::flash::reset_image::<V>(&hw.fch)
    }

    fn route_tpm(hw: &Hw, bus: crate::board::TpmBus) -> bool {
        super::fch::route_tpm::<V>(&hw.fch, bus)
    }

    fn system_reset() -> Option<fstart_core::description::SystemReset> {
        Some(super::reset::SYSTEM_RESET)
    }

    fn map(
        hw: &mut Hw,
        config: &mut Config,
        _: &crate::board::Facts,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        use crate::memory::{self, Reservation, TopOfMemory};
        let (tom, tom2) = ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
        // Validate that the APOB window lies inside decoded DRAM below TOP_MEM
        // and does not overlap firmware early RAM before constructing `'static` slice.
        if V::APOB_BASE >= tom {
            return Err("APOB outside DRAM");
        }
        let max_len = (tom - V::APOB_BASE).min(V::APOB_MAX_SIZE);
        if max_len < 0x40 {
            return Err("APOB outside DRAM");
        }
        // SAFETY: `V::APOB_BASE..V::APOB_BASE + max_len` lies inside decoded
        // DRAM below TOP_MEM; PSP wrote APOB V2 here before releasing the BSP.
        let apob = unsafe { ApobV2::from_address(V::APOB_BASE, max_len as usize) }
            .map_err(|_| "APOB V2")?;
        let apob_len = (apob.total_size() as u64).next_multiple_of(0x1000);
        let apob_end = V::APOB_BASE.checked_add(apob_len).ok_or("APOB size")?;
        let apob_region = Region::new(V::APOB_BASE, apob_len);
        if apob_end > tom || apob_region.overlaps(&V::LAYOUT.early) {
            return Err("APOB bounds");
        }
        let map = apob.system_memory_map().map_err(|_| "APOB V2 sysmap")?;
        if apob_end > map.top {
            return Err("APOB bounds");
        }
        let regions = sysmap::memory_map(super::HOLES, map.top, map.holes().iter().copied())
            .map_err(|_| "memory map")?;
        hw.apob = Some(apob);
        let tseg = memory::place_tseg(
            &regions,
            tom,
            u64::from(TSEG_SIZE),
            &[V::LAYOUT.early, apob_region],
        )
        .ok_or("TSEG placement")?;
        let reservations = [
            Region::new(0, 0x1000),
            Region::new(fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000),
            Region::new(0x9_f000, 0x6_1000),
            apob_region,
            V::LAYOUT.early,
            tseg,
        ]
        .map(Reservation::reserved);
        let mut e820 = memory::build(V::NAME, &regions, TopOfMemory { tom, tom2 }, &reservations)
            .map_err(|_| "memory map")?;
        super::cxl::map_memory::<V>(&mut e820, &apob, config.cxl.as_ref(), tom2.unwrap_or(0));
        if let Err(step) = crate::memory::cache_ram(&regions) {
            crate::flow::fail::<Self>(step);
        }
        let ffs = V::LAYOUT.ffs.ok_or("boot media")?;
        fstart_arch::x86::boot::enable_boot_media_rom_cache();
        let ffs = super::flash::ffs::<V>(&hw.fch, &mut e820, ffs)?;
        Ok(Mapped {
            top_mem: tom,
            top_mem2: tom2,
            apob: None,
            tseg: tseg.base,
            e820,
            ffs: ffs.base..ffs.end(),
            nvram: None,
            options: fstart_options::Report::new(),
            flow: (),
        })
    }

    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        mp: &MpHandle,
        (): &(),
    ) -> Result<(), &'static str> {
        use super::smm::{Config as SmmConfig, Smi};
        use fstart_driver_amd::fch::smi;
        step::<V>(0x58, "SMM");
        if state.psp_bar == 0 {
            return Err("PSP BAR");
        }
        let report = &raw mut SMM_REPORT;
        let fch = &ctx.hw.fch;
        // The ECAM window and the root bridges' PCI windows, mapped in the
        // handler's page tables above 4 GiB.
        let reach = super::ras::smm_root_bridges(ctx, state);
        let smi = Smi {
            fch,
            config: SmmConfig {
                ports: super::acpi::soc(V::PMBASE, false).ports,
                psp_bar: state.psp_bar,
                einj: ctx.config.einj,
                kcs_decoded: ctx.config.kcs_decoded,
                ecam_segments: ctx.hw.ecam.segments.into(),
                report: report as u64,
                ecam_base: ctx.hw.ecam.base,
                firmware_first: super::ras::firmware_first(ctx),
                root_bridges: reach.root_bridges,
                root_bridge_count: reach.root_bridge_count,
                window_count: reach.window_count,
                _reserved: [0; 6],
                windows: reach.windows,
            },
            mapped: reach.mapped,
        };
        // AmdCcxZen6Dxe holds TW_CFG[27] set from TSEG programming to the
        // lock (`ccx::msr::TwCfg::SMM_SETUP`) and locks with
        // `HWCR |= 0x8000_0001` (`PcdAmdSmmLock` 1 in both donors, Dell
        // `0xe85d..0xe872`).
        let setup = fstart_arch::x86::cpu::amd::smm::SetupBits {
            msr: ccx::msr::TW_CFG.index(),
            bits: ccx::msr::TwCfg::SMM_SETUP::SET.value,
            hwcr_lock: fstart_arch::x86::cpu::amd::msr::Hwcr::LOCK_31::SET.value,
        };
        crate::tseg::install_with(V::NAME, mp, &smi, ctx.memory.tseg, TSEG_SIZE, Some(setup))?;
        smi::software_smi(fch.io(), crate::smm::sw::PSP_SMM_INFO);
        // SAFETY: reserved DRAM report written only by the SP7 SMM handler.
        let (smm_info, einj_status) = crate::smm::status::answers(
            &ctx.clock,
            ctx.config.einj,
            || unsafe { fstart_core::mmio::read32(&raw const (*report).smm_info) },
            || unsafe { fstart_core::mmio::read32(&raw const (*report).einj) },
        );
        // Require PSP SmmInfo before DF lock / OS handoff.
        crate::smm::status::smm_info(V::NAME, smm_info)?;
        // EINJ is optional: if the PSP declines or has no hole, boot continues without EINJ.
        let Some(einj_status) = einj_status else {
            return Ok(());
        };
        match einj_status {
            0 => {
                fstart_log::info!("{}: PSP RasEinj ENABLE status 0", V::NAME);
                state.einj = einj::<V>(ctx, state.psp_bar.into())
                    .inspect_err(|what| fstart_log::error!("{}: no EINJ: {}", V::NAME, what))
                    .ok();
            }
            status => fstart_log::error!("{}: PSP RasEinj ENABLE status {:#x}", V::NAME, status),
        }
        Ok(())
    }

    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Up<Self>,
        pci: &PciEcam,
        (): &(),
        identity: &crate::acpi::SmbiosIdentity<'_>,
        mut board: crate::acpi::Board,
    ) -> Tables {
        step::<V>(0x70, "ACPI, SMBIOS");
        if !ctx.config.kcs_decoded {
            board.ipmi = None;
        }
        super::cxl_memory::restore_tom2::<V>(&silicon.state.cxl, &silicon.cpus.mp);
        super::cxl::tables::<V>(&silicon.state.cxl, &mut silicon.tables, &mut silicon.aml);
        if let Some(einj) = silicon.state.einj {
            // Revision 2 for the EINJv2 action, as the vendor's template.
            silicon.tables.push(fstart_acpi::einj::table_revision(
                2,
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
                &psp::einj::entries_wh(einj.mailbox, einj.buffer, einj.v2),
            ));
        }
        // The SMM handler takes no buffer from the OS (EINJ is the PSP's):
        // the WSMT `acpi::build` adds only with an ACPI SMI command port,
        // which SP7 hands over without.
        {
            use fstart_acpi::wsmt;
            silicon.tables.push(wsmt::build_wsmt(
                wsmt::FIXED_COMM_BUFFERS | wsmt::COMM_BUFFER_NESTED_PTR_PROTECTION,
            ));
        }
        let soc = super::acpi::soc(V::PMBASE, ctx.config.legacy_devices);
        let rsdp = super::tables::acpi::<V>(ctx, silicon, pci, &soc, &board);
        if let (Some(rsdp), Some(psp)) = (rsdp, silicon.state.psp.as_mut()) {
            super::tables::send_ivrs::<V>(psp, &ctx.clock, rsdp);
        }
        let smbios = super::tables::smbios::<V>(ctx, silicon, pci, identity);
        Tables {
            rsdp,
            smbios,
            s5: fstart_core::description::AcpiSleepControl::new(
                soc.ports.control,
                crate::acpi::S5_SLEEP_TYPE,
            ),
        }
    }

    /// `Online` CXL windows nothing decodes ([`super::cxl::not_ram`]).
    fn not_ram(ctx: &Ctx<'_, Self>, state: &State) -> alloc::vec::Vec<core::ops::Range<u64>> {
        super::cxl_memory::not_ram::<V>(ctx, state.cxl())
    }

    fn map_payload_ram(
        e820: &fstart_core::services::memory_detect::E820State,
        pci: &PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        crate::noncar::boot::map_ram(e820, pci)
    }
}

/// Query `MP0_C2PMSG_31` for the PSP's EINJ command buffer and lay it out
/// as Venice's `AmdPlatformRasWhDxe` `ApeiEinjInstallBuffer` does
/// (`psp::einj::WhBuffer`). The vendor takes the address from the PSP and
/// looks for no hole (APOB hole type `0x13`, once taken for EINJ's, is the
/// BDAT's: `AmdMemoryHobInfoPeim` stores its base in `PcdBdatAddress`,
/// DEVICES.md §0.6); the buffer must lie whole in a reserved region of the
/// APOB's memory map, never in RAM the OS gets.
fn einj<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, bar: u64) -> Result<Einj, &'static str> {
    use psp::einj::{EINJV2_TYPES, Mailbox, WhBuffer, mailbox_offset};
    let map = (ctx.hw.apob()?)
        .system_memory_map()
        .map_err(|_| "APOB V2 sysmap")?;
    let regions = sysmap::memory_map(super::HOLES, map.top, map.holes().iter().copied())
        .map_err(|_| "memory map")?;
    // SAFETY: die 0 RB 0 PSP BAR mapped by `init_fch`; BSP owns the RAS mailbox before OS boot.
    let mailbox = unsafe { Mailbox::mmio(bar, psp::Variant::Wh) };
    let base = mailbox
        .buffer_of::<WhBuffer>(&ctx.clock, &(0..u64::MAX))
        .map_err(|_| "PSP EINJ buffer")?;
    let end = base + core::mem::size_of::<WhBuffer>() as u64;
    if !regions.iter().any(|r| {
        matches!(r.kind, sysmap::RegionKind::Reserved(_)) && r.base <= base && end <= r.end()
    }) {
        return Err("PSP EINJ buffer outside reserved memory");
    }
    // SAFETY: PSP command buffer inside the reserved ABL hole, identity mapped.
    let buffer = unsafe { WhBuffer::at(base) };
    let trigger = fstart_acpi::einj::trigger(&[psp::einj::trigger(base)]);
    let (types, v2) = buffer.prepare(&trigger).map_err(|_| "EINJ trigger table")?;
    // SAFETY: ring 0, caches enabled; flush buffer for PSP DRAM reads.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };
    fstart_log::info!(
        "{}: PSP EINJ buffer {:#x}, error types {:#x}",
        V::NAME,
        base,
        types.get()
    );
    Ok(Einj {
        mailbox: bar + mailbox_offset(psp::Variant::Wh),
        buffer: base,
        v2: v2.is_set(EINJV2_TYPES::SUPPORTED),
    })
}
