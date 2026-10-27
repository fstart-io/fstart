// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::fch::{ACPI_MMIO, Acpimmio, Fch, Space, gpio::*, testing::World};
use crate::soc::{Soc, picasso::Picasso};
extern crate std;
use std::vec::Vec;

const CONTROL_9: u32 = ACPI_MMIO + 0x1500 + 4 * 9;
const MASTER: u32 = ACPI_MMIO + 0x15fc;
fn world() -> World {
    let world = World::default();
    world.set(
        Space::Mmio,
        CONTROL_9,
        4,
        STATUS_MASK | (1 << 17) | (1 << 22),
    );
    world.set(Space::Mmio, MASTER, 4, u32::MAX);
    world
}
fn gpio<'a>(world: &'a World, bank: &'static Bank) -> Gpio<&'a World> {
    Gpio::new(Fch::new(world, Acpimmio::DIE0), bank)
}
fn writes(world: &World, address: u32) -> Vec<u32> {
    world
        .writes()
        .into_iter()
        .filter(|&(_, a, ..)| a == address)
        .map(|(.., value)| value)
        .collect()
}
#[test]
fn override_preserves_strength_clears_status_and_routes_sci() {
    let world = world();
    gpio(&world, Picasso::GPIO_BANK)
        .configure(
            &[Pad::output(9, true)],
            &[Pad::sci(9, Pull::Up, Trigger::LevelLow)],
        )
        .unwrap();
    let control = writes(&world, CONTROL_9);
    // Drive/reserved bits untouched, output removed, LEVEL_HIGH at the pad
    // (the SCI trigger is in the SMI bank), then the status cleared.
    let configured = (1 << 17) | (1 << 20) | (1 << 8);
    assert_eq!(control, [configured, configured | STATUS_MASK]);
    // GPIO9 is IOMUX function 0; GEVENT22 raises its SCI, low level.
    assert_eq!(writes(&world, ACPI_MMIO + 0xd09), [0]);
    assert_eq!(world.get(Space::Mmio, ACPI_MMIO + 0x240 + 22, 1), 22);
    assert_eq!(world.get(Space::Mmio, ACPI_MMIO + 0x20c, 4), 1 << 22);
    assert_eq!(world.get(Space::Mmio, ACPI_MMIO + 0x208, 4), 0);
    assert_eq!(world.get(Space::Mmio, MASTER, 4) & (1 << 28), 0);
    assert_eq!(world.get(Space::Mmio, MASTER, 4) & (1 << 30), 1 << 30);
}
#[test]
fn validate_before_first_hardware_write() {
    let world = world();
    let gpio = gpio(&world, Picasso::GPIO_BANK);
    for (pads, error) in [
        (
            [Pad::output(9, true), Pad::output(9, false)],
            Error::DuplicatePad,
        ),
        (
            [
                Pad::output(9, true),
                Pad::sci(16, Pull::Up, Trigger::BothEdges),
            ],
            Error::BothEdgesSci,
        ),
        (
            [Pad::output(9, true), Pad::output(63, true)],
            Error::InvalidPad,
        ),
    ] {
        assert_eq!(gpio.configure(&pads, &[]), Err(error));
    }
    assert!(world.writes().is_empty());
}
#[cfg(feature = "zeppelin")]
#[test]
fn banks_without_a_gpio_function_need_native_pads() {
    let world = World::default();
    let gpio = gpio(&world, crate::soc::zeppelin::Zeppelin::GPIO_BANK);
    assert_eq!(
        gpio.configure(&[Pad::input(149, Pull::None)], &[]),
        Err(Error::NoGpioFunction)
    );
    let ready = Pad::native_with(149, 1, Control::DEBOUNCE.val(5));
    gpio.configure(&[ready], &[]).unwrap();
    assert_eq!(world.get(Space::Mmio, ACPI_MMIO + 0xd00 + 149, 1), 1);
    world.set(Space::Mmio, ACPI_MMIO + 0x1500 + 4 * 149, 4, 1 << 16);
    assert_eq!(gpio.get(149), Ok(true));
    assert_eq!(gpio.get(153), Err(Error::InvalidPad));
}
