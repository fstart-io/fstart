// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI tables of the flow: what the dies and the enumeration report,
//! built by [`crate::acpi`] into reserved firmware memory.
use super::MAX_DIES;
use crate::acpi::{self, BoardAcpi, Bridge, DieAcpi, Inputs, SocAcpi};
use core::ops::RangeInclusive;
use fstart_driver_amd::{df::Die, fch, nbio::iommu, soc::Soc};
use fstart_log::Hex;
use fstart_pci::{PciEcam, PciWindowKind, ecam::GlobalEcam};
use heapless::Vec;

/// The ECAM window: its base and bus count.
pub struct Ecam {
    pub base: u64,
    pub buses: u16,
}

/// Build the tables for `dies` with their NBIO IOAPICs at `ioapics` (by die)
/// and the CPUs `apic_ids`; returns the RSDP.
pub fn install<S: Soc>(
    soc: &SocAcpi,
    dies: &[Die<S, GlobalEcam>],
    ioapics: &[u32],
    ecam: &PciEcam,
    window: Ecam,
    apic_ids: &[u32],
    board: &BoardAcpi,
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; 0x2_0000]);
    static mut TABLES: Tables = Tables([0; 0x2_0000]);

    let io: Vec<RangeInclusive<u16>, 8> = ecam
        .windows()
        .iter()
        .filter(|w| w.kind == PciWindowKind::Io && w.size != 0)
        .map(|w| w.base as u16..=(w.end() - 1) as u16)
        .collect();
    let mmio: Vec<RangeInclusive<u64>, 8> = ecam
        .windows()
        .iter()
        .filter(|w| w.kind == PciWindowKind::Mmio && w.size != 0)
        .map(|w| w.base..=w.end() - 1)
        .collect();
    let bridges: Vec<Vec<Bridge, 32>, MAX_DIES> = dies
        .iter()
        .map(|die| {
            ecam.devices()
                .filter(|a| a.bus() == die.bus())
                .filter_map(|a| {
                    let device = ecam.device(a)?;
                    (device.read32(8) >> 16 == 0x0604).then_some(Bridge {
                        devfn: fstart_pci::devfn(a.device(), a.function()),
                        pin: (device.read32(0x3c) >> 8) as u8,
                    })
                })
                .collect()
        })
        .collect();
    let dies: Vec<DieAcpi<'_>, MAX_DIES> = dies
        .iter()
        .zip(&bridges)
        .map(|(die, bridges)| DieAcpi {
            buses: die.bus()..=window.buses as u8 - 1,
            ioapic_id: 1 + die.index(),
            ioapic_base: ioapics[usize::from(die.index())],
            io: &io,
            mmio: &mmio,
            vga: die.index() == 0,
            bridges,
            // SAFETY: the IOMMU MMIO is in the identity-mapped pool.
            iommu: unsafe { iommu::read(die) },
        })
        .collect();
    // SAFETY: HPET general capabilities, decoded by the FCH.
    let hpet_id = unsafe { fstart_core::mmio::read32(fch::HPET as usize as *const u32) };
    let tables = acpi::build(&Inputs {
        soc,
        apic_ids,
        dies: &dies,
        ecam_base: window.base,
        ecam_buses: window.buses,
        hpet_id,
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
