//! Host-clean board facts for the ASPEED BMC SoCs.
//!
//! Only what varies per board: the SoC, the size of the boot flash on CE0
//! and the emulator equivalent. SRAM placement, the 3-byte addressable
//! firmware window and stage geometry are platform constants.

use fstart_core::QemuMachine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspeedSoc {
    Ast2500,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub soc: AspeedSoc,
    /// Size of the SPI flash on FMC CE0; the composite image is padded to it.
    pub flash_size: u64,
    pub qemu_machine: Option<QemuMachine>,
}

pub trait AspeedBoardFacts {
    const FACTS: BoardFacts;
}
