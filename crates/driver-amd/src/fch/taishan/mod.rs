// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Taishan FCH of Zeppelin. The code follows the vendor
//! FchTaishanPei/Dxe and FchMultiFchPei/Dxe tasks with their default policy
//! (sp4r2 report FCH_TAISHAN).
//!
//! Every die has an FCH. Die 0's is the system FCH (LPC, SPI, SMBus, ACPI,
//! ACPI MMIO at 0xfed80000). The others only matter for their xHCI, SATA and
//! xGbE, reached through the SMN of their die, and for a few legacy settings
//! made through a moved ACPI MMIO block ([`multi`]).
//!
//! The platform calls, in order:
//! - before the console: [`crate::fch::enable_acpimmio`], [`pre_init`], then after
//!   the console [`crate::fch::early::set_power_failure_state`].
//! - after the DF init: [`reset_init`], [`sata::reset`] of die 0,
//!   [`multi::reset`].
//! - after DXIO training: [`usb::init`] of die 0, [`multi::usb`];
//!   after the SMU and NBIO pre-enumeration init:
//!   [`crate::fch::Fch::acpi_decode`] with [`ACPI_PORTS`], [`env_init`],
//!   [`sata::init`] of die 0 if the board enables it, [`xgbe_init`] of die 0,
//!   [`multi::env`].
//! - after MP init, SMM: [`crate::fch::smi::FchSmi`] routes the SMI sources.
//! - before PCI devices get their IRQs: [`irq::write_routing`]; the chip init
//!   then calls [`crate::fch::Fch::acpi_ports`].
//! - ReadyToBoot: [`sata::late`] of die 0 if it was enumerated,
//!   [`multi::late`], [`late_init`].
pub mod irq;
pub mod multi;
pub mod sata;
#[cfg(feature = "smm")]
pub mod smm;
mod init;
pub mod usb;

pub use init::{env_init, late_init, pre_init, reset_init};
pub use multi::{acpimmio_base, xgbe_init};

use crate::soc::zeppelin::acpi_io;

/// The ACPI I/O blocks of [`acpi_io`] for [`crate::fch::Fch`].
pub const ACPI_PORTS: crate::fch::AcpiPorts = crate::fch::AcpiPorts {
    event: acpi_io::PM1_EVT,
    control: acpi_io::PM1_CNT,
    timer: acpi_io::PM_TMR,
    gpe: acpi_io::GPE0,
};

/// Board choices for the FCH devices of a die. Die 0's xHCI is always set
/// up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Devices {
    pub xhci: bool,
    pub sata: bool,
}

/// SMBus/ASF I/O (PMx00[15:8]).
const SMBUS_IO: u16 = crate::fch::SMBUS_ASF_IO_BASE;
const ASF_IO: u16 = SMBUS_IO + 0x20;

tock_registers::register_bitfields![u32,
    /// NBIF function strap: [15:0] device ID, bit 28 enable.
    pub NbifStrap [DEVICE_ID OFFSET(0) NUMBITS(16) [], ENABLE OFFSET(28) NUMBITS(1) []]
];

#[cfg(test)]
mod tests;
