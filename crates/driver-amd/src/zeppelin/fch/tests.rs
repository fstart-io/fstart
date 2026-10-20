// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use crate::fam17h::tsc::testing::Delay;
use super::*;
use crate::{
    df,
    fch::{Acpimmio, Bus, Fch, Space},
    smn::testing::{SmnModel, SmnTest},
    soc::Soc,
    zeppelin::{Die, Package, Revision, Zeppelin},
};
use core::cell::RefCell;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{collections::BTreeMap, vec, vec::Vec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Bus(Space, u32, u32),
    Config(PciAddress, u16, u32),
}

/// FCH register spaces, PCI config space with the per-die SMN index/data
/// pair, and the order of all writes.
#[derive(Default)]
struct World {
    bytes: BTreeMap<(Space, u32), u8>,
    /// Index/data I/O pairs: (index port, index) -> value.
    indexed: BTreeMap<(u16, u32), u32>,
    selected: BTreeMap<u16, u32>,
    config: BTreeMap<(PciAddress, u16), u32>,
    smn: BTreeMap<(u8, u32), u32>,
    events: Vec<Event>,
}
const PAIRS: [(u16, u16); 4] = [(0xc00, 0xc01), (0x70, 0x71), (0xcd6, 0xcd7), (0xcd8, 0xcdc)];
fn pair(port: u32) -> Option<(u16, bool)> {
    PAIRS.iter().find_map(|&(index, data)| match port as u16 {
        p if p == index => Some((index, true)),
        p if p == data => Some((index, false)),
        _ => None,
    })
}
impl World {
    fn pirq(&self, source: u8, mode: irq::Mode) -> u8 {
        let index = u32::from(source) | if mode == irq::Mode::Apic { 0x80 } else { 0 };
        self.indexed.get(&(0xc00, index)).copied().unwrap_or(0) as u8
    }
    fn mmio8(&self, address: u32) -> u8 {
        self.bytes
            .get(&(Space::Mmio, address))
            .copied()
            .unwrap_or(0)
    }
    fn position(&self, event: Event) -> usize {
        self.events.iter().position(|&e| e == event).unwrap()
    }
}

#[derive(Clone, Copy)]
struct Hw<'a>(&'a RefCell<World>);
impl Hw<'_> {
    fn fch(&self) -> Fch<Self> {
        Fch::new(*self, Acpimmio::DIE0)
    }
}
impl Bus for Hw<'_> {
    fn read(&self, space: Space, address: u32, bytes: u8) -> u32 {
        let world = self.0.borrow();
        if space == Space::Io
            && let Some((index, false)) = pair(address)
        {
            let selected = world.selected.get(&index).copied().unwrap_or(0);
            return world.indexed.get(&(index, selected)).copied().unwrap_or(0);
        }
        (0..bytes).rev().fold(0, |value, i| {
            let byte = world.bytes.get(&(space, address + u32::from(i))).copied();
            (value << 8) | u32::from(byte.unwrap_or(0))
        })
    }
    fn write(&self, space: Space, address: u32, bytes: u8, value: u32) {
        let mut world = self.0.borrow_mut();
        world.events.push(Event::Bus(space, address, value));
        if space == Space::Io {
            match pair(address) {
                Some((index, true)) => {
                    world.selected.insert(index, value);
                    return;
                }
                Some((index, false)) => {
                    let selected = world.selected.get(&index).copied().unwrap_or(0);
                    world.indexed.insert((index, selected), value);
                    return;
                }
                None => {}
            }
        }
        for i in 0..bytes {
            world
                .bytes
                .insert((space, address + u32::from(i)), (value >> (8 * i)) as u8);
        }
    }
}
impl ConfigRegionAccess for Hw<'_> {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        let world = self.0.borrow();
        let root = PciAddress::new(0, address.bus(), 0, 0);
        if address == root && offset == 0xbc {
            let index = world.config.get(&(root, 0xb8)).copied().unwrap_or(0);
            return world.smn.get(&(address.bus(), index)).copied().unwrap_or(0);
        }
        world
            .config
            .get(&(address, offset))
            .copied()
            .unwrap_or(u32::MAX)
    }
    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        let mut world = self.0.borrow_mut();
        let root = PciAddress::new(0, address.bus(), 0, 0);
        if address == root && offset == 0xbc {
            let index = world.config.get(&(root, 0xb8)).copied().unwrap_or(0);
            world.smn.insert((address.bus(), index), value);
            return;
        }
        world.events.push(Event::Config(address, offset, value));
        world.config.insert((address, offset), value);
    }
}

/// SMN of one die; reads of an address first drain its scripted values.
#[derive(Default)]
struct SmnMock {
    regs: BTreeMap<u32, u32>,
    script: BTreeMap<u32, Vec<u32>>,
}
impl SmnModel for SmnMock {
    fn read(&mut self, address: u32) -> u32 {
        match self.script.get_mut(&address) {
            Some(values) if !values.is_empty() => values.remove(0),
            _ => self.regs.get(&address).copied().unwrap_or(0),
        }
    }
    fn write(&mut self, address: u32, value: u32) {
        self.regs.insert(address, value);
    }
}

const DIE1_BUS: u8 = 0x40;
const DIE1_NONPCI: u32 = 0xbf80_0000;
fn df0() -> PciAddress {
    PciAddress::new(0, 0, 0x18, 0)
}

/// An SP4 world: two dies, die 1 on bus 0x40.
fn sp4() -> RefCell<World> {
    let mut world = World::default();
    world
        .config
        .insert((PciAddress::new(0, 0, 0x18, 1), 0x204), 2);
    world.config.insert((df0(), 0x84), 0);
    world
        .config
        .insert((PciAddress::new(0, 0, 0x19, 0), 0x84), u32::from(DIE1_BUS));
    RefCell::new(world)
}
fn dies(world: &RefCell<World>) -> Vec<Die<Hw<'_>>> {
    // SAFETY: the mock has no concurrent users.
    unsafe { df::dies(Hw(world)) }.collect()
}

#[test]
fn irq_routing_keeps_misc_controls_and_disconnects_the_rest() {
    let world = RefCell::new(World::default());
    let bus = Hw(&world);
    for source in irq::MISC..=irq::MISC2 {
        irq::write(&bus, source, irq::Mode::Pic, 0xa0 | source);
        irq::write(&bus, source, irq::Mode::Apic, 0xb0 | source);
    }
    irq::write(&bus, irq::SIRQA, irq::Mode::Apic, 7);
    irq::write(&bus, 0x13, irq::Mode::Pic, 0x42); // reserved index
    // As the MJ11 lists them: the MISC entries come with zeros.
    let routes = [
        irq::Route {
            source: irq::A,
            pic: irq::NC,
            apic: 16,
        },
        irq::Route {
            source: irq::SCI,
            pic: 9,
            apic: 9,
        },
        irq::Route {
            source: irq::MISC,
            pic: 0,
            apic: 0,
        },
        irq::Route {
            source: irq::MISC2,
            pic: 0,
            apic: 0,
        },
    ];
    irq::write_routing(&bus, &routes).unwrap();

    let w = world.borrow();
    assert_eq!(
        (
            w.pirq(irq::A, irq::Mode::Pic),
            w.pirq(irq::A, irq::Mode::Apic)
        ),
        (irq::NC, 16)
    );
    assert_eq!(
        (
            w.pirq(irq::SCI, irq::Mode::Pic),
            w.pirq(irq::SCI, irq::Mode::Apic)
        ),
        (9, 9)
    );
    for source in irq::MISC..=irq::MISC2 {
        assert_eq!(w.pirq(source, irq::Mode::Pic), 0xa0 | source);
        assert_eq!(w.pirq(source, irq::Mode::Apic), 0xb0 | source);
    }
    assert_eq!(w.pirq(irq::SIRQA, irq::Mode::Apic), irq::NC);
    assert_eq!(w.pirq(0x13, irq::Mode::Pic), 0x42);
    drop(w);

    world.borrow_mut().events.clear();
    let bad = [
        routes[0],
        irq::Route {
            source: 0x80,
            pic: 0,
            apic: 0,
        },
    ];
    assert_eq!(
        irq::write_routing(&bus, &bad),
        Err(irq::Error::InvalidSource(1))
    );
    assert!(world.borrow().events.is_empty());
}

const SATA_PI: u32 = 0x0310_100c;
const SATA_CAP: u32 = 0x0310_1000;
const SATA_MISC: u32 = 0x0310_1800;
const SATA_MSI: u32 = 0x0310_0008;
const fn ssts(port: u32) -> u32 {
    0x0310_1128 + port * 0x80
}

#[test]
fn sata_late_reports_only_ports_with_a_device() {
    let world = RefCell::new(World::default());
    let function = PciAddress::new(0, 3, 0, 2);
    // MSI multiple message capable 3 (16 vectors), plus unrelated bits.
    world
        .borrow_mut()
        .config
        .insert((function, 0xa0), (3 << 17) | 0x0080_7005);
    let smn = SmnTest::new(SmnMock::default());
    {
        let mut model = smn.model();
        model.regs.insert(SATA_PI, 0xff);
        model.regs.insert(SATA_CAP, 0x1f | 0xc000_0000);
        model.regs.insert(SATA_MSI, 0x0700_0001);
        model.regs.insert(ssts(1), 0x123);
        model.regs.insert(ssts(3), 0x113);
        model.regs.insert(ssts(5), 0x101); // device, no PHY communication
    }

    assert_eq!(sata::late(&smn.smn(), 0, Hw(&world), function), 0b1010);
    let model = smn.model();
    assert_eq!(model.regs[&SATA_PI], 0b1010);
    assert_eq!(model.regs[&SATA_CAP], 1 | 0xc000_0000);
    assert_eq!(model.regs[&SATA_MSI], 0x0300_0001);
    assert_eq!(model.regs[&SATA_MISC], 0b1111_0101 << 16);
    drop(model);

    let smn = SmnTest::new(SmnMock::default());
    smn.model().regs.insert(SATA_PI, 0xff);
    assert_eq!(sata::late(&smn.smn(), 0, Hw(&world), function), 1);
    assert_eq!(smn.model().regs[&SATA_CAP], 0);
}

#[test]
fn sata_reset_waits_for_each_phy_but_never_longer_than_1ms() {
    let smn = SmnTest::new(SmnMock::default());
    {
        let mut model = smn.model();
        (0..sata::PORTS).for_each(|port| {
            model.regs.insert(ssts(port), 4);
        });
        model.script.insert(ssts(0), vec![0, 1]);
        model.regs.insert(ssts(6), 3); // stays online
    }
    let mut waited = 0;
    sata::reset(&smn.smn(), &mut Delay(|us| waited += us));
    assert_eq!(waited, 2 * 10 + 1000);
    // DET = offline requested on every port.
    let model = smn.model();
    assert!((0..sata::PORTS).all(|port| model.regs[&(0x0310_112c + port * 0x80)] & 0xf == 4));
}

#[test]
fn reset_init_conditionals() {
    let world = RefCell::new(World::default());
    {
        let mut w = world.borrow_mut();
        w.indexed.insert((0x70, 0x0d), 0xbf); // RTC valid with all of [5:0]
        w.indexed.insert((0xc00, 0x09), 0x66);
        // An SMI both enabled and pending.
        w.bytes.insert((Space::Mmio, 0xfed8_0200), 0x01);
        w.bytes.insert((Space::Mmio, 0xfed8_0204), 0x01);
    }
    reset_init(&Hw(&world).fch(), Revision::A0);
    let w = world.borrow();
    assert_eq!(w.indexed[&(0x70, 0x0d)], 0x80);
    assert_eq!(w.indexed[&(0xc00, 0x09)], 0x10);
    assert_eq!(w.bytes[&(Space::Io, 0x801)], 0x40);
    assert_eq!(w.mmio8(0xfed8_0308), 0x60 | 0x14);
    drop(w);

    let world = RefCell::new(World::default());
    world.borrow_mut().indexed.insert((0x70, 0x0d), 0x9e);
    reset_init(&Hw(&world).fch(), Revision::B2);
    let w = world.borrow();
    assert_eq!(w.indexed[&(0x70, 0x0d)], 0x9e);
    assert!(!w.bytes.contains_key(&(Space::Io, 0x801)));
    assert_eq!(w.mmio8(0xfed8_0308), 0x14);
}

#[test]
fn multi_reset_moves_acpimmio_while_io_is_routed_to_the_die() {
    let world = sp4();
    world.borrow_mut().config.insert((df0(), 0xc0), 0x0000_0003);
    world.borrow_mut().config.insert((df0(), 0xc4), 0x00ff_f004);
    world.borrow_mut().indexed.insert((0xcd6, 0xd5), 0xfe);
    let dies = dies(&world);
    // Die 1's PHYs go offline at once.
    (0..sata::PORTS).for_each(|port| {
        world.borrow_mut().smn.insert((DIE1_BUS, ssts(port)), 4);
    });
    multi::reset(
        &Hw(&world),
        &dies,
        Package::Sp4,
        |_| DIE1_NONPCI,
        &mut Delay(|_| ()),
    );

    let w = world.borrow();
    let mmio = DIE1_NONPCI + (1 << 20);
    assert_eq!(w.indexed[&(0xcd6, 0xd6)], (mmio >> 16) & 0xff);
    assert_eq!(w.indexed[&(0xcd6, 0xd7)], mmio >> 24);
    assert_eq!(w.indexed[&(0xcd6, 0xd5)], 0xfd);
    assert_eq!(w.mmio8(mmio + 0xd02), 1); // SP4 IOMUX pin 2
    // The A-Link decode is only open for its one write.
    assert_eq!(w.indexed[&(0xcd6, 0xe0)], 0);

    let routed = w.position(Event::Config(
        df0(),
        0xc4,
        u32::from(Zeppelin::ioms_fabric_id(1)),
    ));
    let restored = w.position(Event::Config(df0(), 0xc4, 0x00ff_f004));
    let io = |e: &Event| matches!(e, Event::Bus(Space::Io, ..));
    let first_io = w.events.iter().position(io).unwrap();
    let last_io = w.events.iter().rposition(io).unwrap();
    assert!(routed < first_io && last_io < restored);
    assert_eq!(w.config[&(df0(), 0xc0)], 3);

    // The die's own SATA reset, through its SMN.
    assert_eq!(w.smn[&(DIE1_BUS, 0x0310_112c)] & 0xf, 4);
    assert!(!w.smn.keys().any(|&(bus, _)| bus == 0));
}

const NBIF_SATA: u32 = 0x1023_4400;
const NBIF_USB: u32 = 0x1013_4600;
const fn nbif_xgbe(port: u32) -> u32 {
    0x1023_4800 + 0x200 * port
}

#[test]
fn multi_usb_and_env_follow_the_board_and_the_straps() {
    let world = sp4();
    let dies = dies(&world);
    let strap = 1 << 28;
    {
        let mut w = world.borrow_mut();
        w.smn.insert((DIE1_BUS, NBIF_USB), strap | 0x1234);
        w.smn.insert((DIE1_BUS, NBIF_SATA), strap | 0x7901);
        w.smn.insert((DIE1_BUS, nbif_xgbe(0)), strap);
        w.smn.insert((DIE1_BUS, nbif_xgbe(1)), strap);
        w.config
            .insert((PciAddress::new(0, DIE1_BUS, 8, 1), 0), 0x1450_1022);
    }
    let board = [
        Devices {
            xhci: true,
            sata: true,
        },
        Devices {
            xhci: false,
            sata: false,
        },
    ];
    multi::usb(&dies, &board, Package::Sp4, &mut Delay(|_| ()));
    multi::env(
        &Hw(&world),
        &dies,
        &board,
        |_| DIE1_NONPCI,
        0x1000_1458,
        &mut Delay(|_| ()),
    );
    {
        let w = world.borrow();
        assert_eq!(w.smn[&(DIE1_BUS, NBIF_USB)], 0x1234);
        assert_eq!(w.smn[&(DIE1_BUS, NBIF_SATA)], 0x7901);
        let xgbe_off = |port: u32| {
            w.smn
                .get(&(DIE1_BUS, 0x1631_f030 + 4 * port))
                .copied()
                .unwrap_or(0)
        };
        assert_eq!([0, 1, 2, 3].map(xgbe_off), [0, 0, 1, 1]);
        assert!(!w.smn.keys().any(|&(bus, _)| bus == 0));
    }

    // Without the internal bridge every strap of the die goes.
    let world = sp4();
    let dies = self::dies(&world);
    for register in [NBIF_SATA, nbif_xgbe(0), nbif_xgbe(3)] {
        world
            .borrow_mut()
            .smn
            .insert((DIE1_BUS, register), strap | 5);
    }
    multi::env(
        &Hw(&world),
        &dies,
        &board,
        |_| DIE1_NONPCI,
        0,
        &mut Delay(|_| ()),
    );
    let w = world.borrow();
    for register in [NBIF_SATA, nbif_xgbe(0), nbif_xgbe(3)] {
        assert_eq!(w.smn[&(DIE1_BUS, register)], 5);
    }
    assert!(!w.smn.contains_key(&(DIE1_BUS, 0x1631_f030)));
}
