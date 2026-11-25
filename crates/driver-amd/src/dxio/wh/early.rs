// SPDX-License-Identifier: MIT OR Apache-2.0
//! The link the ABL trains before x86 runs (`EarlyBmcLinkTraining`, with
//! its socket and lane): typically a BMC's display on the bonus core.
//!
//! The vendor (`PcieEarlyTrainFixupsWh` → `MpioCleanUpEarlyInitSP5Wh`, Dell
//! `0xf61370`; decompiler-inferred) checks every port of core 6: one whose
//! LTSSM state (`0x1C44_0294 + rc << 20 + port << 12`, bits 5:0, read
//! through MPIO) is in `0x10..=0x1B` is trained; its devfn is the one in
//! its slot's `NB_PROG_DEVICE_REMAP` (or the default), and the bus numbers,
//! windows and BARs the ABL left below it are cleared
//! (`MpioCleanUpEarlyPort` `0xf60df0`, `MpioCleanUpEarlyBridge` `0xf60ac0`).
//! After training it asks MPIO for the lane's early-link information
//! (request `0x0E`, `MpioEarlyBmcGetDeliInfo` `0xf470c0`).

use fstart_pci::{
    ConfigRegionAccess, PCI_BAR0, PCI_BAR5, PCI_CLASS_REVISION, PCI_COMMAND, PCI_IO_BASE,
    PCI_MEMORY_BASE, PCI_PREF_BASE_UPPER32, PCI_PREF_LIMIT_UPPER32, PCI_PREF_MEMORY_BASE,
    PCI_PRIMARY_BUS, PciAddress,
};
use tock_registers::LocalRegisterCopy;

use crate::dxio::rs::mailbox::Error;
use crate::dxio::wh::{Proxy, Width};
use crate::nbio::wh::pcie::{self, Ltssm, PortId};
use crate::smn::SmnAccess;

/// LTSSM states the vendor takes for a trained link.
pub const fn ltssm_trained(value: u32) -> bool {
    matches!(value & 0x3f, 0x10..=0x1b)
}

/// Whether port `port`'s LTSSM ([`pcie::LTSSM`], read through MPIO) is in a
/// trained state.
pub fn trained<A: SmnAccess>(proxy: &Proxy<'_, A>, port: PortId) -> Result<bool, Error> {
    let raw = proxy.read(pcie::LTSSM.at(port).address(), Width::Dword)?;
    let state = LocalRegisterCopy::<u32, Ltssm::Register>::new(raw).read(Ltssm::STATE);
    Ok(ltssm_trained(state))
}

const CLASS: u16 = PCI_CLASS_REVISION;
const COMMAND: u16 = PCI_COMMAND;
const BUSES: u16 = PCI_PRIMARY_BUS;
/// I/O Base/Limit Upper 16 Bits (type 1 header).
const IO_UPPER: u16 = 0x30;

/// Clear what the ABL configured below root port `port` (`bus:dev.fn`):
/// a display behind it loses its command and BARs, a bridge its windows
/// and buses, recursively; then the root port its windows, command and
/// secondary/subordinate buses.
///
/// # Safety
/// `pci` reaches the root port's segment; nothing else uses the buses
/// below the port.
pub unsafe fn clean_up(pci: &impl ConfigRegionAccess, port: PciAddress) {
    // SAFETY: caller's contract.
    unsafe {
        let buses = pci.read(port, BUSES);
        let secondary = (buses >> 8) as u8;
        if buses & 0x00ff_ff00 == 0 || secondary == 0 {
            return;
        }
        clean_up_bridge(pci, PciAddress::new(port.segment(), secondary, 0, 0), 4);
        clear_bridge(pci, port);
    }
}

/// `MpioCleanUpEarlyBridge`; `depth` bounds the recursion.
unsafe fn clean_up_bridge(pci: &impl ConfigRegionAccess, at: PciAddress, depth: u8) {
    // SAFETY: caller's contract.
    unsafe {
        let class = pci.read(at, CLASS) >> 24;
        if class == 3 {
            pci.write(at, COMMAND, 0);
            for bar in (PCI_BAR0..=PCI_BAR5).step_by(4) {
                pci.write(at, bar, 0);
            }
        } else if class == 6 {
            let buses = pci.read(at, BUSES);
            if buses & 0x00ff_ff00 != 0 && depth > 0 {
                let next = PciAddress::new(at.segment(), (buses >> 8) as u8, 0, 0);
                clean_up_bridge(pci, next, depth - 1);
                clear_bridge(pci, at);
            }
        } else if at.function() < 7 {
            let next = PciAddress::new(at.segment(), at.bus(), at.device(), at.function() + 1);
            clean_up_bridge(pci, next, depth);
        }
    }
}

/// Clear a bridge's I/O base/limit (16-bit), memory, prefetchable and
/// upper windows, command, and secondary/subordinate buses.
unsafe fn clear_bridge(pci: &impl ConfigRegionAccess, at: PciAddress) {
    // SAFETY: caller's contract.
    unsafe {
        let io = pci.read(at, PCI_IO_BASE);
        pci.write(at, PCI_IO_BASE, io & 0xffff_0000);
        for reg in [
            PCI_MEMORY_BASE,
            PCI_PREF_MEMORY_BASE,
            PCI_PREF_BASE_UPPER32,
            PCI_PREF_LIMIT_UPPER32,
            IO_UPPER,
            COMMAND,
        ] {
            pci.write(at, reg, 0);
        }
        let buses = pci.read(at, BUSES);
        pci.write(at, BUSES, buses & 0xff00_00ff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Pci(RefCell<BTreeMap<(u8, u8, u8, u16), u32>>);
    impl ConfigRegionAccess for Pci {
        unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
            *self
                .0
                .borrow()
                .get(&(a.bus(), a.device(), a.function(), offset))
                .unwrap_or(&0)
        }
        unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
            self.0
                .borrow_mut()
                .insert((a.bus(), a.device(), a.function(), offset), value);
        }
    }

    #[test]
    fn abl_bus_and_bar_state_below_a_bmc_display_is_cleared() {
        let pci = Pci::default();
        let set = |b, d, f, o, v| unsafe { pci.write(PciAddress::new(0, b, d, f), o, v) };
        // Root port 0:5.2 → bus 0x41 (AST bridge) → bus 0x42 (VGA).
        set(0, 5, 2, BUSES, 0x0042_4100);
        set(0, 5, 2, 0x20, 0xf000_e000);
        set(0, 5, 2, 0x1c, 0x2000_f0f0);
        set(0x41, 0, 0, CLASS, 0x0604_0000);
        set(0x41, 0, 0, BUSES, 0x0042_4241);
        set(0x41, 0, 0, COMMAND, 7);
        set(0x42, 0, 0, CLASS, 0x0300_0000);
        set(0x42, 0, 0, 0x10, 0xe000_0000);
        set(0x42, 0, 0, COMMAND, 3);
        unsafe { clean_up(&pci, PciAddress::new(0, 0, 5, 2)) };
        let get = |b, d, f, o| unsafe { pci.read(PciAddress::new(0, b, d, f), o) };
        assert_eq!(get(0x42, 0, 0, 0x10), 0);
        assert_eq!(get(0x42, 0, 0, COMMAND), 0);
        assert_eq!(get(0x41, 0, 0, BUSES), 0x41);
        assert_eq!(get(0x41, 0, 0, COMMAND), 0);
        assert_eq!(get(0, 5, 2, BUSES), 0);
        assert_eq!(get(0, 5, 2, 0x20), 0);
        assert_eq!(get(0, 5, 2, 0x1c), 0x2000_0000);
        assert!(ltssm_trained(0x11) && !ltssm_trained(0x1c) && !ltssm_trained(4));
    }
}
