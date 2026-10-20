// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h models 00h-0Fh (Zeppelin) in the SP4/SP4r2 packages (EPYC
//! Embedded 3000).
//!
//! The PSP trains DRAM with the vendor ABL and reports the result in the
//! APOB; everything x86 does after that is native: Data Fabric, NBIO, SMU,
//! DXIO (PCIe), CCX and the Taishan FCH. Every die has its own DF, NBIO, SMU
//! and FCH; SP4r2 has one die, SP4 two.
pub mod ccx;
mod ccx_tables;
pub mod dxio;
pub mod fabric;
pub mod fch;
pub mod i2c;
pub mod identity;
pub mod microcode;
pub mod nbio;
mod nbio_tables;
pub mod psp;
pub mod smu;
#[cfg(test)]
mod test_pci;
pub mod topology;

pub use identity::{Package, Revision};

/// GPIO bank 0: 145 pins, but the vendor board tables also use 149-152.
pub const GPIO_BANK: crate::gpio::Bank = crate::gpio::Bank {
    pins: 153,
    reserved: &[],
    events: &[
        (0, 21),
        (1, 19),
        (2, 8),
        (3, 2),
        (4, 4),
        (5, 7),
        (6, 10),
        (7, 11),
        (8, 23),
        (9, 22),
        (16, 12),
        (17, 13),
        (18, 14),
        (21, 5),
        (22, 3),
        (23, 16),
        (24, 15),
        (40, 20),
        (84, 18),
        (86, 9),
        (89, 0),
        (90, 1),
        (91, 6),
        (129, 17),
    ],
};
pub use topology::{Die, MAX_DIES};

/// PCIe ECAM: 128 buses, as the vendor firmware.
pub const ECAM: u64 = 0xf000_0000;
pub const ECAM_BUSES: u16 = 128;
/// The PSP copies the APOB (BIOS directory entry 0x61) here.
pub const APOB_BASE: u64 = 0x3000_0000;
/// The vendor firmware's APOB_NV region is 128 KiB, so the APOB isn't larger.
pub const APOB_SIZE: usize = 0x2_0000;

/// FCH ACPI fixed I/O, the same layout as the vendor firmware.
pub mod acpi_io {
    pub const BASE: u16 = 0x800;
    pub const PM1_EVT: u16 = BASE;
    pub const PM1_CNT: u16 = BASE + 0x04;
    pub const PM_TMR: u16 = BASE + 0x08;
    pub const CSTATE_CONTROL: u16 = BASE + 0x10;
    pub const GPE0: u16 = BASE + 0x20;
}
pub const SMBUS_IO: u16 = 0xb00;
pub const SPI_BASE: u32 = 0xfec1_0000;
