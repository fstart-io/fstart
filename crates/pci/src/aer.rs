//! PCI Express error state through any config-space access (PCIe 6.0
//! §6.2, §7.5.3, §7.8.4): the capability walks, the Advanced Error
//! Reporting capability's registers, a snapshot of what a function holds of
//! an error, its clear and the setup a firmware-first platform gives every
//! function.
//!
//! Everything goes through [`ConfigRegionAccess`] and is free of panics, so
//! an SMM handler (whose image links no panic) can walk a hierarchy with
//! it; a bounded walk stops at an absent function (all ones), a malformed
//! pointer or a list longer than config space holds.

use tock_registers::register_bitfields;

use crate::{ConfigRegionAccess, PciAddress};

/// The PCI Express capability ID.
pub const CAP_PCIE: u8 = 0x10;
/// Extended capability IDs.
pub const EXT_CAP_AER: u16 = 0x0001;
pub const EXT_CAP_DSN: u16 = 0x0003;
pub const EXT_CAP_DVSEC: u16 = 0x0023;

/// AER capability registers (offsets from the capability).
pub mod reg {
    pub const UNCORRECTABLE_STATUS: u16 = 0x04;
    pub const UNCORRECTABLE_MASK: u16 = 0x08;
    pub const UNCORRECTABLE_SEVERITY: u16 = 0x0c;
    pub const CORRECTABLE_STATUS: u16 = 0x10;
    pub const CORRECTABLE_MASK: u16 = 0x14;
    pub const CAPABILITIES_CONTROL: u16 = 0x18;
    pub const ROOT_COMMAND: u16 = 0x2c;
    pub const ROOT_STATUS: u16 = 0x30;
    pub const ERROR_SOURCE: u16 = 0x34;
}

/// PCI Express capability registers (offsets from the capability).
pub mod pcie {
    /// Capability header and PCI Express Capabilities register.
    pub const HEADER: u16 = 0x00;
    /// Device Control (low word) and Device Status (high word, RW1C).
    pub const DEVICE_CONTROL_STATUS: u16 = 0x08;
    pub const SLOT_CAPABILITIES: u16 = 0x14;
}

register_bitfields![u32,
    /// PCI Express capability +0 (32-bit): its header and the PCI Express
    /// Capabilities register.
    pub PcieHeader [
        VERSION OFFSET(16) NUMBITS(4) [],
        PORT_TYPE OFFSET(20) NUMBITS(4) [
            Endpoint = 0,
            LegacyEndpoint = 1,
            RootPort = 4,
            Upstream = 5,
            Downstream = 6,
            PcieToPci = 7,
            PciToPcie = 8,
            RcIntegrated = 9,
            EventCollector = 10
        ],
        SLOT OFFSET(24) NUMBITS(1) []
    ],
    /// PCI Express capability +8 (32-bit): Device Control and Status.
    pub DeviceControlStatus [
        CORRECTABLE_REPORTING OFFSET(0) NUMBITS(1) [],
        NONFATAL_REPORTING OFFSET(1) NUMBITS(1) [],
        FATAL_REPORTING OFFSET(2) NUMBITS(1) [],
        UR_REPORTING OFFSET(3) NUMBITS(1) [],
        /// Correctable, non-fatal, fatal, unsupported request detected
        /// (RW1C).
        DETECTED OFFSET(16) NUMBITS(4) []
    ],
    /// AER +0x30 (32-bit): Root Error Status of a root port or event
    /// collector.
    pub RootErrorStatus [
        ERR_COR OFFSET(0) NUMBITS(1) [],
        MULTIPLE_ERR_COR OFFSET(1) NUMBITS(1) [],
        ERR_UNCOR OFFSET(2) NUMBITS(1) [],
        MULTIPLE_ERR_UNCOR OFFSET(3) NUMBITS(1) [],
        FIRST_UNCOR_FATAL OFFSET(4) NUMBITS(1) [],
        NONFATAL_RECEIVED OFFSET(5) NUMBITS(1) [],
        FATAL_RECEIVED OFFSET(6) NUMBITS(1) []
    ],
    /// AER +0x34 (32-bit): Error Source Identification, the requester ID
    /// of the first ERR_COR and ERR_FATAL/NONFATAL received.
    pub ErrorSource [
        CORRECTABLE OFFSET(0) NUMBITS(16) [],
        UNCORRECTABLE OFFSET(16) NUMBITS(16) []
    ]
];

/// The error bits of the Status and Secondary Status registers (RW1C):
/// master data parity, signaled target abort, received target and master
/// abort, signaled system error, detected parity error.
pub const STATUS_ERRORS: u16 = 0xf900;

/// Bounds of the capability walks.
const MAX_CAPABILITIES: usize = 48;
const MAX_EXT_CAPABILITIES: usize = 960;

/// A PCIe function with its PCI Express capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Function {
    pub address: PciAddress,
    /// Offset of the PCI Express capability.
    pub pcie: u16,
    /// Offset of the AER capability, if it has one.
    pub aer: Option<u16>,
    /// The device/port type ([`PcieHeader::PORT_TYPE`]).
    pub port_type: u8,
    /// The capability version (1 or 2).
    pub version: u8,
    /// A type 1 (bridge) header.
    pub bridge: bool,
}

impl Function {
    pub const fn is_root_port(&self) -> bool {
        self.port_type == 4
    }

    pub const fn is_event_collector(&self) -> bool {
        self.port_type == 10
    }
}

/// The AER capability registers a snapshot holds: 14 dwords of version 1,
/// 18 (the TLP prefix log too) from version 2 (as the AMD CPM's CPER).
pub const AER_DWORDS: usize = 18;
/// The PCI Express capability dwords a snapshot holds: 15 of version 2, 9
/// of version 1.
pub const PCIE_DWORDS: usize = 15;

/// What a function holds of an error, read once, for a CPER record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub function: Function,
    /// Vendor ID (low) and device ID (high).
    pub ids: u32,
    /// Command (low) and Status (high).
    pub command_status: u32,
    /// Revision ID, programming interface, subclass, class (cfg+8).
    pub class: u32,
    /// Primary, secondary, subordinate bus of a bridge (cfg+0x18).
    pub buses: u32,
    /// Secondary Status (low) and Bridge Control (high) of a bridge.
    pub bridge_control_status: u32,
    /// Slot Capabilities, if the port has a slot.
    pub slot: Option<u32>,
    /// The Device Serial Number capability's value.
    pub serial: Option<u64>,
    pub pcie: [u32; PCIE_DWORDS],
    pub pcie_len: u8,
    pub aer: [u32; AER_DWORDS],
    pub aer_len: u8,
}

impl Snapshot {
    fn aer(&self, offset: u16) -> u32 {
        self.aer
            .get(usize::from(offset / 4))
            .copied()
            .filter(|_| offset / 4 < u16::from(self.aer_len))
            .unwrap_or(0)
    }

    /// Unmasked uncorrectable errors.
    pub fn uncorrectable(&self) -> u32 {
        self.aer(reg::UNCORRECTABLE_STATUS) & !self.aer(reg::UNCORRECTABLE_MASK)
    }

    /// Unmasked uncorrectable errors that are fatal.
    pub fn fatal(&self) -> u32 {
        self.uncorrectable() & self.aer(reg::UNCORRECTABLE_SEVERITY)
    }

    /// Unmasked correctable errors.
    pub fn correctable(&self) -> u32 {
        self.aer(reg::CORRECTABLE_STATUS) & !self.aer(reg::CORRECTABLE_MASK)
    }

    /// The Root Error Status of a root port or event collector.
    pub fn root_status(&self) -> u32 {
        if self.function.is_root_port() || self.function.is_event_collector() {
            self.aer(reg::ROOT_STATUS)
        } else {
            0
        }
    }

    /// The Error Source Identification of a root port or event collector.
    pub fn error_source(&self) -> u32 {
        if self.function.is_root_port() || self.function.is_event_collector() {
            self.aer(reg::ERROR_SOURCE)
        } else {
            0
        }
    }

    /// Whether it holds an error at all.
    pub fn has_error(&self) -> bool {
        self.uncorrectable() != 0 || self.correctable() != 0 || self.root_status() & 0x7f != 0
    }
}

/// The AER setup of a firmware-first platform for one function: its
/// masks and severities, the root error command of a root port, and the
/// Device Control error reporting enables (correctable, non-fatal, fatal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub correctable_mask: u32,
    pub uncorrectable_mask: u32,
    pub uncorrectable_severity: u32,
    /// Root Error Command of a root port (its interrupt enables).
    pub root_command: u32,
}

/// Config-space access for the error registers of a hierarchy.
#[derive(Debug, Clone, Copy)]
pub struct Errors<P> {
    pci: P,
}

impl<P: ConfigRegionAccess> Errors<P> {
    /// # Safety
    /// The caller owns the config space of every function it reaches
    /// through it, as [`ConfigRegionAccess::read`]/`write` require.
    pub const unsafe fn new(pci: P) -> Self {
        Self { pci }
    }

    #[inline]
    fn read(&self, address: PciAddress, offset: u16) -> u32 {
        // SAFETY: the constructor's contract; `offset` is dword aligned.
        unsafe { self.pci.read(address, offset & !3) }
    }

    #[inline]
    fn write(&self, address: PciAddress, offset: u16, value: u32) {
        // SAFETY: as `read`.
        unsafe { self.pci.write(address, offset & !3, value) }
    }

    /// The config dword at `offset` (rounded down to a dword).
    pub fn config(&self, address: PciAddress, offset: u16) -> u32 {
        self.read(address, offset)
    }

    /// The Designated Vendor-Specific extended capability of `vendor` with
    /// DVSEC ID `id`.
    pub fn dvsec(&self, address: PciAddress, vendor: u16, id: u16) -> Option<u16> {
        let mut offset = 0x100u16;
        for _ in 0..MAX_EXT_CAPABILITIES {
            let header = self.read(address, offset);
            if header == 0 || header == u32::MAX {
                return None;
            }
            if header & 0xffff == u32::from(EXT_CAP_DVSEC)
                && self.read(address, offset + 4) & 0xffff == u32::from(vendor)
                && self.read(address, offset + 8) & 0xffff == u32::from(id)
            {
                return Some(offset);
            }
            offset = (header >> 20 & 0xffc) as u16;
            if offset < 0x100 {
                return None;
            }
        }
        None
    }

    /// The base of the memory BAR at config `offset` (both halves of a
    /// 64-bit one); `None` for an I/O BAR or an unassigned one.
    pub fn memory_bar(&self, address: PciAddress, offset: u16) -> Option<u64> {
        let low = self.read(address, offset);
        if low & 1 != 0 || low == u32::MAX {
            return None;
        }
        let high = if low >> 1 & 3 == 2 {
            u64::from(self.read(address, offset + 4)) << 32
        } else {
            0
        };
        let base = high | u64::from(low & !0xf);
        (base != 0).then_some(base)
    }

    /// Whether a function answers at `address`.
    pub fn present(&self, address: PciAddress) -> bool {
        self.read(address, 0) & 0xffff != 0xffff
    }

    /// Whether the function at `address` is multi-function (header type
    /// bit 7).
    pub fn multifunction(&self, address: PciAddress) -> bool {
        self.read(address, 0x0c) & 0x0080_0000 != 0
    }

    /// Conventional capability `id`: a bounded walk of the list.
    pub fn capability(&self, address: PciAddress, id: u8) -> Option<u16> {
        if !self.present(address) || self.read(address, 0x04) & 1 << 20 == 0 {
            return None;
        }
        let mut pointer = (self.read(address, 0x34) & 0xfc) as u16;
        for _ in 0..MAX_CAPABILITIES {
            if pointer < 0x40 {
                return None;
            }
            let header = self.read(address, pointer);
            if header & 0xff == u32::from(id) {
                return Some(pointer);
            }
            pointer = (header >> 8 & 0xfc) as u16;
        }
        None
    }

    /// Extended capability `id`: a bounded walk from 0x100.
    pub fn ext_capability(&self, address: PciAddress, id: u16) -> Option<u16> {
        let mut offset = 0x100u16;
        for _ in 0..MAX_EXT_CAPABILITIES {
            let header = self.read(address, offset);
            if header == 0 || header == u32::MAX {
                return None;
            }
            if header & 0xffff == u32::from(id) {
                return Some(offset);
            }
            offset = (header >> 20 & 0xffc) as u16;
            if offset < 0x100 {
                return None;
            }
        }
        None
    }

    /// The PCIe function at `address`, if one answers with a PCI Express
    /// capability.
    pub fn function(&self, address: PciAddress) -> Option<Function> {
        let pcie = self.capability(address, CAP_PCIE)?;
        let header = self.read(address, pcie + pcie::HEADER);
        let header = tock_registers::LocalRegisterCopy::<u32, PcieHeader::Register>::new(header);
        Some(Function {
            address,
            pcie,
            aer: self.ext_capability(address, EXT_CAP_AER),
            port_type: header.read(PcieHeader::PORT_TYPE) as u8,
            version: header.read(PcieHeader::VERSION) as u8,
            bridge: self.read(address, 0x0c) >> 16 & 0x7f == 1,
        })
    }

    /// The secondary and subordinate bus of a bridge.
    pub fn buses(&self, function: &Function) -> Option<(u8, u8)> {
        if !function.bridge {
            return None;
        }
        let buses = self.read(function.address, 0x18);
        let (secondary, subordinate) = ((buses >> 8) as u8, (buses >> 16) as u8);
        (secondary != 0 && subordinate >= secondary).then_some((secondary, subordinate))
    }

    /// The AER registers of `function` and what identifies it.
    pub fn snapshot(&self, function: &Function) -> Snapshot {
        let a = function.address;
        let mut snapshot = Snapshot {
            function: *function,
            ids: self.read(a, 0),
            command_status: self.read(a, 0x04),
            class: self.read(a, 0x08),
            buses: if function.bridge {
                self.read(a, 0x18)
            } else {
                0
            },
            bridge_control_status: if function.bridge {
                self.read(a, 0x3c) & 0xffff_0000 | self.read(a, 0x1c) >> 16
            } else {
                0
            },
            slot: None,
            serial: self.ext_capability(a, EXT_CAP_DSN).map(|dsn| {
                u64::from(self.read(a, dsn + 4)) | u64::from(self.read(a, dsn + 8)) << 32
            }),
            pcie: [0; PCIE_DWORDS],
            pcie_len: if function.version >= 2 { 15 } else { 9 },
            aer: [0; AER_DWORDS],
            aer_len: 0,
        };
        let header = self.read(a, function.pcie);
        if header & 1 << 24 != 0 {
            snapshot.slot = Some(self.read(a, function.pcie + pcie::SLOT_CAPABILITIES));
        }
        let mut offset = function.pcie;
        for (n, dword) in snapshot.pcie.iter_mut().enumerate() {
            if n < usize::from(snapshot.pcie_len) {
                *dword = self.read(a, offset);
            }
            offset = offset.wrapping_add(4);
        }
        if let Some(aer) = function.aer {
            let version = self.read(a, aer) >> 16 & 0xf;
            snapshot.aer_len = if version >= 2 { 18 } else { 14 };
            let mut offset = aer;
            for (n, dword) in snapshot.aer.iter_mut().enumerate() {
                if n < usize::from(snapshot.aer_len) {
                    *dword = self.read(a, offset);
                }
                offset = offset.wrapping_add(4);
            }
        }
        snapshot
    }

    /// Clear what `function` holds of an error: the AER uncorrectable and
    /// correctable status, a root port's Root Error Status, the Device
    /// Status error bits and the Status and Secondary Status error bits
    /// (all RW1C, each written with what it held).
    pub fn clear(&self, function: &Function) {
        let a = function.address;
        if let Some(aer) = function.aer {
            for offset in [reg::UNCORRECTABLE_STATUS, reg::CORRECTABLE_STATUS] {
                let status = self.read(a, aer + offset);
                if status != 0 && status != u32::MAX {
                    self.write(a, aer + offset, status);
                }
            }
            if function.is_root_port() || function.is_event_collector() {
                let status = self.read(a, aer + reg::ROOT_STATUS);
                if status & 0x7f != 0 && status != u32::MAX {
                    self.write(a, aer + reg::ROOT_STATUS, status);
                }
            }
        }
        let control = self.read(a, function.pcie + pcie::DEVICE_CONTROL_STATUS);
        if control >> 16 & 0xf != 0 && control != u32::MAX {
            self.write(
                a,
                function.pcie + pcie::DEVICE_CONTROL_STATUS,
                control & 0x000f_ffff,
            );
        }
        let command = self.read(a, 0x04);
        if (command >> 16) as u16 & STATUS_ERRORS != 0 && command != u32::MAX {
            self.write(
                a,
                0x04,
                command & 0xffff | u32::from(STATUS_ERRORS) << 16 & command,
            );
        }
        if function.bridge {
            let secondary = self.read(a, 0x1c);
            if (secondary >> 16) as u16 & STATUS_ERRORS != 0 && secondary != u32::MAX {
                self.write(
                    a,
                    0x1c,
                    secondary & 0xffff | u32::from(STATUS_ERRORS) << 16 & secondary,
                );
            }
        }
    }

    /// Set `function` up for firmware-first reporting as `setup` says:
    /// the AER masks and severity, a root port's Root Error Command and
    /// its Bridge Control SERR# enable, the Device Control correctable,
    /// non-fatal and fatal reporting enables; then clear what it holds.
    pub fn configure(&self, function: &Function, setup: &Setup) {
        let a = function.address;
        if let Some(aer) = function.aer {
            self.write(a, aer + reg::UNCORRECTABLE_MASK, setup.uncorrectable_mask);
            self.write(
                a,
                aer + reg::UNCORRECTABLE_SEVERITY,
                setup.uncorrectable_severity,
            );
            self.write(a, aer + reg::CORRECTABLE_MASK, setup.correctable_mask);
            if function.is_root_port() {
                self.write(a, aer + reg::ROOT_COMMAND, setup.root_command);
            }
        }
        let control = self.read(a, function.pcie + pcie::DEVICE_CONTROL_STATUS);
        // Status bits written as 0: none cleared here.
        self.write(
            a,
            function.pcie + pcie::DEVICE_CONTROL_STATUS,
            control & 0xffff | 0b111,
        );
        if function.is_root_port() {
            let bridge = self.read(a, 0x3c);
            self.write(a, 0x3c, bridge | 1 << 17);
        }
        self.clear(function);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    /// Config space of a few functions: dwords by (bus, devfn, offset),
    /// all ones elsewhere; writes to the RW1C registers named in `w1c`
    /// clear the bits written.
    #[derive(Default)]
    struct Space {
        dwords: RefCell<BTreeMap<(u8, u8, u16), u32>>,
        w1c: Vec<(u8, u8, u16, u32)>,
    }

    impl Space {
        fn set(&self, bus: u8, devfn: u8, offset: u16, value: u32) {
            self.dwords.borrow_mut().insert((bus, devfn, offset), value);
        }
        fn get(&self, bus: u8, devfn: u8, offset: u16) -> u32 {
            *self
                .dwords
                .borrow()
                .get(&(bus, devfn, offset))
                .unwrap_or(&u32::MAX)
        }
    }

    impl ConfigRegionAccess for &Space {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            let devfn = address.device() << 3 | address.function();
            self.get(address.bus(), devfn, offset)
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            let devfn = address.device() << 3 | address.function();
            let key = (address.bus(), devfn, offset);
            let old = self.get(address.bus(), devfn, offset);
            let w1c = self
                .w1c
                .iter()
                .find(|w| (w.0, w.1, w.2) == key)
                .map_or(0, |w| w.3);
            let new = (value & !w1c) | (old & w1c & !value);
            self.dwords.borrow_mut().insert(key, new);
        }
    }

    /// A root port at 0:1.1 (PCIe cap 0x58 v2, AER at 0x100 then DSN at
    /// 0x150) with a correctable error received from 2:0.0.
    fn root_port(space: &mut Space) {
        let (bus, devfn) = (0, 0x09);
        space.set(bus, devfn, 0, 0x1483_1022);
        space.set(bus, devfn, 0x04, 0x0010_0407);
        space.set(bus, devfn, 0x08, 0x0604_0000);
        space.set(bus, devfn, 0x0c, 0x0081_0000);
        space.set(bus, devfn, 0x18, 0x0002_0200);
        space.set(bus, devfn, 0x1c, 0x0000_0000);
        space.set(bus, devfn, 0x34, 0x50);
        space.set(bus, devfn, 0x3c, 0x0000_01ff);
        space.set(bus, devfn, 0x50, 0x0000_5805);
        space.set(bus, devfn, 0x58, 0x0142_0010);
        space.set(bus, devfn, 0x60, 0x0001_2810);
        space.set(bus, devfn, 0x6c, 0x0008_0000);
        space.set(bus, devfn, 0x100, 0x1502_0001);
        space.set(bus, devfn, 0x110, 0x0000_0001);
        space.set(bus, devfn, 0x114, 0x0000_2000);
        space.set(bus, devfn, 0x130, 0x0000_0001);
        space.set(bus, devfn, 0x134, 0x0000_0200);
        space.set(bus, devfn, 0x150, 0x0001_0003);
        space.set(bus, devfn, 0x154, 0x1122_3344);
        space.set(bus, devfn, 0x158, 0x5566_7788);
        space.w1c = vec![
            (bus, devfn, 0x110, u32::MAX),
            (bus, devfn, 0x130, 0x7f),
            (bus, devfn, 0x60, 0x000f_0000),
            (bus, devfn, 0x04, 0xf900_0000),
        ];
    }

    #[test]
    fn a_root_port_with_a_correctable_error() {
        let mut space = Space::default();
        root_port(&mut space);
        let space = space;
        let errors = unsafe { Errors::new(&space) };
        let rp = PciAddress::new(0, 0, 1, 1);
        let function = errors.function(rp).unwrap();
        assert_eq!(
            (
                function.pcie,
                function.aer,
                function.port_type,
                function.version
            ),
            (0x58, Some(0x100), 4, 2)
        );
        assert!(function.bridge && function.is_root_port());
        assert_eq!(errors.buses(&function), Some((2, 2)));
        assert_eq!(errors.ext_capability(rp, EXT_CAP_DSN), Some(0x150));
        assert_eq!(errors.ext_capability(rp, 0x0d), None);
        let snapshot = errors.snapshot(&function);
        assert_eq!(snapshot.serial, Some(0x5566_7788_1122_3344));
        assert_eq!((snapshot.pcie_len, snapshot.aer_len), (15, 18));
        assert_eq!(snapshot.pcie[0], 0x0142_0010);
        assert_eq!(snapshot.slot, Some(0x0008_0000));
        assert_eq!(snapshot.correctable(), 1);
        assert_eq!(snapshot.uncorrectable(), 0);
        assert_eq!(snapshot.root_status(), 1);
        assert_eq!(snapshot.error_source() & 0xffff, 0x0200);
        assert!(snapshot.has_error());
        errors.clear(&function);
        let after = errors.snapshot(&function);
        assert!(!after.has_error());
        assert_eq!(space.get(0, 9, 0x60) >> 16, 0);
    }

    #[test]
    fn firmware_first_setup_of_a_root_port() {
        let mut space = Space::default();
        root_port(&mut space);
        let space = space;
        let errors = unsafe { Errors::new(&space) };
        let function = errors.function(PciAddress::new(0, 0, 1, 1)).unwrap();
        errors.configure(
            &function,
            &Setup {
                correctable_mask: 0x2000,
                uncorrectable_mask: 0,
                uncorrectable_severity: 0x57ef_6030,
                root_command: 0,
            },
        );
        assert_eq!(space.get(0, 9, 0x108), 0);
        assert_eq!(space.get(0, 9, 0x10c), 0x57ef_6030);
        assert_eq!(space.get(0, 9, 0x114), 0x2000);
        assert_eq!(space.get(0, 9, 0x12c), 0);
        // Device Control: CE, NFE, FE reporting; the status cleared.
        assert_eq!(space.get(0, 9, 0x60), 0x2817);
        // Bridge Control SERR# enable.
        assert_eq!(space.get(0, 9, 0x3c), 0x0002_01ff);
        assert_eq!(space.get(0, 9, 0x110), 0);
    }

    #[test]
    fn absent_and_malformed_functions_end_the_walks() {
        let space = Space::default();
        let errors = unsafe { Errors::new(&space) };
        let a = PciAddress::new(0, 3, 0, 0);
        assert!(!errors.present(a));
        assert_eq!(errors.function(a), None);
        // A capability list pointing at itself, an extended list looping.
        space.set(3, 0, 0, 0x1234_1022);
        space.set(3, 0, 0x04, 1 << 20);
        space.set(3, 0, 0x34, 0x40);
        space.set(3, 0, 0x40, 0x0000_4001);
        space.set(3, 0, 0x100, 0x1000_0002);
        assert_eq!(errors.capability(a, 0x10), None);
        assert_eq!(errors.ext_capability(a, EXT_CAP_AER), None);
    }
}
