// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RTC and CMOS behind the I/O index/data pair at `70h/71h` of every
//! FCH.
use super::{IndexedIo, indexed_io};
use crate::window::Window;
use tock_registers::{RegisterLongName, register_bitfields};

/// CMOS bank 0 index/data.
pub const INDEX: u16 = 0x70;
/// RTC register A.
pub const FREQ_SELECT: u8 = 0x0a;
/// RTC register B.
pub const CONTROL: u8 = 0x0b;
/// RTC register D.
pub const VALID: u8 = 0x0d;

register_bitfields![u8,
    /// Register A.
    pub FreqSelect [
        /// DV0.
        BIT4 OFFSET(4) NUMBITS(1) []
    ],
    /// Register B.
    pub Control [
        DST_EN OFFSET(0) NUMBITS(1) []
    ],
    /// Register D.
    pub Valid [
        VALID OFFSET(0) NUMBITS(6) [],
        BIT6 OFFSET(6) NUMBITS(1) [],
        VRT OFFSET(7) NUMBITS(1) []
    ]
];

/// RTC/CMOS register `index` through the ports `io`.
pub fn register<W: Window, R: RegisterLongName>(io: &W, index: u8) -> IndexedIo<'_, W, R> {
    indexed_io(io, INDEX, index)
}
