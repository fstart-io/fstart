// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use crate::fch::{ACPI_MMIO, Acpimmio, Space, testing::World};
use std::vec;

/// Device and vendor IDs of the LPC bridge.
fn world() -> World {
    let world = World::default();
    world.set(Space::Lpc, 0, 4, 0x1022);
    world
}
fn fch(world: &World) -> Fch<&World> {
    Fch::new(world, Acpimmio::DIE0)
}
const PM: u32 = ACPI_MMIO + 0x300;
fn spi(offset: u32) -> u32 {
    SPI_BASE + offset
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
fn spi_modes_and_speed_override_preserve_unrelated_register_bits() {
    let world = world();
    world.set(Space::Lpc, 0xa0, 4, SPI_BASE | 0xff);
    world.set(Space::Lpc, 0xb8, 4, 0x00200040);
    world.set(Space::Mmio, spi(0x2c), 2, 0x9234);
    world.set(Space::Mmio, spi(0x20), 2, 0x0040);
    world.set(Space::Mmio, spi(0), 4, 0x52045566);
    configure_spi(&fch(&world), CONFIG).unwrap();
    assert_eq!(
        world.writes(),
        vec![
            (Space::Lpc, 0xa0, 4, SPI_BASE | 0xfe),
            (Space::Lpc, 0xb8, 4, 0x01a00040),
            (Space::Mmio, spi(0x2c), 2, 0x1234),
            (
                Space::Mmio,
                spi(0),
                4,
                (0x52045566 & !0x60040000) | 0x40000000
            ),
            (Space::Mmio, spi(0x22), 2, 0x1000),
            (Space::Mmio, spi(0x20), 2, 0x41)
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
    let world = world();
    world.set(Space::Mmio, PM, 4, 0x20001265);
    initialize_smbus(&fch(&world)).unwrap();
    assert_eq!(
        world.writes(),
        vec![
            (Space::Mmio, PM, 4, 0x20000b75),
            (Space::Mmio, ACPI_MMIO + 0xa0e, 1, 41),
            (Space::Mmio, ACPI_MMIO + 0xa00, 1, 0xff),
            (Space::Mmio, ACPI_MMIO + 0xa01, 1, 0x1f),
            (Space::Mmio, ACPI_MMIO + 0x900, 1, 0xff),
            (Space::Mmio, ACPI_MMIO + 0x901, 1, 0x1f)
        ]
    );
    let world = World::default();
    world.set(Space::Lpc, 0, 4, u32::MAX);
    assert_eq!(prepare_lpc(&fch(&world)), Err(Error::Unavailable));
    assert!(world.writes().is_empty());
    let world = self::world();
    assert_eq!(
        configure_spi(&fch(&world), CONFIG),
        Err(Error::WrongSpiBase)
    );
    assert!(world.writes().is_empty());
    world.set(Space::Lpc, 0xa0, 4, SPI_BASE);
    // Status-only busy/absent with CNTRL0 idle must not configure anything.
    world.set(Space::Mmio, spi(0x4c), 4, 0x8000_0000);
    assert_eq!(configure_spi(&fch(&world), CONFIG), Err(Error::SpiBusy));
    assert!(world.writes().is_empty());
    world.set(Space::Mmio, spi(0x4c), 4, u32::MAX);
    assert_eq!(configure_spi(&fch(&world), CONFIG), Err(Error::Unavailable));
    assert!(world.writes().is_empty());
    world.set(Space::Mmio, spi(0x4c), 4, 0);
    world.set(Space::Mmio, spi(0), 4, u32::MAX);
    assert_eq!(configure_spi(&fch(&world), CONFIG), Err(Error::Unavailable));
    assert!(world.writes().is_empty());
    world.set(Space::Mmio, PM, 4, u32::MAX);
    assert_eq!(initialize_smbus(&fch(&world)), Err(Error::Unavailable));
    assert!(world.writes().is_empty());
}
