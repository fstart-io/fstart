//! A DF config-space model for host tests: node registers, FICAA/FICAD
//! instance registers, and the SMN model behind the host bridges.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use fstart_pci::{ConfigRegionAccess, PciAddress};

use super::{NODE0_DEVICE, regs};
use crate::sp3::smn::model::Model as SmnModel;

/// `(node, instance, function, offset)`; `None` is the node's own copy.
pub type Key = (u8, Option<u8>, u8, u16);

#[derive(Default)]
pub struct State {
    pub regs: BTreeMap<Key, u32>,
    pub ficaa: BTreeMap<u8, u32>,
    /// Every DF register write in order (FICAA writes excluded).
    pub log: Vec<(Key, u32)>,
}

#[derive(Clone, Default)]
pub struct DfModel {
    pub state: Rc<RefCell<State>>,
    pub smn: SmnModel,
}

impl DfModel {
    pub fn set(&self, node: u8, inst: Option<u8>, func: u8, off: u16, v: u32) {
        self.state
            .borrow_mut()
            .regs
            .insert((node, inst, func, off), v);
    }

    pub fn get(&self, node: u8, inst: Option<u8>, func: u8, off: u16) -> u32 {
        self.state
            .borrow()
            .regs
            .get(&(node, inst, func, off))
            .copied()
            .unwrap_or(0)
    }

    pub fn ficaa(&self, node: u8) -> u32 {
        self.state.borrow().ficaa.get(&node).copied().unwrap_or(0)
    }

    pub fn log(&self) -> Vec<(Key, u32)> {
        self.state.borrow().log.clone()
    }

    pub fn clear_log(&self) {
        self.state.borrow_mut().log.clear()
    }

    fn df_node(a: PciAddress) -> Option<u8> {
        (a.bus() == 0 && (NODE0_DEVICE..NODE0_DEVICE + 8).contains(&a.device()))
            .then(|| a.device() - NODE0_DEVICE)
    }

    /// The register FICAD currently addresses on `node`.
    fn indirect(&self, node: u8) -> Key {
        let f = self.ficaa(node);
        assert_eq!(f & 1, 1, "FICAD access without an instance FICAA");
        (
            node,
            Some((f >> 16) as u8),
            ((f >> 11) & 7) as u8,
            (f & 0x7FC) as u16,
        )
    }
}

impl ConfigRegionAccess for DfModel {
    unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
        let Some(node) = Self::df_node(a) else {
            // SAFETY: test model.
            return unsafe { self.smn.read(a, offset) };
        };
        let key = match (a.function(), offset) {
            (4, regs::FICAD_LO) => self.indirect(node),
            (4, regs::FICAA) => return self.ficaa(node),
            (f, o) => (node, None, f, o),
        };
        self.get(key.0, key.1, key.2, key.3)
    }

    unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
        let Some(node) = Self::df_node(a) else {
            // SAFETY: test model.
            return unsafe { self.smn.write(a, offset, value) };
        };
        let key = match (a.function(), offset) {
            (4, regs::FICAA) => {
                self.state.borrow_mut().ficaa.insert(node, value);
                return;
            }
            (4, regs::FICAD_LO) => self.indirect(node),
            (f, o) => (node, None, f, o),
        };
        let mut s = self.state.borrow_mut();
        s.regs.insert(key, value);
        s.log.push((key, value));
    }
}
