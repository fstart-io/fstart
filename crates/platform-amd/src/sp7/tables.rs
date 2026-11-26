// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 (Venice / WH) ACPI and SMBIOS table publication.

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use super::fabric;
use super::init::{State, ioapic_id};
use super::{FCH_IOAPIC_ID, Sp7, Variant};
use crate::acpi::{Cpu, Ioapic, Iommu, RootBridgeAcpi, prt};
use crate::flow::{Ctx, Silicon};
use fstart_acpi::ivrs::IommuRegisters;
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::ioapic::IoApic;
use fstart_core::services::memory_detect::E820Kind;
use fstart_driver_amd::apob::{
    event::DimmBits,
    smbios::dram_ranges,
    sysmap::{self, RegionKind},
};
use fstart_driver_amd::ccx;
use fstart_driver_amd::df::wh as df_wh;
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::nbio::{self, ioapic::NBIO_IOAPIC_PINS, wh as nbio_wh};
use fstart_driver_amd::psp;
use fstart_pci::{EcamDevice, PciAddress, PciEcam, ecam::GlobalEcam};

/// Storage for SP7 ACPI tables (page aligned, permanently reserved).
#[repr(align(4096))]
struct AcpiStorage([u8; super::acpi::TABLES_SIZE]);

static ACPI_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
static mut ACPI_TABLES: AcpiStorage = AcpiStorage([0; super::acpi::TABLES_SIZE]);

/// Storage for SP7 SMBIOS tables (4 KiB aligned, permanently reserved).
#[repr(C, align(4096))]
struct SmbiosStorage([u8; fstart_acpi::smbios::BUFFER_SIZE]);

static SMBIOS_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
static mut SMBIOS_TABLES: SmbiosStorage = SmbiosStorage([0; fstart_acpi::smbios::BUFFER_SIZE]);

/// Collect usable RAM ranges from final E820 entries (excluding firmware early
/// RAM, APOB, TSEG, and SMM reservations) for SRAT/NUMA intersection.
fn e820_ram_regions(ctx: &Ctx<'_, impl crate::soc::Init>) -> alloc::vec::Vec<sysmap::Region> {
    ctx.memory
        .e820
        .entries()
        .iter()
        .filter(|e| e.kind == E820Kind::Ram as u32 && e.size > 0)
        .map(|e| sysmap::Region {
            base: e.addr,
            size: e.size,
            kind: RegionKind::Ram,
        })
        .collect()
}

/// Read the active bridge table for one WH root bridge from its own
/// `IOHC::IOHC_Bridge_DevFn` registers, omitting unset `(0, 0)` slots and
/// hidden PCIe ports. Leave entries without a proven remap slot empty: the
/// 24 INTx routing descriptors are not 24 PCIe remap registers.
fn active_rb_bridges(rb_on_iod: u8, root: PciAddress) -> Option<alloc::vec::Vec<prt::Bridge>> {
    let layout = super::mpio::layout();
    // SAFETY: routed root bridge bus, BSP owns SMN during ACPI publication;
    // the handle is the root bridge's IOHC (B0's third is IOHC 3).
    let nb = unsafe { nbio_wh::RootBridge::on_root(GlobalEcam, layout.iohc(rb_on_iod), root) }?;
    let devfns = core::array::from_fn(|slot| layout.slot_devfn(&nb, slot as u8).unwrap_or((0, 0)));
    let all = super::acpi::bridges_from_devfns(devfns);
    let mut filtered = alloc::vec::Vec::new();
    for bridge in all {
        if bridge.device == 0 && bridge.function == 0 {
            continue;
        }
        let function = PciAddress::new(root.segment(), root.bus(), bridge.device, bridge.function);
        if !EcamDevice::new_address(function).is_present() {
            continue;
        }
        filtered.push(bridge);
    }
    Some(filtered)
}

/// Build and install the SP7 ACPI tables (FADT, MADT, MCFG, HPET, DSDT,
/// IVRS, SRAT, SLIT, MSCT, BERT, plus optional EINJ and board tables).
pub fn acpi<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    silicon: &Silicon<Sp7<V>>,
    pci: &PciEcam,
    soc: &crate::acpi::SocAcpi,
    board: &crate::acpi::Board,
) -> Option<u64> {
    if ACPI_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }
    let state = &silicon.state;
    let cpc = cpc::<V>(state, ctx.clock);
    let cxl_windows = super::cxl::windows(ctx.hw.apob.as_ref(), ctx.config.cxl.as_ref());
    let mut numa = numa::<V>(ctx, state, &silicon.cpus.apic_ids, &cxl_windows);
    // CXL windows: one CPU-less domain each, after the CPU domains.
    let cxl_sockets = numa
        .as_mut()
        .map(|n| super::cxl::add_domains(n, &cxl_windows))
        .unwrap_or_default();

    // First pass: capture per-root-bridge active bridge tables so each root
    // bridge carries its own IOHC DevFn mapping in `_PRT` and `RPxx` devices.
    let mut per_rb_bridges: alloc::vec::Vec<Option<alloc::vec::Vec<prt::Bridge>>> =
        alloc::vec::Vec::new();
    let mut matched_roots = alloc::vec::Vec::new();
    for root in pci.roots() {
        let info = root.info();
        let Some(rb) = (state.roots().iter())
            .find(|rb| (rb.segment, rb.bus_base) == (info.segment, info.bus_start))
        else {
            continue;
        };
        let (_, rb_on_iod) = fabric::rb_location(rb.index);
        per_rb_bridges.push(active_rb_bridges(
            rb_on_iod,
            fabric::root_bus(rb.segment, rb.bus_base),
        ));
        matched_roots.push((root, rb));
    }

    // The CXL host bridges' `_OSC` follows the error handling: AER and DPC
    // stay the firmware's when its SMM handles them (firmware-first, NBIO
    // mechanism 2 with the handler's GHESs), as every other root bridge's
    // (`acpi::dsdt`).
    let firmware_first = super::ras::firmware_first(ctx).enabled();
    let mut root_bridges: alloc::vec::Vec<RootBridgeAcpi<'_>> = alloc::vec::Vec::new();
    let mut matched = alloc::vec::Vec::new();
    let mut round_robin: alloc::vec::Vec<(alloc::vec::Vec<u32>, usize)> = alloc::vec::Vec::new();
    for ((root, rb), rb_bridges) in matched_roots.into_iter().zip(per_rb_bridges.iter()) {
        matched.push(rb);
        let bars = state.bars(rb.socket, rb.index);
        let (_, rb_on_iod) = fabric::rb_location(rb.index);
        let per_iod = state.devices().ios.count;
        // Validate IOAPIC ID and pin count against the 32-pin NBIO GSI window
        // allocated by `acpi::fixed::gsi_windows` so windows can never overlap.
        let ioapic = bars.ioapic.and_then(|base| {
            let expected_id = ioapic_id(state.system_index(rb.socket, rb.index)?)?;
            // SAFETY: IOAPIC MMIO was verified by `init_fch` and identity-mapped uncached.
            let dev = unsafe { IoApic::new(base as usize) };
            let pins = dev.redirection_entries();
            // `0xFF` is a valid ID (the 16th root bridge's, base `0xF0`);
            // all-ones MMIO fails the pin count.
            if dev.id() == expected_id && (1..=NBIO_IOAPIC_PINS).contains(&pins) {
                Some(Ioapic {
                    id: expected_id,
                    base,
                })
            } else {
                fstart_log::warn!(
                    "{}: skipping invalid IOAPIC on S{} RB{} (id {:#x}, pins {}, expected 1..={})",
                    V::NAME,
                    rb.socket,
                    rb.index,
                    dev.id(),
                    pins,
                    NBIO_IOAPIC_PINS
                );
                None
            }
        });
        let domain = numa.as_ref().and_then(|n| {
            let socket_rb = fabric::socket_rb(rb.index, per_iod);
            if n.ccx.is_empty() {
                return super::numa::rb_domain(n.nps, rb.socket, socket_rb, per_iod);
            }
            // CCX as NUMA: the root bridges naming the same domains take
            // them in turn (`PcdAmdFabricRoundRobinNumaDomainForCcx`).
            let list = super::numa::rb_ccx_domains(&n.ccx, rb.socket, socket_rb, per_iod);
            let turn = match round_robin.iter_mut().find(|(l, _)| *l == list) {
                Some((_, turn)) => turn,
                None => {
                    round_robin.push((list.clone(), 0));
                    &mut round_robin.last_mut()?.1
                }
            };
            let domain = list.get(*turn % list.len().max(1)).copied();
            *turn += 1;
            domain
        });
        root_bridges.push(RootBridgeAcpi {
            root,
            fch: rb.primary,
            domain,
            ioapic,
            ioapic_features: nbio::ioapic::IOAPIC_FEATURES_ENABLE.address()
                + (u32::from(super::mpio::layout().iohc(rb_on_iod)) * nbio_wh::RB_SMN_STRIDE),
            iommu: state.iommu(rb.socket, rb.index).map(|iommu| Iommu {
                base: iommu.base,
                registers: IommuRegisters {
                    capability: iommu.capability,
                    range: iommu.range,
                    misc: iommu.misc,
                    control: iommu.control,
                    efr: iommu.efr,
                    efr2: iommu.efr2,
                    counter_config: iommu.counter_config,
                },
                also: None,
            }),
            bridges: rb_bridges.as_deref(),
            cxl: super::cxl::is_host_bridge(state.cxl(), rb.socket, rb.index).then_some(
                crate::acpi::CxlHost {
                    firmware_first,
                    isolation: !(ctx.config.cxl.as_ref()).is_some_and(|c| c.dxe.mem_isolation),
                },
            ),
        });
    }

    // Each IOMMU also covers the IOMMU-less root bridge two above it on its
    // IOD (B0 and later; NbioGetBusDecodeRange).
    let keys: alloc::vec::Vec<(u8, u8)> = matched
        .iter()
        .map(|rb: &&fstart_driver_amd::df::RootBridge| (rb.socket, rb.index))
        .collect();
    let a0 = super::nbio::part().a0();
    for (rb, also) in root_bridges.iter_mut().zip(fabric::iommu_also(&keys, a0)) {
        if let Some(iommu) = rb.iommu.as_mut() {
            iommu.also = also;
        }
    }

    // First thread of every core first, matching vendor MADT ordering.
    let mut cpus: alloc::vec::Vec<Cpu> = silicon
        .cpus
        .apic_ids
        .iter()
        .enumerate()
        .map(|(i, &apic_id)| Cpu {
            apic_id,
            domain: numa.as_ref().map_or(0, |n| n.cpus[i]),
            aml: cpc.reference.clone(),
        })
        .collect();
    cpus.sort_unstable_by_key(|cpu| (cpu.apic_id & 1, cpu.apic_id));

    let domains: Option<alloc::vec::Vec<_>> = numa.as_ref().map(|n| {
        let cpu_domains = n.domains - cxl_sockets.len();
        (0..n.domains)
            .map(|d| match d.checked_sub(cpu_domains) {
                Some(cxl) => crate::acpi::Domain::cxl(cxl_sockets[cxl]),
                None => crate::acpi::Domain {
                    physical: n.physical(d),
                    ..crate::acpi::Domain::cpu(n.socket(d))
                },
            })
            .collect()
    });
    let memory: alloc::vec::Vec<_> = numa
        .iter()
        .flat_map(|n| &n.memory)
        .map(|&(base, end, domain)| crate::acpi::MemoryAffinity { base, end, domain })
        .collect();

    // SAFETY: BSP reading architectural C-state MSRs.
    let msr = unsafe { NativeMsr::new() };
    let fch = &ctx.hw.fch;
    // The FCH's controllers; Qilian's GPIO controller has 0x400 bytes of
    // banks and the remote block (GIGABYTE's `\_SB.GPIO`).
    let fch_acpi = fstart_driver_amd::fch::aml::FchAcpi {
        gpio: fstart_driver_amd::fch::aml::GpioBanks::QILIAN,
        ..fstart_driver_amd::fch::aml::FchAcpi::read(fch.mmio(), fch.io())
    };
    let mut sb_aml = fch_acpi
        .sb_aml()
        .inspect_err(|_| fstart_log::error!("ACPI: FCH devices"))
        .ok()?;
    sb_aml.extend_from_slice(&cpc.package);
    sb_aml.extend_from_slice(&silicon.aml);
    // HSMP: an `AMDI0097` per socket and the `HSMP` table.
    let hsmp = super::hsmp::instances(ctx, state);
    for instance in &hsmp {
        match crate::acpi::hsmp::device(instance) {
            Ok(aml) => sb_aml.extend_from_slice(&aml),
            Err(_) => fstart_log::error!("ACPI: HSMP device"),
        }
    }

    // APOB V2 7/6 ABL training/BIST/PMIC errors for BERT (Venice uses RS DIMM bits).
    let (abl, abl_truncated) = (ctx.hw.apob.as_ref())
        .map(|apob| apob.abl_errors(DimmBits::RS))
        .unwrap_or_default();
    if abl_truncated {
        fstart_log::warn!("{}: APOB V2 7/6 event log truncated", V::NAME);
    }

    // Gate boot-time SEL reporting on verified KCS I/O decode.
    let mut facts = *ctx.board;
    if !ctx.config.kcs_decoded {
        facts.bmc_event = None;
    }

    // The SoC's tables and the HMAT of its DRAM (Dell publishes it, PCD
    // 0x39B), with more than one domain.
    let mut tables = silicon.tables.clone();
    if !hsmp.is_empty() {
        tables.push(crate::acpi::hsmp::table(&hsmp));
    }
    // The FCH watchdog for the OS, as both donors' static WDAT, in the
    // period its counter was set to (`sp7::fch::primary`).
    let devices = &ctx.config.devices;
    if let Some(clock) = devices.wdat
        && devices.fch_reset.watchdog_decode
    {
        use fstart_driver_amd::fch::qilian::pm;
        let entries: alloc::vec::Vec<_> = pm::WDAT
            .iter()
            .map(
                |&(action, instruction, offset, value, mask)| fstart_acpi::wdat::Entry {
                    action,
                    instruction,
                    address: u64::from(fstart_driver_amd::fch::ACPI_MMIO + offset),
                    value,
                    mask,
                },
            )
            .collect();
        tables.push(fstart_acpi::wdat::build_wdat(
            fstart_acpi::OEM_ID,
            fstart_acpi::OEM_TABLE_ID,
            fstart_acpi::OEM_REVISION,
            &fstart_acpi::wdat::Watchdog {
                period_ms: clock.period_ms(),
                max_count: pm::WDAT_MAX_COUNT,
                min_count: pm::WDAT_MIN_COUNT,
                flags: pm::WDAT_FLAGS,
            },
            &entries,
        ));
    }
    if let Some(n) = numa.as_ref().filter(|n| n.domains > 1)
        && let Some(hmat) = hmat::<V>(ctx, state, n, cxl_sockets.len())
    {
        tables.push(hmat);
    }

    // SAFETY: guarded by `ACPI_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(ACPI_TABLES)).0 };
    crate::acpi::install(
        &crate::acpi::Soc {
            soc,
            pci: <Sp7<V> as crate::soc::Init>::pci_segment(ctx.hw),
            cpus: &cpus,
            fch_ioapic_id: FCH_IOAPIC_ID,
            root_bridges: &root_bridges,
            numa: domains.as_deref().map(|domains| crate::acpi::Numa {
                domains,
                memory: &memory,
                phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
            }),
            cstates: ccx::power::cstate_info(&msr),
            threads_per_core: ccx::Thread::current().threads_per_core(),
            devices: &silicon.devices,
            ivrs_devices: &[],
            sb_aml: &sb_aml,
            tables: &tables,
            abl: &abl,
            platform_errors: state.platform_errors(),
            firmware_first: Some(super::ras::firmware_first(ctx)).filter(|ff| ff.enabled()),
        },
        // Direct ACPI mode: no SMI command port.
        None,
        &facts,
        board,
        storage,
    )
    .inspect_err(|_| fstart_log::error!("{}: ACPI tables do not fit", V::NAME))
    .ok()
}

/// The HMAT of the DRAM domains (`FabricWhAcpiHmatInstall`): every domain
/// with CPUs an initiator, every CPU domain with DRAM a target whose
/// bandwidth is three quarters of its populated channels at its slowest
/// DIMM's rate (APOB `8/8`), cross-socket at most half the xGMI
/// bandwidth. The last `cxl` domains (CXL windows) are targets with their
/// devices' CDAT performance (`FabricHmatGetCxlCdatPerf`,
/// [`super::cxl_memory::hmat_targets`]).
fn hmat<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    numa: &super::numa::Numa,
    cxl: usize,
) -> Option<alloc::vec::Vec<u8>> {
    use crate::acpi::hmat;
    let slots = ctx.hw.apob.as_ref()?.dmi_slots().ok()?;
    let cpu_domains = numa.domains - cxl;
    let mut initiators: alloc::vec::Vec<(u32, u8)> = alloc::vec::Vec::new();
    for &d in &numa.cpus {
        if !initiators.iter().any(|i| i.0 == d) {
            initiators.push((d, numa.socket(d as usize)));
        }
    }
    initiators.sort_unstable();
    let populated = |socket: u8, channel: u8| {
        ctx.config
            .memory_channels
            .get(usize::from(socket))
            .is_some_and(|&mask| channel < 16 && mask & 1 << channel != 0)
    };
    let mut targets = alloc::vec::Vec::new();
    for d in 0..cpu_domains {
        if !numa.memory.iter().any(|m| m.2 == d as u32) {
            continue;
        }
        let channels = numa.channels(d);
        let dimms = slots.iter().filter(|s| {
            s.populated
                && populated(s.socket, s.channel)
                && channels >> (u32::from(s.socket) * 16 + u32::from(s.channel)) & 1 != 0
        });
        let mut used = 0u32;
        let mut mts = u32::MAX;
        for dimm in dimms {
            used |= 1 << (u32::from(dimm.socket) * 16 + u32::from(dimm.channel));
            mts = mts.min(dimm.configured_mts().unwrap_or(0).into());
        }
        if used == 0 || mts == 0 {
            continue;
        }
        targets.push(hmat::dram(d as u32, numa.socket(d), mts, used.count_ones()));
    }
    let cxl_windows = super::cxl::windows(ctx.hw.apob.as_ref(), ctx.config.cxl.as_ref());
    for (domain, socket, perf) in
        super::cxl_memory::hmat_targets(&state.cxl().memory, cpu_domains as u32, &cxl_windows)
            .into_iter()
            .take(cxl)
    {
        targets.push(hmat::Target {
            domain,
            socket,
            perf,
        });
    }
    let half = super::nbio::half_xgmi_bandwidth(ctx, state);
    fstart_log::info!(
        "{}: HMAT {} initiators, {} DRAM targets, half xGMI {:?} MB/s",
        V::NAME,
        initiators.len(),
        targets.len(),
        half
    );
    hmat::hmat(&initiators, &targets, half)
}

/// `_CPC` of every thread (CPPC in MSRs, the same for all; `ccx::cppc`),
/// from socket 0's SMU as AmdNbioDxe asks it: CppcGetInfo (39h) for the
/// revision, the implemented rows and whether CPPC is on, then the lowest
/// and nominal frequency (40h, 3Ah). Without an answer, no `_CPC` (and
/// no `amd-pstate`). One package, [`super::acpi::CPC_PATH`], every processor's
/// `_CPC` returns ([`super::acpi::shared_cpc`]).
fn cpc<V: Variant>(
    state: &State,
    clock: fstart_driver_amd::ccx::tsc::Tsc,
) -> super::acpi::SharedCpc {
    use fstart_driver_amd::ccx::cppc::{Integers, cpc_entries};
    use fstart_driver_amd::smu::v14::msg;
    let smu = state.smu(clock);
    let info = match msg::cppc_info(&smu) {
        Ok(Some(info)) => info,
        Ok(None) | Err(_) => {
            fstart_log::warn!("{}: SMU CPPC off or unanswered: no _CPC", V::NAME);
            return super::acpi::SharedCpc::default();
        }
    };
    let (lowest_mhz, nominal_mhz) = msg::cppc_frequencies(&smu).unwrap_or_else(|_| {
        fstart_log::warn!("{}: SMU CPPC frequencies unanswered", V::NAME);
        (0, 0)
    });
    fstart_log::info!(
        "{}: _CPC revision {}, rows {:#x}, {}..{} MHz",
        V::NAME,
        info.revision,
        info.supported,
        lowest_mhz,
        nominal_mhz
    );
    let entries = cpc_entries(
        &info,
        &Integers {
            // The vendor asks a platform policy here; OS-directed.
            autonomous_selection: 0,
            lowest_mhz,
            nominal_mhz,
        },
    );
    super::acpi::shared_cpc(info.revision, &entries)
}

/// Hand the installed IVRS to the PSP (C2P 3Fh, `SendIvrsAcpiTable`) as
/// AmdNbioDxe does once it built the table (`CallbackAcpiTableProtocol`
/// `0x2780` → `0x16c40` → `0x14720`, before EndOfDxe's LockDFReg), there
/// gated by PCDs 223h and 376h whose values are not known: what the PSP
/// does with it is not known either, so a refusal is logged and the boot
/// goes on ([`psp::Session::send_unchecked`]).
pub(super) fn send_ivrs<V: Variant>(
    psp: &mut psp::boot::Psp,
    clock: &fstart_driver_amd::ccx::tsc::Tsc,
    rsdp: u64,
) {
    // SAFETY: the tables were laid out in `ACPI_TABLES`, which is only read
    // here; reads outside it are refused.
    let tables = unsafe { &(*core::ptr::addr_of!(ACPI_TABLES)).0 };
    let range = tables.as_ptr_range();
    let read = |address: u64, bytes: &mut [u8]| {
        let start = address as usize as *const u8;
        let fits = range.start <= start
            && (start as usize)
                .checked_add(bytes.len())
                .is_some_and(|end| end <= range.end as usize);
        if fits {
            // SAFETY: inside the table storage.
            unsafe { core::ptr::copy_nonoverlapping(start, bytes.as_mut_ptr(), bytes.len()) };
        }
        fits
    };
    let Some((address, length)) = fstart_acpi::wake::table_from_rsdp_with(&read, rsdp, *b"IVRS")
    else {
        return;
    };
    let request = psp::command::SendIvrsAcpiTable::new(address, length);
    match psp.send_unchecked(clock, request) {
        Ok(_) => fstart_log::info!("{}: IVRS {:#x} sent to the PSP", V::NAME, address),
        Err(error) => {
            let (why, value) = error.parts();
            fstart_log::warn!("{}: PSP SendIvrsAcpiTable: {} {:#x}", V::NAME, why, value)
        }
    }
}

/// Derive NUMA domains for every thread (in MP order) and final usable E820
/// RAM range from APOB V2 `9/0x13` actual NPS and the DFv4.5 DRAM map: each
/// rule of the first IOM's copy (AmdFabricWhDxe `FUN_70e0`) goes to the
/// domain of the channels whose CS hold a copy of it on every IOD of every
/// socket. Rules to a non-DRAM (CXL) target set are not system DRAM here.
fn numa<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    apic_ids: &[u32],
    cxl_windows: &[(u64, u64, u8)],
) -> Option<super::numa::Numa> {
    let nps = state.apob_nps().or_else(|| {
        fstart_log::warn!("{}: no APOB V2 NPS record: no SRAT", V::NAME);
        None
    })?;
    // The UMCs' DRAM rules cover the RAM but the CXL windows.
    let windows: alloc::vec::Vec<(u64, u64)> = cxl_windows.iter().map(|w| (w.0, w.1)).collect();
    let regions = super::cxl::numa_regions(&e820_ram_regions(ctx), &windows);
    if regions.is_empty() {
        return None;
    }
    let ccds = state.thread_ccds();
    if ccds.len() != apic_ids.len() {
        return None;
    }
    let devices = state.devices();
    let node = |socket, die| {
        // SAFETY: BSP owns the DF indirect registers of live nodes; read-only scan.
        unsafe { df_wh::Node::new(GlobalEcam, socket, die) }
    };
    // The rule copies every CS holds, per node.
    let mut cs = alloc::vec::Vec::new();
    for &(socket, die) in state.nodes() {
        let node = node(socket, die)?;
        let copies: alloc::vec::Vec<[u32; df_wh::CS_DRAM_RULES]> = devices
            .cs
            .instances()
            .map(|c| core::array::from_fn(|j| node.dram_rule_ctl(c, j)))
            .collect();
        cs.push((socket, die, copies));
    }
    let node0 = node(0, 0)?;
    // The first IOM the vendor's topology map lists (a harvested one is
    // not there).
    let iom = state.ioms().first().copied().unwrap_or(devices.iom.first);
    let mut rules: alloc::vec::Vec<(u64, u64, u32)> = alloc::vec::Vec::new();
    for slot in 0..df_wh::DRAM_RULES {
        let Some(rule) = node0.dram_rule(iom, slot) else {
            continue;
        };
        if node0.dram_rule_non_dram(iom, slot) {
            continue;
        }
        let ctl = node0.dram_rule_ctl(iom, slot);
        let channels = cs.iter().fold(0u32, |mask, (socket, die, copies)| {
            let on_iod = u32::from(df_wh::rule_channels(ctl, copies));
            mask | on_iod << (u32::from(*die) * 8 + u32::from(*socket) * 16)
        });
        if channels == 0 {
            fstart_log::warn!(
                "{}: DRAM rule {} ({:#x}..{:#x}) on no channel: no SRAT",
                V::NAME,
                slot,
                rule.base,
                rule.end
            );
            return None;
        }
        rules.push((rule.base, rule.end, channels));
    }
    match super::numa::domains_with(
        nps,
        state.sockets(),
        rules.iter().copied(),
        &regions,
        ccds.iter().copied(),
        ctx.config.ccx_as_numa,
    ) {
        Ok(n) => {
            fstart_log::info!(
                "{}: NPS{} domains={} (DF IOM {:#x}, {} rules)",
                V::NAME,
                nps.domains_per_socket().unwrap_or(0),
                n.domains,
                iom,
                rules.len()
            );
            Some(n)
        }
        Err(_) => {
            fstart_log::warn!("{}: DFv4.5 DRAM rules do not fit the NPS: no SRAT", V::NAME);
            None
        }
    }
}

/// Build the SMBIOS tables (including Type 16/17/19 DDR5 DIMM inventory from
/// APOB V2 `8/8` + `1/0x11`); returns the SMBIOS 3.x entry point address.
pub fn smbios<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    silicon: &Silicon<Sp7<V>>,
    pci: &PciEcam,
    identity: &crate::acpi::SmbiosIdentity<'_>,
) -> Option<u64> {
    if SMBIOS_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }

    let apob = ctx.hw.apob.as_ref();
    let regions = apob
        .and_then(|apob| apob.system_memory_map().ok())
        .and_then(|map| sysmap::memory_map(super::HOLES, map.top, map.holes().iter().copied()).ok())
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    // The internal bridges of the live per-root-bridge tables, as `_PRT`
    // has them (not the RS default devfns of `acpi::BRIDGES`).
    let mut internal: alloc::vec::Vec<prt::Bridge> = alloc::vec::Vec::new();
    for rb in silicon.state.roots() {
        let (_, rb_on_iod) = fabric::rb_location(rb.index);
        let root = fabric::root_bus(rb.segment, rb.bus_base);
        for bridge in active_rb_bridges(rb_on_iod, root).unwrap_or_default() {
            let devfn = (bridge.device, bridge.function);
            if bridge.internal && !internal.iter().any(|b| (b.device, b.function) == devfn) {
                internal.push(bridge);
            }
        }
    }
    let smt = ccx::Thread::current().threads_per_core() > 1;
    // SAFETY: guarded by `SMBIOS_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(SMBIOS_TABLES)).0 };
    let address = storage.as_ptr() as u64;
    let channels = ctx.config.memory_channels;
    let socket_shift = silicon.state.socket_shift();
    let ecc = silicon.state.ecc_capable();
    crate::smbios::build_with(
        storage,
        &crate::smbios::System {
            soc: &crate::smbios::SocSmbios {
                // SMBIOS 3.x has no dedicated socket code for SP7 yet.
                socket_upgrade: 0x02,
                max_dimm_mib: ctx.config.max_dimm_mib,
                channels_per_die: 16,
            },
            identity,
            rom_size: V::ROM_SIZE,
            apob: None,
            regions: &regions,
            cpus: &silicon.cpus,
            pci,
            bridges: &internal,
            ports: &[],
        },
        |apic_id| {
            let socket = apic_id.checked_shr(socket_shift.into()).unwrap_or(0);
            (socket as u8, apic_id >> u32::from(smt))
        },
        || {
            let dimms = apob
                .and_then(|apob| apob.memory_inventory().ok())
                .map(|dimms| {
                    dimms
                        .into_iter()
                        .filter(|d| {
                            channels.get(usize::from(d.socket)).is_some_and(|&mask| {
                                d.device.channel < 16 && mask & (1u16 << d.device.channel) != 0
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            (dimms, ecc)
        },
    )
    .inspect_err(|what| fstart_log::error!("{}: SMBIOS: {}", V::NAME, what))
    .ok()
    .map(|_| address)
}
