//! Host-clean board facts for the SpacemiT SoCs.
//!
//! Only what varies per board belongs here. SRAM placement and the stage
//! geometry the boot ROM dictates are platform constants.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpacemitSoc {
    K3,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub soc: SpacemitSoc,
}

pub trait SpacemitBoardFacts {
    const FACTS: BoardFacts;
}
