// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 (Venice / WH) ACPI and SMBIOS table generation.
//!
//! Full parity with [`crate::rs::tables`]:
//! - NUMA (`SRAT`, `SLIT`, `MSCT`, per-CPU and per-RootBridge `_PXM`) derived
//!   from APOB V2 `9/0x13` actual NPS, DFv4.5 56-bit DRAM rules (`F7x200`),
//!   and APOB V2 `3/3` dual-IOD CCD topology.
//! - Primary-thread-first MADT CPU ordering, validated WH NBIO IOAPICs, WH
//!   IOMMUs (`IVRS`), active WH IOHC `_PRT` bridge assignments, Qilian FCH
//!   AML, APEI (`HEST`, `BERT` with APOB V2 `7/6` ABL errors, optional `EINJ`),
//!   and `WSMT`.
//! - SMBIOS Type 16/17/19 populated from APOB V2 `8/8` + `1/0x11` DDR5 SPD
//!   inventory (16 channels) and ECC capability.

use core::sync::atomic::{AtomicBool, Ordering};

use super::init::{FlowState, State, ioapic_id};
use super::{FCH_IOAPIC_ID, Sp7, Variant, decode_wh_dram_dst};
use crate::acpi::{Cpu, Ioapic, Iommu, RootBridgeAcpi};
use crate::flow::{Ctx, Silicon};
use fstart_acpi::ivrs::IommuRegisters;
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::ioapic::IoApic;
use fstart_driver_amd::apob::{event::DimmBits, smbios::dram_ranges, sysmap};
use fstart_driver_amd::ccx;
use fstart_driver_amd::df::wh as df_wh;
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::nbio::{self, wh as nbio_wh};
use fstart_driver_amd::psp;
use fstart_pci::{PciEcam, ecam::GlobalEcam};

static ACPI_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
const ACPI_TABLE_CAPACITY: usize = 0x8_0000;
#[repr(C, align(4096))]
struct AcpiBuffer([u8; ACPI_TABLE_CAPACITY]);
static mut ACPI_TABLES: AcpiBuffer = AcpiBuffer([0; ACPI_TABLE_CAPACITY]);

static SMBIOS_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
#[repr(C, align(4096))]
struct SmbiosBuffer([u8; fstart_acpi::smbios::BUFFER_SIZE]);
static mut SMBIOS_TABLES: SmbiosBuffer = SmbiosBuffer([0; fstart_acpi::smbios::BUFFER_SIZE]);

/// Build and install the ACPI tables; returns the RSDP physical address.
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
    let numa = numa::<V>(ctx, state, flow, &silicon.cpus.apic_ids);

    // Match enumerated PCI roots to DF root bridges by `bus_base`, never blind `.zip()`.
    let mut root_bridges: alloc::vec::Vec<RootBridgeAcpi<'_>> = alloc::vec::Vec::new();
    for root in pci.roots() {
        let bus_base = root.info().bus_start;
        let Some(rb) = state.roots().iter().find(|rb| rb.bus_base == bus_base) else {
            continue;
        };
        let bars = state.bars(rb.index);
        let die = rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD;
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        // Validate IOAPIC ID and redirection pin count before publishing MADT entry.
        let ioapic = bars.ioapic.and_then(|base| {
            let expected_id = ioapic_id(rb.index);
            // SAFETY: IOAPIC MMIO mapped by `init_fch` and identity-mapped uncached.
            let dev = unsafe { IoApic::new(base as usize) };
            let pins = dev.redirection_entries();
            if expected_id != u8::MAX && dev.id() == expected_id && (1..=256).contains(&pins) {
                Some(Ioapic {
                    id: expected_id,
                    base,
                })
            } else {
                fstart_log::warn!(
                    "{}: skipping invalid IOAPIC on RB{} (id {:#x}, pins {})",
                    V::NAME,
                    rb.index,
                    dev.id(),
                    pins
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
        });
    }

    // Read active WH IOHC DevFn assignments from the primary root bridge so
    // `_PRT` reflects programmed bridge addresses instead of static defaults.
    let active_bridges = state
        .roots()
        .iter()
        .find(|rb| rb.primary)
        .and_then(|primary| {
            let rb_on_iod = primary.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: primary root bridge bus is routed and BSP owns SMN.
            let nb = unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, primary.bus_base) }?;
            let devfns = core::array::from_fn(|slot| nb.bridge_devfn(slot));
            let all_zero = devfns.iter().all(|&(d, f)| d == 0 && f == 0);
            (!all_zero).then(|| super::acpi::bridges_from_devfns(devfns))
        });
    static mut ACTIVE_BRIDGES: [crate::acpi::prt::Bridge; super::acpi::BRIDGES.len()] =
        super::acpi::BRIDGES;
    let mut effective_soc = *soc;
    if let Some(bridges) = active_bridges {
        // SAFETY: guarded by `ACPI_TABLES_CLAIMED`, written at most once before
        // immutable borrow for the remainder of `acpi`.
        unsafe {
            let dst = core::ptr::addr_of_mut!(ACTIVE_BRIDGES);
            (*dst) = bridges;
            effective_soc.bridges = &*dst;
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
    let abl = flow.apob.abl_errors(DimmBits::RS);

    // SAFETY: guarded by `ACPI_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(ACPI_TABLES)).0 };
    crate::acpi::install(
        &crate::acpi::Soc {
            soc: &effective_soc,
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
        ctx.board,
        board,
        storage,
    )
    .inspect_err(|_| fstart_log::error!("{}: ACPI tables do not fit", V::NAME))
    .ok()
}

/// Derive NUMA domains for every thread (in MP order) and RAM range from APOB
/// V2 `9/0x13` actual NPS and DFv4.5 DRAM address rules; returns `None` (no
/// SRAT/SLIT/MSCT) if the APOB or DF rules are incomplete.
fn numa<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    flow: &FlowState,
    apic_ids: &[u32],
) -> Option<super::numa::Numa> {
    let nps = state.apob_nps().or_else(|| {
        fstart_log::warn!("{}: no APOB V2 NPS record: no SRAT", V::NAME);
        None
    })?;
    let regions = {
        let map = flow.apob.system_memory_map().ok()?;
        sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied()).ok()?
    };
    // SAFETY: BSP owns DF indirect registers on node (0, 0); read-only scan.
    let node0 = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }?;
    // `CcxInitWh` (`InitializeDramAddressRanges`) reads CCM instance `0x25`;
    // fall back to `0x24` if `0x25` yields no valid rules.
    let mut rules: alloc::vec::Vec<(u64, u64, u8, u16)> = alloc::vec::Vec::new();
    for instance in [df_wh::DRAM_INSTANCE_SECONDARY, df_wh::DRAM_INSTANCE_PRIMARY] {
        rules.clear();
        for slot in 0..df_wh::DRAM_RULES {
            if let Some(rule) = node0.dram_rule(instance, slot) {
                let (socket, cs) = decode_wh_dram_dst(rule.dst)?;
                rules.push((rule.base, rule.end, socket, cs));
            }
        }
        if !rules.is_empty() {
            break;
        }
    }
    if rules.is_empty() {
        fstart_log::warn!("{}: no valid DFv4.5 DRAM rules: no SRAT", V::NAME);
        return None;
    }
    let ccds = state.thread_ccds();
    if ccds.len() != apic_ids.len() {
        return None;
    }
    super::numa::domains(
        nps,
        1,
        ctx.config.memory_channels,
        rules.into_iter(),
        &regions,
        ccds.into_iter(),
    )
    .inspect(|n| {
        fstart_log::info!(
            "{}: NPS{} domains={}",
            V::NAME,
            nps.domains_per_socket().unwrap_or(0),
            n.domains
        )
    })
    .inspect_err(|_| fstart_log::error!("{}: NUMA domains inconsistent: no SRAT", V::NAME))
    .ok()
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
