//! Host-clean board facts for the ASPEED BMC SoCs.
//!
//! Only what varies per board: the SoC, the size of the boot flash on CE0
//! and the emulator equivalent. SRAM placement, the 3-byte addressable
//! firmware window and stage geometry are platform constants.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspeedSoc {
    Ast2500,
}

/// A QEMU machine that runs the board's image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspeedEmulator {
    /// `ast2500-evb`, executing the composite SPI image in place from
    /// FMC CE0.
    Ast2500Evb,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub soc: AspeedSoc,
    /// Size of the SPI flash on FMC CE0; the composite image is padded to it.
    pub flash_size: u64,
    /// The QEMU machine the board runs on, if any.
    pub emulator: Option<AspeedEmulator>,
}

pub trait AspeedBoardFacts {
    const FACTS: BoardFacts;
}
