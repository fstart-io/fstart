// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::legacy::*;
use crate::fch::{ACPI_MMIO, Acpimmio, Fch, Space, testing::World};
use std::vec::Vec;

const PM: u32 = ACPI_MMIO + 0x300;
const SD_POWER: u32 = ACPI_MMIO + 0x1e40 + 25 * 2;
fn world() -> World {
    let world = World::default();
    world.set(Space::Mmio, PM + 0x40, 4, 0x80001003);
    world.set(Space::Mmio, PM + 0xd3, 1, 0xff);
    world.set(Space::Mmio, PM + 0xe8, 1, 0xff);
    world.set(Space::Mmio, SD_POWER, 1, 0xff);
    world.set(Space::Lpc, 0x40, 1, 0xff);
    world.set(Space::Lpc, 0xa4, 4, 0x20);
    world.set(Space::Lpc, 0xd0, 4, 0x80);
    world.set(Space::Mmio, ACPI_MMIO + 0x200, 4, 0x400);
    world.set(Space::Mmio, ACPI_MMIO + 0x204, 4, 0x400);
    for (port, index, value) in [
        (0xcd6, 0x0a, 0x80),
        (0xcd6, 0xc0, 0xa0),
        (0x70, 0xb, 0xff),
        (0x70, 0xa, 0xff),
        (0xc00, 8, 0xff),
        (0xc00, 9, 0xff),
    ] {
        world.set_indexed(port, index, value);
    }
    // ACPI status is W1C, not ordinary RAM.
    world.set(Space::Io, 0x401, 1, 0x45);
    world.0.borrow_mut().w1c.insert((Space::Io, 0x401));
    world
}
#[test]
fn legacy_preserves_control_bits_and_uses_indexed_byte_w1c_irq_transactions() {
    let world = world();
    let fch = Fch::new(&world, Acpimmio::DIE0);
    disable_rom_sharing(&world);
    assert_eq!(world.get(Space::Lpc, 0x40, 1), 0xe7);
    initialize(&fch, true, false);
    lpc_init(&world);
    assert_eq!(world.get(Space::Mmio, PM + 0x40, 4), 0x80000001);
    assert_eq!(world.get(Space::Mmio, PM + 0xd3, 1), 0xbf);
    assert_eq!(world.get(Space::Mmio, PM + 0xe8, 1), 0xfe);
    assert_eq!(world.get(Space::Mmio, SD_POWER, 1), 0xf7);
    assert_eq!(world.get(Space::Lpc, 0xa4, 4), 0x21);
    assert_eq!(world.get(Space::Lpc, 0xd0, 4), 0x80000080);
    assert_eq!(world.indexed(0xcd6, 0x0a), 0x98);
    assert_eq!(world.indexed(0xcd6, 0xc0), 0xa2);
    assert_eq!(world.indexed(0x70, 0xb), 0xfe);
    assert_eq!(world.indexed(0x70, 0xa), 0xef);
    assert_eq!(world.indexed(0xc00, 8), 0xff);
    assert_eq!(world.indexed(0xc00, 9), 0x99);
    let writes = world.writes();
    let io = |port| {
        writes
            .iter()
            .filter(move |&&(space, address, ..)| (space, address) == (Space::Io, port))
            .map(|&(.., value)| value)
    };
    assert_eq!(io(0xb14).collect::<Vec<_>>(), [0]);
    assert_eq!(io(0xb2a).collect::<Vec<_>>(), [0x2f]);
    assert_eq!(io(0x401).collect::<Vec<_>>(), [0x40, 0x40]);
    assert_eq!(world.get(Space::Io, 0x401, 1), 5);
    let expected = [
        (0x80, 16),
        (0x81, 17),
        (0x82, 18),
        (0x83, 19),
        (0x97, 16),
        (0x9a, 16),
        (0xc3, 5),
        (0xe2, 7),
        (0xf6, 3),
        (0xf7, 4),
        (0xf8, 15),
        (0xf9, 5),
        (0xf0, 10),
        (0xf1, 11),
        (0xf2, 14),
        (0xf3, 6),
        (0xf4, 14),
        (0xf5, 15),
    ];
    let irq_writes = writes
        .windows(2)
        .filter_map(|pair| {
            let [(_, index_port, _, index), (_, data_port, _, irq)] = pair else {
                unreachable!()
            };
            (*index_port == 0xc00 && *data_port == 0xc01 && index & 0x80 != 0)
                .then_some((*index, *irq))
        })
        .collect::<Vec<_>>();
    assert_eq!(irq_writes, expected);
}
#[test]
fn retained_legacy_sd_and_lpc_only_policy_do_not_power_off_or_mask_espi() {
    let world = world();
    initialize(&Fch::new(&world, Acpimmio::DIE0), false, true);
    assert!(world.writes().iter().all(|&(space, ..)| space == Space::Io));
}
#[test]
fn irq14_and_15_take_the_routed_internal_sources() {
    let world = world();
    // Reset: IRQ14/15 from the legacy IDE, IRQ1/12 from the IMC.
    world.set_indexed(0xc00, 8, 0);
    initialize(&Fch::new(&world, Acpimmio::DIE0), true, false);
    assert_eq!(world.indexed(0xc00, 8), 0xfa);
    assert_eq!(internal_irq(source::I2C[2]), Some(14));
    assert_eq!(internal_irq(source::GPIO), Some(7));
}
