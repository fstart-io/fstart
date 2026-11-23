// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 / WH silicon bring-up on the shared AMD boot flow.
//!
//! | POST | Step |
//! |---|---|
//! | 20 | DFv4.5 bus routes, MMIO and I/O planning (broadcast to all live local IODs) |
//! | 30 | WH NBIO (early bus, IOHC TOM, 512 KiB IOMMU + L1/L2, IOAPIC, PSP BAR), Qilian FCH + secondary FCH |
//! | 38 | Dual-IOD WH MPIO v2 ASK/DELI PCIe link training & PERST release |
//! | 3a | Qilian SMU USB init |
//! | 50 | Zen 6 / Zen 6c dual-IOD MP release, microcode, SMU v14 |
//! | 58 | SP7 SMM in 16 MiB TSEG (`sp7::smm`), PSP registration, RAS/EINJ |
//! | 60 | PCI enumeration & soldered/BMC VGA discovery |
//! | 70 | ACPI (`SRAT`/`SLIT`/`MSCT`/`IVRS`/`HEST`/`BERT`/`EINJ`/`WSMT`), SMBIOS |

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use super::{
    Bars, Config, ECAM_BASE, ECAM_BUSES, FCH_IOAPIC_ID, LaneDescriptor, Sp7, TSEG_SIZE, Variant,
    place_bars, resolve_wh_roots, socket_phys_ccd,
};
use crate::flow::{Ctx, Mapped, Silicon as Up, Tables};
use crate::layout::Region;
use crate::soc::{Entered, Fabric, Init};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::ioapic::IoApic;
use fstart_arch::x86::mp::{CpuIdentity, MpHandle};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::apob::{
    sysmap,
    v2::{ApobV2, CoreMapV2},
};
use fstart_driver_amd::ccx::{self, mp::Fam17hCpu, tsc::Tsc, wh as cpu};
use fstart_driver_amd::df::dram::Nps;
use fstart_driver_amd::df::plan::RootBridgePlan;
use fstart_driver_amd::df::{RootBridge, plan, v4, wh as df_wh};
use fstart_driver_amd::fch::{FchHw, Native, qilian};
use fstart_driver_amd::nbio::{self, ioapic::nbio_ioapic_id, iohc, wh as nbio_wh};
use fstart_driver_amd::psp::{self, C2pTransport};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::{self, SmuMessages, v14 as smu_v14};
use fstart_pci::{ConfigRegionAccess, PciAddress, PciEcam, ecam::GlobalEcam};
use heapless::Vec;
use tock_registers::interfaces::Readable;

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
}

/// Per-flow state carried outside [`State`]: the validated APOB V2 in DRAM.
pub struct FlowState {
    pub apob: ApobV2<'static>,
}

/// PSP ACPI EINJ mailbox and command buffer discovered after `RasEinj::ENABLE`.
#[derive(Debug, Clone, Copy)]
struct Einj {
    mailbox: u64,
    buffer: u64,
}

/// What the silicon steps hand on.
pub struct State {
    dies: Vec<u8, 2>,
    roots: Vec<RootBridge, 8>,
    plan: plan::Plan,
    bars: [Bars; 8],
    iommus: [Option<nbio::iommu::Iommu>; 8],
    lanes: Vec<(u8, PciAddress, bool), 32>,
    ccds: Vec<(u8, CoreMapV2), 2>,
    launch: alloc::vec::Vec<cpu::Launch>,
    apob_nps: Option<Nps>,
    ecc_capable: bool,
    psp_bar: u32,
    psp: Option<psp::boot::Psp>,
    einj: Option<Einj>,
}

impl State {
    pub(crate) fn dies(&self) -> &[u8] {
        &self.dies
    }

    pub(crate) fn roots(&self) -> &[RootBridge] {
        &self.roots
    }

    pub(crate) fn bars(&self, rb_index: u8) -> Bars {
        self.bars
            .get(usize::from(rb_index))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn iommu(&self, rb_index: u8) -> Option<nbio::iommu::Iommu> {
        self.iommus.get(usize::from(rb_index)).copied().flatten()
    }

    pub(crate) fn apob_nps(&self) -> Option<Nps> {
        self.apob_nps
    }

    pub(crate) fn ecc_capable(&self) -> bool {
        self.ecc_capable
    }

    /// `(socket, socket_phys_ccd)` for every logical CPU in MP order (BSP first,
    /// then APs in launch order) for SRAT/SLIT NUMA domain assignment.
    pub(crate) fn thread_ccds(&self) -> alloc::vec::Vec<(u8, u8)> {
        let bsp = self.ccds.first().and_then(|(die, map)| {
            map.ccds
                .first()
                .map(|ccd| (0u8, socket_phys_ccd(*die, ccd.id)))
        });
        bsp.into_iter()
            .chain(
                self.launch
                    .iter()
                    .map(|ap| (ap.socket, socket_phys_ccd(ap.die, ap.phys_ccd))),
            )
            .collect()
    }

    /// Root port BDF associated with socket-wide `lane` (if its link is up).
    pub(crate) fn port(&self, lane: u8) -> Option<PciAddress> {
        self.lanes
            .iter()
            .find(|&&(l, _, up)| l == lane && up)
            .map(|&(_, port, _)| port)
    }

    /// Root PCI bus for SMN accesses targeting local IOD `die` (preferring RB 0
    /// on that die, falling back to the lowest routed root bridge on that die).
    pub(crate) fn die_bus(&self, die: u8) -> Option<u8> {
        self.roots
            .iter()
            .filter(|rb| rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD == die)
            .min_by_key(|rb| rb.index)
            .map(|rb| rb.bus_base)
    }

    /// SMU v14 mailbox handle routed through local IOD `die`'s primary root bus.
    fn smu_on(&self, die: u8, clock: Tsc) -> smu::Smu<GlobalEcam, Tsc> {
        let bus = self.die_bus(die).unwrap_or(0);
        // SAFETY: BSP owns SMN on IOD `die`'s primary root bus during cold init.
        smu::Smu::new(
            unsafe { Smn::on_bus(GlobalEcam, bus) },
            smu_v14::MAILBOX,
            clock,
            smu_v14::ID_LIMIT,
        )
    }

    fn smu(&self, clock: Tsc) -> smu::Smu<GlobalEcam, Tsc> {
        self.smu_on(0, clock)
    }
}

/// WH NBIO IOAPIC ID for physical root bridge `rb`: `0xF1 + rb`.
pub(super) const fn ioapic_id(rb: u8) -> u8 {
    nbio_ioapic_id(fstart_driver_amd::cpu::Silicon::Gn, rb)
}

/// Probe live local (socket 0) DF IODs (`00:18.0`, `00:19.0`) via PCI Vendor ID `0x1022`.
fn probe_local_dies() -> Vec<u8, 2> {
    let mut dies = Vec::new();
    for die in 0u8..2 {
        if let Some(addr) = df_wh::node_address(0, die, 0) {
            // SAFETY: PCI bus 0 ECAM is active; reading Vendor/Device ID is side-effect free.
            let id = unsafe { GlobalEcam.read(addr, 0) };
            if (id & 0xffff) == 0x1022 {
                let _ = dies.push(die);
            }
        }
    }
    dies
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
    let mailbox = psp::Mailbox::new(
        unsafe { psp::MmioTransport::new(bar.into()) },
        psp::Variant::Gn,
    );
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
    type Lanes<'a> = &'a [LaneDescriptor];
    type Runtime = ();
    type FchUp = State;
    type Trained = State;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, cpu::Threads>;
    type State = State;

    fn enter(_: &Config, msr: &NativeMsr) -> Entered<Self> {
        fstart_driver_amd::fch::post_native(0x10);
        ccx::msr::enable_ecam(msr, ECAM_BASE, ECAM_BUSES);
        fstart_pci::ecam::init(ECAM_BASE as usize);
        // SAFETY: exclusive FCH programming before console.
        unsafe { fstart_driver_amd::fch::acpimmio::enable_acpimmio() };
        let clock = Tsc::measured(msr);
        // SAFETY: BSP only, ACPI MMIO and ECAM decoded.
        let fch = unsafe { Native::new(Tsc::native(msr)) };
        qilian::take_over(&fch);
        let signature = CpuIdentity::current().signature;
        let revision = cpu::Revision::from_signature(signature);
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
        Ok(hw.revision)
    }

    fn pci_segment(_: &Hw) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: ECAM_BASE,
            bus_start: 0,
            bus_end: (ECAM_BUSES - 1) as u8,
        }
    }

    fn psp(_: &Hw) -> psp::Variant {
        psp::Variant::Gn
    }

    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State, &'static str> {
        let config = ctx.config;
        let fch = &ctx.hw.fch;
        step::<V>(0x20, "DFv4.5");
        let dies = probe_local_dies();
        if !dies.contains(&0) {
            return Err("DF node0 absent");
        }
        // SAFETY: BSP owns DF indirect registers on node (0, 0) during cold init.
        let node0 = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }.ok_or("DF node0")?;
        if node0
            .df()
            .reg(v4::SYSTEM_CONFIG)
            .is_set(v4::SystemConfig::OTHER_SOCKET_PRESENT)
        {
            return Err("multi-socket SP7 not yet supported");
        }
        let routes = node0.bus_routes().map_err(|_| "DFv4.5 bus maps")?;
        let roots = resolve_wh_roots(&routes, &dies)?;
        let tops = ccx::msr::memory_limits(ctx.msr).map_err(|_| "TOP_MEM")?;
        let plan = plan::plan(
            &plan::Input {
                tom: tops.0,
                tom2: tops.1.unwrap_or(0),
                ecam_base: ECAM_BASE,
                ecam_size: u64::from(ECAM_BUSES) << 20,
                phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
                root_bridges: &roots,
                mmio_pairs: v4::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .map_err(|_| "DF plan")?;

        // Broadcast MMIO and I/O routing rules to EVERY live local IOD (`0x18`, `0x19`).
        for &die in &dies {
            // SAFETY: BSP owns DF routing tables on live local IOD `die`.
            let node = unsafe { df_wh::Node::new(GlobalEcam, 0, die) }.ok_or("DF node")?;
            v4::program(node.df(), &plan.mmio, &plan.io);
        }

        step::<V>(0x30, "WH NBIO (IOAPIC, TOM, PSP BAR) & FCH");
        let dram_tops = iohc::DramTops {
            tom: tops.0,
            tom2: tops.1,
        };
        let mut bars = [Bars::default(); 8];
        let mut iommus = [None; 8];
        let mut psp_bar: Option<u32> = None;
        for rb in &plan.root_bridges {
            let idx = usize::from(rb.index);
            if idx >= bars.len() {
                return Err("NBIO index out of range");
            }
            let (rb_bars, maybe_psp) =
                place_bars(rb.index, rb.below_pools.non_pci).ok_or("NBIO BAR placement")?;
            bars[idx] = rb_bars;
            let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: root bridge bus is routed; BSP owns SMN during cold init.
            let nb = unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus.base) }
                .ok_or("WH NBIO handle")?;
            // Early NBIO bus number + SB_LOCATION setup (clears SB_LOCATION on
            // non-primary root bridges so unclaimed config accesses do not hang).
            nb.early(rb.primary);
            if let Some(iommu_base) = rb_bars.iommu {
                if nb.map_iommu(iommu_base as u32) {
                    nb.iommu_init(config.nbio);
                    // SAFETY: IOMMU BAR just mapped below 4 GiB, identity-mapped uncached.
                    iommus[idx] = unsafe { nb.iommu(iommu_base as u32) };
                } else {
                    fstart_log::warn!("{}: RB{} IOMMU capability absent", V::NAME, rb.index);
                }
            }
            // Program NB TOP_OF_DRAM on every root bridge's IOHC.
            // SAFETY: D0F0x90 of this root bridge's host bridge.
            let slot1 = unsafe {
                fstart_pci::ConfigRegister::new(
                    GlobalEcam,
                    PciAddress::new(0, nb.bus(), 0, 0),
                    iohc::NB_TOP_OF_DRAM_SLOT1,
                )
            };
            iohc::set_tom(&nb, &slot1, dram_tops, iohc::TopStyle::Tom3);

            if let Some(ioapic_base) = rb_bars.ioapic {
                let id = ioapic_id(rb.index);
                nb.init_ioapic(ioapic_base, id);
                // SAFETY: IOAPIC MMIO just enabled on this root bridge.
                let ioapic = unsafe { IoApic::new(ioapic_base as usize) };
                ioapic.set_id(id);
                ioapic.mask_all();
            }
            nb.init_ioapic_routes();
            // Program the PSP BAR on die 0 RB 0's IOHC and verify C2P responds.
            if let Some(pbar) = maybe_psp {
                iohc::map_psp(&nb, pbar).map_err(|_| "PSP BAR")?;
                psp_bar = Some(pbar);
            }
            // Relocate secondary Qilian FCH ACPI MMIO on IOD1 RB0 (`SMN 0x02D0_13D4`).
            if let Some(sec_fch) = rb_bars.secondary_fch {
                let iod = rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD;
                if qilian::init_secondary_fch(nb.smn(), iod, sec_fch) {
                    fstart_log::info!(
                        "{}: secondary Qilian FCH{} ACPI MMIO {:#x}",
                        V::NAME,
                        iod,
                        sec_fch
                    );
                }
            }
            fstart_log::info!(
                "{}: RB{} (iod {} rb {}) bus {:#x}-{:#x} mmio {:#x}+{:#x} non_pci {:#x}+{:#x}",
                V::NAME,
                rb.index,
                rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD,
                rb_on_iod,
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
        let c2p =
            unsafe { psp::MmioTransport::new(psp_bar.into()) }.read(psp::mailbox::C2pReg::Cmd);
        fstart_log::info!("{}: PSP BAR {:#x}, C2P {:#x}", V::NAME, psp_bar, c2p);
        if c2p == u32::MAX {
            return Err("PSP BAR");
        }
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

        // Parse APOB V2 core topology per live local IOD, NPS, and ECC capability.
        // SAFETY: APOB V2 in DRAM was already validated and reserved in `map()`.
        let apob = unsafe { ApobV2::from_address(V::APOB_BASE, V::APOB_MAX_SIZE as usize) }
            .map_err(|_| "APOB V2")?;
        let mut ccds: Vec<(u8, CoreMapV2), 2> = Vec::new();
        for &die in &dies {
            match apob.core_topology(0, die) {
                Ok(map) => {
                    fstart_log::info!(
                        "{}: IOD{} topology: {} CCDs, {} threads",
                        V::NAME,
                        die,
                        map.ccds.len(),
                        map.threads()
                    );
                    let _ = ccds.push((die, map));
                }
                Err(_) if die == 0 => return Err("APOB V2 core topology"),
                Err(_) => fstart_log::warn!("{}: IOD{} has no APOB V2 3/3 record", V::NAME, die),
            }
        }
        let apob_nps = apob.actual_nps().ok();
        let ecc_capable = apob.ecc_capable().unwrap_or(false);

        let state = State {
            dies,
            roots,
            plan,
            bars,
            iommus,
            lanes: Vec::new(),
            ccds,
            launch: alloc::vec::Vec::new(),
            apob_nps,
            ecc_capable,
            psp_bar,
            psp: Some(psp),
            einj: None,
        };

        // Probe SMU v14 mailbox on die 0.
        step::<V>(0x31, "SMU v14");
        let smu = state.smu(ctx.clock);
        let mut test = [0xaa55_aa55, 0, 0, 0, 0, 0];
        if smu.send(smu::TEST, &mut test).is_err() {
            fstart_log::warn!("{}: SMU v14 test failed", V::NAME);
        } else {
            let mut version = [0; 6];
            if smu.send(0x02, &mut version).is_ok() {
                fstart_log::info!("{}: SMU v14 firmware {:#x}", V::NAME, version[0]);
            }
        }

        Ok(state)
    }

    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        mut state: State,
        lanes: &[LaneDescriptor],
    ) -> Result<State, &'static str> {
        step::<V>(0x38, "WH MPIO v2");
        let trained = super::mpio::train(ctx, &state, lanes)?;
        state.lanes = trained.lanes;
        Ok(state)
    }

    fn init_chipset(ctx: &Ctx<'_, Self>, state: State, (): ()) -> Result<State, &'static str> {
        step::<V>(0x3a, "Qilian USB");
        for &die in &state.dies {
            let Some(bus) = state.die_bus(die) else {
                continue;
            };
            let smu = state.smu_on(die, ctx.clock);
            // SAFETY: BSP owns SMN on IOD `die`'s root bus during cold init.
            let smn = unsafe { Smn::on_bus(GlobalEcam, bus) };
            match qilian::usb::ColdBoot::new(&ctx.config.usb)
                .and_then(|mut usb| usb.run(&smn, &smu))
            {
                Ok(()) => fstart_log::info!("{}: Qilian IOD{} xHCI initialized", V::NAME, die),
                Err(_) => fstart_log::warn!(
                    "{}: Qilian IOD{} xHCI SMU init skipped/rejected",
                    V::NAME,
                    die
                ),
            }
        }
        Ok(state)
    }

    fn cpu_driver(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        step::<V>(0x50, "Zen 6 MP");
        state.launch.clear();
        for (die, map) in &state.ccds {
            let die_aps =
                cpu::launch_order::<{ cpu::MAX_THREADS_PER_DIE }>(0, *die, map, *die == 0)
                    .map_err(|_| "WH CPU topology")?;
            state.launch.extend_from_slice(&die_aps);
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
        let patch = microcode.and_then(|bytes| {
            ccx::microcode::Bundle::<Self>::parse(bytes)
                .ok()?
                .select(signature)
                .ok()
        });
        match &patch {
            Some(p) => fstart_log::info!("{}: microcode patch {:#x}", V::NAME, p.revision()),
            None => fstart_log::warn!("{}: no microcode patch for this CPU", V::NAME),
        }
        Ok(Fam17hCpu::multi_socket(
            V::NAME,
            cpu::Threads {
                revision: ctx.hw.revision,
                policy: ctx.config.ccx,
                microcode: patch,
            },
            cpu::thread_init,
        ))
    }

    fn threads(_: &Ctx<'_, Self>, state: &State) -> Result<u16, &'static str> {
        u16::try_from(1 + state.launch.len()).map_err(|_| "WH thread count")
    }

    /// Release one AP at a time through its owning IOD's SMN `ThreadEnable`
    /// register pair, waiting up to 50 ms for each AP to check in.
    fn release_aps(ctx: &Ctx<'_, Self>, state: &State, aps: u16) -> Result<(), u32> {
        if usize::from(aps) != state.launch.len() {
            return Err(0);
        }
        for (i, &ap) in state.launch.iter().enumerate() {
            let index = i as u32 + 1;
            let Some(bus) = state.die_bus(ap.die) else {
                fstart_log::error!("{}: no root bus for AP {} on IOD{}", V::NAME, index, ap.die);
                return Err(index);
            };
            // SAFETY: BSP owns SMN on IOD `ap.die`'s root bus during MP bring-up.
            let smn = unsafe { Smn::on_bus(GlobalEcam, bus) };
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
        _: &Ctx<'_, Self>,
        state: State,
        _: &MpHandle,
        _: &[u32],
    ) -> Result<State, &'static str> {
        Ok(state)
    }

    fn fabric<'a>(hw_ctx: &Ctx<'_, Self>, state: &'a State) -> Fabric<'a> {
        Fabric {
            segment: Self::pci_segment(hw_ctx.hw),
            bridges: &state.plan.root_bridges,
        }
    }

    fn display_port(ctx: &Ctx<'_, Self>, state: &State, _: &PciEcam) -> Option<PciAddress> {
        state.port(ctx.config.display?)
    }

    fn route_legacy_vga(_: &Ctx<'_, Self>, state: &State, root: &RootBridgePlan) {
        // Broadcast VGA routing to every live local DF node (`0x18`, `0x19`).
        for &die in &state.dies {
            // SAFETY: BSP owns DF VGA routing register on live local IOD `die`.
            if let Some(node) = unsafe { df_wh::Node::new(GlobalEcam, 0, die) } {
                node.route_legacy_vga(root.fabric_id);
            }
        }
    }

    /// Program TxRequester IDs on every visible PCIe root port so port MSIs
    /// reach the WH IOMMUs with their real PCI requester identity.
    fn after_resources(_: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        for rb in &state.roots {
            let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: routed root bridge, BSP owns SMN.
            let Some(nb) =
                (unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus_base) })
            else {
                continue;
            };
            for slot in 0..usize::from(nbio_wh::PCIE_BRIDGES_PER_RB) {
                let (dev, func) = nb.bridge_devfn(slot);
                if dev == 0 && func == 0 {
                    continue;
                }
                if fstart_pci::EcamDevice::new(rb.bus_base, dev, func).is_present() {
                    let devfn = (dev << 3) | (func & 7);
                    let core = (slot / 9) as u8;
                    let port = (slot % 9) as u8;
                    nb.set_requester(core, port, rb.bus_base, devfn);
                }
            }
        }
        Ok(())
    }

    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        for soldered in &ctx.config.mpio.required {
            if !state
                .port(soldered.lane)
                .is_some_and(|port| soldered.behind.found(pci, port))
            {
                fstart_log::error!(
                    "{}: lane {}: expected soldered device absent behind root port",
                    V::NAME,
                    soldered.lane
                );
                return Err("WH MPIO endpoints");
            }
        }
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
        let psp = state.psp.as_mut().ok_or("PSP session")?;
        // SAFETY: APs are parked and SMM is locked.
        unsafe { psp::boot::ready_to_boot(psp, &ctx.clock, ctx.board.discrete_tpm) }.map_err(
            |(what, error)| {
                let (why, value) = error.parts();
                fstart_log::error!("{}: PSP {}: {} {:#x}", V::NAME, what, why, value);
                "PSP BootDone"
            },
        )?;
        Ok(())
    }
}

impl<V: Variant> crate::flow::Soc for Sp7<V> {
    const TSEG_SIZE: u32 = TSEG_SIZE;
    type Flow = FlowState;

    fn map(
        _: &mut Hw,
        _: &mut Config,
        _: &crate::board::Facts,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        use crate::memory::{self, Reservation, TopOfMemory};
        let (tom, tom2) = ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
        // SAFETY: PSP writes APOB V2 at `V::APOB_BASE` before releasing the BSP;
        // the validated total size is reserved below.
        let apob = unsafe { ApobV2::from_address(V::APOB_BASE, V::APOB_MAX_SIZE as usize) }
            .map_err(|_| "APOB V2")?;
        let apob_len = (apob.total_size() as u64).next_multiple_of(0x1000);
        let apob_region = Region::new(V::APOB_BASE, apob_len);
        let map = apob.system_memory_map().map_err(|_| "APOB V2 sysmap")?;
        let regions = sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied())
            .map_err(|_| "memory map")?;
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
        let e820 = memory::build(V::NAME, &regions, TopOfMemory { tom, tom2 }, &reservations)
            .map_err(|_| "memory map")?;
        if let Err(step) = crate::memory::cache_ram(&regions) {
            crate::flow::fail::<Self>(step);
        }
        let ffs = V::LAYOUT.ffs.ok_or("boot media")?;
        fstart_arch::x86::boot::enable_boot_media_rom_cache();
        crate::media::mount(e820.entries(), &V::LAYOUT, ffs).map_err(|error| error.step())?;
        Ok(Mapped {
            top_mem: tom,
            top_mem2: tom2,
            apob: None,
            tseg: tseg.base,
            e820,
            ffs: ffs.base..ffs.end(),
            nvram: None,
            options: fstart_options::Report::new(),
            flow: FlowState { apob },
        })
    }

    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        mp: &MpHandle,
        flow: &FlowState,
    ) -> Result<(), &'static str> {
        use super::smm::{Config as SmmConfig, Smi};
        use fstart_driver_amd::fch::smi;
        step::<V>(0x58, "SMM");
        if state.psp_bar == 0 {
            return Err("PSP BAR");
        }
        let report = &raw mut SMM_REPORT;
        let fch = &ctx.hw.fch;
        let smi = Smi {
            fch,
            config: SmmConfig {
                ports: super::acpi::soc(V::PMBASE, false).ports,
                psp_bar: state.psp_bar,
                einj: ctx.config.einj,
                report: report as u64,
            },
        };
        crate::tseg::install(V::NAME, mp, &smi, ctx.memory.tseg, TSEG_SIZE)?;
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
                state.einj = einj::<V>(ctx, state.psp_bar.into(), flow)
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
        flow: &FlowState,
        identity: &crate::acpi::SmbiosIdentity<'_>,
        board: crate::acpi::Board,
    ) -> Tables {
        step::<V>(0x70, "ACPI, SMBIOS");
        if let Some(einj) = silicon.state.einj {
            silicon.tables.push(fstart_acpi::einj::table(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
                &psp::einj::entries(einj.mailbox, einj.buffer),
            ));
        }
        let soc = super::acpi::soc(V::PMBASE, ctx.config.legacy_devices);
        let rsdp = super::tables::acpi::<V>(ctx, silicon, pci, flow, &soc, &board);
        let smbios = super::tables::smbios::<V>(ctx, silicon, pci, flow, identity);
        Tables {
            rsdp,
            smbios,
            s5: fstart_core::description::AcpiSleepControl::new(
                soc.ports.control,
                crate::acpi::S5_SLEEP_TYPE,
            ),
        }
    }

    fn map_payload_ram(
        e820: &fstart_core::services::memory_detect::E820State,
        pci: &PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        crate::noncar::boot::map_ram(e820, pci)
    }
}

/// Query `MP0_C2PMSG_31` for the PSP's EINJ command buffer inside the ABL's
/// `ReservedRasEinj` hole and lay out the trigger action table and DF vendor
/// descriptor (matching Venice `AmdPlatformRasWhDxe::ApeiEinjInstall`).
fn einj<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    bar: u64,
    flow: &FlowState,
) -> Result<Einj, &'static str> {
    use fstart_driver_amd::apob::rs::HOLE_RAS_EINJ;
    use psp::einj::{Buffer, ERROR_TYPES, MAILBOX_OFFSET, Mailbox};
    let map = flow
        .apob
        .system_memory_map()
        .map_err(|_| "APOB V2 sysmap")?;
    let hole = sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied())
        .map_err(|_| "memory map")?
        .into_iter()
        .find(|r| r.kind == sysmap::RegionKind::Unknown(HOLE_RAS_EINJ))
        .ok_or("no ReservedRasEinj hole")?;
    // SAFETY: die 0 RB 0 PSP BAR mapped by `init_fch`; BSP owns the RAS mailbox before OS boot.
    let mailbox = unsafe { Mailbox::mmio(bar) };
    let base = mailbox
        .buffer(&ctx.clock, &(hole.base..hole.end()))
        .map_err(|_| "PSP EINJ buffer")?;
    // SAFETY: PSP command buffer inside the reserved ABL hole, identity mapped.
    let buffer = unsafe { Buffer::at(base) };
    let trigger = fstart_acpi::einj::trigger(&[psp::einj::trigger(base)]);
    let types = buffer.prepare(&trigger).map_err(|_| "EINJ trigger table")?;
    if types.is_set(ERROR_TYPES::VENDOR) {
        let df = fstart_pci::EcamDevice::new(0, 0x18, 0).read32(0);
        buffer.describe_vendor_device(0x18, 0, df);
    }
    // SAFETY: ring 0, caches enabled; flush buffer for PSP DRAM reads.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };
    fstart_log::info!(
        "{}: PSP EINJ buffer {:#x}, error types {:#x}",
        V::NAME,
        base,
        types.get()
    );
    Ok(Einj {
        mailbox: bar + MAILBOX_OFFSET,
        buffer: base,
    })
}
