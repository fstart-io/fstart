// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h Raven/Picasso/Pollock register geometry, not Intel-compatible IP.
pub mod pinmux;
pub mod tsc;
pub const ACPI_MMIO: usize = 0xfed8_0000;
pub const ECAM: u64 = 0xf800_0000;
pub const ECAM_BUSES: u8 = 64;
pub const UART_BASES: [usize; 4] = [0xfedc9000, 0xfedca000, 0xfedce000, 0xfedcf000];
pub const UART_AOAC: [u8; 4] = [11, 12, 16, 26];
pub const EARLY_AOAC: &[u8] = &[17, 7, 8, 9, 27];
pub const GPIO_EVENTS: &[(u8, u8)] = &[
    (0, 21),
    (1, 19),
    (2, 8),
    (3, 2),
    (4, 4),
    (5, 7),
    (6, 10),
    (7, 11),
    (8, 23),
    (9, 22),
    (16, 12),
    (17, 13),
    (18, 14),
    (21, 5),
    (22, 3),
    (23, 16),
    (24, 15),
    (40, 20),
    (84, 18),
    (86, 9),
    (89, 0),
    (90, 1),
    (91, 6),
    (129, 17),
];
