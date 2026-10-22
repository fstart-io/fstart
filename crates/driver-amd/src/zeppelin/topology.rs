// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin die topology and per-die register transports.
//!
//! Every die has its own Data Fabric PCI device on bus 0 (0x18 + die) and its
//! own root bus. The SMN addresses are the same on every die and are reached
//! through D0F0 0xb8/0xbc of that die's root bus. A DF "broadcast" access in
//! AGESA terms is a direct access to the die's own register
//! ([`crate::fam17h::registers::BROADCAST`]).
use crate::fam17h::{
    registers::{BROADCAST, Fabric, FabricPort, SmnPort},
    smu,
};
use fstart_pci::{ConfigRegionAccess, PciAddress};

/// Two dies on SP4, one on SP4r2.
pub const MAX_DIES: usize = 2;

/// Non-PCI MMIO pool of a die, the top 8 MiB of its below-4G DF window: the
/// PSP BAR at +0, the FCH ACPI MMIO of dies >= 1 at +1 MiB, the IOMMU (512
/// KiB) at +2 MiB, the NBIO IOAPIC at +7 MiB.
pub const NONPCI_MMIO_SIZE: u32 = 8 << 20;
pub const NONPCI_PSP: u32 = 0;
pub const NONPCI_IOMMU: u32 = 2 << 20;
pub const NONPCI_IOAPIC: u32 = 7 << 20;

const DF_F0_BUS_BASE: u16 = 0x84;
const DF_F1_SYSTEM_NODES: u16 = 0x204;

/// Number of dies from DF1x204 (live topology).
pub fn die_count(pci: &impl ConfigRegionAccess) -> u8 {
    (unsafe { pci.read(PciAddress::new(0, 0, 0x18, 1), DF_F1_SYSTEM_NODES) } & 0xff) as u8
}

/// Fabric ID of the IOMS (the PCIe root) of a die.
pub const fn ioms_fabric_id(die: u8) -> u8 {
    (die << 5) | 4
}

/// One die: its index, root bus and register transports.
#[derive(Clone, Copy)]
pub struct Die<P> {
    pci: P,
    index: u8,
    bus: u8,
}
impl<P: ConfigRegionAccess + Copy> Die<P> {
    /// # Safety
    /// `index` is below [`die_count`]. The caller serializes all SMN, FICAA
    /// and SMU mailbox accesses of this die (BSP only during silicon init).
    pub unsafe fn new(pci: P, index: u8) -> Self {
        let bus = (unsafe { pci.read(PciAddress::new(0, 0, 0x18 + index, 0), DF_F0_BUS_BASE) }
            & 0xff) as u8;
        Self { pci, index, bus }
    }
    pub const fn index(&self) -> u8 {
        self.index
    }
    /// Root bus of the die (0 on die 0).
    pub const fn bus(&self) -> u8 {
        self.bus
    }
    pub const fn ioms_fabric_id(&self) -> u8 {
        ioms_fabric_id(self.index)
    }
    pub fn pci(&self) -> P {
        self.pci
    }
    pub fn smn(&self) -> SmnPort<P> {
        // SAFETY: exclusivity is the caller's contract of `Die::new`.
        unsafe { SmnPort::on_bus(self.pci, self.bus) }
    }
    pub fn df(&self) -> FabricPort<P> {
        // SAFETY: as above.
        unsafe { FabricPort::for_node(self.pci, self.index) }
    }
    /// Read the die's own DF register (no FICAA indirection).
    pub fn df_direct(&self, function: u8, offset: u16) -> u32 {
        self.df().read(function, offset, BROADCAST)
    }
    /// Send a message to this die's SMU.
    pub fn smu(
        &self,
        message: u32,
        arguments: [u32; smu::ARGUMENT_COUNT],
        delay_us: &mut impl FnMut(u32),
    ) -> Result<[u32; smu::ARGUMENT_COUNT], smu::Error> {
        super::smu::MAILBOX.request(&mut self.smn(), message, arguments, delay_us)
    }
}

/// All dies of the system, in order.
/// # Safety
/// As [`Die::new`]: the caller serializes the dies' register accesses.
pub unsafe fn dies<P: ConfigRegionAccess + Copy>(pci: P) -> impl Iterator<Item = Die<P>> {
    (0..die_count(&pci)).map(move |index| unsafe { Die::new(pci, index) })
}
