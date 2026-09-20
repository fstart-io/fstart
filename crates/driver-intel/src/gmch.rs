//! Register values shared by the GMCH-generation northbridges (i945,
//! Pineview, GM965).

/// `SMRAM` (host bridge config 0x9d) fields; bits match coreboot's
/// `cpu/intel/smm/gen1/smmrelocate.c`.
pub mod smram {
    use tock_registers::register_bitfields;

    register_bitfields![u8,
        pub SMRAM [
            C_BASE_SEG OFFSET(0) NUMBITS(3) [
                Legacy = 0b010
            ],
            G_SMRAME OFFSET(3) NUMBITS(1) [],
            D_LCK OFFSET(4) NUMBITS(1) [],
            D_OPEN OFFSET(6) NUMBITS(1) []
        ]
    ];

    /// SMRAM visible outside SMM during installation.
    #[must_use]
    pub fn open() -> u8 {
        (SMRAM::D_OPEN::SET + SMRAM::G_SMRAME::SET + SMRAM::C_BASE_SEG::Legacy).value
    }

    /// SMRAM hidden outside SMM.
    #[must_use]
    pub fn closed() -> u8 {
        (SMRAM::G_SMRAME::SET + SMRAM::C_BASE_SEG::Legacy).value
    }

    /// SMRAM closed and locked until reset.
    #[must_use]
    pub fn locked() -> u8 {
        (SMRAM::D_LCK::SET + SMRAM::G_SMRAME::SET + SMRAM::C_BASE_SEG::Legacy).value
    }
}
