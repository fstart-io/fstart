//! Host-clean board facts for the ASPEED BMC SoCs.
//!
//! Only what varies per board: the SoC, the size of the boot flash on CE0
//! and the emulator equivalent. SRAM placement, the 3-byte addressable
//! firmware window and stage geometry are platform constants.

use crate::layout::{self, Layout};

/// The BMC SoC. Variants that share a die are listed apart where firmware
/// must treat them differently but cannot tell them apart itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspeedSoc {
    /// AST2400: ARM926EJ-S with DDR2/DDR3, host graphics and a ColdFire
    /// coprocessor.
    Ast2400,
    /// AST1250: the AST2400 die without host graphics, PCIe, the ColdFire
    /// coprocessor and two of the four system UARTs. It reads the same
    /// silicon revision as the AST2400, so only the board can say which
    /// one it carries.
    Ast1250,
    /// AST2500: ARM1176JZF-S with DDR3/DDR4, host graphics and a ColdFire
    /// coprocessor.
    Ast2500,
    /// AST2600: dual Cortex-A7 with a Cortex-M3 coprocessor, booted by its
    /// ROM. Not supported yet: plans for it fail.
    Ast2600,
}

impl AspeedSoc {
    /// The SoC's address map.
    pub const fn layout(self) -> &'static Layout {
        match self {
            Self::Ast2400 | Self::Ast1250 => &layout::AST2400,
            Self::Ast2500 => &layout::AST2500,
            Self::Ast2600 => &layout::AST2600,
        }
    }

    /// Whether the SoC serves the host graphics, which takes the VGA
    /// memory carve-out at the top of DRAM.
    pub const fn has_host_graphics(self) -> bool {
        !matches!(self, Self::Ast1250)
    }
}

/// A QEMU machine that runs the board's image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspeedEmulator {
    /// `palmetto-bmc`, an AST2400 executing the composite SPI image in
    /// place from FMC CE0.
    PalmettoBmc,
    /// `ast2500-evb`, executing the composite SPI image in place from
    /// FMC CE0.
    Ast2500Evb,
    /// `ast2600-evb`. Not supported yet.
    Ast2600Evb,
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
