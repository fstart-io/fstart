// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::legacy::*;
use crate::fch::{ACPI_MMIO, Acpimmio, Fch, Space, testing::World};
use std::vec::Vec;

const PM: u32 = ACPI_MMIO + 0x300;
const SD_POWER: u32 = ACPI_MMIO + 0x1e40 + 25 * 2;
/// Vilboz: the console on UART 0, I2C 2 and 3.
fn routes() -> Routes {
    Routes::new([0], [2, 3]).unwrap()
}
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
    initialize(&fch, true, false, &routes());
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
        (0x90, 9),
        (0x97, 16),
        (0x9a, 16),
        (0xc3, 5),
        (0xe2, 7),
        // I2C 0-5: 2 and 3 on IRQ 10 and 11, as coreboot's Zork.
        (0xf0, 0x1f),
        (0xf1, 0x1f),
        (0xf2, 10),
        (0xf3, 11),
        (0xf6, 0x1f),
        (0xf7, 0x1f),
        // UART 0-3: the console on COM1's IRQ 4.
        (0xf4, 4),
        (0xf5, 0x1f),
        (0xf8, 0x1f),
        (0xf9, 0x1f),
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
    initialize(&Fch::new(&world, Acpimmio::DIE0), false, true, &routes());
    assert!(world.writes().iter().all(|&(space, ..)| space == Space::Io));
}
#[test]
fn irq14_and_15_take_the_routed_internal_sources() {
    let world = world();
    // Reset: IRQ14/15 from the legacy IDE, IRQ1/12 from the IMC.
    world.set_indexed(0xc00, 8, 0);
    initialize(&Fch::new(&world, Acpimmio::DIE0), true, false, &routes());
    assert_eq!(world.indexed(0xc00, 8), 0xfa);
}

#[test]
fn every_enabled_controller_gets_an_irq_of_its_own() {
    let routes = routes();
    assert_eq!(routes.irq(source::UART[0]), Some(4));
    assert_eq!(routes.irq(source::I2C[2]), Some(10));
    assert_eq!(routes.irq(source::I2C[3]), Some(11));
    assert_eq!(routes.irq(source::UART[1]), None);
    assert_eq!(routes.irq(source::GPIO), Some(7));
    assert_eq!(routes.irq(source::SCI), Some(crate::fch::SCI_IRQ));
    // Two UARTs and all six I2C controllers: the shared inputs run out.
    let all = Routes::new([0, 1], 0..5).unwrap();
    let mut irqs: Vec<u8> = (0..2)
        .map(|n| all.irq(source::UART[n]).unwrap())
        .chain((0..5).map(|n| all.irq(source::I2C[n]).unwrap()))
        .collect();
    assert_eq!(irqs, [4, 3, 10, 11, 6, 14, 15]);
    irqs.sort_unstable();
    irqs.dedup();
    assert_eq!(irqs.len(), 7);
    assert_eq!(Routes::new([0, 1], 0..6), Err(TooManyControllers));
    assert_eq!(Routes::new([4], []), Err(TooManyControllers));
}

#[test]
fn sci_is_routed_even_if_previous_firmware_disabled_it() {
    let world = world();
    world.set_indexed(0xc00, u32::from(source::SCI | 0x80), 0x1f);
    initialize(&Fch::new(&world, Acpimmio::DIE0), true, false, &routes());
    assert_eq!(world.indexed(0xc00, u32::from(source::SCI | 0x80)), 9);
}
