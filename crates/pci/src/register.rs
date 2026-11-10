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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeProbeError {
    NotBridge,
    BusRange,
    Readback,
}

/// Temporarily route one otherwise unused secondary bus and read dev0/fn0's
/// identity. Restore the original routing before returning. This does not
/// size BARs, enable bus mastering, execute option ROMs or allocate resources.
/// # Safety
/// Caller exclusively owns the bridge and an unused, mapped/routed secondary
/// bus in the SAME PCI segment. No downstream transaction may be in flight.
pub unsafe fn probe_direct_child<P: ConfigRegionAccess>(
    pci: &P,
    bridge: PciAddress,
    secondary: u8,
) -> Result<Option<u32>, BridgeProbeError> {
    use crate::config::{PCI_BRIDGE_BUSES as B, PCI_PRIMARY_BUS};
    use tock_registers::interfaces::ReadWriteable;
    if secondary <= bridge.bus() {
        return Err(BridgeProbeError::BusRange);
    }
    // Header type at byte0E, within its aligned config dword.
    if (unsafe { pci.read(bridge, 0x0c) } >> 16) & 0x7f != 1 {
        return Err(BridgeProbeError::NotBridge);
    }
    let buses = unsafe { ConfigRegister::<_, B::Register>::new(pci, bridge, PCI_PRIMARY_BUS) };
    let original = buses.get();
    buses.modify(
        B::PRIMARY.val(bridge.bus().into())
            + B::SECONDARY.val(secondary.into())
            + B::SUBORDINATE.val(secondary.into()),
    );
    let expected = u32::from(bridge.bus()) | u32::from(secondary) << 8 | u32::from(secondary) << 16;
    let routed = buses.get() & 0x00ff_ffff == expected;
    let child = PciAddress::new(bridge.segment(), secondary, 0, 0);
    let identity = if routed {
        unsafe { pci.read(child, 0) }
    } else {
        u32::MAX
    };
    buses.set(original);
    if !routed || buses.get() != original {
        return Err(BridgeProbeError::Readback);
    }
    Ok((identity & 0xffff != 0xffff && identity & 0xffff != 0).then_some(identity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_bridge_probe_restores_routing_without_touching_bars() {
        struct Model(core::cell::Cell<u32>);
        impl ConfigRegionAccess for Model {
            unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
                match (a.bus(), offset) {
                    (0, 0x0c) => 0x10000,
                    (0, 0x18) => self.0.get(),
                    (2, 0) => 0x15338086,
                    _ => panic!("unexpected probe access"),
                }
            }
            unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
                assert_eq!((a.bus(), offset), (0, 0x18));
                self.0.set(value);
            }
        }
        let model = Model(core::cell::Cell::new(0x77000000));
        assert_eq!(
            unsafe { probe_direct_child(&model, PciAddress::new(0, 0, 1, 1), 2) },
            Ok(Some(0x15338086))
        );
        assert_eq!(model.0.get(), 0x77000000);
        assert_eq!(
            unsafe { probe_direct_child(&model, PciAddress::new(0, 0, 1, 1), 0) },
            Err(BridgeProbeError::BusRange)
        );
    }
    #[test]
    fn devfn_and_bdf_packing() {
        assert_eq!(devfn(0x14, 3), 0xa3);
        assert_eq!(devfn_parts(0xa3), (0x14, 3));
        assert_eq!(bdf(0x40, 0x14, 3), 0x40a3);
    }
}
