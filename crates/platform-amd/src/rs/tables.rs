// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI and SMBIOS tables of an RS SoC, gathered for the shared
//! builders ([`crate::acpi`], [`crate::smbios`]): an IOS per root bridge
//! with its IOAPIC and IOMMU, the threads, the DRAM of the APOB's memory
//! map.
extern crate alloc;

use super::init::State;
use super::{Rs, Variant};
use crate::acpi::{Cpu, Ioapic, Iommu, RootBridgeAcpi, SocAcpi};
use crate::flow::{Ctx, Silicon};
use fstart_acpi::ivrs::IommuRegisters;
use fstart_driver_amd::apob::{self, smbios::dram_ranges, sysmap};
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::nbio::ioapic::IOAPIC_FEATURES_ENABLE;
use fstart_pci::PciEcam;

pub use super::acpi::{FCH_IOAPIC_ID, ioapic_id};

/// Build the ACPI tables; returns the RSDP.
pub fn acpi<V: Variant>(
    ctx: &Ctx<'_, Rs<V>>,
    silicon: &Silicon<Rs<V>>,
    pci: &PciEcam,
    soc: &SocAcpi,
    board: &crate::acpi::Board,
) -> Option<u64> {
    const TABLE_CAPACITY: usize = 0x8_0000;
    #[repr(C, align(4096))]
    struct Tables([u8; TABLE_CAPACITY]);
    static mut TABLES: Tables = Tables([0; TABLE_CAPACITY]);

    let state: &State = &silicon.state;
    let numa = numa(ctx, state, &silicon.cpus.apic_ids);
    let ios_domain = |ios| {
        numa.as_ref()
            .and_then(|numa| super::numa::ios_domain(numa.nps, ios))
    };
    let root_bridges: alloc::vec::Vec<RootBridgeAcpi<'_>> = pci
        .roots()
        .iter()
        .zip(state.roots())
        .map(|(root, rb)| {
            let bars = state.bars(rb.index);
            RootBridgeAcpi {
                root,
                fch: rb.primary,
                domain: ios_domain(rb.index),
                ioapic: bars.ioapic.map(|base| Ioapic {
                    id: ioapic_id(rb.index),
                    base,
                }),
                ioapic_features: IOAPIC_FEATURES_ENABLE.address() + (u32::from(rb.index) << 20),
                iommu: bars.iommu.map(|iommu| Iommu {
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
                bridges: None,
                cxl: None,
            }
        })
        .collect();
    // The first thread of every core first, as the vendors list them.
    let mut cpus: alloc::vec::Vec<Cpu> = silicon
        .cpus
        .apic_ids
        .iter()
        .enumerate()
        .map(|(i, &apic_id)| Cpu {
            apic_id,
            domain: numa.as_ref().map_or(0, |numa| numa.cpus[i]),
            aml: alloc::vec::Vec::new(),
        })
        .collect();
    cpus.sort_unstable_by_key(|cpu| (cpu.apic_id & 1, cpu.apic_id));
    let domains = numa
        .as_ref()
        .map(|numa| alloc::vec![crate::acpi::Domain { socket: 0 }; numa.domains]);
    let memory: alloc::vec::Vec<_> = numa
        .iter()
        .flat_map(|numa| &numa.memory)
        .map(|&(base, end, domain)| crate::acpi::MemoryAffinity { base, end, domain })
        .collect();
    // SAFETY: the BSP, reading the architectural C-state MSRs.
    let msr = unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() };
    // The FCH's UARTs, I2C and GPIO controllers as they were left.
    let fch = &ctx.hw.fch;
    let mut sb_aml = fstart_driver_amd::fch::aml::FchAcpi::read(fch.mmio(), fch.io())
        .sb_aml()
        .inspect_err(|_| fstart_log::error!("ACPI: FCH devices"))
        .ok()?;
    sb_aml.extend_from_slice(&silicon.aml);
    let abl: alloc::vec::Vec<_> = ctx
        .memory
        .apob
        .map(|apob| {
            crate::acpi::apei::abl_errors(apob, fstart_driver_amd::apob::event::DimmBits::RS)
        })
        .unwrap_or_default();
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    crate::acpi::install(
        &crate::acpi::Soc {
            soc,
            pci: <Rs<V> as crate::soc::Init>::pci_segment(ctx.hw),
            cpus: &cpus,
            fch_ioapic_id: FCH_IOAPIC_ID,
            root_bridges: &root_bridges,
            numa: domains.as_deref().map(|domains| crate::acpi::Numa {
                domains,
                memory: &memory,
                phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
            }),
            cstates: fstart_driver_amd::ccx::power::cstate_info(&msr),
            threads_per_core: fstart_driver_amd::ccx::Thread::current().threads_per_core(),
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
    .inspect_err(|_| fstart_log::error!("ACPI: tables don't fit"))
    .ok()
}

/// The NUMA domains of the threads (in MP order: the BSP, then the APs in
/// release order) and of the RAM; `None`, and no SRAT, if the APOB's NPS
/// or the DF's DRAM map doesn't give them.
fn numa<V: Variant>(
    ctx: &Ctx<'_, Rs<V>>,
    state: &State,
    apic_ids: &[u32],
) -> Option<super::numa::Numa> {
    use fstart_driver_amd::apob::nps;
    use fstart_driver_amd::df::v4;
    let apob = ctx.memory.apob?;
    let nps = nps::nps(apob)
        .inspect_err(|_| fstart_log::warn!("rs: no APOB NPS record: no SRAT"))
        .ok()?;
    let regions = {
        let map = sysmap::system_memory_map(apob).ok()?;
        sysmap::memory_map(fstart_driver_amd::psp::Variant::Gn, map.top, map.holes()).ok()?
    };
    // SAFETY: the BSP owns the DF; reads only.
    let df = unsafe { v4::Df::new(fstart_pci::ecam::GlobalEcam, 0) };
    let dram = v4::dram(&df)?;
    let ccds = state.thread_ccds();
    if ccds.len() != apic_ids.len() {
        return None;
    }
    super::numa::domains(
        nps,
        ctx.config.memory_channels,
        dram.ranges(),
        &regions,
        ccds.into_iter(),
    )
    .inspect(|numa| {
        fstart_log::info!(
            "rs: NPS{} domains={}",
            nps.domains_per_socket().unwrap_or(0),
            numa.domains
        )
    })
    .inspect_err(|_| fstart_log::error!("rs: the NUMA domains don't add up: no SRAT"))
    .ok()
}

/// Build the SMBIOS tables; returns their entry point's address.
pub fn smbios<V: Variant>(
    ctx: &Ctx<'_, Rs<V>>,
    silicon: &Silicon<Rs<V>>,
    pci: &PciEcam,
    identity: &crate::acpi::SmbiosIdentity<'_>,
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; fstart_acpi::smbios::BUFFER_SIZE]);
    static mut TABLES: Tables = Tables([0; fstart_acpi::smbios::BUFFER_SIZE]);

    let regions = ctx
        .memory
        .apob
        .and_then(|apob| {
            let map = sysmap::system_memory_map(apob).ok()?;
            sysmap::memory_map(fstart_driver_amd::psp::Variant::Gn, map.top, map.holes()).ok()
        })
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    let smt = fstart_driver_amd::ccx::Thread::current().threads_per_core() > 1;
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    crate::smbios::build_with(
        storage,
        &crate::smbios::System {
            soc: &crate::smbios::SocSmbios {
                // AMD Zen: SMBIOS 3.3 has no code for SP5 or SP6.
                socket_upgrade: 0x02,
                max_dimm_mib: ctx.config.max_dimm_mib,
                channels_per_die: 12,
            },
            identity,
            rom_size: V::LAYOUT.flash.size as u32,
            apob: ctx.memory.apob,
            regions: &regions,
            cpus: &silicon.cpus,
            pci,
            bridges: &super::acpi::BRIDGES,
            ports: &[],
        },
        |apic_id| (0, apic_id >> u32::from(smt)),
        // The DDR5 SPD copies and positions of the RS APOB, of the board's
        // channels.
        || {
            let channels = ctx.config.memory_channels;
            let dimms = ctx
                .memory
                .apob
                .and_then(|apob| apob::rs::memory::inventory(apob).ok())
                .map(|dimms| {
                    dimms
                        .into_iter()
                        .filter(|d| channels & 1 << d.device.channel != 0)
                        .collect()
                })
                .unwrap_or_default();
            let ecc = ctx
                .memory
                .apob
                .and_then(|apob| apob::rs::memory::read(apob).ok())
                .is_some_and(|memory| memory.ecc_capable);
            (dimms, ecc)
        },
    )
    .inspect_err(|what| fstart_log::error!("SMBIOS: {}", what))
    .ok()
    .map(|_| address)
}
