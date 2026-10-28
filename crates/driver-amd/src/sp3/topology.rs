//! Shared topology types: sockets, DF nodes and root bridges.
//!
//! Discovery lives in [`crate::sp3::df`]; these types are what the other blocks
//! (NBIO, DXIO, ACPI, PCI) consume.

use crate::sp3::soc::Generation;

/// Most root bridges a system has: 2 sockets × 4.
pub const MAX_ROOT_BRIDGES: usize = Generation::MAX_SOCKETS * 4;

/// One DF node: a ZP die (Naples) or an IOD (Rome, Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub socket: u8,
    /// Die within the socket (always 0 on Rome/Milan).
    pub die: u8,
    /// PCI device of the node's DF functions on bus 0 (`0x18 + node`).
    pub device: u8,
}

/// One PCI root bridge: a ZP die's NBIO (Naples) or an IOMS/NBIO quadrant
/// (Rome, Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootBridge {
    pub socket: u8,
    /// Die (Naples) or NBIO quadrant / IOMS index (Rome, Milan) within the
    /// socket.
    pub index: u8,
    /// DF fabric ID of the IOMS, as DstFabricID in the address maps.
    pub fabric_id: u16,
    /// First bus routed to this root bridge.
    pub bus_base: u8,
    /// Last bus routed to this root bridge.
    pub bus_limit: u8,
    /// The root bridge with the FCH (legacy IO/MMIO, VGA by default).
    pub primary: bool,
}

impl RootBridge {
    /// Physical index over the system: `socket * per_socket + index`.
    pub const fn physical(&self, per_socket: usize) -> usize {
        self.socket as usize * per_socket + self.index as usize
    }
}
