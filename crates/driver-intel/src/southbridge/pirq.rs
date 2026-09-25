//! PCI interrupt (PIRQ) routing: slot/pin to PIRQ line to GSI.
//!
//! One declaration of the routing feeds everything that depends on it: the
//! RCBA `DxxIP`/`DxxIR` registers firmware programs, the LPC `PIRQx_ROUT`
//! registers, and the ACPI `_PRT` the OS reads.
//!
//! Ported from coreboot `southbridge/intel/common/rcba_pirq.c`. The important
//! part is *where the GSI comes from*: a device pin delivers on the PIRQ line
//! the router was actually programmed with, and PIRQs map to GSIs starting at
//! 16. Deriving the GSI from the pin index instead (as a hand-written table
//! does) silently drops the interrupt of every pin the router rotates.

/// PIRQs are mapped to ACPI/APIC GSIs starting at 16.
pub const GSI_BASE: u8 = 16;

/// Lowest and highest slot the RCBA router covers. Slots outside this range
/// (the northbridge's integrated graphics, for example) use the 1:1 pin to
/// PIRQ mapping.
const ROUTED_SLOT_RANGE: (u8, u8) = (0x13, 0x1f);

/// Slot 24 has no router register and no device behind it.
const RESERVED_SLOT: u8 = 0x18;

/// PCI interrupt pin (INTA#..INTD#).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PciPin {
    A = 1,
    B = 2,
    C = 3,
    D = 4,
}

impl PciPin {
    /// Every pin, in PCI order.
    pub const ALL: [Self; 4] = [Self::A, Self::B, Self::C, Self::D];

    /// ACPI `_PRT` pin index (0..3).
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8 - 1
    }

    /// Pin for an ACPI `_PRT` index.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::A),
            1 => Some(Self::B),
            2 => Some(Self::C),
            3 => Some(Self::D),
            _ => None,
        }
    }

    /// Pin for a PCI config space `Interrupt Pin` value (0 means none).
    #[must_use]
    pub const fn from_config(value: u8) -> Option<Self> {
        match value {
            0 => None,
            value => Self::from_index(value - 1),
        }
    }
}

/// Programmable interrupt request line (PIRQA#..PIRQH#).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Pirq {
    A = 0,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
}

impl Pirq {
    /// Every line, in chipset order.
    pub const ALL: [Self; 8] = [
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
    ];

    /// Index into the PIRQ range (also the GSI offset from [`GSI_BASE`]).
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// Line for a PIRQ index.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::A),
            1 => Some(Self::B),
            2 => Some(Self::C),
            3 => Some(Self::D),
            4 => Some(Self::E),
            5 => Some(Self::F),
            6 => Some(Self::G),
            7 => Some(Self::H),
            _ => None,
        }
    }
}

/// One RCBA `DxxIR` field: the PIRQ line a slot's interrupt pin is wired to.
#[derive(Clone, Copy, Debug)]
pub struct PinRoute {
    pub slot: u8,
    pub pin: PciPin,
    pub line: Pirq,
}

/// One RCBA `DxxIP` register: which interrupt pin each function asserts.
#[derive(Clone, Copy, Debug)]
pub struct SlotPins {
    pub slot: u8,
    pub functions: [Option<PciPin>; 8],
}

/// Routing data for one chipset and board wiring.
#[derive(Clone, Copy, Debug)]
pub struct PirqRouting {
    /// `DxxIP`: pin per function, per routed slot.
    pub slot_pins: &'static [SlotPins],
    /// `DxxIR`: PIRQ line per interrupt pin. One entry per `_PRT` route.
    pub pin_routes: &'static [PinRoute],
}

impl PirqRouting {
    fn route(&self, slot: u8, pin: PciPin) -> Option<&PinRoute> {
        self.pin_routes
            .iter()
            .find(|route| route.slot == slot && route.pin == pin)
    }

    /// PIRQ line a device pin delivers on.
    ///
    /// Slots the router does not cover keep the 1:1 pin/PIRQ relation they are
    /// hardwired to (coreboot `map_pirq`).
    #[must_use]
    pub fn line(&self, slot: u8, pin: PciPin) -> Pirq {
        if let Some(route) = self.route(slot, pin) {
            return route.line;
        }
        Pirq::from_index(pin.index()).expect("pin index is within the PIRQ range")
    }

    /// APIC-mode GSI a device pin delivers on.
    #[must_use]
    pub fn gsi(&self, slot: u8, pin: PciPin) -> u8 {
        GSI_BASE + self.line(slot, pin).index()
    }

    /// `(slot, pin, gsi)` for every route the root bus `_PRT` must publish.
    pub fn root_bus_routes(&self) -> impl Iterator<Item = (u8, PciPin, u8)> + '_ {
        self.pin_routes
            .iter()
            .map(|route| (route.slot, route.pin, GSI_BASE + route.line.index()))
    }

    /// RCBA `DxxIP` value for a slot (0 when the slot asserts no pin).
    #[must_use]
    pub fn ip_value(&self, slot: u8) -> u32 {
        self.slot_pins
            .iter()
            .find(|pins| pins.slot == slot)
            .map_or(0, |pins| {
                pins.functions
                    .iter()
                    .enumerate()
                    .map(|(function, pin)| match pin {
                        Some(pin) => u32::from(*pin as u8) << (4 * function),
                        None => 0,
                    })
                    .sum()
            })
    }

    /// RCBA `DxxIR` value for a slot (0 when the slot has no routed pin).
    #[must_use]
    pub fn ir_value(&self, slot: u8) -> u16 {
        self.pin_routes
            .iter()
            .filter(|route| route.slot == slot)
            .map(|route| u16::from(route.line.index()) << (4 * route.pin.index()))
            .sum()
    }

    /// Whether this slot has an RCBA router register.
    #[must_use]
    pub const fn slot_is_routed(slot: u8) -> bool {
        slot >= ROUTED_SLOT_RANGE.0 && slot <= ROUTED_SLOT_RANGE.1 && slot != RESERVED_SLOT
    }

    /// Every pin the `DxxIP` assignment uses, for the fail-closed check that an
    /// advertised route exists for each of them.
    #[must_use = "the fail-closed route check consumes every assigned pin"]
    pub fn assigned_pins(&self) -> impl Iterator<Item = (u8, PciPin)> + '_ {
        self.slot_pins.iter().flat_map(|pins| {
            pins.functions
                .iter()
                .flatten()
                .map(move |pin| (pins.slot, *pin))
        })
    }
}

/// ICH7/NM10 routing as programmed by coreboot's Pineview early init.
///
/// `DxxIR` values are the effective 16-bit contents of `0x3140`/`0x3142`/
/// `0x3144`/`0x3146`/`0x3148` after coreboot's overlapping stores; the `DxxIP`
/// values are the pin each function asserts.
pub const ICH7_PINEVIEW_ROUTING: PirqRouting = PirqRouting {
    slot_pins: &[
        SlotPins {
            slot: 0x1b,
            functions: [Some(PciPin::A), None, None, None, None, None, None, None],
        },
        SlotPins {
            slot: 0x1c,
            functions: [
                Some(PciPin::A),
                Some(PciPin::B),
                Some(PciPin::C),
                Some(PciPin::D),
                Some(PciPin::A),
                Some(PciPin::B),
                None,
                None,
            ],
        },
        SlotPins {
            // UHCI 1-4 on functions 0-3, EHCI on function 7.
            slot: 0x1d,
            functions: [
                Some(PciPin::A),
                Some(PciPin::B),
                Some(PciPin::C),
                Some(PciPin::D),
                None,
                None,
                None,
                Some(PciPin::A),
            ],
        },
        SlotPins {
            // The bridge itself asserts INTA; its downstream buses have their
            // own `_PRT` in the PCIB scope.
            slot: 0x1e,
            functions: [Some(PciPin::A), None, None, None, None, None, None, None],
        },
        SlotPins {
            // SATA on function 2 and SMBus on function 3 share INTB.
            slot: 0x1f,
            functions: [
                None,
                Some(PciPin::A),
                Some(PciPin::B),
                Some(PciPin::B),
                None,
                Some(PciPin::D),
                None,
                None,
            ],
        },
    ],
    pin_routes: &[
        // The integrated graphics sit outside the routed slots and keep the
        // 1:1 pin/PIRQ relation, which the root bus `_PRT` still publishes.
        PinRoute {
            slot: 0x02,
            pin: PciPin::A,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x02,
            pin: PciPin::B,
            line: Pirq::B,
        },
        // D27IR = 0x0146
        PinRoute {
            slot: 0x1b,
            pin: PciPin::A,
            line: Pirq::G,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::B,
            line: Pirq::E,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::C,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D28IR = 0x3201
        PinRoute {
            slot: 0x1c,
            pin: PciPin::A,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::B,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::C,
            line: Pirq::C,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::D,
            line: Pirq::D,
        },
        // D29IR = 0x0237
        PinRoute {
            slot: 0x1d,
            pin: PciPin::A,
            line: Pirq::H,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::B,
            line: Pirq::D,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::C,
            line: Pirq::C,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D30IR = 0x0146
        PinRoute {
            slot: 0x1e,
            pin: PciPin::A,
            line: Pirq::G,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::B,
            line: Pirq::E,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::C,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D31IR = 0x0132
        PinRoute {
            slot: 0x1f,
            pin: PciPin::A,
            line: Pirq::C,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::B,
            line: Pirq::D,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::C,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::D,
            line: Pirq::A,
        },
    ],
};

/// ICH8/ICH8-M routing as programmed by coreboot's ICH8 early init.
///
/// The mobile parts (X61's ICH8-M) route seven slots instead of five, so the
/// same data drives both the registers and the `_PRT`. Slots the router does
/// not cover (the integrated graphics and the PEG port) keep the 1:1 relation;
/// listing them here is what tells an OS about them at all.
pub const ICH8_ROUTING: PirqRouting = PirqRouting {
    slot_pins: &[
        SlotPins {
            slot: 0x19,
            functions: [Some(PciPin::A), None, None, None, None, None, None, None],
        },
        SlotPins {
            // Function 7 (second EHCI) shares INTD.
            slot: 0x1a,
            functions: [
                Some(PciPin::A),
                Some(PciPin::B),
                None,
                None,
                None,
                None,
                None,
                Some(PciPin::C),
            ],
        },
        SlotPins {
            slot: 0x1b,
            functions: [Some(PciPin::B), None, None, None, None, None, None, None],
        },
        SlotPins {
            slot: 0x1c,
            functions: [
                Some(PciPin::A),
                Some(PciPin::B),
                Some(PciPin::C),
                Some(PciPin::D),
                None,
                None,
                None,
                None,
            ],
        },
        SlotPins {
            slot: 0x1d,
            functions: [
                Some(PciPin::A),
                Some(PciPin::B),
                Some(PciPin::C),
                Some(PciPin::D),
                None,
                None,
                None,
                Some(PciPin::D),
            ],
        },
        SlotPins {
            slot: 0x1e,
            functions: [Some(PciPin::A), None, None, None, None, None, None, None],
        },
        SlotPins {
            slot: 0x1f,
            functions: [
                None,
                Some(PciPin::C),
                Some(PciPin::B),
                Some(PciPin::A),
                None,
                None,
                None,
                None,
            ],
        },
    ],
    pin_routes: &[
        // Slots outside the router: 1:1 pin to PIRQ.
        PinRoute {
            slot: 0x02,
            pin: PciPin::A,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x02,
            pin: PciPin::B,
            line: Pirq::B,
        },
        // D25IR = 0x0001
        PinRoute {
            slot: 0x19,
            pin: PciPin::A,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x19,
            pin: PciPin::B,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x19,
            pin: PciPin::C,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x19,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D26IR = 0x0654
        PinRoute {
            slot: 0x1a,
            pin: PciPin::A,
            line: Pirq::E,
        },
        PinRoute {
            slot: 0x1a,
            pin: PciPin::B,
            line: Pirq::F,
        },
        PinRoute {
            slot: 0x1a,
            pin: PciPin::C,
            line: Pirq::G,
        },
        PinRoute {
            slot: 0x1a,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D27IR = 0x0010
        PinRoute {
            slot: 0x1b,
            pin: PciPin::A,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::B,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::C,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1b,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D28IR = 0x7654
        PinRoute {
            slot: 0x1c,
            pin: PciPin::A,
            line: Pirq::E,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::B,
            line: Pirq::F,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::C,
            line: Pirq::G,
        },
        PinRoute {
            slot: 0x1c,
            pin: PciPin::D,
            line: Pirq::H,
        },
        // D29IR = 0x3210
        PinRoute {
            slot: 0x1d,
            pin: PciPin::A,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::B,
            line: Pirq::B,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::C,
            line: Pirq::C,
        },
        PinRoute {
            slot: 0x1d,
            pin: PciPin::D,
            line: Pirq::D,
        },
        // D30IR = 0x0076
        PinRoute {
            slot: 0x1e,
            pin: PciPin::A,
            line: Pirq::G,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::B,
            line: Pirq::H,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::C,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1e,
            pin: PciPin::D,
            line: Pirq::A,
        },
        // D31IR = 0x1007
        PinRoute {
            slot: 0x1f,
            pin: PciPin::A,
            line: Pirq::H,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::B,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::C,
            line: Pirq::A,
        },
        PinRoute {
            slot: 0x1f,
            pin: PciPin::D,
            line: Pirq::B,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    const ROUTING: PirqRouting = ICH7_PINEVIEW_ROUTING;

    #[test]
    fn gsi_follows_the_programmed_pirq_not_the_pin_index() {
        // D29IR = 0x0237 rotates USB INTA onto PIRQH: the OS must be told 23,
        // not 16. This is the bug a pin-indexed table hides.
        assert_eq!(ROUTING.line(0x1d, PciPin::A), Pirq::H);
        assert_eq!(ROUTING.gsi(0x1d, PciPin::A), 23);
        assert_eq!(ROUTING.gsi(0x1d, PciPin::B), 19);
        assert_eq!(ROUTING.gsi(0x1d, PciPin::C), 18);
        assert_eq!(ROUTING.gsi(0x1d, PciPin::D), 16);
        // D31IR = 0x0132: SATA and SMBus share INTB -> PIRQD.
        assert_eq!(ROUTING.gsi(0x1f, PciPin::B), 19);
        // D27IR = 0x0146: HD Audio INTA -> PIRQG.
        assert_eq!(ROUTING.gsi(0x1b, PciPin::A), 22);
    }

    #[test]
    fn slots_outside_the_router_keep_the_one_to_one_mapping() {
        // The integrated graphics (slot 2) and anything else outside the
        // routed range deliver on the PIRQ matching the pin.
        assert_eq!(ROUTING.line(0x02, PciPin::A), Pirq::A);
        assert_eq!(ROUTING.gsi(0x02, PciPin::A), 16);
        assert_eq!(ROUTING.gsi(0x02, PciPin::B), 17);
        // The reserved slot has no register and must not be routed either.
        assert!(!PirqRouting::slot_is_routed(RESERVED_SLOT));
        assert_eq!(ROUTING.line(RESERVED_SLOT, PciPin::C), Pirq::C);
        assert!(PirqRouting::slot_is_routed(0x1f));
    }

    #[test]
    fn root_bus_routes_match_the_programmed_registers() {
        let routes: alloc::vec::Vec<(u8, PciPin, u8)> = ROUTING.root_bus_routes().collect();
        assert_eq!(
            routes,
            alloc::vec![
                (0x02, PciPin::A, 16),
                (0x02, PciPin::B, 17),
                (0x1b, PciPin::A, 22),
                (0x1b, PciPin::B, 20),
                (0x1b, PciPin::C, 17),
                (0x1b, PciPin::D, 16),
                (0x1c, PciPin::A, 17),
                (0x1c, PciPin::B, 16),
                (0x1c, PciPin::C, 18),
                (0x1c, PciPin::D, 19),
                (0x1d, PciPin::A, 23),
                (0x1d, PciPin::B, 19),
                (0x1d, PciPin::C, 18),
                (0x1d, PciPin::D, 16),
                (0x1e, PciPin::A, 22),
                (0x1e, PciPin::B, 20),
                (0x1e, PciPin::C, 17),
                (0x1e, PciPin::D, 16),
                (0x1f, PciPin::A, 18),
                (0x1f, PciPin::B, 19),
                (0x1f, PciPin::C, 17),
                (0x1f, PciPin::D, 16),
            ]
        );
    }

    #[test]
    fn register_values_encode_the_routes() {
        assert_eq!(ROUTING.ir_value(0x1d), 0x0237);
        assert_eq!(ROUTING.ir_value(0x1f), 0x0132);
        assert_eq!(ROUTING.ir_value(0x1b), 0x0146);
        assert_eq!(ROUTING.ir_value(0x1c), 0x3201);
        assert_eq!(ROUTING.ir_value(0x1e), 0x0146);
        // DxxIP: pin per function, four bits each.
        assert_eq!(ROUTING.ip_value(0x1d), 0x1000_4321);
        assert_eq!(ROUTING.ip_value(0x1f), 0x0040_2210);
    }

    #[test]
    fn ich8_assigned_pins_have_advertised_routes() {
        for (slot, pin) in ICH8_ROUTING.assigned_pins() {
            assert!(
                ICH8_ROUTING.route(slot, pin).is_some(),
                "slot {slot:#04x} pin {pin:?} has no _PRT route"
            );
        }
    }

    #[test]
    fn every_assigned_pin_has_an_advertised_route() {
        for (slot, pin) in ROUTING.assigned_pins() {
            assert!(
                ROUTING.route(slot, pin).is_some(),
                "slot {slot:#04x} pin {pin:?} has no _PRT route"
            );
        }
    }

    #[test]
    fn ich8_register_values_match_the_shared_data() {
        // The ICH8-M router values the driver used to keep privately.
        assert_eq!(ICH8_ROUTING.ir_value(0x19), 0x0001);
        assert_eq!(ICH8_ROUTING.ir_value(0x1a), 0x0654);
        assert_eq!(ICH8_ROUTING.ir_value(0x1b), 0x0010);
        assert_eq!(ICH8_ROUTING.ir_value(0x1c), 0x7654);
        assert_eq!(ICH8_ROUTING.ir_value(0x1d), 0x3210);
        assert_eq!(ICH8_ROUTING.ir_value(0x1e), 0x0076);
        assert_eq!(ICH8_ROUTING.ir_value(0x1f), 0x1007);
        assert_eq!(ICH8_ROUTING.ip_value(0x1d), 0x4000_4321);
        assert_eq!(ICH8_ROUTING.ip_value(0x1f), 0x0000_1230);
        // Every routed pin has a row, and the non-routed slots keep 1:1.
        assert_eq!(ICH8_ROUTING.gsi(0x1c, PciPin::D), 23);
        assert_eq!(ICH8_ROUTING.gsi(0x02, PciPin::A), 16);
        assert_eq!(ICH8_ROUTING.root_bus_routes().count(), 30);
    }
}
