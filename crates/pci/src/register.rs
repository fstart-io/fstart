//! Typed config-space registers and device/function numbering.

use core::marker::PhantomData;

use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

use crate::{ConfigRegionAccess, PciAddress};

/// Device and function in the 8-bit form of config addresses, ACPI `_ADR`
/// halves, routing tables and chipset remap registers.
pub const fn devfn(device: u8, function: u8) -> u8 {
    device << 3 | function
}

/// The device and function of a [`devfn`].
pub const fn devfn_parts(devfn: u8) -> (u8, u8) {
    (devfn >> 3, devfn & 7)
}

/// The 16-bit bus/device/function of a function (the PCIe requester ID, the
/// IVRS and DMAR device ID).
pub const fn bdf(bus: u8, device: u8, function: u8) -> u16 {
    (bus as u16) << 8 | devfn(device, function) as u16
}

/// A 32-bit config register of one function, with the bitfields `R`.
///
/// Every access goes through `P`; the type implements the tock interfaces so
/// field reads, writes and `modify` work on config space as on MMIO.
#[derive(Clone, Copy)]
pub struct ConfigRegister<P, R: RegisterLongName = ()> {
    pci: P,
    address: PciAddress,
    offset: u16,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess, R: RegisterLongName> ConfigRegister<P, R> {
    /// # Safety
    /// `offset` is a dword-aligned register of the function at `address`, and
    /// the caller owns it as [`ConfigRegionAccess::read`]/`write` require.
    pub const unsafe fn new(pci: P, address: PciAddress, offset: u16) -> Self {
        Self {
            pci,
            address,
            offset,
            _reg: PhantomData,
        }
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for ConfigRegister<P, R> {
    type T = u32;
    type R = R;

    #[inline(always)]
    fn get(&self) -> u32 {
        // SAFETY: the constructor's contract.
        unsafe { self.pci.read(self.address, self.offset) }
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for ConfigRegister<P, R> {
    type T = u32;
    type R = R;

    #[inline(always)]
    fn set(&self, value: u32) {
        // SAFETY: the constructor's contract.
        unsafe { self.pci.write(self.address, self.offset, value) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devfn_and_bdf_packing() {
        assert_eq!(devfn(0x14, 3), 0xa3);
        assert_eq!(devfn_parts(0xa3), (0x14, 3));
        assert_eq!(bdf(0x40, 0x14, 3), 0x40a3);
    }
}
