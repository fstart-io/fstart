//! Lenovo PMH7 deep-sleep hub (IO base 0x15e0).
//!
//! Ported from coreboot `ec/lenovo/pmh7`. The hub exposes an indexed
//! register file: address low/high at base+0x0c/0x0d, data at base+0x0e.
//! It gates the display backlight, dock-event SMI source, touchpad and
//! trackpoint power, ultrabay power, and (on boards that have one) the
//! discrete GPU.

use fstart_core::pio::{inb, outb};
use fstart_log::{Hex, info};
use tock_registers::fields::FieldValue;
use tock_registers::{LocalRegisterCopy, RegisterLongName, register_bitfields};

register_bitfields![u8,
    Display [BACKLIGHT_ENABLE OFFSET(5) NUMBITS(1) []],
    InputPower [
        TRACKPOINT_OFF OFFSET(0) NUMBITS(1) [],
        TOUCHPAD_OFF OFFSET(2) NUMBITS(1) []
    ],
    DockEvents [SMI_ENABLE OFFSET(3) NUMBITS(1) []],
    UltrabayPower [POWER_OFF OFFSET(0) NUMBITS(1) []]
];

/// Bind each field namespace to its indexed PMH7 register.
trait Pmh7Register: RegisterLongName {
    const INDEX: u8;
}

impl Pmh7Register for Display::Register {
    const INDEX: u8 = 0x50;
}
impl Pmh7Register for InputPower::Register {
    const INDEX: u8 = 0x51;
}
impl Pmh7Register for DockEvents::Register {
    const INDEX: u8 = 0x60;
}
impl Pmh7Register for UltrabayPower::Register {
    const INDEX: u8 = 0x62;
}

const PMH7_ADDR_L: u16 = 0x0c;
const PMH7_ADDR_H: u16 = 0x0d;
const PMH7_DATA: u16 = 0x0e;

const REG_ID: u8 = 0xc2;
const REG_REV: u8 = 0xc3;

/// An initialized PMH7 hub handle.
#[derive(Debug, Clone, Copy)]
pub struct Pmh7 {
    base: u16,
}

impl Pmh7 {
    /// Create a hub handle for the given IO base (`0x15e0` on ThinkPads).
    #[must_use]
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    fn read_register(&self, reg: u8) -> u8 {
        // SAFETY: the PMH7 IO window is decoded by board LPC config.
        unsafe {
            outb(self.base + PMH7_ADDR_L, reg);
            outb(self.base + PMH7_ADDR_H, 0);
            inb(self.base + PMH7_DATA)
        }
    }

    fn write_register(&self, reg: u8, val: u8) {
        // SAFETY: see `read_register`.
        unsafe {
            outb(self.base + PMH7_ADDR_L, reg);
            outb(self.base + PMH7_ADDR_H, 0);
            outb(self.base + PMH7_DATA, val);
        }
    }

    fn modify<R: Pmh7Register>(&self, fields: FieldValue<u8, R>) {
        let mut value = LocalRegisterCopy::<u8, R>::new(self.read_register(R::INDEX));
        value.modify(fields);
        self.write_register(R::INDEX, value.get());
    }

    /// Probe the hub by reading its ID register; returns `(id, revision)`.
    pub fn probe(&self) -> Option<(u8, u8)> {
        let id = self.read_register(REG_ID);
        if id == 0 || id == 0xff {
            return None;
        }
        Some((id, self.read_register(REG_REV)))
    }

    pub fn backlight_enable(&self, on: bool) {
        self.modify(Display::BACKLIGHT_ENABLE.val(u8::from(on)));
    }

    pub fn dock_event_enable(&self, on: bool) {
        self.modify(DockEvents::SMI_ENABLE.val(u8::from(on)));
    }

    /// Input power controls are active-low (clear = powered).
    pub fn touchpad_enable(&self, on: bool) {
        self.modify(InputPower::TOUCHPAD_OFF.val(u8::from(!on)));
    }

    pub fn trackpoint_enable(&self, on: bool) {
        self.modify(InputPower::TRACKPOINT_OFF.val(u8::from(!on)));
    }

    pub fn ultrabay_power_enable(&self, on: bool) {
        self.modify(UltrabayPower::POWER_OFF.val(u8::from(!on)));
    }

    pub fn log_identity(&self) {
        match self.probe() {
            Some((id, rev)) => info!(
                "lenovo-pmh7: id={} revision={} at base {}",
                Hex(u64::from(id)),
                Hex(u64::from(rev)),
                Hex(u64::from(self.base))
            ),
            None => info!(
                "lenovo-pmh7: no device at base {}",
                Hex(u64::from(self.base))
            ),
        }
    }
}
