// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI tables of the die SoCs: what the dies and the enumeration
//! report, built by [`crate::acpi`] into reserved firmware memory.
use super::{DieSoc, MAX_DIES, Silicon};
use crate::acpi::{self, Board, Cpu, Domain, Inputs, Ioapic, Iommu, RootBridgeAcpi};
use fstart_acpi::ivrs::IommuRegisters;
use fstart_driver_amd::{
    ccx::{Thread, power},
    df::Die,
    fch,
    nbio::{ioapic::IOAPIC_FEATURES_ENABLE, iommu},
};
use fstart_log::Hex;
use fstart_pci::{PciRoot, ecam::GlobalEcam};
use heapless::Vec;

/// The ID [`crate::soc::Hw::init_ioapics`] gives the FCH
/// IOAPIC; the NBIO IOAPIC of die n has 1 + n.
pub const FCH_IOAPIC_ID: u8 = 0;

/// The root bridge of die `die`, `root` as the allocator enumerated it,
/// with its NBIO IOAPIC at `ioapic_base` and its IOMMU. With `numa` the
/// die is a proximity domain.
pub fn root_bridge(
    die: u8,
    root: &PciRoot,
    ioapic_base: u32,
    iommu: Option<iommu::Iommu>,
    numa: bool,
) -> RootBridgeAcpi<'_> {
    RootBridgeAcpi {
        root,
        fch: die == 0,
        domain: numa.then_some(die.into()),
        ioapic: Some(Ioapic {
            id: 1 + die,
            base: ioapic_base.into(),
        }),
        // Every die reaches its own NBIO's registers.
        ioapic_features: IOAPIC_FEATURES_ENABLE.address(),
        iommu: iommu.map(|iommu| Iommu {
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
    }
}

/// The threads `apic_ids` in MADT order: the first thread of every core in
/// APIC ID order, then the second threads (thread 1 of a Zen core is APIC
/// ID bit 0). With `numa` their die (APIC ID \[5:4\]) is their domain.
pub fn cpus(apic_ids: &[u32], numa: bool) -> alloc::vec::Vec<Cpu> {
    let mut ids = apic_ids.to_vec();
    ids.sort_unstable_by_key(|&id| (id & 1, id));
    ids.into_iter()
        .map(|apic_id| Cpu {
            apic_id,
            domain: if numa {
                Thread::die_of(apic_id).into()
            } else {
                0
            },
            aml: alloc::vec::Vec::new(),
        })
        .collect()
}

/// Build the tables for `dies` with the root bridges `roots` the allocator
/// enumerated, their NBIO IOAPICs at `ioapics` (by die) and what the
/// silicon init left; returns the RSDP.
pub fn install<S: DieSoc>(
    dies: &[Die<S, GlobalEcam>],
    roots: &[PciRoot],
    ioapics: &[u32],
    pci: fstart_core::description::PciRootInfo,
    silicon: &Silicon<S>,
    board: &Board,
) -> Option<u64> {
    // Room for a full 128-KiB ROM in both _ROM and VFCT plus platform tables.
    const TABLE_CAPACITY: usize = 0x8_0000;
    #[repr(C, align(4096))]
    struct Tables([u8; TABLE_CAPACITY]);
    static mut TABLES: Tables = Tables([0; TABLE_CAPACITY]);

    // The dies are the domains if they are NUMA nodes.
    let memory = S::die_memory(dies);
    let root_bridges: Vec<RootBridgeAcpi<'_>, MAX_DIES> = dies
        .iter()
        .zip(roots)
        .map(|(die, root)| {
            let index = die.index();
            // SAFETY: the IOMMU MMIO is in the identity-mapped pool.
            let iommu = unsafe { iommu::read(die) };
            root_bridge(
                index,
                root,
                ioapics[usize::from(index)],
                iommu,
                memory.is_some(),
            )
        })
        .collect();
    let cpus = cpus(&silicon.cpus.apic_ids, memory.is_some());
    // One socket.
    let domains: Vec<Domain, MAX_DIES> = dies.iter().map(|_| Domain { socket: 0 }).collect();
    // SAFETY: HPET general capabilities, decoded by the FCH.
    let hpet_block_id = unsafe { fstart_core::mmio::read32(fch::HPET as usize as *const u32) };
    // Every core runs the BSP's P-state table and C-state setup.
    // SAFETY: the BSP, reading the architectural P-state and C-state MSRs.
    let msr = unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() };
    let pstates = power::pstates(&msr);
    let tables = acpi::build(&Inputs {
        soc: S::ACPI,
        pci,
        cpus: &cpus,
        fch_ioapic_id: FCH_IOAPIC_ID,
        hpet_block_id,
        root_bridges: &root_bridges,
        numa: memory.as_ref().map(|memory| acpi::Numa {
            domains: &domains,
            memory,
            phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
        }),
        power: Some(power::Power {
            pstates: &pstates,
            cstates: power::cstate_info(&msr),
            threads_per_core: Thread::current().threads_per_core(),
            c2_latency_us: power::c2_latency_us(fstart_arch::x86::mp::CpuIdentity::current()),
        }),
        // The OS enables SCI_EN through the FCH SMM handler.
        acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
            smi_cmd: fch::smi::APM_CNT.into(),
            acpi_enable: fch::smi::ACPI_ENABLE,
            acpi_disable: fch::smi::ACPI_DISABLE,
        }),
        bmc_sx_notify: None,
        tpm: None,
        devices: &silicon.devices,
        sb_aml: &silicon.aml,
        tables: &silicon.tables,
        board,
    })
    .ok()?;
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    match tables.assemble_into(storage, address) {
        Ok(length) => {
            fstart_log::info!("ACPI: {} bytes at {}", length as u32, Hex(address));
            Some(address)
        }
        Err(_) => {
            fstart_log::error!("ACPI: tables don't fit");
            None
        }
    }
}
