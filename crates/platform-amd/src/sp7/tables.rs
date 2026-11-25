// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 (Venice / WH) ACPI and SMBIOS table publication.

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use super::init::{FlowState, State, ioapic_id};
use super::{FCH_IOAPIC_ID, Sp7, Variant, decode_wh_dram_dst};
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
use fstart_pci::{EcamDevice, PciEcam, ecam::GlobalEcam};

/// Storage for SP7 ACPI tables (512 KiB aligned, permanently reserved).
#[repr(align(4096))]
struct AcpiStorage([u8; 0x8_0000]);

static ACPI_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
static mut ACPI_TABLES: AcpiStorage = AcpiStorage([0; 0x8_0000]);

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
fn active_rb_bridges(rb_on_iod: u8, bus_base: u8) -> Option<alloc::vec::Vec<prt::Bridge>> {
    // SAFETY: routed root bridge bus, BSP owns SMN during ACPI publication.
    let nb = unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, bus_base) }?;
    let layout = super::mpio::layout();
    let devfns = core::array::from_fn(|slot| layout.slot_devfn(&nb, slot as u8).unwrap_or((0, 0)));
    let all = super::acpi::bridges_from_devfns(devfns);
    let mut filtered = alloc::vec::Vec::new();
    for bridge in all {
        if bridge.device == 0 && bridge.function == 0 {
            continue;
        }
        if !EcamDevice::new(bus_base, bridge.device, bridge.function).is_present() {
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
    flow: &FlowState,
    soc: &crate::acpi::SocAcpi,
    board: &crate::acpi::Board,
) -> Option<u64> {
    if ACPI_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }
    let state = &silicon.state;
    let numa = numa::<V>(ctx, state, &silicon.cpus.apic_ids);

    // First pass: capture per-root-bridge active bridge tables so each root
    // bridge carries its own IOHC DevFn mapping in `_PRT` and `RPxx` devices.
    let mut per_rb_bridges: alloc::vec::Vec<Option<alloc::vec::Vec<prt::Bridge>>> =
        alloc::vec::Vec::new();
    let mut matched_roots = alloc::vec::Vec::new();
    for root in pci.roots() {
        let bus_base = root.info().bus_start;
        let Some(rb) = state.roots().iter().find(|rb| rb.bus_base == bus_base) else {
            continue;
        };
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        per_rb_bridges.push(active_rb_bridges(rb_on_iod, rb.bus_base));
        matched_roots.push((root, rb));
    }

    let mut root_bridges: alloc::vec::Vec<RootBridgeAcpi<'_>> = alloc::vec::Vec::new();
    for ((root, rb), rb_bridges) in matched_roots.into_iter().zip(per_rb_bridges.iter()) {
        let bars = state.bars(rb.index);
        let die = rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD;
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        // Validate IOAPIC ID and pin count against the 32-pin NBIO GSI window
        // allocated by `acpi::fixed::gsi_windows` so windows can never overlap.
        let ioapic = bars.ioapic.and_then(|base| {
            let expected_id = ioapic_id(rb.index);
            // SAFETY: IOAPIC MMIO was verified by `init_fch` and identity-mapped uncached.
            let dev = unsafe { IoApic::new(base as usize) };
            let pins = dev.redirection_entries();
            if expected_id != u8::MAX
                && dev.id() == expected_id
                && (1..=NBIO_IOAPIC_PINS).contains(&pins)
            {
                Some(Ioapic {
                    id: expected_id,
                    base,
                })
            } else {
                fstart_log::warn!(
                    "{}: skipping invalid IOAPIC on RB{} (id {:#x}, pins {}, expected 1..={})",
                    V::NAME,
                    rb.index,
                    dev.id(),
                    pins,
                    NBIO_IOAPIC_PINS
                );
                None
            }
        });
        let domain = numa
            .as_ref()
            .and_then(|n| super::numa::rb_domain(n.nps, 0, die, rb_on_iod));
        root_bridges.push(RootBridgeAcpi {
            root,
            fch: rb.primary,
            domain,
            ioapic,
            ioapic_features: nbio::ioapic::IOAPIC_FEATURES_ENABLE.address()
                + (u32::from(rb_on_iod) * nbio_wh::RB_SMN_STRIDE),
            iommu: state.iommu(rb.index).map(|iommu| Iommu {
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
            }),
            bridges: rb_bridges.as_deref(),
        });
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
            aml: alloc::vec::Vec::new(),
        })
        .collect();
    cpus.sort_unstable_by_key(|cpu| (cpu.apic_id & 1, cpu.apic_id));

    let domains = numa
        .as_ref()
        .map(|n| alloc::vec![crate::acpi::Domain { socket: 0 }; n.domains]);
    let memory: alloc::vec::Vec<_> = numa
        .iter()
        .flat_map(|n| &n.memory)
        .map(|&(base, end, domain)| crate::acpi::MemoryAffinity { base, end, domain })
        .collect();

    // SAFETY: BSP reading architectural C-state MSRs.
    let msr = unsafe { NativeMsr::new() };
    let fch = &ctx.hw.fch;
    let mut sb_aml = fstart_driver_amd::fch::aml::FchAcpi::read(fch.mmio(), fch.io())
        .sb_aml()
        .inspect_err(|_| fstart_log::error!("ACPI: FCH devices"))
        .ok()?;
    sb_aml.extend_from_slice(&silicon.aml);

    // APOB V2 7/6 ABL training/BIST/PMIC errors for BERT (Venice uses RS DIMM bits).
    let (abl, abl_truncated) = flow.apob.abl_errors(DimmBits::RS);
    if abl_truncated {
        fstart_log::warn!("{}: APOB V2 7/6 event log truncated", V::NAME);
    }

    // Gate boot-time SEL reporting on verified KCS I/O decode.
    let mut facts = *ctx.board;
    if !ctx.config.kcs_decoded {
        facts.bmc_event = None;
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
            tables: &silicon.tables,
            abl: &abl,
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

/// Derive NUMA domains for every thread (in MP order) and final usable E820
/// RAM range from APOB V2 `9/0x13` actual NPS and DFv4.5 DRAM address rules;
/// tries every CCM instance until one passes full coverage validation.
fn numa<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    apic_ids: &[u32],
) -> Option<super::numa::Numa> {
    let nps = state.apob_nps().or_else(|| {
        fstart_log::warn!("{}: no APOB V2 NPS record: no SRAT", V::NAME);
        None
    })?;
    let regions = e820_ram_regions(ctx);
    if regions.is_empty() {
        return None;
    }
    let ccds = state.thread_ccds();
    if ccds.len() != apic_ids.len() {
        return None;
    }
    // SAFETY: BSP owns DF indirect registers on node (0, 0); read-only scan.
    let node0 = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }?;
    // `CcxInitWh` (`InitializeDramAddressRanges`) reads CCM instance `0x25`;
    // try both `0x25` and `0x24` and accept the first rule set that passes
    // full E820 RAM + domain coverage validation.
    let mut rules: alloc::vec::Vec<(u64, u64, u8, u16)> = alloc::vec::Vec::new();
    for instance in [df_wh::DRAM_INSTANCE_SECONDARY, df_wh::DRAM_INSTANCE_PRIMARY] {
        rules.clear();
        let mut valid = true;
        for slot in 0..df_wh::DRAM_RULES {
            if let Some(rule) = node0.dram_rule(instance, slot) {
                let Some((socket, cs)) = decode_wh_dram_dst(rule.dst) else {
                    valid = false;
                    break;
                };
                rules.push((rule.base, rule.end, socket, cs));
            }
        }
        if !valid || rules.is_empty() {
            continue;
        }
        if let Ok(n) = super::numa::domains(
            nps,
            1,
            ctx.config.memory_channels,
            rules.iter().copied(),
            &regions,
            ccds.iter().copied(),
        ) {
            fstart_log::info!(
                "{}: NPS{} domains={} (DF CCM {:#x})",
                V::NAME,
                nps.domains_per_socket().unwrap_or(0),
                n.domains,
                instance
            );
            return Some(n);
        }
    }
    fstart_log::warn!("{}: no consistent DFv4.5 DRAM rules: no SRAT", V::NAME);
    None
}

/// Build the SMBIOS tables (including Type 16/17/19 DDR5 DIMM inventory from
/// APOB V2 `8/8` + `1/0x11`); returns the SMBIOS 3.x entry point address.
pub fn smbios<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    silicon: &Silicon<Sp7<V>>,
    pci: &PciEcam,
    flow: &FlowState,
    identity: &crate::acpi::SmbiosIdentity<'_>,
) -> Option<u64> {
    if SMBIOS_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }

    let regions = flow
        .apob
        .system_memory_map()
        .ok()
        .and_then(|map| {
            sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied()).ok()
        })
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    let smt = ccx::Thread::current().threads_per_core() > 1;
    // SAFETY: guarded by `SMBIOS_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(SMBIOS_TABLES)).0 };
    let address = storage.as_ptr() as u64;
    let channels = ctx.config.memory_channels;
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
            bridges: &super::acpi::BRIDGES,
            ports: &[],
        },
        |apic_id| (0, apic_id >> u32::from(smt)),
        || {
            let dimms = flow
                .apob
                .memory_inventory()
                .ok()
                .map(|dimms| {
                    dimms
                        .into_iter()
                        .filter(|d| {
                            d.device.channel < 16 && (channels & (1u16 << d.device.channel)) != 0
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
