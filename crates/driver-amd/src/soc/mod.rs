// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs. A SoC is a list of IP versions and the constants its IP
//! blocks differ in; the IP blocks themselves are the crate's modules, each
//! with its common code and the versions where the code differs:
//!
//! | IP | [`picasso`] | [`zeppelin`] |
//! |----|-------------|--------------|
//! | [`crate::ccx`] | `ccx::picasso` (AmdCcxZenRv) | `ccx::zeppelin` (AmdCcxZenZp) |
//! | [`crate::df`] | `df::picasso` (AmdFabricRv) | `df::zeppelin` (AmdFabricZp) |
//! | [`crate::nbio`] | `nbio::picasso` (AmdNbioBaseRV) | `nbio::zeppelin` (AmdNbioBaseZP) |
//! | [`crate::dxio`] | `dxio::picasso` | `dxio::zeppelin` |
//! | [`crate::smu`] | `smu::v10` (AmdNbioSmuV10) | `smu::v9` (AmdNbioSmuV9) |
//! | [`crate::fch`] | `fch::sandstone` (FchSandstone) | `fch::taishan` (FchTaishan) |
//! | [`crate::psp`], [`crate::apob`] | common | common |
//! | [`crate::acp`] | ACP 3.x | - |
//!
//! The names in parentheses are the AGESA modules of the version.
use crate::{fch::gpio::Bank, smu::Mailbox};

#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

/// What the IP blocks of a SoC differ in.
pub trait Soc: 'static {
    /// Dies of the largest package.
    const MAX_DIES: usize;
    /// The SMU mailbox of every die.
    const SMU: Mailbox;
    /// The SMU message of the DXIO firmware ([`crate::dxio`]).
    const DXIO_SERVICE: u32;
    /// The FCH GPIO bank.
    const GPIO_BANK: &'static Bank;
    /// MMIO bases of the FCH I2C controllers, numbered like the vendor
    /// AmdI2CMasterPei instances ([`crate::fch::i2c`]).
    const I2C: &'static [usize];
    /// Fabric ID of the IOMS (the PCIe root complex) of `die`.
    fn ioms_fabric_id(die: u8) -> u8;
}
