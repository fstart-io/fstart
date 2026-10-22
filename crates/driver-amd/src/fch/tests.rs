// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::{testing::World, *};
use std::{vec, vec::Vec};

const AOAC: u32 = ACPI_MMIO + 0x1e00;
/// Every device off with control 0xc6 and power/clock ready.
fn world() -> World {
    let world = World::default();
    for id in 0..29 {
        world.set(Space::Mmio, AOAC + 0x40 + 2 * id, 1, 0xc6);
        world.set(Space::Mmio, AOAC + 0x41 + 2 * id, 1, 3);
    }
    world
}
fn fch(world: &World) -> Fch<&World> {
    Fch::new(world, Acpimmio::DIE0)
}
fn control_writes(world: &World) -> Vec<(u32, u32)> {
    world
        .writes()
        .into_iter()
        .map(|(_, address, _, value)| ((address - AOAC - 0x40) / 2, value))
        .collect()
}
fn state_reads(world: &World) -> Vec<u32> {
    world
        .reads()
        .into_iter()
        .filter(|&(_, address)| (address - AOAC) & 1 == 1)
        .map(|(_, address)| (address - AOAC - 0x41) / 2)
        .collect()
}

#[test]
fn aoac_all_devices_started_before_wait_with_whole_preflight() {
    let world = world();
    fch(&world)
        .power_devices(&[17, 7, 8, 9, 27], 0, || panic!("already ready"))
        .unwrap();
    assert_eq!(
        control_writes(&world),
        vec![(17, 0xcd), (7, 0xcd), (8, 0xcd), (9, 0xcd), (27, 0xcd)]
    );
    assert_eq!(state_reads(&world), [17, 7, 8, 9, 27]);
    let world = self::world();
    assert_eq!(
        fch(&world).power_devices(&[17, 6], 1, || {}),
        Err(Error::InvalidDevice)
    );
    assert!(world.writes().is_empty());
    world.set(Space::Mmio, AOAC + 0x40 + 2 * 7, 1, 0xff);
    assert_eq!(
        fch(&world).power_devices(&[17, 7], 1, || {}),
        Err(Error::Unavailable(7))
    );
    assert!(world.writes().is_empty());
}
#[test]
fn aoac_all_ones_is_not_ready_and_failed_programming_never_continues_waiting() {
    let world = world();
    world.set(Space::Mmio, AOAC + 0x41 + 2 * 17, 1, 0xff);
    assert_eq!(
        fch(&world).power_devices(&[17, 7], 1, || panic!("unavailable")),
        Err(Error::Unavailable(17))
    );
    assert_eq!(world.writes().len(), 2);
    assert_eq!(state_reads(&world), [17]);
    let world = self::world();
    world.set(Space::Mmio, AOAC + 0x41 + 2 * 17, 1, 0);
    let mut delays = 0;
    assert_eq!(
        fch(&world).power_devices(&[17, 7], 2, || delays += 1),
        Err(Error::PowerTimeout(17))
    );
    assert_eq!(delays, 2);
    assert_eq!(state_reads(&world), [17, 17, 17]);
}

#[test]
fn early_lpc_aperture_retains_flags_and_exact_decode_widths_and_order() {
    let world = World::default();
    world.set(Space::Lpc, 0, 4, 0x1022);
    world.set(Space::Mmio, ACPI_MMIO + 0x3ec, 1, 0x30);
    world.set(Space::Lpc, 0x48, 4, 0xdeadffef);
    world.set(Space::Lpc, 0xa0, 4, 0x123456f7);
    early::prepare_lpc(&fch(&world)).unwrap();
    assert_eq!(
        world.writes(),
        vec![
            (Space::Mmio, ACPI_MMIO + 0x3ec, 1, 0x31),
            (Space::Lpc, 0x44, 4, 0),
            (Space::Lpc, 0x48, 4, 0xff80),
            (Space::Lpc, 0x44, 4, 0),
            (Space::Lpc, 0x4c, 4, 0),
            (Space::Lpc, 0x60, 4, 0),
            (Space::Lpc, 0x90, 4, 0),
            (Space::Lpc, 0xa0, 4, SPI_BASE | 0xf7)
        ]
    );
}

#[test]
fn early_smbus_and_power_failure_state() {
    let world = World::default();
    world.set(Space::Mmio, ACPI_MMIO + 0x300, 4, 0x1234_5678);
    early::initialize_smbus(&fch(&world)).unwrap();
    early::set_power_failure_state(&fch(&world), early::PowerFailure::Previous);
    assert_eq!(
        world.writes(),
        vec![
            (Space::Mmio, ACPI_MMIO + 0x300, 4, 0x1234_0b78 | 1 << 4),
            (Space::Mmio, ACPI_MMIO + 0xa0e, 1, 41),
            (Space::Mmio, ACPI_MMIO + 0xa00, 1, 0xff),
            (Space::Mmio, ACPI_MMIO + 0xa01, 1, 0x1f),
            (Space::Mmio, ACPI_MMIO + 0x900, 1, 0xff),
            (Space::Mmio, ACPI_MMIO + 0x901, 1, 0x1f),
            (Space::Mmio, ACPI_MMIO + 0x35b, 1, 7),
        ]
    );
}

#[test]
fn abcfg_transactions_reset_index_after_read_and_write() {
    let world = World::default();
    world.set_indexed(alink::INDEX, 0xc000_0054, 0x1234_5678);
    alink::abcfg(&world, (0x54, 0xff, 0x11));
    let index = 0xc000_0054;
    assert_eq!(
        world.0.borrow().log,
        [
            testing::Access::Write(Space::Io, 0xcd8, 4, index),
            testing::Access::Read(Space::Io, 0xcdc),
            testing::Access::Write(Space::Io, 0xcd8, 4, 0),
            testing::Access::Write(Space::Io, 0xcd8, 4, index),
            testing::Access::Write(Space::Io, 0xcdc, 4, 0x1234_5611),
            testing::Access::Write(Space::Io, 0xcd8, 4, 0),
        ]
    );
}
