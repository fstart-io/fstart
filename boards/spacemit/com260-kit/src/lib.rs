//! SpacemiT CoM260 kit (K3) board support package.

#![no_std]

use fstart_platform_spacemit::facts::{BoardFacts, SpacemitBoardFacts, SpacemitSoc};

/// CoM260 kit marker.
pub struct Board;

impl SpacemitBoardFacts for Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: SpacemitSoc::K3,
    };
}
