// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::{vec, vec::Vec};
struct Io {
    lpc: [u32; 64],
    pm: [u8; 256],
    spi: [u8; 256],
    writes: Vec<(u8, u16, u8, u32)>,
}
impl Io {
    fn new() -> Self {
        let mut io = Self {
            lpc: [0; 64],
            pm: [0; 256],
            spi: [0; 256],
            writes: Vec::new(),
        };
        io.lpc[0] = 0x1022;
        io
    }
}
impl Registers for Io {
    fn lpc_read32(&mut self, offset: u16) -> u32 {
        self.lpc[usize::from(offset) / 4]
    }
    fn lpc_write32(&mut self, offset: u16, value: u32) {
        self.writes.push((0, offset, 4, value));
        self.lpc[usize::from(offset) / 4] = value;
    }
    fn pm_read8(&mut self, offset: u16) -> u8 {
        self.pm[usize::from(offset)]
    }
    fn pm_write8(&mut self, offset: u16, value: u8) {
        self.writes.push((1, offset, 1, u32::from(value)));
        self.pm[usize::from(offset)] = value;
    }
    fn pm_read32(&mut self, offset: u16) -> u32 {
        let offset = usize::from(offset);
        u32::from_le_bytes(self.pm[offset..offset + 4].try_into().unwrap())
    }
    fn pm_write32(&mut self, offset: u16, value: u32) {
        self.writes.push((1, offset, 4, value));
        let offset = usize::from(offset);
        self.pm[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn spi_read16(&mut self, offset: u16) -> u16 {
        let offset = usize::from(offset);
        u16::from_le_bytes(self.spi[offset..offset + 2].try_into().unwrap())
    }
    fn spi_write16(&mut self, offset: u16, value: u16) {
        self.writes.push((2, offset, 2, u32::from(value)));
        let offset = usize::from(offset);
        self.spi[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn spi_read32(&mut self, offset: u16) -> u32 {
        let offset = usize::from(offset);
        u32::from_le_bytes(self.spi[offset..offset + 4].try_into().unwrap())
    }
    fn spi_write32(&mut self, offset: u16, value: u32) {
        self.writes.push((2, offset, 4, value));
        let offset = usize::from(offset);
        self.spi[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn smbus_write8(&mut self, asf: bool, offset: u16, value: u8) {
        self.writes
            .push((if asf { 4 } else { 3 }, offset, 1, u32::from(value)));
    }
}
const CONFIG: SpiConfig = SpiConfig {
    mode: ReadMode::Dual122,
    normal: Speed::Mhz33,
    fast: Speed::Mhz66,
    alternate: Speed::Mhz66,
    tpm: Speed::Mhz66,
    burst_4dw: false,
};
#[test]
fn lpc_aperture_retains_flags_and_exact_decode_widths_and_order() {
    let mut io = Io::new();
    io.pm[0xec] = 0x30;
    io.lpc[0x48 / 4] = 0xdeadffef;
    io.lpc[0xa0 / 4] = 0x123456f7;
    prepare_lpc(&mut io).unwrap();
    assert_eq!(
        io.writes,
        vec![
            (1, 0xec, 1, 0x31),
            (0, 0x44, 4, 0),
            (0, 0x48, 4, 0xff80),
            (0, 0x44, 4, 0),
            (0, 0x4c, 4, 0),
            (0, 0x60, 4, 0),
            (0, 0x90, 4, 0),
            (0, 0xa0, 4, SPI_BASE | 0xf7)
        ]
    );
}
#[test]
fn spi_modes_and_speed_override_preserve_unrelated_register_bits() {
    let mut io = Io::new();
    io.lpc[0xa0 / 4] = SPI_BASE | 0xff;
    io.lpc[0xb8 / 4] = 0x00200040;
    io.spi[0x2c..0x2e].copy_from_slice(&0x9234u16.to_le_bytes());
    io.spi[0x20..0x22].copy_from_slice(&0x0040u16.to_le_bytes());
    io.spi[..4].copy_from_slice(&0x52045566u32.to_le_bytes());
    configure_spi(&mut io, CONFIG).unwrap();
    assert_eq!(
        io.writes,
        vec![
            (0, 0xa0, 4, SPI_BASE | 0xfe),
            (0, 0xb8, 4, 0x01a00040),
            (2, 0x2c, 2, 0x1234),
            (2, 0, 4, (0x52045566 & !0x60040000) | 0x40000000),
            (2, 0x22, 2, 0x1000),
            (2, 0x20, 2, 0x41)
        ]
    );
    assert_eq!(
        CONFIG
            .with_fast_override(ReadMode::Quad144, Speed::Khz800)
            .speed_word(),
        0x5555
    );
    assert_eq!(
        CONFIG
            .with_fast_override(ReadMode::Quad144, Speed::Mhz100)
            .speed_word(),
        0x1400
    );
    let unchanged = CONFIG.with_fast_override(ReadMode::Quad144, Speed::Mhz66);
    assert_eq!(unchanged.speed_word(), 0x1000);
    assert_eq!(unchanged.mode_bits(), 0x40040000);
    for a in [
        Speed::Mhz66,
        Speed::Mhz33,
        Speed::Mhz22,
        Speed::Mhz16,
        Speed::Mhz100,
        Speed::Khz800,
    ] {
        for b in [
            Speed::Mhz66,
            Speed::Mhz33,
            Speed::Mhz22,
            Speed::Mhz16,
            Speed::Mhz100,
            Speed::Khz800,
        ] {
            assert_eq!(a.slower(b).rank(), a.rank().min(b.rank()));
        }
    }
}
#[test]
fn smbus_w1c_decode_and_preflight_failures() {
    let mut io = Io::new();
    io.pm[..4].copy_from_slice(&0x20001265u32.to_le_bytes());
    initialize_smbus(&mut io).unwrap();
    assert_eq!(
        io.writes,
        vec![
            (1, 0, 4, 0x20000b75),
            (3, 0xe, 1, 41),
            (3, 0, 1, 0xff),
            (3, 1, 1, 0x1f),
            (4, 0, 1, 0xff),
            (4, 1, 1, 0x1f)
        ]
    );
    let mut io = Io::new();
    io.lpc[0] = u32::MAX;
    assert_eq!(prepare_lpc(&mut io), Err(Error::Unavailable));
    assert!(io.writes.is_empty());
    let mut io = Io::new();
    assert_eq!(configure_spi(&mut io, CONFIG), Err(Error::WrongSpiBase));
    assert!(io.writes.is_empty());
    io.lpc[0xa0 / 4] = SPI_BASE;
    // Status-only busy/absent with CNTRL0 idle must not configure anything.
    assert_eq!(io.spi_read32(0), 0);
    io.spi[0x4c..0x50].copy_from_slice(&0x80000000u32.to_le_bytes());
    assert_eq!(configure_spi(&mut io, CONFIG), Err(Error::SpiBusy));
    assert!(io.writes.is_empty());
    io.spi[0x4c..0x50].fill(0xff);
    assert_eq!(configure_spi(&mut io, CONFIG), Err(Error::Unavailable));
    assert!(io.writes.is_empty());
    io.spi[0x4c..0x50].fill(0);
    io.spi[..4].fill(0xff);
    assert_eq!(configure_spi(&mut io, CONFIG), Err(Error::Unavailable));
    assert!(io.writes.is_empty());
    io.pm[..4].fill(0xff);
    assert_eq!(initialize_smbus(&mut io), Err(Error::Unavailable));
    assert!(io.writes.is_empty());
}
