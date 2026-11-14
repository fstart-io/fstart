//! The silicon steps of the IOD SoCs: DF3, the SSP/GN NBIO quadrants, SMU
//! V11, two-pass DXIO, Huashan FCHs and the per-CCD SMU thread release
//! (INTEGRATION_NOTES §3–§9). A step takes the SoC's die as `V`: what
//! Milan does differently (fused NBIO quadrants, the IOMMU BAR in PEI,
//! SMEE-dependent address bits, the PSP FCH lock) is a branch on
//! `V::SILICON`, which a board that carries one of the two SoCs links
//! only one side of.
extern crate alloc;
use crate::sp3::silicon::cpu::{Sp3Cpu, release_all, this_thread};
use crate::sp3::silicon::hw::{Pci, Tsc, msr, secondary_fch};
use crate::sp3::silicon::hw::{SMU_POLL_US, SMU_TIMEOUT_US, primary_fch};
use crate::sp3::silicon::steps::*;
use alloc::vec::Vec;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::MpHandle;
use fstart_driver_amd::apob::ccx::{CoreMap, core_map};
use fstart_driver_amd::apob::phy::phy_override;
use fstart_driver_amd::ccx::init::Ccx;
use fstart_driver_amd::ccx::microcode;
use fstart_driver_amd::ccx::release::{self, Launch};
use fstart_driver_amd::ccx::topology::{ApicLayout, Location};
use fstart_driver_amd::ccx::x2apic::{self, ApicMode};
use fstart_driver_amd::ccx::{brand, mca, pm as power, security};
use fstart_driver_amd::cpu;
use fstart_driver_amd::df::regs;
use fstart_driver_amd::df::topology::MAX_ROOT_BRIDGES;
use fstart_driver_amd::df::v3::{Df3, SocketMemory};
use fstart_driver_amd::dxio;
use fstart_driver_amd::dxio::encode::Ancillary;
use fstart_driver_amd::dxio::lanes::Engine;
use fstart_driver_amd::dxio::ports::{FchPerst, PortSetup};
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::dxio::{DMA_SIZE, DmaBuffer, Dxio, Unit};
use fstart_driver_amd::fch::huashan::usb::UsbPolicy;
use fstart_driver_amd::fch::hw as fch;
use fstart_driver_amd::nbio::fuse::Fuses;
use fstart_driver_amd::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_amd::nbio::iohc::DramTops;
use fstart_driver_amd::nbio::iohc::PSP_BAR_SIZE as BAR_SIZE;
use fstart_driver_amd::nbio::{Nbio, ssp as nbio_ssp};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::Smu as SmuMailbox;
use fstart_driver_amd::smu::v11::{self, CcxDonePolicy, FeaturePolicy, Sp3Workarounds};
use fstart_driver_amd::window::Window;
use tock_registers::interfaces::Readable;
use tock_registers::interfaces::Writeable;
use zerocopy::IntoBytes;
pub type DfVersion = Df3;
/// SMU V11 mailbox of one socket.
pub type Smu = SmuMailbox<Pci, Tsc>;
/// The per-socket silicon handles the later steps use (Rome and Milan).
/// Opaque outside this crate.
pub struct Silicon {
    /// Every present NBIO, physical order (= `Topology::root_bridges`).
    pub nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES>,
    pub smus: heapless::Vec<Smu, 2>,
    /// APOB 3/3 core map per socket.
    pub maps: heapless::Vec<CoreMap, 2>,
    /// The threads MP brings up, the order the SMU releases them in and
    /// each socket's SMN, once the CPU driver is made.
    threads: u16,
    order: heapless::Vec<Launch, { release::MAX_THREADS }>,
    smns: heapless::Vec<Smn<Pci>, 2>,
    cac: power::CacWeights,
}
const NBIO_SMU_BAR: u64 = 1 << 20;
const IOMMU_BAR: u64 = 512 << 10;
const IOAPIC_BAR: u64 = 0x1000;

/// Every socket's first root bus ([`socket_bus`]), indexed by socket.
fn socket_buses(ctx: &Ctx) -> Result<[u8; 2], &'static str> {
    let mut buses = [0; 2];
    for socket in 0..ctx.topology.sockets.min(2) {
        buses[usize::from(socket)] = socket_bus(ctx, socket)?;
    }
    Ok(buses)
}

/// Socket `socket`'s first root bus: its IOD's SMN and SMU.
fn socket_bus(ctx: &Ctx, socket: u8) -> Result<u8, &'static str> {
    ctx.topology
        .root_bridges
        .iter()
        .filter(|rb| rb.socket == socket)
        .map(|rb| rb.bus_base)
        .min()
        .step("socket root bus")
}

/// The UMC channels with DIMMs, from the APOB SPD copy (the AGESA memory
/// info PPI the scrubber setup reads, SOC_DF §3.6).
fn socket_memory(ctx: &Ctx, socket: u8) -> SocketMemory {
    let present = fstart_driver_amd::apob::mem::spd(&ctx.apob, socket, 0)
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

/// The APIC IDs of a socket's threads from its core map (logical CCD /
/// complex / core positions, CCX §3.7), for the PPTable CPPC map.
fn apic_ids(generation: cpu::Silicon, socket: u8, map: &CoreMap) -> impl Iterator<Item = u16> + '_ {
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

/// Start the SMU hotplug firmware for the board's I2C-signaled slots and
/// release every hotplug port to train (Oxide `zen_fabric_pcie_hotplug_init`,
/// the vendor's `SmuV11DxeGN` hotplug path): the switch base, the table on
/// every socket, the slots, the I2C master for the SMU, the flags and the
/// start on socket 0, then each slot's power from its presence.
fn hotplug<V: super::Variant>(
    ctx: &Ctx,
    setup: &PortSetup<'_, '_, Pci, FchPerst<'_>>,
    smus: &[Smu],
    policy: &dxio::hotplug::HotplugPolicy,
) -> Result<(), &'static str> {
    use fstart_driver_amd::dxio::hotplug;
    let table = match setup.hotplug_table() {
        Ok(table) => table,
        Err(error) => {
            let detail = alloc::format!("{error:?}");
            fstart_log::error!("dxio: hotplug table: {}", detail.as_str());
            None
        }
    };
    let Some(table) = table else {
        let ports = setup.hotplug_prepare();
        if ports > 0 {
            fstart_log::info!("dxio: {} native hotplug ports released", ports as u32);
        }
        return Ok(());
    };
    let bytes = zerocopy::IntoBytes::as_bytes(&table);
    let buffer = dma_alloc(bytes.len(), 0x1000)?;
    buffer.copy_from_slice(bytes);
    let sockets: heapless::Vec<&Smu, 2> = smus.iter().collect();
    if let Err(error) = hotplug::load_v11(V::SILICON, &sockets, buffer.as_ptr() as u64, policy) {
        let detail = alloc::format!("{error:?}");
        fstart_log::error!("dxio: hotplug table load: {}", detail.as_str());
        return Ok(());
    }
    let ports = setup.hotplug_prepare();
    // SAFETY: board I2C users of controller 0 run before DXIO; the SMU
    // owns the bus from here on.
    if unsafe { fch::smbus::i2c_for_smu(ctx.timer) }.is_err() {
        fstart_log::error!("dxio: I2C0 not ready for the SMU hotplug firmware");
    }
    if let Err(error) = hotplug::enable_v11(sockets[0], policy) {
        let detail = alloc::format!("{error:?}");
        fstart_log::error!("dxio: hotplug start: {}", detail.as_str());
        return Ok(());
    }
    // The SMU may hide the slots' bridges shortly after the start.
    fstart_core::services::Timer::delay_ms(&ctx.timer, 100);
    setup.hotplug_started();
    fstart_log::info!("dxio: hotplug started, {} ports", ports as u32);
    Ok(())
}

/// CPPC as AmdAcpiDxe and the CCX PCCT service set it up after CCX done:
/// query each socket's SMU (`CppcGetInfo`); initialize one 64 KiB PCC
/// region per enabled controller with its own signature, doorbell and local
/// PPTable slot map. One aligned MTRR extent makes every region uncached
/// on all threads. A failed controller is not advertised.
fn cppc<V: super::Variant>(
    ctx: &Ctx,
    si: &Silicon,
    mp: &MpHandle,
) -> Result<Vec<Cppc>, &'static str> {
    use fstart_arch::x86::mtrr;
    use fstart_driver_amd::smu::v11::cppc::{REGION_SIZE, signature};

    let generation = V::SILICON;
    if !FeaturePolicy::default().cppc {
        return Ok(Vec::new());
    }
    // A socket's SMU serves only that socket's PPTable CPPC map. Never
    // append another socket's CPUs to a region initialized only by SMU0.
    let controllers: Vec<_> = si
        .smus
        .iter()
        .enumerate()
        .filter_map(|(socket, smu)| {
            let info = v11::cppc_info(smu).ok()?;
            if !info.enabled() {
                return None;
            }
            let map = si.maps.get(socket)?;
            let bar = si
                .nbios
                .iter()
                .zip(&ctx.topology.root_bridges)
                .filter(|(_, rb)| usize::from(rb.socket) == socket)
                .find_map(|(nbio, _)| nbio.smu_bar())?;
            Some((socket as u8, smu, map, bar, info))
        })
        .collect();
    if controllers.is_empty() {
        return Ok(Vec::new());
    }
    // Cover adjacent per-controller regions with one power-of-two MTRR,
    // rather than consuming a variable MTRR for every socket.
    let size = REGION_SIZE * controllers.len().next_power_of_two() as u64;
    let index = (0..unsafe { mtrr::variable_count() })
        .find(|&i| !mtrr::is_valid_mask(unsafe { mtrr::read_variable(i) }.1));
    let Some(index) = index else {
        fstart_log::warn!("cppc: no free variable MTRR");
        return Ok(Vec::new());
    };
    let region = dma_alloc(size as usize, size as usize)?;
    let base = region.as_ptr() as u64;
    // SAFETY: all CPUs are firmware-owned and parked; this fresh allocation
    // is reserved through handoff and aligned for the single MTRR extent.
    let uncached = || unsafe {
        fstart_arch::x86::writeback_invalidate_caches();
        mtrr::disable_cache();
        mtrr::set_variable(index, base, size, 0);
        mtrr::enable_cache();
    };
    mp.scope(|s| s.broadcast(&uncached));
    uncached();
    let mut channels = Vec::new();
    for (physical, (socket, smu, map, smu_bar, info)) in controllers.into_iter().enumerate() {
        let base = base + physical as u64 * REGION_SIZE;
        let subspace = channels.len() as u8;
        // SAFETY: the header belongs to this fresh, uncached PCC region.
        unsafe { fstart_core::mmio::write32(base as *mut u32, signature(subspace)) };
        if v11::cppc_set_table(smu, base).is_err() {
            fstart_log::warn!("cppc: socket {} table initialization failed", socket);
            continue;
        }
        let (lowest_mhz, nominal_mhz) = v11::cppc_frequencies(smu, generation).unwrap_or((0, 0));
        let slots = apic_ids(generation, socket, map).map(u32::from).collect();
        fstart_log::info!(
            "cppc: socket {} subspace {} PCC {:#x} MTRR {} revision {} mask {:#x} {}-{} MHz",
            socket,
            subspace,
            base,
            index,
            info.revision,
            info.supported,
            lowest_mhz,
            nominal_mhz
        );
        channels.push(Cppc {
            base,
            smu_bar,
            info,
            lowest_mhz,
            nominal_mhz,
            slots,
        });
    }
    Ok(channels)
}

impl Silicon {
    pub fn df_init<V: super::Variant>(ctx: &mut Ctx) -> Result<(), &'static str> {
        let memory: heapless::Vec<SocketMemory, 2> = (0..ctx.topology.sockets)
            .map(|s| socket_memory(ctx, s))
            .collect();
        match V::SILICON {
            cpu::Silicon::Ssp => fstart_driver_amd::df::ssp::init(
                &ctx.df,
                &ctx.topology,
                ctx.identity.revision,
                &Default::default(),
                &memory,
            ),
            cpu::Silicon::Gn => fstart_driver_amd::df::gn::init(
                &ctx.df,
                &ctx.topology,
                &Default::default(),
                &memory,
            ),
            _ => return Err("DF init"),
        }
        Ok(())
    }

    /// NBIO PEI (NBIO_SMU §3.2–3.5; `nbio::sp3` module order): SB location,
    /// socket table, base init with the SMU BAR, the PSP BAR per socket, the
    /// IOMMU, TOM.
    pub fn nbio_early<V: super::Variant>(ctx: &mut Ctx) -> Result<Silicon, &'static str> {
        let generation = V::SILICON;
        let policy = ctx.policy.nbio;
        let buses = match generation {
            cpu::Silicon::Gn => socket_buses(ctx)?,
            _ => [0; 2],
        };
        let fuses: heapless::Vec<Fuses, 2> = (0..ctx.topology.sockets)
            .map(|s| match generation {
                // SAFETY: read-only fuse words through the socket's SMN.
                cpu::Silicon::Gn => unsafe {
                    Fuses::read(&Smn::on_bus(ctx.pci, buses[usize::from(s)]))
                },
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
        nbios.iter().for_each(nbio_ssp::sb_location);
        nbios
            .iter()
            .enumerate()
            .filter(|(i, _)| first_of_socket(*i))
            .for_each(|(_, n)| nbio_ssp::socket_table(n, &policy));
        for (i, n) in nbios.iter().enumerate() {
            nbio_ssp::base_init(n, &policy);
            let bar = ctx.non_pci[i].alloc(NBIO_SMU_BAR).step("SMU BAR space")?;
            n.set_smu_bar(bar);
        }
        // PSP BAR on IOHC0 of each socket (PSP_APOB §3.2).
        for (i, rb) in ctx.topology.root_bridges.iter().enumerate() {
            if rb.index != 0 {
                continue;
            }
            let base = ctx.non_pci[i].alloc(BAR_SIZE).step("PSP BAR space")?;
            nbios[i].set_psp_bar(base);
            let bar = nbios[i].psp_bar().step("PSP BAR")?;
            if rb.socket == 0 {
                ctx.psp_bar = bar;
                msr().reg(fstart_driver_amd::ccx::msr::PSP_ADDRESS).set(bar);
            }
        }
        if generation == cpu::Silicon::Gn {
            for (i, n) in nbios.iter().enumerate() {
                n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).step("IOMMU BAR space")?);
            }
        }
        nbios.iter().for_each(|n| nbio_ssp::iommu_init(n, &policy));
        let tops = DramTops {
            tom: ctx.tops.tom,
            tom2: ctx.tops.tom2,
        };
        nbios.iter().for_each(|n| n.set_tom(tops));
        Ok(Silicon {
            nbios,
            smus: heapless::Vec::new(),
            maps: heapless::Vec::new(),
            threads: 0,
            order: heapless::Vec::new(),
            smns: heapless::Vec::new(),
            cac: power::CacWeights::new(),
        })
    }

    /// SMU per socket (NBIO_SMU §3.8): mailbox, PPTable with the board's power
    /// policy, tools DRAM, SP3 workarounds, SYSHUB bit 11 per NBIO.
    pub fn smu_init<V: super::Variant>(
        ctx: &mut Ctx,
        si: &mut Silicon,
    ) -> Result<(), &'static str> {
        let generation = V::SILICON;
        for s in 0..ctx.topology.sockets {
            // SAFETY: the BSP owns this bus's SMN index/data pair during init.
            let smn = unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, s)?) };
            let smu = Smu::new(smn, v11::MAILBOX, ctx.timer, v11::id_limit(generation))
                .with_timeout_us(SMU_TIMEOUT_US)
                .with_poll_us(SMU_POLL_US);
            let _ = si.smus.push(smu);
            let map = core_map(&ctx.apob, s).step("APOB core map")?;
            let _ = si.maps.push(map);
        }
        let power = ctx.policy.power;
        for (s, smu) in si.smus.iter().enumerate() {
            let ids: Vec<u16> = apic_ids(generation, s as u8, &si.maps[s]).collect();
            let table: &[u8] = match generation {
                cpu::Silicon::Ssp => {
                    let t = v11::pptable::RomePpTable::new(&power, &ids);
                    alloc::boxed::Box::leak(alloc::boxed::Box::new(t)).as_bytes()
                }
                cpu::Silicon::Gn => {
                    let t = v11::pptable::MilanPpTable::new(&power, &ids);
                    alloc::boxed::Box::leak(alloc::boxed::Box::new(t)).as_bytes()
                }
                _ => return Err("PPTable"),
            };
            let dma = dma_alloc(table.len(), 0x1000)?;
            dma.copy_from_slice(table);
            v11::transfer_pptable(smu, dma.as_ptr() as u64, dma.len() as u32).step("PPTable")?;
            let pages = if generation == cpu::Silicon::Gn { 4 } else { 1 };
            let tools = dma_alloc(pages * 0x1000, 0x1000)?;
            v11::set_tools_dram(smu, tools.as_ptr() as u64).step("SMU tools DRAM")?;
            v11::sp3_workarounds(smu, generation, &Sp3Workarounds::default())
                .step("SMU workarounds")?;
        }
        si.nbios.iter().for_each(nbio_ssp::smu_syshub_bit11);
        Ok(())
    }

    /// Socket 0's USB (needs its SMU) and socket 1's FCH (FCH_SP3 §3.1, §4.2).
    pub fn fch_secondaries<V: super::Variant>(
        ctx: &mut Ctx,
        si: &Silicon,
    ) -> Result<(), &'static str> {
        let generation = V::SILICON;
        fstart_driver_amd::fch::huashan::usb::reset(
            &ctx.fch,
            generation,
            0,
            &si.smus[0],
            &UsbPolicy::default_for(generation, 0),
            ctx.resume,
        )
        .step("USB socket 0")?;
        if ctx.topology.sockets > 1 {
            let fch1 = secondary_fch(socket_bus(ctx, 1)?, ctx.timer);
            fch::multi::huashan_socket1(
                &fch1,
                generation,
                &si.smus[1],
                &UsbPolicy::default_for(generation, 1),
                &ctx.policy.sata[1],
                ctx.resume,
            )
            .step("FCH socket 1")?;
        }
        Ok(())
    }

    /// Two-pass DXIO training of every socket with the board's engines, then
    /// the port setup (PCIE_DXIO §3.3–3.9).
    pub fn dxio<V: super::Variant>(
        ctx: &mut Ctx,
        si: &Silicon,
        engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
    ) -> Result<(), &'static str> {
        let generation = V::SILICON;
        let policy = ctx.policy.dxio;
        // The vendor PCIe speed workaround reads its fuse on socket 0 (bus 0).
        // Milan skips the fuse test when PCD 0xAF is 0x40; where that PCD
        // comes from is not decoded, so Milan takes the test too.
        // SAFETY: a read-only SMN register of socket 0.
        let fuse =
            unsafe { Smn::on_bus(ctx.pci, socket_bus(ctx, 0)?).read32(dxio::ssp::SPEED_FUSE) };
        fstart_log::info!("dxio: speed fuse {:#x}", fuse);
        let mut engines = engines.clone();
        engines
            .iter_mut()
            .for_each(|e| dxio::ssp::speed_fixup(e, Some(fuse)));
        let engines = &engines;
        // DXIO requests (ReloadSm, the state machine) can take long; AGESA
        // polls without a bound. Own mailboxes with a 30 s bound.
        let buses = socket_buses(ctx)?;
        let dxio_smus: heapless::Vec<Smu, 2> = (0..ctx.topology.sockets)
            .map(|s| {
                // SAFETY: the BSP owns this bus's SMN index/data pair during init.
                let smn = unsafe { Smn::on_bus(ctx.pci, buses[usize::from(s)]) };
                Smu::new(smn, v11::MAILBOX, ctx.timer, v11::id_limit(generation))
                    .with_poll_us(SMU_POLL_US)
                    .with_timeout_us(30_000_000)
            })
            .collect();
        let _ = &si.smus;
        let phy = if generation == cpu::Silicon::Gn {
            phy_override(&ctx.apob).step("APOB PHY override")?
        } else {
            None
        };
        match phy {
            Some(bytes) => {
                fstart_log::info!("dxio: APOB PHY override {} bytes", bytes.len() as u32)
            }
            None => fstart_log::info!("dxio: APOB PHY override absent"),
        }
        let ancillary = Ancillary {
            phy_override: phy,
            ..Default::default()
        };
        let mut units: heapless::Vec<Unit<'_, Smu>, 2> = heapless::Vec::new();
        for (s, smu) in dxio_smus.iter().enumerate() {
            let buffer = dma_alloc(DMA_SIZE, 0x1000)?;
            // SAFETY: a firmware-owned, identity-mapped buffer for this unit only.
            let dma =
                unsafe { DmaBuffer::new(buffer.as_mut_ptr(), DMA_SIZE, buffer.as_ptr() as u64) }
                    .step("DXIO DMA buffer")?;
            let dxio = Dxio::new(smu, v11::msg::DXIO);
            if let Some(unit) = Unit::new(
                generation,
                (s as u8, 0),
                dxio,
                dma,
                engines,
                &policy,
                &ancillary,
            )
            .step("DXIO engines")?
            {
                let _ = units.push(unit);
            }
        }
        // Socket 0's reset pins through host MMIO, as the donors do: on Milan
        // SMN writes to them are lost. Socket 1 through its SMN (the SSP donor;
        // GN relocates socket 1's ACPI-MMIO instead, which fstart does not).
        let fch0 = primary_fch(ctx.timer);
        let fch1 = if ctx.topology.sockets > 1 {
            Some(secondary_fch(socket_bus(ctx, 1)?, ctx.timer))
        } else {
            None
        };
        let mut fchs: heapless::Vec<((u8, u8), &dyn Window), 8> = heapless::Vec::new();
        let _ = fchs.push(((0, 0), &fch0.acpi));
        if let Some(fch1) = &fch1 {
            let _ = fchs.push(((1, 0), &fch1.acpi));
        }
        // SAFETY: every socket's FCH is live; DXIO owns their PERST GPIOs.
        let perst = unsafe { FchPerst::new(generation, fchs, None) };
        let mut setup = PortSetup::new(generation, &si.nbios, engines, perst)
            .with_features(policy.pcie_features);
        trained(dxio::train(
            generation, &mut units, &mut setup, &ctx.timer, &policy,
        ))?;
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
        ctx.ports =
            dxio::ports::attached(&ctx.topology, ctx.pci_root.segment, engines, setup.ports());
        hotplug::<V>(ctx, &setup, &dxio_smus, &policy.hotplug)?;
        // 2P xGMI (`dxio::ports::xgmi_init`) needs F1x330 and the CCD mask of
        // each socket; not wired until a 2P board exists.
        if ctx.topology.sockets > 1 {
            fstart_log::warn!("dxio: 2P xGMI init not wired");
        }
        Ok(())
    }

    /// SATA env per socket (FCH_SP3 §3.1).
    pub fn fch_env<V: super::Variant>(ctx: &Ctx, _si: &Silicon) -> Result<(), &'static str> {
        if !fstart_driver_amd::fch::huashan::sata::env(&ctx.fch, V::SILICON, &ctx.policy.sata[0]) {
            // Enclosure messages only drive LEDs; the controller stays usable.
            fstart_log::warn!("sp3: SATA socket 0 SGPIO messages timed out");
        }
        if ctx.topology.sockets > 1 {
            let fch1 = secondary_fch(socket_bus(ctx, 1)?, ctx.timer);
            if !fch::multi::huashan_socket1_env(&fch1, V::SILICON, &ctx.policy.sata[1]) {
                fstart_log::warn!("sp3: SATA socket 1 SGPIO messages timed out");
            }
        }
        Ok(())
    }

    /// CCX on the BSP, SMU release of every thread through the reset vector,
    /// CCX on every thread, then the BSP tail and CCX-done per socket
    /// (CCX_SP3 §2, NBIO_SMU §3.8).
    /// CCX on the BSP and the driver every thread runs, with the patches
    /// of `microcode`; the order the threads are released in.
    pub fn cpu_driver<V: super::Variant>(
        ctx: &mut Ctx,
        si: &mut Silicon,
        microcode: Option<&'static [u8]>,
    ) -> Result<Sp3Cpu, &'static str> {
        let generation = V::SILICON;
        let policy = ctx.policy.ccx;
        let threads: usize = si.maps.iter().map(CoreMap::threads).sum();
        let decision = x2apic::decide(generation, policy.apic, threads);
        if decision.mode == ApicMode::X2Apic || decision.downcore_to_7 {
            // The arch MP core drives the LAPIC through xAPIC MMIO.
            fstart_log::error!("sp3: {} threads need x2APIC", threads as u32);
            return Err("APIC mode");
        }
        let ccx = Ccx::new(ctx.identity, decision.mode, policy.table).step("CCX")?;
        let cpu = msr();
        let bsp = this_thread(generation);
        ccx.run_tables(&cpu, &bsp);
        brand::write(&cpu, &brand::read(&si.smus[0], brand::message(generation)));
        let candidates = microcode_candidates(microcode);
        let patch = microcode::select(ctx.identity.signature, candidates.iter().copied());
        match &patch {
            Some(p) => {
                // SAFETY: ring 0 with caching enabled; the FFS copy of the
                // patch is identity mapped and stays reserved.
                unsafe { ccx.load_microcode(&cpu, &bsp, p) }.step("microcode")?;
                fstart_log::info!("sp3: microcode patch {:#x}", p.revision());
            }
            None => fstart_log::warn!("sp3: no microcode patch for this CPU"),
        }
        ccx.early(&cpu, &bsp);
        // SAFETY: DfGlobalCtrl of node 0, read only.
        let df_global = unsafe { ctx.df.node(0, regs::GLOBAL_CTRL) }.get();
        let wdt = policy
            .wdt
            .and_then(|w| power::wdt_setting(generation, w, df_global));
        let cac = power::read_cac_weights(generation, &si.smus[0]).step("CAC weights")?;
        let order = release::sp3_order(&si.maps).step("thread release order")?;

        let mut psp_addr = [None; 2];
        for (i, rb) in ctx.topology.root_bridges.iter().enumerate() {
            if rb.index == 0 {
                psp_addr[usize::from(rb.socket & 1)] = si.nbios[i].psp_bar();
            }
        }
        let driver = Sp3Cpu::new(
            ccx,
            policy,
            patch,
            wdt,
            policy.wdt_severity,
            cac.clone(),
            psp_addr,
        );
        let buses = socket_buses(ctx)?;
        let smns: heapless::Vec<Smn<Pci>, 2> = (0..ctx.topology.sockets)
            // SAFETY: the BSP owns this bus's SMN index/data pair during init.
            .map(|s| unsafe { Smn::on_bus(ctx.pci, buses[usize::from(s)]) })
            .collect();
        // SMEE on the BSP before the APs copy its SYSCFG (the donor's order):
        // set afterwards, the APs disagree and the OS clears SME/SEV per CPU.
        security::set_smee(&cpu, policy.smee);
        si.threads = threads as u16;
        si.order = order;
        si.smns = smns;
        si.cac = cac;
        Ok(driver)
    }

    pub fn threads(si: &Silicon) -> u16 {
        si.threads
    }

    pub fn release_aps<V: super::Variant>(_ctx: &Ctx, si: &Silicon, aps: u16) -> Result<(), u32> {
        let generation = V::SILICON;
        if usize::from(aps) != si.order.len() {
            return Err(0);
        }
        release_all(&si.order, |l: &Launch| {
            let s = usize::from(l.socket);
            release::launch(&si.smns[s], &si.smus[s], l, generation)
        })
    }

    /// Every thread ran its CCX steps: the BSP's last ones, then the SMU's
    /// CCX-done.
    pub fn cpus_up<V: super::Variant>(
        ctx: &mut Ctx,
        si: &mut Silicon,
        mp: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<(), &'static str> {
        let generation = V::SILICON;
        let cpu = msr();
        let bsp = this_thread(generation);
        let cac = si.cac.clone();
        mca::init(&cpu, &bsp);
        power::write_cac_weights(&cpu, &cac);

        ctx.apic_ids = apic_ids.to_vec();
        let rev = ctx.identity.revision;
        // Keep CC6 disabled until boot-time calibration finishes. Milan's early
        // CC6 PPI belongs to the topology-change reset path, not normal boot.
        let features = v11::feature_mask(rev, &FeaturePolicy::default());
        for (socket, smu) in si.smus.iter().enumerate() {
            v11::ccx_done(
                smu,
                generation,
                features,
                &CcxDonePolicy::donor(rev),
                &ctx.timer,
            )
            .inspect_err(|_| {
                fstart_log::error!("sp3: socket {} CCX completion failed", socket as u32);
            })
            .step("SMU CCX done")?;
        }
        {
            ctx.cppc = cppc::<V>(ctx, si, mp)?;
        }
        Ok(())
    }

    pub fn pci_pre<V: super::Variant>(ctx: &mut Ctx, si: &mut Silicon) -> Result<(), &'static str> {
        let generation = V::SILICON;
        set_fch_ioapic_id(generation);
        for (i, n) in si.nbios.iter().enumerate() {
            let rb = ctx.topology.root_bridges[i];
            n.set_ioapic_bar(ctx.non_pci[i].alloc(IOAPIC_BAR).step("IOAPIC BAR space")?);
            n.set_ioapic_id(nbio_ioapic_id(generation, 4 * rb.socket + rb.index));
            if generation == cpu::Silicon::Ssp {
                n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).step("IOMMU BAR space")?);
            }
        }
        Ok(())
    }

    /// After enumeration: NBIF HW-init lock, IOMMU enable.
    pub fn pci_post(_ctx: &Ctx, si: &Silicon) -> Result<(), &'static str> {
        for n in &si.nbios {
            n.nbif_hw_init_lock();
            n.iommu_enable();
        }
        Ok(())
    }

    /// SATA late tasks of both sockets (FCH_SP3 §2.3); Milan asks SMM for the PSP
    /// FCH lock (SW SMI D2) when the handler is installed.
    pub fn fch_late<V: super::Variant>(ctx: &Ctx, _si: &Silicon) -> Result<(), &'static str> {
        fstart_driver_amd::fch::huashan::sata::late(&ctx.fch, &ctx.policy.sata[0]);
        if ctx.topology.sockets > 1 {
            let fch1 = secondary_fch(socket_bus(ctx, 1)?, ctx.timer);
            fstart_driver_amd::fch::huashan::sata::late(&fch1, &ctx.policy.sata[1]);
        }
        if V::SILICON == cpu::Silicon::Gn && ctx.smm_installed {
            fstart_driver_amd::fch::huashan::smm::request_fch_lock(&ctx.fch);
        }
        Ok(())
    }

    /// The USB SMI acknowledges go to socket 0's IOD (both controllers there).
    pub fn usb_buses(ctx: &Ctx, _si: &Silicon) -> Result<[u8; 8], &'static str> {
        let mut buses = [0; 8];
        buses[0] = socket_bus(ctx, 0)?;
        Ok(buses)
    }

    pub fn usb_bus_count(_ctx: &Ctx) -> u8 {
        1
    }

    /// NUMA from the APOB NPS mode (ACPI_SMM §3.5): DRAM by the channel of
    /// each rule's CS, threads by their physical CCD (APOB 3/3 lists the
    /// present CCDs in logical order), root bridges by their IOMS quadrant.
    /// `None` with one domain (AmdAcpiDxe builds no SRAT then) or without
    /// NPS information.
    pub fn numa<V: super::Variant>(ctx: &Ctx, si: &Silicon) -> Option<NumaPlan> {
        use fstart_driver_amd::apob::nps;
        use fstart_driver_amd::df::Target;
        use fstart_driver_amd::df::dram::{self, ccd_channel, domain_of, ioms_channel};
        use fstart_driver_amd::df::v3;

        let mode = match nps::nps(&ctx.apob) {
            Ok(mode) => mode,
            Err(e) => {
                use fstart_driver_amd::apob::ApobError as E;
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
        // Each IOD's PIE describes its local DRAM, not the whole system.
        let memory = dram::memory_from_maps(
            (0..ctx.topology.sockets)
                .map(|node| dram::read::<Df3, _>(&ctx.df, node, Target::Instance(v3::PIE))),
            &domains,
        );
        let smt = this_thread(V::SILICON).smt();
        let layout = ApicLayout::new(V::SILICON, smt);
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
        Some(NumaPlan {
            domains: domains.iter().map(socket_of).collect(),
            memory,
            cpus,
            root_bridges,
        })
    }

    pub fn nbios(&self) -> &[Nbio<Pci>] {
        &self.nbios
    }
}
