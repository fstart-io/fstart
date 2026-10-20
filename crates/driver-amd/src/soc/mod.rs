// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs: what a SoC is made of, the IP versions it uses and the
//! constants its IP blocks differ in.
use crate::smu::Mailbox;

/// What the IP blocks of a SoC differ in.
pub trait Soc: 'static {
    /// Dies of the largest package.
    const MAX_DIES: usize;
    /// The SMU mailbox of every die.
    const SMU: Mailbox;
    /// The SMU message of the DXIO firmware ([`crate::fam17h::dxio`]).
    const DXIO_SERVICE: u32;
    /// Fabric ID of the IOMS (the PCIe root complex) of `die`.
    fn ioms_fabric_id(die: u8) -> u8;
}
