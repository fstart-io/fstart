//! System Management Network access.
//!
//! Every root bus has an SMN index/data pair in the host bridge's config
//! space (D0F0 `0xB8`/`0xBC`, NBIO_SMU §3.1). Naples reaches each die through
//! that die's root bus; Rome and Milan reach each IOD through any of its
//! root buses and address the NBIO quadrants by SMN offset.
//!
//! The pair is a shared index: callers serialize access (the BSP owns it
//! during boot; SMM code must save and restore the index).

use core::marker::PhantomData;

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

/// D0F0 SMN index register.
pub const SMN_INDEX: u16 = 0xB8;
/// D0F0 SMN data register.
pub const SMN_DATA: u16 = 0xBC;

/// An SMN address (32-bit, dword aligned for register accesses).
pub type SmnAddr = u32;

/// 00.0 of `bus`, without `PciAddress::new`: its device/function range
/// asserts are a panic path, which an SMM image (the SP3 handler reaches
/// SMN) may not link, and 00.0 has nothing to check.
fn host_bridge(bus: u8) -> PciAddress {
    const { assert!(size_of::<PciAddress>() == size_of::<u32>()) };
    // SAFETY: `PciAddress` is one `u32` packed segment:bus:device:function
    // = 16:8:5:3 (pci_types docs; same size checked above, the test below
    // pins the packing), and every `u32` is a valid address.
    unsafe { core::mem::transmute::<u32, PciAddress>(u32::from(bus) << 8) }
}

/// SMN access through the host bridge of one root bus.
#[derive(Clone, Copy)]
pub struct Smn<P> {
    pci: P,
    host: PciAddress,
}

impl<P: ConfigRegionAccess> Smn<P> {
    /// SMN through the host bridge (device 0, function 0) of `bus`.
    pub fn new(pci: P, bus: u8) -> Self {
        Self {
            pci,
            host: host_bridge(bus),
        }
    }

    /// The root bus whose host bridge carries the accesses.
    pub fn bus(&self) -> u8 {
        self.host.bus()
    }

    /// Read the dword at `addr`.
    ///
    /// # Safety
    /// The caller owns the index/data pair for the duration of the call and
    /// the access has no side effects the caller does not expect.
    pub unsafe fn read32(&self, addr: SmnAddr) -> u32 {
        // SAFETY: the caller's contract covers the index/data pair.
        unsafe {
            self.pci.write(self.host, SMN_INDEX, addr & !3);
            self.pci.read(self.host, SMN_DATA)
        }
    }

    /// Write the dword at `addr`.
    ///
    /// # Safety
    /// As [`read32`](Self::read32); the write must be valid for the target.
    pub unsafe fn write32(&self, addr: SmnAddr, value: u32) {
        // SAFETY: the caller's contract covers the index/data pair.
        unsafe {
            self.pci.write(self.host, SMN_INDEX, addr & !3);
            self.pci.write(self.host, SMN_DATA, value)
        }
    }

    /// Read-modify-write: clear `clear`, then set `set`.
    ///
    /// # Safety
    /// As [`write32`](Self::write32).
    pub unsafe fn modify32(&self, addr: SmnAddr, clear: u32, set: u32) {
        // SAFETY: forwarded contract.
        unsafe { self.write32(addr, (self.read32(addr) & !clear) | set) }
    }

    /// A typed register at `addr` with the bitfields `R`.
    ///
    /// # Safety
    /// `addr` is a register the caller owns as [`write32`](Self::write32)
    /// requires, for the lifetime of the returned value.
    pub unsafe fn register<R: RegisterLongName>(&self, addr: SmnAddr) -> SmnRegister<P, R>
    where
        P: Clone,
    {
        SmnRegister {
            smn: self.clone(),
            addr,
            _reg: PhantomData,
        }
    }
}

/// A 32-bit SMN register with the bitfields `R`; implements the
/// tock-registers interfaces so field reads, writes and `modify` work as on
/// MMIO.
#[derive(Clone, Copy)]
pub struct SmnRegister<P, R: RegisterLongName = ()> {
    smn: Smn<P>,
    addr: SmnAddr,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for SmnRegister<P, R> {
    type T = u32;
    type R = R;

    fn get(&self) -> u32 {
        // SAFETY: the constructor's contract.
        unsafe { self.smn.read32(self.addr) }
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for SmnRegister<P, R> {
    type T = u32;
    type R = R;

    fn set(&self, value: u32) {
        // SAFETY: the constructor's contract.
        unsafe { self.smn.write32(self.addr, value) }
    }
}

#[cfg(test)]
pub(crate) mod model {
    //! A config-space model with an SMN behind every host bridge, for tests.
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use fstart_pci::{ConfigRegionAccess, PciAddress};

    use super::{SMN_DATA, SMN_INDEX};

    /// Shared state: config dwords and SMN dwords per bus.
    #[derive(Default)]
    pub struct State {
        pub config: BTreeMap<(u8, u8, u8, u16), u32>,
        pub smn: BTreeMap<(u8, u32), u32>,
        pub index: BTreeMap<u8, u32>,
        pub log: Vec<(u8, u32, u32)>,
    }

    /// Cloneable handle on the model.
    #[derive(Clone, Default)]
    pub struct Model(pub Rc<RefCell<State>>);

    impl ConfigRegionAccess for Model {
        unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
            let s = self.0.borrow();
            if (a.device(), a.function(), offset) == (0, 0, SMN_DATA) {
                let index = s.index.get(&a.bus()).copied().unwrap_or(0);
                return s.smn.get(&(a.bus(), index)).copied().unwrap_or(0);
            }
            let key = (a.bus(), a.device(), a.function(), offset);
            s.config.get(&key).copied().unwrap_or(0xffff_ffff)
        }

        unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
            let mut s = self.0.borrow_mut();
            match (a.device(), a.function(), offset) {
                (0, 0, SMN_INDEX) => {
                    s.index.insert(a.bus(), value);
                }
                (0, 0, SMN_DATA) => {
                    let index = s.index.get(&a.bus()).copied().unwrap_or(0);
                    s.smn.insert((a.bus(), index), value);
                    s.log.push((a.bus(), index, value));
                }
                _ => {
                    s.config
                        .insert((a.bus(), a.device(), a.function(), offset), value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::Model;
    use super::*;
    use tock_registers::register_bitfields;

    register_bitfields![u32, TEST [LOW OFFSET(0) NUMBITS(4) [], HIGH OFFSET(28) NUMBITS(4) []]];

    #[test]
    fn host_bridge_is_device_0_function_0_of_the_bus() {
        assert!((0..=u8::MAX).all(|bus| host_bridge(bus) == PciAddress::new(0, bus, 0, 0)));
    }

    #[test]
    fn index_data_round_trip() {
        let model = Model::default();
        let smn = Smn::new(model.clone(), 0x40);
        unsafe {
            smn.write32(0x13b1_02e2, 0x1234);
            assert_eq!(smn.read32(0x13b1_02e0), 0x1234, "index is dword aligned");
            smn.modify32(0x13b1_02e0, 0x0f00, 0x8000_0000);
        }
        assert_eq!(model.0.borrow().smn[&(0x40, 0x13b1_02e0)], 0x8000_1034);
    }

    #[test]
    fn typed_register() {
        let model = Model::default();
        let smn = Smn::new(model.clone(), 0);
        let reg = unsafe { smn.register::<TEST::Register>(0x100) };
        reg.write(TEST::LOW.val(5) + TEST::HIGH.val(0xa));
        assert_eq!(reg.read(TEST::HIGH), 0xa);
        assert_eq!(reg.get(), 0xa000_0005);
    }
}
