//! A builder for a system's SMBIOS tables: the platform starts it with the
//! board's identity, each part that knows a piece of the inventory adds it
//! (a SoC its processor packages, raminit or the memory training record
//! the DIMMs, the PCI enumeration the slots and onboard devices), and
//! [`Builder::assemble_into`] writes the structures in one fixed order.
//!
//! The order is the one every platform wrote by hand before: identity
//! (types 0, 1, 3, 2 and 11), the packages (7, then 4), the memory array
//! (16, 17, 19), the slots (9), the onboard devices (41), then 32 and 127.
//! Handles follow it, so a contributor cannot reorder another's structures.

extern crate alloc;

use super::{SlotFunction, SlotUsage, SmbiosIdentity, SmbiosWriter, SystemSlot, WriteError};
use alloc::{borrow::ToOwned, string::String, vec::Vec};
use fstart_core::memory_info::MemoryDevice;

/// Type 16 of the system memory.
struct MemoryArray {
    max_capacity_kib: u64,
    ecc: u8,
}

/// A type 17 with its labels.
struct Dimm {
    device: MemoryDevice,
    locator: String,
    bank: String,
}

/// A type 19 range, end inclusive.
struct MappedRange {
    start: u64,
    last: u64,
    partition_width: u8,
}

/// A type 9 with its boot-time state.
struct Slot<'a> {
    slot: SystemSlot<'a>,
    usage: SlotUsage,
    base: SlotFunction,
    peers: Vec<SlotFunction>,
}

/// A type 41.
struct OnboardDevice<'a> {
    designation: &'a str,
    device_type: u8,
    instance: u8,
    segment: u16,
    bus: u8,
    devfn: u8,
}

/// The SMBIOS tables of a system, gathered from whoever knows each part.
pub struct Builder<'a> {
    identity: &'a SmbiosIdentity<'a>,
    rom_size: u32,
    uefi: bool,
    /// The packages with their socket labels.
    #[cfg(feature = "x86")]
    packages: Vec<(String, super::Package<'a>)>,
    memory: Option<MemoryArray>,
    dimms: Vec<Dimm>,
    ranges: Vec<MappedRange>,
    slots: Vec<Slot<'a>>,
    onboard: Vec<OnboardDevice<'a>>,
}

impl<'a> Builder<'a> {
    /// Tables with the board's `identity` (types 0-3 and 11); `rom_size` is
    /// the firmware image's extent in the flash, `uefi` whether the payload
    /// gives UEFI boot services.
    pub fn new(identity: &'a SmbiosIdentity<'a>, rom_size: u32, uefi: bool) -> Self {
        Self {
            identity,
            rom_size,
            uefi,
            #[cfg(feature = "x86")]
            packages: Vec::new(),
            memory: None,
            dimms: Vec::new(),
            ranges: Vec::new(),
            slots: Vec::new(),
            onboard: Vec::new(),
        }
    }

    /// The package in processor socket `socket`: its caches (type 7) and
    /// type 4, labeled with the board's name of the socket, else
    /// `Socket<n>`.
    #[cfg(feature = "x86")]
    pub fn package(&mut self, socket: u8, package: super::Package<'a>) -> &mut Self {
        let label = match self.identity.processor_sockets.get(usize::from(socket)) {
            Some(label) => (*label).to_owned(),
            None => alloc::format!("Socket{socket}"),
        };
        self.packages.push((label, package));
        self
    }

    /// The system memory (type 16): the most it takes, in KiB, and its
    /// SMBIOS error-correction type. Its device count is the DIMMs added;
    /// without it, no DIMM or mapped range is written.
    pub fn memory_array(&mut self, max_capacity_kib: u64, ecc: u8) -> &mut Self {
        self.memory = Some(MemoryArray {
            max_capacity_kib,
            ecc,
        });
        self
    }

    /// A DIMM position of the memory array (type 17), installed or not.
    /// The labels are the caller's wiring facts, never guessed here.
    pub fn dimm(&mut self, device: &MemoryDevice, locator: &str, bank: &str) -> &mut Self {
        self.dimms.push(Dimm {
            device: *device,
            locator: locator.to_owned(),
            bank: bank.to_owned(),
        });
        self
    }

    /// DRAM at `base..end` (type 19) on the memory array; an empty range is
    /// skipped. `partition_width` is the modules interleaved there, 0 if
    /// unknown.
    pub fn mapped_range(&mut self, base: u64, end: u64, partition_width: u8) -> &mut Self {
        if end > base {
            self.ranges.push(MappedRange {
                start: base,
                last: end - 1,
                partition_width,
            });
        }
        self
    }

    /// A physical slot (type 9): the lowest-ordered function in it, and
    /// the others of a slot split into several links (see
    /// [`SmbiosWriter::add_system_slot`]).
    pub fn system_slot(
        &mut self,
        slot: &SystemSlot<'a>,
        usage: SlotUsage,
        base: SlotFunction,
        peers: &[SlotFunction],
    ) -> &mut Self {
        self.slots.push(Slot {
            slot: *slot,
            usage,
            base,
            peers: peers.to_vec(),
        });
        self
    }

    /// An enabled onboard device (type 41) of SMBIOS `device_type`; its
    /// instance counts the devices of that type added before it.
    pub fn onboard_device(
        &mut self,
        designation: &'a str,
        device_type: u8,
        segment: u16,
        bus: u8,
        devfn: u8,
    ) -> &mut Self {
        let instance = self
            .onboard
            .iter()
            .filter(|device| device.device_type == device_type)
            .count() as u8
            + 1;
        self.onboard.push(OnboardDevice {
            designation,
            device_type,
            instance,
            segment,
            bus,
            devfn,
        });
        self
    }

    /// Write the tables into `bytes`, which the OS will find at `address`;
    /// returns the length.
    pub fn assemble_into(&self, bytes: &mut [u8], address: u64) -> Result<usize, WriteError> {
        super::assemble_into(bytes, address, |w| self.write(w))
    }

    fn write(&self, w: &mut SmbiosWriter<'_>) {
        w.add_identity(self.identity, self.rom_size, self.uefi);
        #[cfg(feature = "x86")]
        for (socket, package) in &self.packages {
            w.add_package(socket, package);
        }
        if let Some(memory) = &self.memory {
            w.add_physical_memory_array(
                memory.max_capacity_kib,
                self.dimms.len() as u16,
                memory.ecc,
            );
            for dimm in &self.dimms {
                w.add_dimm(&dimm.device, &dimm.locator, &dimm.bank);
            }
            for range in &self.ranges {
                w.add_memory_array_mapped_address(range.start, range.last, range.partition_width);
            }
        }
        for slot in &self.slots {
            w.add_system_slot(&slot.slot, slot.usage, slot.base, &slot.peers);
        }
        for device in &self.onboard {
            w.add_onboard_device(
                device.designation,
                device.device_type,
                device.instance,
                device.segment,
                device.bus,
                device.devfn,
            );
        }
        w.add_system_boot_info();
        w.add_end_of_table();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smbios::{BUFFER_SIZE, assemble_into};
    use std::vec;

    fn by_hand(identity: &SmbiosIdentity<'_>, w: &mut SmbiosWriter<'_>) {
        w.add_identity(identity, 8 << 20, true);
        w.add_physical_memory_array(64 << 20, 2, 6);
        w.add_dimm(&MemoryDevice::unknown(0, 0), "A0", "A");
        w.add_dimm(&MemoryDevice::empty(0, 1), "A1", "A");
        w.add_memory_array_mapped_address(0, (3 << 30) - 1, 0);
        w.add_system_slot(
            &SystemSlot::new("PCIE1"),
            SlotUsage::Available,
            SlotFunction::none(16),
            &[],
        );
        w.add_onboard_device("Onboard LAN", 5, 1, 0, 2, 0);
        w.add_onboard_device("Onboard LAN", 5, 2, 0, 3, 0);
        w.add_system_boot_info();
        w.add_end_of_table();
    }

    #[test]
    fn writes_in_the_fixed_order_whatever_the_order_of_contributions() {
        let identity = SmbiosIdentity::new("Vendor", "Board");
        let mut expected = vec![0; BUFFER_SIZE];
        let len = assemble_into(&mut expected, 0x1000, |w| by_hand(&identity, w)).unwrap();

        let mut builder = Builder::new(&identity, 8 << 20, true);
        builder
            .onboard_device("Onboard LAN", 5, 0, 2, 0)
            .system_slot(
                &SystemSlot::new("PCIE1"),
                SlotUsage::Available,
                SlotFunction::none(16),
                &[],
            )
            .mapped_range(0, 3 << 30, 0)
            .mapped_range(4 << 30, 4 << 30, 0)
            .dimm(&MemoryDevice::unknown(0, 0), "A0", "A")
            .memory_array(64 << 20, 6)
            .dimm(&MemoryDevice::empty(0, 1), "A1", "A")
            .onboard_device("Onboard LAN", 5, 0, 3, 0);
        let mut bytes = vec![0; BUFFER_SIZE];
        assert_eq!(builder.assemble_into(&mut bytes, 0x1000), Ok(len));
        assert_eq!(bytes[..len], expected[..len]);
    }

    #[test]
    fn no_memory_array_no_dimms() {
        let identity = SmbiosIdentity::new("Vendor", "Board");
        let mut expected = vec![0; BUFFER_SIZE];
        let len = assemble_into(&mut expected, 0x1000, |w| {
            w.add_identity(&identity, 8 << 20, false);
            w.add_system_boot_info();
            w.add_end_of_table();
        })
        .unwrap();
        let mut builder = Builder::new(&identity, 8 << 20, false);
        builder
            .dimm(&MemoryDevice::empty(0, 0), "A0", "A")
            .mapped_range(0, 1 << 30, 0);
        let mut bytes = vec![0; BUFFER_SIZE];
        assert_eq!(builder.assemble_into(&mut bytes, 0x1000), Ok(len));
        assert_eq!(bytes[..len], expected[..len]);
    }
}
