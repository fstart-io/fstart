//! Register values shared by the GMCH-generation northbridges (i945,
//! Pineview, GM965).

/// Decode ESMRAMC TSEG size in bytes.
///
/// `Some(0)` means TSEG is disabled; `None` is a reserved size encoding.
#[must_use]
pub const fn tseg_size_bytes(esmramc: u8) -> Option<u32> {
    if esmramc & 1 == 0 {
        return Some(0);
    }

    match (esmramc >> 1) & 3 {
        0 => Some(1024 * 1024),
        1 => Some(2 * 1024 * 1024),
        2 => Some(8 * 1024 * 1024),
        _ => None,
    }
}

/// `SMRAM` (host bridge config 0x9d) fields; bits match coreboot's
/// `cpu/intel/smm/gen1/smmrelocate.c`.
pub mod smram {
    use tock_registers::{LocalRegisterCopy, register_bitfields};

    register_bitfields![u8,
        pub SMRAM [
            C_BASE_SEG OFFSET(0) NUMBITS(3) [
                Legacy = 0b010
            ],
            G_SMRAME OFFSET(3) NUMBITS(1) [],
            D_LCK OFFSET(4) NUMBITS(1) [],
            D_CLS OFFSET(5) NUMBITS(1) [],
            D_OPEN OFFSET(6) NUMBITS(1) []
        ]
    ];

    /// Readback confirms writes from normal mode can reach SMRAM.
    pub fn is_open(value: u8) -> bool {
        LocalRegisterCopy::<u8, SMRAM::Register>::new(value)
            .matches_all(SMRAM::D_OPEN::SET + SMRAM::D_LCK::CLEAR + SMRAM::G_SMRAME::SET)
    }

    /// Readback confirms SMRAM is hidden and its configuration locked.
    pub fn is_locked(value: u8) -> bool {
        LocalRegisterCopy::<u8, SMRAM::Register>::new(value)
            .matches_all(SMRAM::D_OPEN::CLEAR + SMRAM::D_LCK::SET + SMRAM::G_SMRAME::SET)
    }

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

#[cfg(test)]
mod tests {
    use super::tseg_size_bytes;

    #[test]
    fn smram_readback_requires_open_or_closed_locked_state() {
        use super::smram;
        assert!(smram::is_open(smram::open()));
        assert!(!smram::is_open(smram::locked()));
        assert!(smram::is_locked(smram::locked()));
        assert!(!smram::is_locked(smram::open()));
        assert!(!smram::is_locked(smram::closed()));
        assert!(!smram::is_locked(smram::locked() | (1 << 6)));
        assert!(!smram::is_locked(smram::locked() & !(1 << 3)));
    }

    #[test]
    fn tseg_size_decode_rejects_reserved_encoding() {
        assert_eq!(tseg_size_bytes(0), Some(0));
        assert_eq!(tseg_size_bytes(1), Some(1024 * 1024));
        assert_eq!(tseg_size_bytes(3), Some(2 * 1024 * 1024));
        assert_eq!(tseg_size_bytes(5), Some(8 * 1024 * 1024));
        assert_eq!(tseg_size_bytes(7), None);
    }
}
