//! Frequency-indexed timing tables (coreboot `raminit_tables.c`).
//!
//! Row 0 is the 133 MHz reference clock, FRQ 3..10; row 1 is the 100 MHz
//! reference, FRQ 7..12.

pub const FRQ_REFI: [[u32; 8]; 2] = [
    [3120, 4160, 5200, 6240, 7280, 8320, 9360, 10400],
    [5460, 6240, 7020, 7800, 8580, 9360, 0, 0],
];
pub const FRQ_XS: [[u8; 8]; 2] = [[4, 6, 7, 8, 10, 11, 12, 14], [7, 8, 9, 10, 11, 12, 0, 0]];
pub const FRQ_MOD: [[u8; 8]; 2] = [
    [12, 12, 12, 12, 15, 16, 18, 20],
    [12, 12, 14, 15, 17, 18, 0, 0],
];
pub const FRQ_WLO: [[u8; 8]; 2] = [[4, 5, 6, 6, 8, 8, 9, 10], [6, 6, 7, 8, 9, 9, 0, 0]];
pub const FRQ_CKE: [[u8; 8]; 2] = [[3, 3, 4, 4, 5, 6, 6, 7], [4, 4, 5, 5, 6, 6, 0, 0]];
pub const FRQ_XPDLL: [[u8; 8]; 2] = [
    [10, 13, 16, 20, 23, 26, 29, 32],
    [17, 20, 22, 24, 27, 32, 0, 0],
];
pub const FRQ_XP: [[u8; 8]; 2] = [[3, 4, 4, 5, 6, 7, 8, 8], [5, 5, 6, 6, 7, 8, 0, 0]];
pub const FRQ_AONPD: [[u8; 8]; 2] = [[4, 5, 6, 8, 8, 10, 11, 12], [6, 8, 8, 9, 10, 11, 0, 0]];

/// Row and column of an FRQ in the tables above; `None` outside them.
pub const fn index(frq: u32, base_freq: u32) -> Option<(usize, usize)> {
    match base_freq {
        100 if frq >= 7 && frq <= 12 => Some((1, (frq - 7) as usize)),
        133 if frq >= 3 && frq <= 10 => Some((0, (frq - 3) as usize)),
        _ => None,
    }
}

include!("patterns.rs");
