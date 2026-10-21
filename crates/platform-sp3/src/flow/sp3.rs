//! Rome and Milan silicon steps: DF3, the SSP/GN NBIO quadrants, SMU V11,
//! two-pass DXIO, Huashan FCHs and the per-CCD SMU thread release
//! (INTEGRATION_NOTES §3–§9). Milan deltas are inline (`Generation::Milan`
//! branches): fused NBIO quadrants, the IOMMU BAR in PEI, early CC6, SMEE
//! dependent address bits, the PSP FCH lock.

extern crate alloc;

use alloc::vec::Vec;

use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::{AP_BOOTSTRAP_ADDRESS, MpConfig, MpHandle};
use fstart_driver_zen::apob::ccx::{CoreMap, core_map};
use fstart_driver_zen::ccx::release::{self, Launch};
use fstart_driver_zen::ccx::topology::{ApicLayout, Location};
use fstart_driver_zen::ccx::x2apic::{self, ApicMode};
use fstart_driver_zen::ccx::{ApInputs, Ccx, brand, mca, microcode, power, security};
use fstart_driver_zen::df::plan::Plan;
use fstart_driver_zen::df::topology::Topology;
use fstart_driver_zen::df::v3::{Df3, SocketMemory};
use fstart_driver_zen::df::{BusRange, Df, Version, program, regs};
use fstart_driver_zen::dxio::engine::MAX_ENGINES;
use fstart_driver_zen::dxio::lanes::Engine;
use fstart_driver_zen::dxio::ports::{FchPerst, PortSetup};
use fstart_driver_zen::dxio::{self, DMA_SIZE, DmaBuffer, Dxio, Unit};
use fstart_driver_zen::fch;
use fstart_driver_zen::fch::huashan::{sata::SataPolicy, usb::UsbPolicy};
use fstart_driver_zen::nbio::fuse::Fuses;
use fstart_driver_zen::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_zen::nbio::iohc::DramTops;
use fstart_driver_zen::nbio::{Nbio, sp3 as nbio_sp3};
use fstart_driver_zen::psp::bar::BAR_SIZE;
use fstart_driver_zen::psp::directory::{Efs, bios};
use fstart_driver_zen::smn::Smn;
use fstart_driver_zen::smu::mailbox::Mailbox;
use fstart_driver_zen::smu::v11::{self, CcxDonePolicy, FeaturePolicy, Sp3Workarounds};
use fstart_driver_zen::soc::Generation;
use fstart_driver_zen::topology::MAX_ROOT_BRIDGES;
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use super::{Ctx, OrHalt, dma_alloc, fail};
use crate::board::Board;
use crate::entry::ApRoute;
use crate::hw::{Pci, Tsc, msr, secondary_fch};
use crate::mp::{Sp3Cpu, release_all, this_thread};

super::steps!();

pub(crate) const MMIO_PAIRS: u8 = <Df3 as Version>::MMIO_PAIRS;

/// SMU V11 mailbox of one socket.
pub(crate) type Smu = Mailbox<Pci, Tsc>;

/// The per-socket silicon handles the later steps use.
pub(crate) struct Silicon {
    /// Every present NBIO, physical order (= `Topology::root_bridges`).
    pub nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES>,
    pub smus: heapless::Vec<Smu, 2>,
    /// APOB 3/3 core map per socket.
    pub maps: heapless::Vec<CoreMap, 2>,
}

const NBIO_SMU_BAR: u64 = 1 << 20;
const IOMMU_BAR: u64 = 512 << 10;
const IOAPIC_BAR: u64 = 0x1000;

pub(crate) fn bus_maps(df: &Df<Pci>, topology: &mut Topology, ranges: &[BusRange]) {
    program::bus_maps::<Df3, _>(df, topology, ranges);
}

pub(crate) fn program_maps(df: &Df<Pci>, topology: &Topology, plan: &Plan) {
    program::mmio::<Df3, _>(df, &topology.nodes, &plan.mmio);
    program::io::<Df3, _>(df, &topology.nodes, &plan.io);
    program::vga::<Df3, _>(df, &topology.nodes, topology.primary().fabric_id);
}

/// Socket `socket`'s first root bus: its IOD's SMN and SMU.
fn socket_bus(ctx: &Ctx, socket: u8) -> u8 {
    ctx.topology
        .root_bridges
        .iter()
        .filter(|rb| rb.socket == socket)
        .map(|rb| rb.bus_base)
        .min()
        .or_halt("socket root bus")
}

/// The UMC channels with DIMMs, from the APOB SPD copy (the AGESA memory
/// info PPI the scrubber setup reads, SOC_DF §3.6).
fn socket_memory(ctx: &Ctx, socket: u8) -> SocketMemory {
    let present = fstart_driver_zen::apob::mem::spd(&ctx.apob, socket, 0)
        .map(|t| {
            t.dimms()
                .filter(|(_, _, r)| r.present())
                .fold(0u16, |m, (c, _, _)| m | 1 << c)
        })
        .unwrap_or(0);
    SocketMemory {
        channels: 8,
        present,
    }
}

pub(crate) fn df_init(ctx: &mut Ctx) {
    let memory: heapless::Vec<SocketMemory, 2> = (0..ctx.topology.sockets)
        .map(|s| socket_memory(ctx, s))
        .collect();
    match ctx.generation {
        #[cfg(feature = "rome")]
        Generation::Rome => fstart_driver_zen::df::rome::init(
            &ctx.df,
            &ctx.topology,
            ctx.identity.revision,
            &Default::default(),
            &memory,
        ),
        #[cfg(feature = "milan")]
        Generation::Milan => {
            fstart_driver_zen::df::milan::init(&ctx.df, &ctx.topology, &Default::default(), &memory)
        }
        _ => fail("DF init"),
    }
}

/// NBIO PEI (NBIO_SMU §3.2–3.5; `nbio::sp3` module order): SB location,
/// socket table, base init with the SMU BAR, the PSP BAR per socket, the
/// IOMMU, TOM.
pub(crate) fn nbio_early(ctx: &mut Ctx) -> Silicon {
    let generation = ctx.generation;
    let policy = ctx.policy.nbio;
    let fuses: heapless::Vec<Fuses, 2> = (0..ctx.topology.sockets)
        .map(|s| match generation {
            // SAFETY: read-only fuse words through the socket's SMN.
            Generation::Milan => unsafe { Fuses::read(&Smn::new(ctx.pci, socket_bus(ctx, s))) },
            _ => Fuses::NONE,
        })
        .collect();
    let nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES> = ctx
        .topology
        .root_bridges
        .iter()
        // SAFETY: discovered root bridges with routed buses; the BSP owns
        // the SMN pairs during boot.
        .map(|rb| unsafe { Nbio::new(ctx.pci, generation, rb, fuses[usize::from(rb.socket)]) })
        .collect();
    let first_of_socket = |i: usize| {
        let rb = &ctx.topology.root_bridges[i];
        !ctx.topology.root_bridges[..i]
            .iter()
            .any(|r| r.socket == rb.socket)
    };
    nbios.iter().for_each(nbio_sp3::sb_location);
    nbios
        .iter()
        .enumerate()
        .filter(|(i, _)| first_of_socket(*i))
        .for_each(|(_, n)| nbio_sp3::socket_table(n, &policy));
    for (i, n) in nbios.iter().enumerate() {
        nbio_sp3::base_init(n, &policy);
        let bar = ctx.non_pci[i].alloc(NBIO_SMU_BAR).or_halt("SMU BAR space");
        n.set_smu_bar(bar);
    }
    // PSP BAR on IOHC0 of each socket (PSP_APOB §3.2).
    for (i, rb) in ctx.topology.root_bridges.iter().enumerate() {
        if rb.index != 0 {
            continue;
        }
        let base = ctx.non_pci[i].alloc(BAR_SIZE).or_halt("PSP BAR space");
        nbios[i].set_psp_bar(base);
        let bar = nbios[i].psp_bar().or_halt("PSP BAR");
        if rb.socket == 0 {
            ctx.psp_bar = bar;
            msr().write(fstart_driver_zen::psp::bar::MSR_PSP_ADDR, bar);
        }
    }
    if generation == Generation::Milan {
        for (i, n) in nbios.iter().enumerate() {
            n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).or_halt("IOMMU BAR space"));
        }
    }
    nbios.iter().for_each(|n| nbio_sp3::iommu_init(n, &policy));
    let tops = DramTops {
        tom: ctx.tops.tom,
        tom2: ctx.tops.tom2,
    };
    nbios.iter().for_each(|n| n.set_tom(tops));
    Silicon {
        nbios,
        smus: heapless::Vec::new(),
        maps: heapless::Vec::new(),
    }
}

/// The APIC IDs of a socket's threads from its core map (logical CCD /
/// complex / core positions, CCX §3.7), for the PPTable CPPC map.
fn apic_ids(generation: Generation, socket: u8, map: &CoreMap) -> impl Iterator<Item = u16> + '_ {
    let smt = map.cores().next().is_some_and(|(_, _, c)| c.threads[1]);
    let layout = ApicLayout::new(generation, smt);
    map.ccds.iter().enumerate().flat_map(move |(d, ccd)| {
        ccd.ccxs.iter().enumerate().flat_map(move |(x, ccx)| {
            ccx.cores.iter().enumerate().flat_map(move |(c, core)| {
                (0..2u8)
                    .filter(move |&t| core.threads[usize::from(t)])
                    .map(move |t| {
                        layout.encode(Location {
                            socket,
                            die: d as u8,
                            ccx: x as u8,
                            core: c as u8,
                            thread: t,
                        }) as u16
                    })
            })
        })
    })
}

/// SMU per socket (NBIO_SMU §3.8): mailbox, PPTable with the board's power
/// policy, tools DRAM, SP3 workarounds, SYSHUB bit 11 per NBIO.
pub(crate) fn smu_init(ctx: &mut Ctx, si: &mut Silicon) {
    let generation = ctx.generation;
    for s in 0..ctx.topology.sockets {
        let smn = Smn::new(ctx.pci, socket_bus(ctx, s));
        // SAFETY: the socket's SMU mailbox, owned by the BSP during boot.
        let smu = unsafe { Smu::new(smn, v11::REGS, ctx.timer, v11::id_limit(generation)) };
        let _ = si.smus.push(smu);
        let map = core_map(&ctx.apob, s).or_halt("APOB core map");
        let _ = si.maps.push(map);
    }
    #[cfg(any(feature = "rome", feature = "milan"))]
    let power = ctx.policy.power;
    for (s, smu) in si.smus.iter().enumerate() {
        let ids: Vec<u16> = apic_ids(generation, s as u8, &si.maps[s]).collect();
        let table: &[u8] = match generation {
            #[cfg(feature = "rome")]
            Generation::Rome => {
                let t = v11::pptable::RomePpTable::new(&power, &ids);
                alloc::boxed::Box::leak(alloc::boxed::Box::new(t)).as_bytes()
            }
            #[cfg(feature = "milan")]
            Generation::Milan => {
                let t = v11::pptable::MilanPpTable::new(&power, &ids);
                alloc::boxed::Box::leak(alloc::boxed::Box::new(t)).as_bytes()
            }
            _ => fail("PPTable"),
        };
        let dma = dma_alloc(table.len(), 0x1000);
        dma.copy_from_slice(table);
        v11::transfer_pptable(smu, dma.as_ptr() as u64, dma.len() as u32).or_halt("PPTable");
        let pages = if generation == Generation::Milan {
            4
        } else {
            1
        };
        let tools = dma_alloc(pages * 0x1000, 0x1000);
        v11::set_tools_dram(smu, tools.as_ptr() as u64).or_halt("SMU tools DRAM");
        v11::sp3_workarounds(smu, generation, &Sp3Workarounds::default())
            .or_halt("SMU workarounds");
    }
    si.nbios.iter().for_each(nbio_sp3::smu_syshub_bit11);
}

/// Socket 0's USB (needs its SMU) and socket 1's FCH (FCH_SP3 §3.1, §4.2).
pub(crate) fn fch_secondaries(ctx: &mut Ctx, si: &Silicon) {
    let generation = ctx.generation;
    fch::huashan::usb::reset(
        &ctx.fch,
        generation,
        0,
        &si.smus[0],
        &UsbPolicy::default_for(generation, 0),
        ctx.resume,
    )
    .or_halt("USB socket 0");
    if ctx.topology.sockets > 1 {
        let fch1 = secondary_fch(socket_bus(ctx, 1), ctx.timer);
        fch::multi::huashan_socket1(
            &fch1,
            generation,
            &si.smus[1],
            &UsbPolicy::default_for(generation, 1),
            &SataPolicy::socket1(),
            ctx.resume,
        )
        .or_halt("FCH socket 1");
    }
}

/// Two-pass DXIO training of every socket with the board's engines, then
/// the port setup (PCIE_DXIO §3.3–3.9).
pub(crate) fn dxio(
    ctx: &mut Ctx,
    si: &Silicon,
    engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
) {
    let generation = ctx.generation;
    let policy = ctx.policy.dxio;
    // DXIO requests (ReloadSm, the state machine) can take long; AGESA
    // polls without a bound. Own mailboxes with a 30 s bound.
    let dxio_smus: heapless::Vec<Smu, 2> = (0..ctx.topology.sockets)
        .map(|s| {
            let smn = Smn::new(ctx.pci, socket_bus(ctx, s));
            // SAFETY: the socket's SMU mailbox; the BSP sends one message at
            // a time and the step's other mailbox is idle meanwhile.
            unsafe { Smu::new(smn, v11::REGS, ctx.timer, v11::id_limit(generation)) }
                .with_timeout_us(30_000_000)
        })
        .collect();
    let _ = &si.smus;
    let mut units: heapless::Vec<Unit<'_, Smu>, 2> = heapless::Vec::new();
    for (s, smu) in dxio_smus.iter().enumerate() {
        let buffer = dma_alloc(DMA_SIZE, 0x1000);
        // SAFETY: a firmware-owned, identity-mapped buffer for this unit only.
        let dma = unsafe { DmaBuffer::new(buffer.as_mut_ptr(), DMA_SIZE, buffer.as_ptr() as u64) }
            .or_halt("DXIO DMA buffer");
        let dxio = Dxio::new(smu, v11::msg::DXIO);
        if let Some(unit) =
            Unit::new(generation, s as u8, 0, dxio, dma, engines, &policy).or_halt("DXIO engines")
        {
            let _ = units.push(unit);
        }
    }
    let fchs = (0..ctx.topology.sockets)
        .map(|s| ((s, 0), Smn::new(ctx.pci, socket_bus(ctx, s))))
        .collect();
    // SAFETY: every socket's FCH is live; DXIO owns their PERST GPIOs.
    let perst = unsafe { FchPerst::new(generation, fchs, None) };
    let mut setup = PortSetup::new(generation, &si.nbios, engines, perst);
    super::dxio_or_halt(dxio::train(generation, &mut units, &mut setup, &ctx.timer, &policy));
    setup.finish();
    setup.clear_early_buses();
    for p in setup.ports() {
        fstart_log::info!("dxio: port {}.{} mapped", p.socket, p.remap_index() as u32);
    }
    // 2P xGMI (`dxio::ports::xgmi_init`) needs F1x330 and the CCD mask of
    // each socket; not wired until a 2P board exists.
    if ctx.topology.sockets > 1 {
        fstart_log::warn!("dxio: 2P xGMI init not wired");
    }
}

/// SATA env per socket (FCH_SP3 §3.1).
pub(crate) fn fch_env(ctx: &Ctx, _si: &Silicon) {
    fch::huashan::sata::env(&ctx.fch, ctx.generation, &SataPolicy::socket0());
    if ctx.topology.sockets > 1 {
        let fch1 = secondary_fch(socket_bus(ctx, 1), ctx.timer);
        fch::multi::huashan_socket1_env(&fch1, ctx.generation, &SataPolicy::socket1());
    }
}

fn wbinvd() {
    // SAFETY: cache write-back only.
    unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) }
}

/// Patches of BIOS directory entry 0x66, copied to RAM (the patch loader
/// reads them at their address; flash is uncached).
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

/// CCX on the BSP, SMU release of every thread through the reset vector,
/// CCX on every thread, then the BSP tail and CCX-done per socket
/// (CCX_SP3 §2, NBIO_SMU §3.8).
pub(crate) fn mp<B: Board>(ctx: &mut Ctx, si: &Silicon) -> MpHandle {
    let generation = ctx.generation;
    let policy = ctx.policy.ccx;
    let threads: usize = si.maps.iter().map(CoreMap::threads).sum();
    let decision = x2apic::decide(generation, policy.apic, threads);
    if decision.mode == ApicMode::X2Apic || decision.downcore_to_7 {
        // The arch MP core drives the LAPIC through xAPIC MMIO.
        fstart_log::error!("sp3: {} threads need x2APIC", threads as u32);
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
    match &patch {
        Some(p) => {
            ccx.load_microcode(&cpu, &bsp, p, wbinvd)
                .or_halt("microcode");
            fstart_log::info!("sp3: microcode patch {:#x}", p.patch_id());
        }
        None => fstart_log::warn!("sp3: no microcode patch for this CPU"),
    }
    ccx.early(&cpu, &bsp);
    let features = v11::feature_mask(ctx.identity.revision, &FeaturePolicy::default());
    if generation == Generation::Milan {
        si.smus
            .iter()
            .try_for_each(|smu| v11::early_cc6(smu, features, 0))
            .or_halt("SMU early CC6");
    }
    // SAFETY: DfGlobalCtrl of node 0, read only.
    let df_global = unsafe { ctx.df.node(0, regs::GLOBAL_CTRL) }.get();
    let wdt = policy
        .wdt
        .and_then(|w| power::wdt_setting(generation, w, df_global));
    let cac = power::read_cac_weights(generation, &si.smus[0]).or_halt("CAC weights");
    let order = release::sp3_order(&si.maps).or_halt("thread release order");

    let mut psp_addr = [None; 2];
    for (i, rb) in ctx.topology.root_bridges.iter().enumerate() {
        if rb.index == 0 {
            psp_addr[usize::from(rb.socket & 1)] = si.nbios[i].psp_bar();
        }
    }
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
        psp_addr,
    );
    let smns: heapless::Vec<Smn<Pci>, 2> = (0..ctx.topology.sockets)
        .map(|s| Smn::new(ctx.pci, socket_bus(ctx, s)))
        .collect();
    let route = ApRoute::to(AP_BOOTSTRAP_ADDRESS).or_halt("AP route");
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&driver];
    let mut launch = |aps: u16| {
        if usize::from(aps) != order.len() {
            return Err(0);
        }
        route_aps(route);
        release_all(&order, |l: &Launch| {
            let s = usize::from(l.socket);
            release::launch(&smns[s], &si.smus[s], l, generation)
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
    let rev = ctx.identity.revision;
    for smu in &si.smus {
        v11::ccx_done(
            smu,
            generation,
            features,
            &CcxDonePolicy::donor(rev),
            &ctx.timer,
        )
        .or_halt("SMU CCX done");
    }
    mp
}

/// PCI enumeration prerequisites (NBIO_SMU §3.5 step 6): NBIO IOAPIC BAR
/// and ID, Rome's IOMMU BAR; the FCH IOAPIC ID.
pub(crate) fn pci_pre(ctx: &mut Ctx, si: &mut Silicon) {
    let generation = ctx.generation;
    super::set_fch_ioapic_id(generation);
    for (i, n) in si.nbios.iter().enumerate() {
        let rb = ctx.topology.root_bridges[i];
        n.set_ioapic_bar(ctx.non_pci[i].alloc(IOAPIC_BAR).or_halt("IOAPIC BAR space"));
        n.set_ioapic_id(nbio_ioapic_id(generation, 4 * rb.socket + rb.index));
        if generation == Generation::Rome {
            n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).or_halt("IOMMU BAR space"));
        }
    }
}

/// After enumeration: NBIF HW-init lock, IOMMU enable.
pub(crate) fn pci_post(_ctx: &Ctx, si: &Silicon) {
    for n in &si.nbios {
        n.nbif_hw_init_lock();
        n.iommu_enable();
    }
}

/// FCH late tasks of socket 0 (FCH_SP3 §2.3); Milan asks SMM for the PSP
/// FCH lock (SW SMI D2) when the handler is installed.
pub(crate) fn fch_late(ctx: &Ctx, _si: &Silicon) {
    fch::huashan::sata::late(&ctx.fch, &SataPolicy::socket0());
    if ctx.generation == Generation::Milan && ctx.smm.is_some() {
        fch::huashan::smm::request_fch_lock(&ctx.fch);
    }
}

/// The USB SMI acknowledges go to socket 0's IOD (both controllers there).
pub(crate) fn usb_buses(ctx: &Ctx, _si: &Silicon) -> [u8; 8] {
    let mut buses = [0; 8];
    buses[0] = socket_bus(ctx, 0);
    buses
}

pub(crate) fn usb_bus_count(_ctx: &Ctx) -> u8 {
    1
}
