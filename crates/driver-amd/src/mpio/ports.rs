// SPDX-License-Identifier: MIT OR Apache-2.0
//! Siena/NFX native wrapper ports and devfn-remap slots. NFX templates at
//! FFA4A63C/FFA4B37C: nine engines per regular wrapper, four on even-root
//! Gen3 wrappers. Native slot/LBID is NOT the firmware's wrapper-local ID.
use super::rs::{self, Ask, Kind, Lane, Port, Status};
use crate::{
    nbio::iohc::NbProgDeviceRemap as Remap,
    smn::{SmnAccess, SmnReg},
};
use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable};

pub const NATIVE_DEVFNS: [u8; 24] = [
    9, 10, 11, 12, 13, 14, 15, 17, 18, 25, 26, 27, 28, 29, 30, 31, 33, 34, 41, 42, 43, 44, 57, 58,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Root,
    Port,
    Missing,
    Duplicate,
    Unsupported,
    Unavailable,
    Verify,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub ask: usize,
    pub policy: usize,
    pub root: u8,
    pub wrapper: u8,
    pub local_port: u8,
    pub slot: u8,
    pub devfn: u8,
    pub logical_bridge: u8,
}
pub fn slot(root: u8, wrapper: u8, local: u8) -> Result<u8, Error> {
    if root >= 4 {
        return Err(Error::Root);
    }
    match (wrapper, local) {
        (0, 0..=8) => Ok(local),
        (1, 0..=8) => Ok(9 + local),
        (2, 0..=3) if root.is_multiple_of(2) => Ok(18 + local),
        _ => Err(Error::Port),
    }
}
pub fn remap_register(root: u8, slot: u8) -> Result<SmnReg<Remap::Register>, Error> {
    if root >= 4 || slot >= 24 {
        return Err(Error::Root);
    }
    Ok(SmnReg::new(
        0x13b1_00b8
            + u32::from(root) * 0x100000
            + u32::from(slot) * 4
            + if slot >= 20 { 0x280 } else { 0 },
    ))
}
/// Bind by translated start lane/kind, as NFX does, not array order. Honor
/// validated explicit requests as well as native/automatic device functions.
pub fn bind(roots: &[u8], ports: &[Port], asks: &[Ask]) -> Result<Vec<Binding, 32>, Error> {
    if ports.len() != asks.len() || ports.len() > 32 {
        return Err(Error::Missing);
    }
    let mut matched = [false; 32];
    let mut occupied = [[false; 24]; 4];
    let mut result = Vec::new();
    for (index, ask) in asks.iter().enumerate() {
        let kind = ask.kind().ok_or(Error::Unsupported)?;
        let start = ask.lane().read(Lane::START);
        let mut candidate = None;
        for (i, port) in ports.iter().enumerate() {
            let mapping = rs::translate(roots, port.start, port.end).map_err(|_| Error::Root)?;
            if port.kind == kind && u32::from(mapping.start) == start {
                if candidate.is_some() {
                    return Err(Error::Duplicate);
                }
                candidate = Some((i, mapping));
            }
        }
        let (i, mapping) = candidate.ok_or(Error::Missing)?;
        if core::mem::replace(&mut matched[i], true) {
            return Err(Error::Duplicate);
        }
        if kind == Kind::Sata {
            continue;
        }
        if ask.status().read(Status::STATE) == 0 && !ports[i].present {
            continue;
        }
        let local = ask.status().read(Status::LOCAL_PORT_ID) as u8;
        let slot = slot(mapping.root_index, mapping.wrapper, local)?;
        if core::mem::replace(
            &mut occupied[mapping.root_index as usize][slot as usize],
            true,
        ) {
            return Err(Error::Duplicate);
        }
        if ports[i].device > 31 || ports[i].function > 7 {
            return Err(Error::Unsupported);
        }
        let devfn = if ports[i].device == 0 && ports[i].function == 0 {
            NATIVE_DEVFNS[slot as usize]
        } else {
            fstart_pci::devfn(ports[i].device, ports[i].function)
        };
        let logical_bridge = NATIVE_DEVFNS
            .iter()
            .position(|&d| d == devfn)
            .ok_or(Error::Unsupported)? as u8;
        result
            .push(Binding {
                ask: index,
                policy: i,
                root: mapping.root_index,
                wrapper: mapping.wrapper,
                local_port: local,
                slot,
                devfn,
                logical_bridge,
            })
            .map_err(|_| Error::Missing)?;
    }
    if matched[..ports.len()].iter().any(|v| !*v) {
        return Err(Error::Missing);
    }
    remap_plan(roots, &result)?;
    Ok(result)
}

/// NFX allocation: honor active ports, remove their devfns from the native
/// pool, then fill vacant slots in order. Every map is a permutation.
pub fn remap_plan(roots: &[u8], bindings: &[Binding]) -> Result<[[u8; 24]; 4], Error> {
    let mut mask = 0u8;
    for &root in roots {
        if root >= 4 || mask & (1 << root) != 0 {
            return Err(Error::Root);
        }
        mask |= 1 << root;
    }
    let mut maps = [[0u8; 24]; 4];
    let mut used = [0u32; 4];
    for b in bindings {
        if b.root >= 4 || b.slot >= 24 || mask & (1 << b.root) == 0 {
            return Err(Error::Root);
        }
        let index = NATIVE_DEVFNS
            .iter()
            .position(|&d| d == b.devfn)
            .ok_or(Error::Unsupported)?;
        let root = b.root as usize;
        if maps[root][b.slot as usize] != 0 || used[root] & (1 << index) != 0 {
            return Err(Error::Duplicate);
        }
        maps[root][b.slot as usize] = b.devfn;
        used[root] |= 1 << index;
    }
    for &root in roots {
        let r = root as usize;
        for slot in &mut maps[r] {
            if *slot != 0 {
                continue;
            }
            let free = (0..24)
                .find(|&index| used[r] & (1 << index) == 0)
                .ok_or(Error::Missing)?;
            *slot = NATIVE_DEVFNS[free];
            used[r] |= 1 << free;
        }
    }
    Ok(maps)
}
/// Preserve upper24 bits. Preflight every register before the first write.
/// Caller owns all host-bridge SMN views and has completed the MAP phase.
pub fn program(smn: &impl SmnAccess, roots: &[u8], bindings: &[Binding]) -> Result<(), Error> {
    let maps = remap_plan(roots, bindings)?;
    let mut seen = 0u8;
    for &root in roots {
        if root >= 4 || seen & (1 << root) != 0 {
            return Err(Error::Root);
        }
        seen |= 1 << root;
        for slot in 0..24 {
            if smn.reg(remap_register(root, slot)?).get() == u32::MAX {
                return Err(Error::Unavailable);
            }
        }
    }
    for &root in roots {
        for (slot, &devfn) in maps[root as usize].iter().enumerate() {
            let reg = smn.reg(remap_register(root, slot as u8)?);
            reg.modify(Remap::DEVFN.val(devfn.into()));
            if reg.get() & 255 != u32::from(devfn) {
                return Err(Error::Verify);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_template_slots_and_discontinuous_register_bank() {
        assert_eq!(slot(0, 0, 8), Ok(8));
        assert_eq!(slot(3, 1, 8), Ok(17));
        assert_eq!(slot(0, 2, 2), Ok(20));
        assert_eq!(slot(2, 2, 3), Ok(21));
        assert_eq!(slot(3, 2, 0), Err(Error::Port));
        assert_eq!(slot(0, 0, 9), Err(Error::Port));
        assert_eq!(remap_register(0, 20).unwrap().address(), 0x13b10388);
        assert_eq!(remap_register(2, 0).unwrap().address(), 0x13d100b8);
        assert_eq!(NATIVE_DEVFNS[18], 0x29);
        assert_eq!(NATIVE_DEVFNS[20], 0x2b);
    }
    #[test]
    fn measured_map_ids_honor_board_requests_and_lbid_is_not_physical_slot() {
        use zerocopy::FromBytes;
        let mut policies = Vec::<Port, 32>::new();
        let mut asks = Vec::<Ask, 32>::new();
        let entries = [
            (0, 3, 0, 1, 4),
            (4, 7, 1, 1, 3),
            (8, 11, 2, 1, 2),
            (12, 15, 3, 1, 1),
            (88, 89, 3, 3, 3),
            (90, 91, 4, 3, 4),
            (132, 132, 1, 0, 0),
            (133, 133, 2, 0, 0),
        ];
        for (start, end, local, device, function) in entries {
            let mut port = Port::pcie(start, end);
            port.device = device;
            port.function = function;
            let (_, ask) = rs::build(&[0, 2, 3], &port, 0).unwrap();
            let mut bytes = [0; 52];
            bytes.copy_from_slice(ask.bytes());
            let status = Status::STATE.val(2) + Status::LOCAL_PORT_ID.val(local);
            bytes[32..36].copy_from_slice(&status.value.to_le_bytes());
            asks.push(Ask::read_from_bytes(&bytes).unwrap()).unwrap();
            policies.push(port).unwrap();
        }
        asks.reverse(); // never derive mapping from returned array position
        let bindings = bind(&[0, 2, 3], &policies, &asks).unwrap();
        let first = bindings.iter().find(|b| b.policy == 0).unwrap();
        assert_eq!(
            (first.slot, first.devfn, first.logical_bridge),
            (0, 0x0c, 3)
        );
        let m2 = bindings.iter().find(|b| b.policy == 4).unwrap();
        assert_eq!(
            (m2.root, m2.slot, m2.devfn, m2.logical_bridge),
            (3, 12, 0x1b, 11)
        );
        let map = remap_plan(&[0, 2, 3], &bindings).unwrap();
        assert_eq!(&map[0][..4], &[0x0c, 0x0b, 0x0a, 0x09]);
        assert_eq!(map[3][11], 0x1d);
        assert_eq!(map[3][12], 0x1b);
        assert_eq!(map[3][13], 0x1c);
        for root in [0, 2, 3] {
            for d in NATIVE_DEVFNS {
                assert_eq!(map[root].iter().filter(|&&v| v == d).count(), 1);
            }
        }
        policies[1].function = 4;
        assert_eq!(bind(&[0, 2, 3], &policies, &asks), Err(Error::Duplicate));
    }
    #[test]
    fn remap_preserves_reserved_bits_and_readback_matches() {
        let hw = crate::smn::testing::SmnSpace::default();
        for root in [0, 2, 3] {
            for slot in 0..24 {
                hw.set(remap_register(root, slot).unwrap(), 0xabcd1200);
            }
        }
        program(&hw.smn(), &[0, 2, 3], &[]).unwrap();
        for root in [0, 2, 3] {
            for (slot, &devfn) in NATIVE_DEVFNS.iter().enumerate() {
                assert_eq!(
                    hw.get(remap_register(root, slot as u8).unwrap()),
                    0xabcd1200 | u32::from(devfn)
                );
            }
        }
    }
}
