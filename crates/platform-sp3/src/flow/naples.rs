//! Naples silicon steps: DF2, one NBIO and SMU V9 per ZP die, per-die
//! DXIO, Taishan FCHs on every die and the per-die SMU thread release
//! (INTEGRATION_NOTES §3–§9, the Naples paragraphs).

extern crate alloc;

use alloc::vec::Vec;

use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::{AP_BOOTSTRAP_ADDRESS, MpConfig, MpHandle};
use fstart_driver_zen::ccx::release::{self, Launch, SmuPwr};
use fstart_driver_zen::ccx::x2apic::{self, ApicMode};
use fstart_driver_zen::ccx::{ApInputs, Ccx, brand, mca, microcode, power, security};
use fstart_driver_zen::df::plan::Plan;
use fstart_driver_zen::df::topology::Topology;
use fstart_driver_zen::df::v2::Df2;
use fstart_driver_zen::df::{BusRange, Df, Version, program, regs};
use fstart_driver_zen::dxio::engine::MAX_ENGINES;
use fstart_driver_zen::dxio::lanes::Engine;
use fstart_driver_zen::dxio::ports::{FchPerst, PortSetup};
use fstart_driver_zen::dxio::{self, DMA_SIZE, DmaBuffer, Dxio, Unit};
use fstart_driver_zen::fch::multi::{self, NaplesDie};
use fstart_driver_zen::fch::taishan::{self, usb::DieUsb};
use fstart_driver_zen::nbio::fuse::Fuses;
use fstart_driver_zen::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_zen::nbio::iohc::DramTops;
use fstart_driver_zen::nbio::{Nbio, naples as nbio_naples};
use fstart_driver_zen::psp::bar::BAR_SIZE;
use fstart_driver_zen::psp::directory::{Efs, bios};
use fstart_driver_zen::smn::Smn;
use fstart_driver_zen::smu::mailbox::Mailbox;
use fstart_driver_zen::smu::v9;
use fstart_driver_zen::topology::MAX_ROOT_BRIDGES;
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use super::{Ctx, OrHalt, dma_alloc, fail};
use crate::board::Board;
use crate::entry::ApRoute;
use crate::hw::{Pci, Tsc, msr, secondary_fch};
use crate::mp::{Sp3Cpu, release_all, this_thread};

super::steps!();

pub(crate) const MMIO_PAIRS: u8 = <Df2 as Version>::MMIO_PAIRS;

/// SMU V9 mailbox of one die.
pub(crate) type Smu = Mailbox<Pci, Tsc>;

/// Per-die handles, physical order (= `Topology::root_bridges`: one root
/// bridge per die).
pub(crate) struct Silicon {
    pub nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES>,
    pub smus: heapless::Vec<Smu, MAX_ROOT_BRIDGES>,
}

const IOMMU_BAR: u64 = 512 << 10;
const IOAPIC_BAR: u64 = 0x1000;

pub(crate) fn bus_maps(df: &Df<Pci>, topology: &mut Topology, ranges: &[BusRange]) {
    program::bus_maps::<Df2, _>(df, topology, ranges);
}

pub(crate) fn program_maps(df: &Df<Pci>, topology: &Topology, plan: &Plan) {
    program::mmio::<Df2, _>(df, &topology.nodes, &plan.mmio);
    program::io::<Df2, _>(df, &topology.nodes, &plan.io);
    let primary = topology.primary().fabric_id;
    program::vga::<Df2, _>(df, &topology.nodes, primary);
}

/// Populated UMCs per node from the APOB SPD copy (bit = UMC).
fn umc_present(ctx: &Ctx) -> [u8; 8] {
    let mut present = [0u8; 8];
    for (node, n) in ctx.topology.nodes.iter().enumerate() {
        present[node] = fstart_driver_zen::apob::mem::spd(&ctx.apob, n.socket, n.die)
            .map(|t| {
                t.dimms()
                    .filter(|(_, _, r)| r.present())
                    .fold(0u8, |m, (c, _, _)| m | 1 << (c & 7))
            })
            .unwrap_or(0);
    }
    present
}

/// DF PEI init and, as this flow has no PEI/DXE split, the DXE-entry PIE
/// power management right after (SOC_DF §3, DF_ZP §3.9).
pub(crate) fn df_init(ctx: &mut Ctx) {
    use fstart_driver_zen::df::naples;
    let policy = naples::Policy::default();
    let rev = ctx.identity.revision;
    naples::init(&ctx.df, &ctx.topology, rev, &policy, &umc_present(ctx));
    naples::late_init(&ctx.df, &ctx.topology, rev, &policy, false);
}

/// NBIO base init per die, TOM, PSP BAR on every die (PSP_APOB §3.2:
/// AGESA only die 0; a superset).
pub(crate) fn nbio_early(ctx: &mut Ctx) -> Silicon {
    let policy = ctx.policy.nbio;
    let rev = ctx.identity.revision;
    let nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES> = ctx
        .topology
        .root_bridges
        .iter()
        // SAFETY: discovered root bridges with routed buses.
        .map(|rb| unsafe { Nbio::new(ctx.pci, ctx.generation, rb, Fuses::NONE) })
        .collect();
    let tops = DramTops {
        tom: ctx.tops.tom,
        tom2: ctx.tops.tom2,
    };
    for (i, n) in nbios.iter().enumerate() {
        nbio_naples::base_init(n, &policy, rev, None);
        n.set_tom(tops);
        n.set_psp_bar(ctx.non_pci[i].alloc(BAR_SIZE).or_halt("PSP BAR space"));
    }
    ctx.psp_bar = nbios[0].psp_bar().or_halt("PSP BAR");
    msr().write(fstart_driver_zen::psp::bar::MSR_PSP_ADDR, ctx.psp_bar);
    Silicon {
        nbios,
        smus: heapless::Vec::new(),
    }
}

/// SMU V9 DXE entry per die: policy table, AGM log, perfmon pages.
pub(crate) fn smu_init(ctx: &mut Ctx, si: &mut Silicon) {
    for n in &si.nbios {
        // SAFETY: the die's SMU mailbox, owned by the BSP during boot.
        let smu = unsafe { Smu::new(n.smn(), v9::REGS, ctx.timer, v9::ID_LIMIT) };
        let table = v9::PolicyTable::new(&v9::Policy::default());
        let dma = dma_alloc(0x1000, 0x1000);
        dma[..table.as_bytes().len()].copy_from_slice(table.as_bytes());
        let agm = dma_alloc(0x1000, 0x1000);
        let perfmon = dma_alloc(0x1000, 0x1000);
        let buffers = v9::DxeBuffers {
            policy_table: dma.as_ptr() as u64,
            agm_log: agm.as_ptr() as u64,
            perfmon: perfmon.as_ptr() as u64,
        };
        v9::dxe_init(&smu, n.bus(), &buffers).or_halt("SMU DXE init");
        let _ = si.smus.push(smu);
    }
}

/// USB of die 0 and the secondary FCHs of dies 1..7 (FCH_SP3 §4.1).
pub(crate) fn fch_secondaries(ctx: &mut Ctx, si: &Silicon) {
    taishan::usb::reset(&ctx.fch, DieUsb::default_for(0), ctx.resume);
    for (node, n) in si.nbios.iter().enumerate().skip(1) {
        let hw = secondary_fch(n.bus(), ctx.timer);
        multi::naples_die(&hw, &NaplesDie::default_for(node as u8), ctx.resume);
    }
}

/// DXIO per die, each run to completion (PCD 0x1E = 0, PCIE_DXIO §3.3).
pub(crate) fn dxio(
    ctx: &mut Ctx,
    si: &Silicon,
    engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
) {
    let generation = ctx.generation;
    let policy = ctx.policy.dxio;
    let mut units: heapless::Vec<Unit<'_, Smu>, MAX_ROOT_BRIDGES> = heapless::Vec::new();
    for (rb, smu) in ctx.topology.root_bridges.iter().zip(&si.smus) {
        let buffer = dma_alloc(DMA_SIZE, 0x1000);
        // SAFETY: a firmware-owned, identity-mapped buffer for this unit.
        let dma = unsafe { DmaBuffer::new(buffer.as_mut_ptr(), DMA_SIZE, buffer.as_ptr() as u64) }
            .or_halt("DXIO DMA buffer");
        let dxio = Dxio::new(smu, v9::msg::DXIO);
        if let Some(unit) = Unit::new(generation, rb.socket, rb.index, dxio, dma, engines, &policy)
            .or_halt("DXIO engines")
        {
            let _ = units.push(unit);
        }
    }
    let fchs = ctx
        .topology
        .root_bridges
        .iter()
        .map(|rb| ((rb.socket, rb.index), Smn::new(ctx.pci, rb.bus_base)))
        .collect();
    // The donor also drives PERST on bus 0x40 (die 1 of the equal split).
    let also = si.nbios.get(1).map(|n| n.smn());
    // SAFETY: live FCHs; DXIO owns their PERST GPIOs.
    let perst = unsafe { FchPerst::new(generation, fchs, also) };
    let mut setup = PortSetup::new(generation, &si.nbios, engines, perst);
    super::dxio_or_halt(dxio::train(
        generation, &mut units, &mut setup, &ctx.timer, &policy,
    ));
    setup.finish();
    setup.clear_early_buses();
}

/// SATA env (die 0) and the xGbE ports off (FCH_SP3 §3.2 af49).
pub(crate) fn fch_env(ctx: &Ctx, si: &Silicon) {
    taishan::sata::env(&ctx.fch, true);
    taishan::xgbe::disable(&ctx.fch, 0xf);
    for (node, n) in si.nbios.iter().enumerate().skip(1) {
        let hw = secondary_fch(n.bus(), ctx.timer);
        multi::naples_die_env(&hw, NaplesDie::default_for(node as u8).sata);
    }
}

use crate::hw::wbinvd;

fn microcode_candidates(ctx: &Ctx) -> Vec<&'static [u8]> {
    let Ok(dirs) = Efs::new(ctx.flash).and_then(|e| e.bios_directories(ctx.generation)) else {
        return Vec::new();
    };
    dirs.all(bios::MICROCODE)
        .filter_map(|(dir, e)| dir.data(e).ok())
        .map(|patch| {
            let copy = dma_alloc(patch.len(), 16);
            copy.copy_from_slice(patch);
            &*copy
        })
        .collect()
}

fn route_aps(route: ApRoute) {
    #[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
    crate::entry::set_ap_route(route);
    #[cfg(not(all(target_os = "none", fstart_entry = "amd-psp-reset")))]
    let _ = route;
}

/// CCX on the BSP, SMU release per die, CCX on every thread, CCX-done per
/// die (CCX_SP3 §2, NBIO_SMU §3.8 V9).
pub(crate) fn mp<B: Board>(ctx: &mut Ctx, si: &Silicon) -> MpHandle {
    let generation = ctx.generation;
    let policy = ctx.policy.ccx;
    let mut dies = [[None; 4]; 2];
    for (rb, n) in ctx.topology.root_bridges.iter().zip(&si.nbios) {
        // SAFETY: SMU::PWR::THREAD_CONFIGURATION of the die, read only.
        let cfg = unsafe { n.smn().read32(SmuPwr::NAPLES.thread_cfg) };
        dies[usize::from(rb.socket & 1)][usize::from(rb.index & 3)] = Some(cfg);
    }
    let order = release::naples_order(&dies[..usize::from(ctx.topology.sockets)])
        .or_halt("thread release order");
    let threads = order.len() + 1;
    let decision = x2apic::decide(generation, policy.apic, threads);
    if decision.mode == ApicMode::X2Apic {
        fail("APIC mode");
    }
    let ccx = Ccx::new(ctx.identity, decision.mode, policy.table).or_halt("CCX");
    let cpu = msr();
    let bsp = this_thread(generation);
    ccx.run_tables(&cpu, &bsp);
    brand::write(&cpu, &brand::read(generation, &si.smus[0]));
    let candidates = microcode_candidates(ctx);
    let patch = microcode::select(
        generation,
        ctx.identity.signature,
        candidates.iter().copied(),
    );
    if let Some(p) = &patch {
        ccx.load_microcode(&cpu, &bsp, p, wbinvd)
            .or_halt("microcode");
    }
    ccx.early(&cpu, &bsp);
    // SAFETY: DfGlobalCtrl of node 0, read only.
    let df_global = unsafe { ctx.df.node(0, regs::GLOBAL_CTRL) }.get();
    let wdt = policy
        .wdt
        .and_then(|w| power::wdt_setting(generation, w, df_global));
    let cac = power::read_cac_weights(generation, &si.smus[0]).or_halt("CAC weights");
    let driver = Sp3Cpu::new(
        ccx,
        &policy,
        ApInputs {
            microcode: patch,
            wdt,
            wdt_severity: policy.wdt_severity,
            cac_weights: &cac,
            rmp: None,
        },
        wdt,
        [Some(ctx.psp_bar), None],
    );
    let node = |l: &Launch| {
        ctx.topology
            .root_bridges
            .iter()
            .position(|rb| rb.socket == l.socket && rb.index == l.die)
    };
    let route = ApRoute::to(AP_BOOTSTRAP_ADDRESS).or_halt("AP route");
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&driver];
    let mut launch = |aps: u16| {
        if usize::from(aps) != order.len() {
            return Err(0);
        }
        route_aps(route);
        release_all(&order, |l| {
            let n = node(l).ok_or(release::ReleaseError::BadTopology {
                socket: l.socket,
                die: l.die,
            })?;
            release::launch(&si.nbios[n].smn(), &si.smus[n], l, generation)
        })
    };
    let mp = fstart_arch::x86::mp::mp_init_with_launcher(
        &MpConfig {
            cpu_drivers: &drivers,
            num_cpus: threads as u16,
            max_cpus: B::MAX_CPUS,
        },
        &mut launch,
    )
    .or_halt("MP init");
    route_aps(ApRoute::PARK);
    if driver.failures() != 0 {
        fail("AP microcode");
    }
    security::set_smee(&cpu, policy.smee);
    mca::init(&cpu, &bsp);
    power::write_cac_weights(&cpu, &cac);
    ctx.apic_ids = super::collect_apic_ids(&mp);
    // TODO(naples): the QOS bit (PCD 0x17) and the per-revision removals
    // are not decoded (NBIO_SMU open item); the SP3 base mask is sent.
    let features = v9::feature_mask(false, 0);
    for (i, smu) in si.smus.iter().enumerate() {
        v9::ccx_done(smu, features).or_halt("SMU CCX done");
        v9::after_ap_launch(smu, i == 0).or_halt("SMU after AP launch");
    }
    mp
}

/// IOAPIC BAR/ID and IOMMU BAR per die before enumeration.
pub(crate) fn pci_pre(ctx: &mut Ctx, si: &mut Silicon) {
    let generation = ctx.generation;
    super::set_fch_ioapic_id(generation);
    for (i, n) in si.nbios.iter().enumerate() {
        n.set_ioapic_bar(ctx.non_pci[i].alloc(IOAPIC_BAR).or_halt("IOAPIC BAR space"));
        n.set_ioapic_id(nbio_ioapic_id(generation, i as u8));
        n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).or_halt("IOMMU BAR space"));
    }
}

pub(crate) fn pci_post(_ctx: &Ctx, si: &Silicon) {
    for n in &si.nbios {
        n.nbif_hw_init_lock();
        n.iommu_enable();
    }
}

/// Taishan has no late task beyond the shared `common::late` one.
pub(crate) fn fch_late(_ctx: &Ctx, _si: &Silicon) {}

/// Every die's root bus: each die has its USB controllers.
pub(crate) fn usb_buses(ctx: &Ctx, si: &Silicon) -> [u8; 8] {
    let mut buses = [0; 8];
    si.nbios
        .iter()
        .zip(buses.iter_mut())
        .for_each(|(n, b)| *b = n.bus());
    let _ = ctx;
    buses
}

pub(crate) fn usb_bus_count(ctx: &Ctx) -> u8 {
    ctx.topology.root_bridges.len() as u8
}

/// Naples domains (one per die unless interleaved, ACPI_SMM §3.5) are not
/// wired: one domain.
pub(crate) fn numa(_ctx: &Ctx, _si: &Silicon) -> Option<super::NumaPlan> {
    None
}
