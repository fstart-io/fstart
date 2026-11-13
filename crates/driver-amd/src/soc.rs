// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the IP blocks need to know of the chip they are part of. The
//! platform's SoCs implement [`Soc`]; the driver never names one.
use crate::{fch::gpio::Bank, smu::Mailbox};

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
