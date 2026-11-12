//! Shared topology types: sockets, DF nodes and root bridges.
//!
//! Discovery lives in [`crate::sp3::df`]; these types are what the other blocks
//! (NBIO, DXIO, ACPI, PCI) consume.

use crate::sp3::soc::Generation;

pub use crate::df::{MAX_ROOT_BRIDGES, RootBridge};

const _: () = assert!(MAX_ROOT_BRIDGES == Generation::MAX_SOCKETS * 4);

/// One DF node: a ZP die (Naples) or an IOD (Rome, Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub socket: u8,
    /// Die within the socket (always 0 on Rome/Milan).
    pub die: u8,
    /// PCI device of the node's DF functions on bus 0 (`0x18 + node`).
    pub device: u8,
}
