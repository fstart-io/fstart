//! Rome and Milan silicon steps: DF3, the SSP/GN NBIO quadrants, SMU V11,
//! two-pass DXIO, Huashan FCHs and the per-CCD SMU thread release
//! (INTEGRATION_NOTES §3–§9). Milan deltas are inline (`Generation::Milan`
//! branches): fused NBIO quadrants, the IOMMU BAR in PEI, early CC6, SMEE
//! dependent address bits, the PSP FCH lock.

extern crate alloc;

use alloc::vec::Vec;

use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::{AP_BOOTSTRAP_ADDRESS, MpConfig, MpHandle};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::sp3::apob::ccx::{CoreMap, core_map};
use fstart_driver_amd::sp3::ccx::release::{self, Launch};
use fstart_driver_amd::sp3::ccx::topology::{ApicLayout, Location};
use fstart_driver_amd::sp3::ccx::x2apic::{self, ApicMode};
use fstart_driver_amd::sp3::ccx::{ApInputs, Ccx, brand, mca, microcode, power, security};
use fstart_driver_amd::sp3::df::plan::Plan;
use fstart_driver_amd::sp3::df::topology::Topology;
use fstart_driver_amd::sp3::df::v3::{Df3, SocketMemory};
use fstart_driver_amd::sp3::df::{BusRange, Df, Version, program, regs};
use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::dxio::ports::{FchPerst, PortSetup};
use fstart_driver_amd::sp3::dxio::{self, DMA_SIZE, DmaBuffer, Dxio, Unit};
use fstart_driver_amd::sp3::fch;
use fstart_driver_amd::sp3::fch::huashan::{sata::SataPolicy, usb::UsbPolicy};
use fstart_driver_amd::sp3::nbio::fuse::Fuses;
use fstart_driver_amd::sp3::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_amd::sp3::nbio::iohc::DramTops;
use fstart_driver_amd::sp3::nbio::{Nbio, sp3 as nbio_sp3};
use fstart_driver_amd::sp3::psp::bar::BAR_SIZE;
use fstart_driver_amd::sp3::smu::mailbox::Mailbox;
use fstart_driver_amd::sp3::smu::v11::{self, CcxDonePolicy, FeaturePolicy, Sp3Workarounds};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::topology::MAX_ROOT_BRIDGES;
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use super::{Ctx, OrHalt, dma_alloc, fail, microcode_candidates};
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
    let present = fstart_driver_amd::sp3::apob::mem::spd(&ctx.apob, socket, 0)
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
        Generation::Rome => fstart_driver_amd::sp3::df::rome::init(
            &ctx.df,
            &ctx.topology,
            ctx.identity.revision,
            &Default::default(),
            &memory,
        ),
        #[cfg(feature = "milan")]
        Generation::Milan => fstart_driver_amd::sp3::df::milan::init(
            &ctx.df,
            &ctx.topology,
            &Default::default(),
            &memory,
        ),
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
            Generation::Milan => unsafe { Fuses::read(&Smn::on_bus(ctx.pci, socket_bus(ctx, s))) },
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
            msr().write(fstart_driver_amd::sp3::psp::bar::MSR_PSP_ADDR, bar);
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
        // SAFETY: the BSP owns this bus's SMN index/data pair during init.
        let smn = unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, s)) };
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
    // The vendor PCIe speed workaround reads its fuse on socket 0 (bus 0).
    // Milan skips the fuse test when PCD 0xAF is 0x40; where that PCD
    // comes from is not decoded, so Milan takes the test too.
    // SAFETY: a read-only SMN register of socket 0.
    let fuse = unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, 0)).read32(dxio::sp3::SPEED_FUSE) };
    fstart_log::info!("dxio: speed fuse {:#x}", fuse);
    let mut engines = engines.clone();
    engines
        .iter_mut()
        .for_each(|e| dxio::sp3::speed_fixup(e, Some(fuse)));
    let engines = &engines;
    // DXIO requests (ReloadSm, the state machine) can take long; AGESA
    // polls without a bound. Own mailboxes with a 30 s bound.
    let dxio_smus: heapless::Vec<Smu, 2> = (0..ctx.topology.sockets)
        .map(|s| {
            // SAFETY: the BSP owns this bus's SMN index/data pair during init.
            let smn = unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, s)) };
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
        // SAFETY: the BSP owns this bus's SMN index/data pair during init.
        .map(|s| ((s, 0), unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, s)) }))
        .collect();
    // SAFETY: every socket's FCH is live; DXIO owns their PERST GPIOs.
    let perst = unsafe { FchPerst::new(generation, fchs, None) };
    let mut setup = PortSetup::new(generation, &si.nbios, engines, perst);
    super::dxio_or_halt(dxio::train(
        generation, &mut units, &mut setup, &ctx.timer, &policy,
    ));
    for u in &units {
        for (i, e) in u.engines().iter().enumerate() {
            fstart_log::info!(
                "dxio: fw engine {} kind {} lanes {}-{} mac {} port {} bdf {:#x} train {:#x} caps {:#x}",
                i as u32,
                e.kind,
                e.start_lane,
                e.end_lane,
                e.mac_id,
                e.mac_port_id,
                e.bdf_num.get(),
                e.train_state,
                e.caps.get()
            );
        }
    }
    setup.finish();
    setup.clear_early_buses();
    si.nbios
        .iter()
        .for_each(|n| n.set_bus_routes(&ctx.topology.root_bridges));
    for p in setup.ports() {
        fstart_log::info!(
            "dxio: port {}.{} devfn {:#x} bridge {} trained {}",
            p.socket,
            p.nbio,
            p.devfn,
            p.bridge,
            p.trained as u8
        );
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

use fstart_arch::x86::writeback_invalidate_caches;

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
    let candidates = microcode_candidates();
    let patch = microcode::select(
        generation,
        ctx.identity.signature,
        candidates.iter().copied(),
    );
    match &patch {
        Some(p) => {
            // SAFETY: ring 0 with caching enabled.
            ccx.load_microcode(&cpu, &bsp, p, || unsafe { writeback_invalidate_caches() })
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
        // SAFETY: the BSP owns this bus's SMN index/data pair during init.
        .map(|s| unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, s)) })
        .collect();
    // SMEE on the BSP before the APs copy its SYSCFG (the donor's order):
    // set afterwards, the APs disagree and the OS clears SME/SEV per CPU.
    security::set_smee(&cpu, policy.smee);
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
    ctx.cppc = cppc(ctx, si, &mp);
    mp
}

/// CPPC as AmdAcpiDxe and the CCX PCCT service set it up after CCX done:
/// ask socket 0's SMU (`CppcGetInfo`); when enabled, a 64 KiB region
/// uncached on every thread by a free variable MTRR, the PCC signature,
/// `CppcSetTable` and the two `_CPC` frequencies.
fn cppc(ctx: &Ctx, si: &Silicon, mp: &MpHandle) -> Option<super::Cppc> {
    use fstart_arch::x86::mtrr;
    use fstart_driver_amd::sp3::smu::v11::cppc::{REGION_SIZE, SIGNATURE};

    let generation = ctx.generation;
    if !FeaturePolicy::default().cppc {
        return None;
    }
    let smu = si.smus.first()?;
    let info = v11::cppc_info(smu).ok()?;
    if !info.enabled() {
        fstart_log::info!(
            "cppc: off (answer {:#x})",
            info.supported << 2 | u32::from(info.revision)
        );
        return None;
    }
    let smu_bar = si.nbios.iter().find_map(|n| n.smu_bar())?;
    // SAFETY (MTRR reads/writes): BSP and parked APs run nothing else; the
    // region is fresh heap inside the reserved early region.
    let index = (0..unsafe { mtrr::variable_count() })
        .find(|&i| !mtrr::is_valid_mask(unsafe { mtrr::read_variable(i) }.1));
    let Some(index) = index else {
        fstart_log::warn!("cppc: no free variable MTRR");
        return None;
    };
    let region = dma_alloc(REGION_SIZE as usize, REGION_SIZE as usize);
    let base = region.as_ptr() as u64;
    let uncached = || unsafe {
        fstart_arch::x86::writeback_invalidate_caches();
        mtrr::disable_cache();
        mtrr::set_variable(index, base, REGION_SIZE, 0);
        mtrr::enable_cache();
    };
    mp.scope(|s| s.broadcast(&uncached));
    uncached();
    // SAFETY: the region is ours and now uncached.
    unsafe { core::ptr::write_volatile(base as *mut u32, SIGNATURE) };
    v11::cppc_set_table(smu, base).ok()?;
    let (lowest_mhz, nominal_mhz) = v11::cppc_frequencies(smu, generation).unwrap_or((0, 0));
    let slots = si
        .maps
        .iter()
        .enumerate()
        .flat_map(|(s, map)| apic_ids(generation, s as u8, map).map(u32::from))
        .collect();
    fstart_log::info!(
        "cppc: PCC {:#x} MTRR {} revision {} mask {:#x} {}-{} MHz",
        base,
        index,
        info.revision,
        info.supported,
        lowest_mhz,
        nominal_mhz
    );
    Some(super::Cppc {
        base,
        smu_bar,
        info,
        lowest_mhz,
        nominal_mhz,
        slots,
    })
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

/// NUMA from the APOB NPS mode (ACPI_SMM §3.5): DRAM by the channel of
/// each rule's CS, threads by their physical CCD (APOB 3/3 lists the
/// present CCDs in logical order), root bridges by their IOMS quadrant.
/// `None` with one domain (AmdAcpiDxe builds no SRAT then) or without
/// NPS information.
pub(crate) fn numa(ctx: &Ctx, si: &Silicon) -> Option<super::NumaPlan> {
    use fstart_driver_amd::sp3::apob::nps;
    use fstart_driver_amd::sp3::df::dram::{
        self, ccd_channel, df3_channel, domain_of, ioms_channel,
    };
    use fstart_driver_amd::sp3::df::{Target, v3};

    let mode = match nps::nps(&ctx.apob) {
        Ok(mode) => mode,
        Err(e) => {
            use fstart_driver_amd::sp3::apob::ApobError as E;
            let (what, value) = match e {
                E::NotFound { kind, .. } => ("missing", kind),
                E::TooShort { kind, .. } => ("short", kind),
                E::Value { value, .. } => ("value", value),
                _ => ("unreadable", 0),
            };
            fstart_log::warn!("numa: APOB NPS {} {:#x}: one domain", what, value);
            return None;
        }
    };
    fstart_log::info!("numa: NPS{}", mode.domains_per_socket().unwrap_or(0));
    let domains = dram::numa_domains(mode, ctx.topology.sockets);
    if domains.len() <= 1 {
        return None;
    }
    let memory = dram::read::<Df3, _>(&ctx.df, 0, Target::Instance(v3::PIE))
        .ranges()
        .filter_map(|(base, end, dst)| {
            let (socket, channel) = df3_channel(dst)?;
            Some((base, end, domain_of(&domains, socket, channel)?))
        })
        .collect();
    let smt = (fstart_arch::x86::cpuid(0x8000_001e).1 >> 8) & 0xff != 0;
    let layout = ApicLayout::new(ctx.generation, smt);
    let cpus = ctx
        .apic_ids
        .iter()
        .map(|&id| {
            let at = layout.decode(id);
            let ccd = si
                .maps
                .get(usize::from(at.socket))
                .and_then(|m| m.ccds.get(usize::from(at.die)))
                .map_or(at.die, |c| c.id);
            domain_of(&domains, at.socket, ccd_channel(ccd)).unwrap_or(0)
        })
        .collect();
    let root_bridges = ctx
        .topology
        .root_bridges
        .iter()
        .map(|rb| domain_of(&domains, rb.socket, ioms_channel(rb.index)).unwrap_or(0))
        .collect();
    let socket_of = |d: &dram::NumaDomain| (d.channels.trailing_zeros() / 16) as u8;
    Some(super::NumaPlan {
        domains: domains.iter().map(socket_of).collect(),
        memory,
        cpus,
        root_bridges,
    })
}
