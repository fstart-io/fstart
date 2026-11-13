// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS tables of the die SoCs ([`crate::smbios`]): one socket, the
//! DRAM ranges of the APOB's memory map, what the board attaches to the
//! DXIO engines, built into reserved firmware memory.
extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::smbios::Attached;
use fstart_driver_amd::dxio::{Descriptor, EngineType};
use fstart_pci::PciAddress;

/// The PCIe ports of `descriptors[die]` the board attached something to:
/// each with its bridge on the root bus `roots[die]`, `(segment, bus)`,
/// and its lanes.
pub fn ports(
    descriptors: &[&'static [Descriptor]],
    roots: &[(u16, u8)],
) -> Vec<(Option<PciAddress>, u8, &'static Attached<'static>)> {
    descriptors
        .iter()
        .zip(roots)
        .flat_map(|(engines, &(segment, bus))| {
            engines.iter().filter_map(move |engine| {
                let attached = engine
                    .attached
                    .filter(|_| engine.engine == EngineType::Pcie)?;
                let bridge = PciAddress::new(segment, bus, engine.device, engine.function);
                Some((Some(bridge), engine.width(), attached))
            })
        })
        .collect()
}

#[cfg(all(feature = "bundle", target_os = "none"))]
pub use stage::install;

#[cfg(all(feature = "bundle", target_os = "none"))]
mod stage {
    use super::super::{Ctx, DieSoc};
    use alloc::vec::Vec;
    use fstart_driver_amd::apob::{
        smbios::{DramRange, dram_ranges},
        sysmap,
    };

    /// Build the tables; returns their entry point's address.
    pub fn install<S: DieSoc>(
        identity: &fstart_acpi::smbios::SmbiosIdentity<'_>,
        ctx: &Ctx<'_, S>,
        cpus: &crate::mp::Cpus,
        pci: &fstart_pci::PciEcam,
    ) -> Option<u64> {
        #[repr(C, align(4096))]
        struct Tables([u8; fstart_acpi::smbios::BUFFER_SIZE]);
        static mut TABLES: Tables = Tables([0; fstart_acpi::smbios::BUFFER_SIZE]);

        let apob = ctx.memory.apob;
        let regions: Vec<DramRange> = apob
            .and_then(|apob| sysmap::first_system_memory_map(apob).ok()?.sorted().ok())
            .and_then(|map| {
                sysmap::memory_map(S::psp(ctx.hw), map.top, map.holes().iter().copied()).ok()
            })
            .map(|regions| dram_ranges(&regions))
            .unwrap_or_default();
        let roots: Vec<(u16, u8)> = pci
            .roots()
            .iter()
            .map(|root| (root.info().segment, *root.buses().start()))
            .collect();
        let ports = super::ports(S::descriptors(ctx.config), &roots);
        let threads_per_core = fstart_driver_amd::ccx::Thread::current().threads_per_core();
        // SAFETY: firmware BSS in the reserved early region, written once.
        let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
        let address = storage.as_ptr() as u64;
        crate::smbios::build_into(
            storage,
            &crate::smbios::System {
                soc: &S::SMBIOS,
                identity,
                rom_size: S::LAYOUT.flash.size as u32,
                apob,
                regions: &regions,
                cpus,
                pci,
                bridges: S::ACPI.bridges,
                ports: &ports,
            },
            // One socket; the SMT threads of a core are adjacent APIC IDs.
            |apic_id| (0, apic_id / u32::from(threads_per_core)),
            |socket, _| socket == 0,
        )
        .inspect_err(|what| fstart_log::error!("smbios: {}", *what))
        .ok()
        .map(|_| address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::smbios::SystemSlot;

    static SLOT: Attached<'static> = Attached::Slot(SystemSlot::new("PCIE_1"));
    static M2: Attached<'static> = Attached::Slot(SystemSlot::new("M.2"));

    /// MJ11-EC1-like engines: SATA lanes, an M.2 x4, an untagged port and
    /// a slot split x8/x8; a second die with nothing attached.
    #[test]
    fn the_attached_ports_with_their_bridges_and_lanes() {
        static DIE0: [Descriptor; 5] = [
            Descriptor::sata(0, 3, fstart_driver_amd::dxio::SataChannel::Other),
            Descriptor::pcie(8, 11, 1, 3).attached(&M2),
            Descriptor::pcie(15, 15, 1, 1),
            Descriptor::pcie(31, 24, 3, 1).attached(&SLOT),
            Descriptor::pcie(23, 16, 3, 3).attached(&SLOT),
        ];
        static DIE1: [Descriptor; 1] = [Descriptor::pcie(0, 15, 3, 1)];
        let ports = ports(&[&DIE0, &DIE1], &[(0, 0), (0, 0x40)]);
        let found: Vec<(u8, u8, u8, u8)> = ports
            .iter()
            .map(|(a, lanes, _)| {
                let a = a.unwrap();
                (a.bus(), a.device(), a.function(), *lanes)
            })
            .collect();
        assert_eq!(found, [(0, 1, 3, 4), (0, 3, 1, 8), (0, 3, 3, 8)]);
        assert!(core::ptr::eq(ports[1].2, &SLOT) && core::ptr::eq(ports[0].2, &M2));
    }
}
