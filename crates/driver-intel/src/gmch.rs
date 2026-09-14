//! Register values shared by the GMCH-generation northbridges (i945,
//! Pineview, GM965).

/// `SMRAM` (host bridge config 0x9d) values; bits match coreboot's
/// `cpu/intel/smm/gen1/smmrelocate.c`.
pub mod smram {
    const G_SMRAME: u8 = 1 << 3;
    const D_LCK: u8 = 1 << 4;
    const D_OPEN: u8 = 1 << 6;
    const C_BASE_SEG: u8 = 0b010;

    /// SMRAM visible outside SMM (installation).
    pub const OPEN: u8 = D_OPEN | G_SMRAME | C_BASE_SEG;
    /// SMRAM hidden outside SMM.
    pub const CLOSED: u8 = G_SMRAME | C_BASE_SEG;
    /// Closed and locked until reset.
    pub const LOCKED: u8 = D_LCK | G_SMRAME | C_BASE_SEG;
}
