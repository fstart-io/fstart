// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso indirect register transports (SMN, Data Fabric).
//! Index/data pairs are exclusively owned by the BSP during silicon init.
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// SMN is a chipset register network, not an x86 physical address space.
pub trait Smn {
    fn read(&mut self, address: u32) -> u32;
    fn write(&mut self, address: u32, value: u32);
    /// Clear-mask semantics (not the preserve-mask semantics of some Intel APIs).
    fn update(&mut self, address: u32, clear: u32, set: u32) {
        let value = self.read(address);
        self.write(address, (value & !clear) | set);
    }
}

pub struct SmnPort<P> {
    pci: P,
}
impl<P: ConfigRegionAccess> SmnPort<P> {
    /// # Safety
    /// PCI D0F0 exists and its B8/BC SMN index/data pair is exclusively owned.
    pub unsafe fn new(pci: P) -> Self {
        Self { pci }
    }
}
impl<P: ConfigRegionAccess> Smn for SmnPort<P> {
    fn read(&mut self, address: u32) -> u32 {
        assert!(address & 3 == 0);
        let gnb = PciAddress::new(0, 0, 0, 0);
        unsafe {
            self.pci.write(gnb, 0xb8, address);
            self.pci.read(gnb, 0xbc)
        }
    }
    fn write(&mut self, address: u32, value: u32) {
        assert!(address & 3 == 0);
        let gnb = PciAddress::new(0, 0, 0, 0);
        unsafe {
            self.pci.write(gnb, 0xb8, address);
            self.pci.write(gnb, 0xbc, value);
        }
    }
}

register_bitfields![u32,
    Ficaa [INSTANCE_ACCESS OFFSET(0) NUMBITS(1) [], REGISTER OFFSET(2) NUMBITS(9) [],
        FUNCTION OFFSET(11) NUMBITS(3) [], INSTANCE OFFSET(16) NUMBITS(8) []]
];
/// The broadcast selector is special: never send 0xff through FICAA.
pub const BROADCAST: u8 = 0xff;
pub trait Fabric {
    fn read(&mut self, function: u8, offset: u16, instance: u8) -> u32;
    fn write(&mut self, function: u8, offset: u16, instance: u8, value: u32);
    fn update(&mut self, function: u8, offset: u16, instance: u8, clear: u32, set: u32) {
        let value = self.read(function, offset, instance);
        self.write(function, offset, instance, (value & !clear) | set);
    }
}
pub struct FabricPort<P> {
    pci: P,
}
impl<P: ConfigRegionAccess> FabricPort<P> {
    /// # Safety
    /// D18F0..F7 exist; the FICAA/FICAD pair is exclusively owned by this caller.
    pub unsafe fn new(pci: P) -> Self {
        Self { pci }
    }
    fn target(&self, function: u8, offset: u16, instance: u8) -> (PciAddress, u16) {
        assert!(function < 8 && offset < 2048 && offset & 3 == 0);
        if instance == BROADCAST {
            return (PciAddress::new(0, 0, 0x18, function), offset);
        }
        let control = LocalRegisterCopy::<u32, Ficaa::Register>::new(0);
        let index = control.get()
            | (Ficaa::INSTANCE_ACCESS::SET
                + Ficaa::REGISTER.val(u32::from(offset >> 2))
                + Ficaa::FUNCTION.val(u32::from(function))
                + Ficaa::INSTANCE.val(u32::from(instance)))
            .value;
        let df4 = PciAddress::new(0, 0, 0x18, 4);
        unsafe {
            self.pci.write(df4, 0x5c, index);
        }
        (df4, 0x98)
    }
}
impl<P: ConfigRegionAccess> Fabric for FabricPort<P> {
    fn read(&mut self, function: u8, offset: u16, instance: u8) -> u32 {
        let (address, offset) = self.target(function, offset, instance);
        unsafe { self.pci.read(address, offset) }
    }
    fn write(&mut self, function: u8, offset: u16, instance: u8, value: u32) {
        let (address, offset) = self.target(function, offset, instance);
        unsafe {
            self.pci.write(address, offset, value);
        }
    }
}
