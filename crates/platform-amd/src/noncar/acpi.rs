// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI tables of the flow: what the dies and the enumeration report,
//! built by [`crate::acpi`] into reserved firmware memory.
use super::{MAX_DIES, pci::Root};
use crate::acpi::{self, BoardAcpi, Bridge, DieAcpi, Inputs, SocAcpi};
use core::ops::RangeInclusive;
use fstart_driver_amd::{
    ccx::power,
    df::Die,
    fch,
    nbio::iommu::{self, Iommu},
};
use fstart_log::Hex;
use fstart_pci::{MAX_PCI_ROOT_WINDOWS, PciEcam, ecam::GlobalEcam};
use heapless::Vec;

/// The ECAM window: its base and bus count.
pub struct Ecam {
    pub base: u64,
    pub buses: u16,
}

/// The ACPI host bridge of a root complex: its buses and windows and the
/// bridges the enumeration found on its root bus.
pub struct HostBridge {
    buses: RangeInclusive<u8>,
    io: Vec<RangeInclusive<u16>, MAX_PCI_ROOT_WINDOWS>,
    mmio: Vec<RangeInclusive<u64>, MAX_PCI_ROOT_WINDOWS>,
    bridges: Vec<Bridge, 32>,
}
impl HostBridge {
    pub fn new(root: &Root, pci: &PciEcam) -> Self {
        let buses = root.buses();
        let bridges = pci
            .devices()
            .filter(|a| a.bus() == *buses.start())
            .filter_map(|a| {
                let device = pci.device(a)?;
                (device.read32(8) >> 16 == 0x0604).then_some(Bridge {
                    devfn: fstart_pci::devfn(a.device(), a.function()),
                    pin: (device.read32(0x3c) >> 8) as u8,
                })
            })
            .collect();
        Self {
            io: root.io().collect(),
            mmio: root.mmio().collect(),
            buses,
            bridges,
        }
    }

    /// The host bridge of die `die` with its NBIO IOAPIC at `ioapic_base`
    /// (ID 1 + die) and its IOMMU.
    pub fn acpi(&self, die: u8, ioapic_base: u32, iommu: Option<Iommu>) -> DieAcpi<'_> {
        DieAcpi {
            buses: self.buses.clone(),
            ioapic_id: 1 + die,
            ioapic_base,
            io: &self.io,
            mmio: &self.mmio,
            vga: die == 0,
            bridges: &self.bridges,
            iommu,
        }
    }
}

/// Build the tables for `dies` with their host bridges `hosts`, their NBIO
/// IOAPICs at `ioapics` (by die) and the CPUs `apic_ids`; returns the RSDP.
pub fn install<S: super::Soc>(
    soc: &SocAcpi,
    dies: &[Die<S, GlobalEcam>],
    hosts: &[HostBridge],
    ioapics: &[u32],
    window: Ecam,
    apic_ids: &[u32],
    board: &BoardAcpi,
) -> Option<u64> {
    // Room for a full 128-KiB ROM in both _ROM and VFCT plus platform tables.
    const TABLE_CAPACITY: usize = 0x8_0000;
    #[repr(C, align(4096))]
    struct Tables([u8; TABLE_CAPACITY]);
    static mut TABLES: Tables = Tables([0; TABLE_CAPACITY]);

    let all_dies = dies;
    let dies: Vec<DieAcpi<'_>, MAX_DIES> = dies
        .iter()
        .zip(hosts)
        .map(|(die, host)| {
            let index = die.index();
            // SAFETY: the IOMMU MMIO is in the identity-mapped pool.
            host.acpi(index, ioapics[usize::from(index)], unsafe {
                iommu::read(die)
            })
        })
        .collect();
    let numa = S::die_memory(all_dies);
    // SAFETY: HPET general capabilities, decoded by the FCH.
    let hpet_id = unsafe { fstart_core::mmio::read32(fch::HPET as usize as *const u32) };
    // Every core runs the BSP's P-state table and C-state setup.
    // SAFETY: the BSP, reading the architectural P-state and C-state MSRs.
    let msr = unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() };
    let pstates = power::pstates(&msr);
    let threads_per_core = ((fstart_arch::x86::cpuid(0x8000_001e).1 >> 8) & 0xff) as u8 + 1;
    let tables = acpi::build(&Inputs {
        soc,
        apic_ids,
        dies: &dies,
        ecam_base: window.base,
        ecam_buses: window.buses,
        hpet_id,
        power: Some(acpi::CpuPower {
            pstates: &pstates,
            cstates: power::cstate_info(&msr),
            threads_per_core,
        }),
        numa: numa.as_ref().map(|memory| acpi::DieNuma {
            memory,
            phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
        }),
        board,
    })
    .ok()?;
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    match fstart_acpi::platform::assemble_into(
        storage,
        address,
        &tables.platform,
        &tables.dsdt_aml,
        &tables.extra,
    ) {
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
