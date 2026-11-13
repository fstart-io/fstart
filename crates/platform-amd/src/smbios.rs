// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS tables of every AMD SoC of the flow, gathered with the
//! shared builder (`fstart_acpi::smbios::Builder`): the board's identity, a
//! processor package per socket with the CPUID of one of its own threads,
//! and from the driver (`fstart_driver_amd::apob::smbios`) the DIMM
//! positions of the APOB's SMBIOS seeds with their SPD copies and the DRAM
//! ranges of the flow's memory map.
//!
//! The slots and onboard devices are what the board attaches to its PCIe
//! ports, and the controllers behind the SoC's internal bridges.
//!
//! A SoC says what its tables differ in ([`SocSmbios`]); its flow says
//! where a thread is, which dies there are, what is on the ports and where
//! the tables go.

/// What a SoC's SMBIOS tables differ in.
#[derive(Debug, Clone, Copy)]
pub struct SocSmbios {
    /// Processor upgrade (socket) code, 0x06 (none) for a BGA package.
    pub socket_upgrade: u8,
    /// The largest DIMM a position takes, MiB.
    pub max_dimm_mib: u32,
    /// Memory channels per die: how the APOB numbers its DIMM seeds.
    pub channels_per_die: u8,
}

#[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
pub use stage::{System, build_into};

#[cfg(all(any(feature = "bundle", feature = "bundle-stage"), target_os = "none"))]
mod stage {
    extern crate alloc;

    use super::SocSmbios;
    use alloc::vec::Vec;
    use fstart_acpi::smbios::{self, Attached, SmbiosIdentity};
    use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
    use fstart_driver_amd::apob::{
        Apob, mem,
        smbios::{self as apob_smbios, DramRange, Processor, dimms},
    };
    use fstart_pci::{PciAddress, PciEcam};

    /// What a flow knows of the system.
    pub struct System<'a> {
        pub soc: &'a SocSmbios,
        pub identity: &'a SmbiosIdentity<'a>,
        /// The extent of the firmware image in the flash.
        pub rom_size: u32,
        pub apob: Option<&'a Apob<'a>>,
        /// The RAM of the flow's memory map, before firmware allocations.
        pub regions: &'a [DramRange],
        pub cpus: &'a crate::mp::Cpus,
        /// The enumeration: the cards in the slots and the onboard devices.
        pub pci: &'a PciEcam,
        /// The bridges of a root bus (`SocAcpi::bridges`): the SoC's own
        /// controllers behind the internal ones are onboard devices.
        pub bridges: &'a [crate::acpi::prt::Bridge],
        /// What the board attaches to its DXIO engines, `(bridge, lanes,
        /// attached)`, without a bridge if the engine is not PCIe.
        pub ports: &'a [(Option<PciAddress>, u8, &'a Attached<'a>)],
    }

    /// The functions behind the internal bridges of every root bus.
    fn internal_functions(pci: &PciEcam, bridges: &[crate::acpi::prt::Bridge]) -> Vec<PciAddress> {
        pci.roots()
            .iter()
            .flat_map(|root| {
                let (segment, bus) = (root.info().segment, *root.buses().start());
                bridges
                    .iter()
                    .filter(|bridge| bridge.internal)
                    .map(move |b| PciAddress::new(segment, bus, b.device, b.function))
            })
            .filter_map(|bridge| pci.bridge_devices(bridge))
            .flatten()
            .collect()
    }

    /// The CPUID of the thread `index` of the MP start: its package's.
    fn snapshot(mp: &fstart_arch::x86::mp::MpHandle, index: usize) -> Option<ProcessorSnapshot> {
        if index == 0 {
            return Some(ProcessorSnapshot::read());
        }
        // The bounded queue transfers an owned value from the AP: no shared
        // result slot of our own.
        let queue = heapless::mpmc::MpMcQueue::<ProcessorSnapshot, 2>::new();
        mp.scope(|scope| {
            scope.run_on(index as u16, &|| {
                let _ = queue.enqueue(ProcessorSnapshot::read());
            })
        });
        queue.dequeue()
    }

    /// Build the tables of `system` into `storage` and publish the legacy
    /// entry point; returns the length. `place` is the socket of a thread's
    /// APIC ID and its core there, `die_exists` whether a socket has a die.
    /// Call before the APs are parked: a thread of every socket reads its
    /// CPUID.
    pub fn build_into(
        storage: &mut [u8],
        system: &System<'_>,
        place: impl Fn(u32) -> (u8, u32),
        die_exists: impl Fn(u8, u8) -> bool,
    ) -> Result<usize, &'static str> {
        let apic_ids = &system.cpus.apic_ids;
        let mut sockets: Vec<u8> = apic_ids.iter().map(|&id| place(id).0).collect();
        sockets.sort_unstable();
        sockets.dedup();
        let threads: Vec<Vec<u32>> = sockets
            .iter()
            .map(|&socket| {
                let of_socket = |id: &u32| place(*id).0 == socket;
                apic_ids.iter().copied().filter(of_socket).collect()
            })
            .collect();
        let mut processors = Vec::new();
        for (&socket, threads) in sockets.iter().zip(&threads) {
            // A thread of this package, not the BSP's copy.
            let leader = apic_ids.iter().position(|id| place(*id).0 == socket);
            let snapshot = leader
                .and_then(|index| snapshot(&system.cpus.mp, index))
                .ok_or("SMBIOS package CPUID")?;
            let mut cores: Vec<u32> = threads.iter().map(|&id| place(id).1).collect();
            cores.sort_unstable();
            cores.dedup();
            processors.push(Processor {
                socket,
                snapshot,
                apic_ids: threads,
                enabled_cores: cores.len() as u16,
            });
        }
        let soc = system.soc;
        let dimms = system
            .apob
            .map(|apob| dimms(apob, soc.channels_per_die, die_exists))
            .unwrap_or_default();
        let ecc = system
            .apob
            .and_then(|apob| mem::smbios_info(apob).ok())
            .is_some_and(|seeds| seeds.ecc());
        let mut tables = smbios::Builder::new(
            system.identity,
            system.rom_size,
            fstart_payload::SELECTED == fstart_payload::Selected::Uefi,
        );
        apob_smbios::add_packages(&mut tables, &processors, soc.socket_upgrade);
        // The positions come from the board's ABL seeds, not from assumed
        // slots or the installed DIMMs.
        apob_smbios::add_memory(&mut tables, &dimms, system.regions, soc.max_dimm_mib, ecc);
        tables.pci_ports(system.ports, Some(system.pci));
        let internal = internal_functions(system.pci, system.bridges);
        tables.pci_onboard_devices(system.pci, |address| internal.contains(&address));
        let address = storage.as_ptr() as u64;
        let used = tables
            .assemble_into(storage, address)
            .map_err(|_| "SMBIOS tables")?;
        // For Linux without UEFI.
        // SAFETY: both flows route C0000-FFFFF to DRAM and reserve it in
        // the e820; nothing else lives there.
        unsafe { smbios::publish_legacy_entry_point(storage) };
        fstart_log::info!(
            "smbios: {} packages, {} positions, {} installed, {} bytes at {:#x}",
            processors.len() as u32,
            dimms.len() as u32,
            dimms.iter().filter(|d| d.device.is_populated()).count() as u32,
            used as u32,
            address
        );
        Ok(used)
    }
}
