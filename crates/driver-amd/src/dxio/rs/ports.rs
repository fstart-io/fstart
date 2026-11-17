// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where MPIO's trained ports appear in PCI: the IOHC's device remap and
//! logical bridges. Each IOS has 24 root-port slots: wrappers 0 and 1 with
//! nine ports each, and on an even IOS the four ports of wrapper 2 (the
//! S8040's NFX port templates at `FFA4A63C`/`FFA4B37C`). A slot's root port
//! appears at the devfn its remap register names; by default the slot's own
//! of [`NATIVE_DEVFNS`], whose index is the logical bridge ID MPIO's UPDATE
//! takes and the IOHC's bridge controls are indexed by.
//!
//! MPIO reports a trained port by its wrapper-local port ID, not by a slot:
//! [`bind`] matches each ASK to its board port by the start lane it was
//! sent with and gives it its slot, devfn and logical bridge; [`remap`]
//! then lays every IOS's devfns out, the board's requests first.
use super::ask::{self, Ask, Kind, Lane, Port, Status};
use crate::nbio::iohc::NbProgDeviceRemap;
use crate::smn::{SmnAccess, SmnReg};
use heapless::Vec;
use tock_registers::interfaces::ReadWriteable;

/// Root-port slots of an IOS.
pub const SLOTS: usize = 24;
/// The devfn of each slot's root port, by default: the slot's logical
/// bridge ID is its index here.
pub const NATIVE_DEVFNS: [u8; SLOTS] = [
    0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x11, 0x12, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x21, 0x22, 0x29, 0x2a, 0x2b, 0x2c, 0x39, 0x3a,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A wrapper and port no slot has.
    Port {
        wrapper: u8,
        port: u8,
    },
    /// An ASK no board port was sent as, or a board port with no ASK.
    Unmatched,
    /// Two ports at one slot, devfn or start lane.
    Duplicate,
    /// A requested devfn no slot has.
    Devfn(u8),
    Lanes(ask::Error),
}

/// The slot of wrapper-local port `port` of `wrapper` on IOS `ios`.
pub const fn slot(ios: u8, wrapper: u8, port: u8) -> Result<u8, Error> {
    match (wrapper, port) {
        (0, 0..=8) => Ok(port),
        (1, 0..=8) => Ok(9 + port),
        (2, 0..=3) if ios.is_multiple_of(2) => Ok(18 + port),
        _ => Err(Error::Port { wrapper, port }),
    }
}

/// The wrapper and wrapper-local port of slot `slot`.
pub const fn wrapper_port(slot: u8) -> (u8, u8) {
    match slot {
        0..=8 => (0, slot),
        9..=17 => (1, slot - 9),
        _ => (2, slot - 18),
    }
}

/// The remap register of `slot` of an IOS, relative to IOS 0 (the slots of
/// wrapper 2 are `0x280` further on).
pub const fn remap_register(slot: u8) -> SmnReg<NbProgDeviceRemap::Register> {
    SmnReg::new(0x13b1_00b8 + slot as u32 * 4 + if slot >= 20 { 0x280 } else { 0 })
}

/// A trained PCIe port in PCI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The ASK, and the board port it was sent as, by index.
    pub ask: usize,
    pub port: usize,
    pub ios: u8,
    pub slot: u8,
    pub devfn: u8,
    /// The index of `devfn` in [`NATIVE_DEVFNS`].
    pub logical_bridge: u8,
}

/// The root port of every PCIe ASK that trained or whose board port is
/// present, by the board port it was sent as (`ports`, by start lane and
/// kind; the ASKs come back in any order). `ios` are the present IOS.
pub fn bind(ios: &[u8], ports: &[Port], asks: &[Ask]) -> Result<Vec<Binding, 32>, Error> {
    let mut bindings: Vec<Binding, 32> = Vec::new();
    let mut matched: Vec<usize, 32> = Vec::new();
    for (index, sent) in asks.iter().enumerate() {
        let start = sent.lane().read(Lane::START);
        let mut candidates = ports.iter().enumerate().filter(|(_, port)| {
            Some(port.kind) == sent.kind()
                && ask::translate(ios, port.start, port.end)
                    .is_ok_and(|lanes| u32::from(lanes.start) == start)
        });
        let (number, port) = candidates.next().ok_or(Error::Unmatched)?;
        if candidates.next().is_some() || matched.contains(&number) {
            return Err(Error::Duplicate);
        }
        matched.push(number).map_err(|_| Error::Duplicate)?;
        let trained = sent.status().read(Status::STATE) != 0;
        if port.kind != Kind::Pcie || !(trained || port.present) {
            continue;
        }
        let lanes = ask::translate(ios, port.start, port.end).map_err(Error::Lanes)?;
        let local = sent.status().read(Status::LOCAL_PORT_ID) as u8;
        let slot = slot(lanes.root_index, lanes.wrapper, local)?;
        let devfn = match (port.device, port.function) {
            (0, 0) => NATIVE_DEVFNS[usize::from(slot)],
            (device, function) => fstart_pci::devfn(device, function),
        };
        let logical_bridge = NATIVE_DEVFNS
            .iter()
            .position(|&d| d == devfn)
            .ok_or(Error::Devfn(devfn))? as u8;
        let binding = Binding {
            ask: index,
            port: number,
            ios: lanes.root_index,
            slot,
            devfn,
            logical_bridge,
        };
        if bindings
            .iter()
            .any(|b| b.ios == binding.ios && (b.slot == slot || b.devfn == devfn))
        {
            return Err(Error::Duplicate);
        }
        bindings.push(binding).map_err(|_| Error::Duplicate)?;
    }
    if matched.len() != ports.len() {
        return Err(Error::Unmatched);
    }
    Ok(bindings)
}

/// The devfn of every slot of IOS `ios`: the bound ports' requests, then
/// the other slots take the devfns left, in order (the NFX allocation:
/// every map is a permutation of [`NATIVE_DEVFNS`]).
pub fn remap(ios: u8, bindings: &[Binding]) -> [u8; SLOTS] {
    let bound = || bindings.iter().filter(|b| b.ios == ios);
    let mut free = NATIVE_DEVFNS
        .into_iter()
        .filter(|devfn| !bound().any(|b| b.devfn == *devfn));
    core::array::from_fn(|slot| match bound().find(|b| usize::from(b.slot) == slot) {
        Some(b) => b.devfn,
        None => free.next().unwrap_or(0),
    })
}

/// Program [`remap`] into the remap registers of `nbio`'s IOS (the other
/// bits kept), after MPIO mapped the ports and before it configures them.
pub fn program(nbio: &impl SmnAccess, ios: u8, bindings: &[Binding]) {
    for (slot, devfn) in (0u8..).zip(remap(ios, bindings)) {
        nbio.reg(remap_register(slot))
            .modify(NbProgDeviceRemap::DEVFN.val(devfn.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::FromBytes;

    #[test]
    fn slots_and_the_discontinuous_register_bank() {
        assert_eq!(slot(0, 0, 8), Ok(8));
        assert_eq!(slot(3, 1, 8), Ok(17));
        assert_eq!(slot(0, 2, 2), Ok(20));
        assert_eq!(slot(2, 2, 3), Ok(21));
        assert!(slot(3, 2, 0).is_err() && slot(0, 0, 9).is_err());
        assert_eq!(wrapper_port(20), (2, 2));
        assert_eq!(remap_register(20).address(), 0x13b1_0388);
        assert_eq!(remap_register(0).address(), 0x13b1_00b8);
        assert_eq!(NATIVE_DEVFNS[18], 0x29);
    }

    #[test]
    fn requests_are_honored_and_the_logical_bridge_is_not_the_slot() {
        let mut ports = Vec::<Port, 32>::new();
        let mut asks = Vec::<Ask, 32>::new();
        // start, end, returned local port, requested device and function
        for (start, end, local, device, function) in [
            (0, 3, 0, 1, 4),
            (4, 7, 1, 1, 3),
            (8, 11, 2, 1, 2),
            (12, 15, 3, 1, 1),
            (88, 89, 3, 3, 3),
            (90, 91, 4, 3, 4),
            (132, 132, 1, 0, 0),
            (133, 133, 2, 0, 0),
        ] {
            let port = Port {
                device,
                function,
                ..Port::pcie(start, end)
            };
            let (_, sent) = ask::build(&[0, 2, 3], &port, 0).unwrap();
            let mut bytes = [0; 52];
            bytes.copy_from_slice(sent.bytes());
            let status = Status::STATE.val(2) + Status::LOCAL_PORT_ID.val(local);
            bytes[32..36].copy_from_slice(&status.value.to_le_bytes());
            asks.push(Ask::read_from_bytes(&bytes).unwrap()).unwrap();
            ports.push(port).unwrap();
        }
        // The returned order says nothing.
        asks.reverse();
        let bindings = bind(&[0, 2, 3], &ports, &asks).unwrap();
        let first = bindings.iter().find(|b| b.port == 0).unwrap();
        assert_eq!(
            (first.slot, first.devfn, first.logical_bridge),
            (0, 0x0c, 3)
        );
        let m2 = bindings.iter().find(|b| b.port == 4).unwrap();
        assert_eq!(
            (m2.ios, m2.slot, m2.devfn, m2.logical_bridge),
            (3, 12, 0x1b, 11)
        );
        let ios0 = remap(0, &bindings);
        assert_eq!(&ios0[..4], &[0x0c, 0x0b, 0x0a, 0x09]);
        let ios3 = remap(3, &bindings);
        assert_eq!((ios3[11], ios3[12], ios3[13]), (0x1d, 0x1b, 0x1c));
        for map in [ios0, ios3, remap(2, &bindings)] {
            for devfn in NATIVE_DEVFNS {
                assert_eq!(map.iter().filter(|&&d| d == devfn).count(), 1);
            }
        }
        ports[1].function = 4;
        assert_eq!(bind(&[0, 2, 3], &ports, &asks), Err(Error::Duplicate));
    }

    #[test]
    fn program_keeps_the_other_bits() {
        use crate::smn::testing::SmnSpace;
        let hw = SmnSpace::default();
        for slot in 0..SLOTS as u8 {
            hw.set(remap_register(slot), 0xabcd_1200);
        }
        program(&hw.smn(), 0, &[]);
        for (slot, &devfn) in NATIVE_DEVFNS.iter().enumerate() {
            assert_eq!(
                hw.get(remap_register(slot as u8)),
                0xabcd_1200 | u32::from(devfn)
            );
        }
    }
}
