// SPDX-License-Identifier: MIT OR Apache-2.0
//! MPIO, the PCIe and SATA lane firmware of the RS I/O die (Genoa, Siena):
//! DXIO's successor, a mailbox of its own instead of an SMU message, and a
//! port description (the ASK) instead of DXIO's engines.
//!
//! A board describes its ports in board lanes ([`Port`]); [`ask`]
//! translates them to an IOS's wrapper and builds the ASKs, [`ancillary`]
//! the per-port PHY and speed records. One [`session::Session`] per I/O die
//! sends them through the [`mailbox`] and runs the setup phases (map,
//! configure, PERST, train), and [`ports`] places the trained ports in PCI
//! (device remap, logical bridges) between them; before configure the
//! session writes the cores' [`strap`]s.
//!
//! The S8040's V2.00 and NFX V0.02 AmdNbioPei are the source,
//! instruction-checked; the S8040 and the Onyx trained with them. Genoa's
//! public `MpioInitLib.h` agrees on the ASK, the six arguments, the
//! message IDs and the phases.
pub mod ancillary;
pub mod ask;
pub mod mailbox;
pub mod ports;
pub mod session;
pub mod strap;

pub use ask::{Kind, LinkSpeed, Port};

/// What a [`session::Session`] needs of its DMA window besides the reads
/// and writes.
impl super::DmaBuffer {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The window's first byte, for the owner's cache maintenance.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}
