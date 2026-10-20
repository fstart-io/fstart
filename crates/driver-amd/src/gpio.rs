// SPDX-License-Identifier: MIT OR Apache-2.0
//! Banked FCH GPIO. Configuration preserves reserved/drive-strength bits and
//! clears W1C interrupt/wake status deliberately, not via an accidental RMW.
use crate::ACPI_MMIO;
#[cfg(feature = "picasso")]
use crate::picasso::pinmux::gpio_function;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
    registers::ReadWrite,
};

register_bitfields![u32,
    pub Control [
        DEBOUNCE OFFSET(0) NUMBITS(8) [], LEVEL OFFSET(8) NUMBITS(1) [],
        POLARITY OFFSET(9) NUMBITS(2) [High=0, Low=1, Both=2],
        STATUS_ENABLE OFFSET(11) NUMBITS(1) [], DELIVERY_ENABLE OFFSET(12) NUMBITS(1) [],
        WAKE OFFSET(13) NUMBITS(3) [], PIN OFFSET(16) NUMBITS(1) [],
        PULL_UP OFFSET(20) NUMBITS(1) [], PULL_DOWN OFFSET(21) NUMBITS(1) [],
        OUTPUT OFFSET(22) NUMBITS(1) [], OUTPUT_ENABLE OFFSET(23) NUMBITS(1) [],
        INTERRUPT_STATUS OFFSET(28) NUMBITS(1) [], WAKE_STATUS OFFSET(29) NUMBITS(1) []
    ],
    Master [MASK_STATUS OFFSET(28) NUMBITS(1) [], INTERRUPTS OFFSET(30) NUMBITS(1) []]
];
pub const STATUS_MASK: u32 = (1 << 28) | (1 << 29);
pub const CONFIG_MASK: u32 =
    0xff | (7 << 8) | (3 << 11) | (7 << 13) | (3 << 20) | (3 << 22) | STATUS_MASK;

#[derive(Debug, Clone, Copy)]
pub enum Pull {
    None,
    Up,
    Down,
}
impl Pull {
    const fn bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Up => 1 << 20,
            Self::Down => 1 << 21,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    EdgeHigh,
    EdgeLow,
    LevelHigh,
    LevelLow,
    BothEdges,
}
impl Trigger {
    #[cfg(feature = "picasso")]
    const fn bits(self) -> u32 {
        match self {
            Self::EdgeHigh => 0,
            Self::EdgeLow => 1 << 9,
            Self::LevelHigh => 1 << 8,
            Self::LevelLow => (1 << 8) | (1 << 9),
            Self::BothEdges => 2 << 9,
        }
    }
    const fn high(self) -> bool {
        matches!(self, Self::EdgeHigh | Self::LevelHigh)
    }
    const fn level(self) -> bool {
        matches!(self, Self::LevelHigh | Self::LevelLow)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Delivery {
    Status,
    StatusAndDelivery,
}
#[derive(Debug, Clone, Copy)]
pub enum Event {
    None,
    Sci(Trigger),
    Smi(Trigger),
}
#[derive(Debug, Clone, Copy)]
pub struct Pad {
    pub pin: u8,
    pub function: u8,
    pub control: u32,
    pub event: Event,
}
impl Pad {
    /// IOMUX `function` driven as an output: the form of the vendor CPM
    /// tables, which name functions by number.
    pub const fn driven(pin: u8, function: u8, high: bool, pull: Pull) -> Self {
        Self {
            control: pull.bits() | (1 << 23) | if high { 1 << 22 } else { 0 },
            ..Self::native(pin, function, pull)
        }
    }
    pub const fn native(pin: u8, function: u8, pull: Pull) -> Self {
        assert!(function < 4);
        Self {
            pin,
            function,
            control: pull.bits(),
            event: Event::None,
        }
    }
    #[cfg(feature = "picasso")]
    pub const fn input(pin: u8, pull: Pull) -> Self {
        Self::native(pin, gpio_function(pin), pull)
    }
    #[cfg(feature = "picasso")]
    pub const fn output(pin: u8, high: bool) -> Self {
        Self {
            control: (1 << 23) | if high { 1 << 22 } else { 0 },
            ..Self::input(pin, Pull::None)
        }
    }
    #[cfg(feature = "picasso")]
    pub const fn nc(pin: u8) -> Self {
        Self::input(pin, Pull::Down)
    }
    #[cfg(feature = "picasso")]
    pub const fn interrupt(pin: u8, pull: Pull, trigger: Trigger, delivery: Delivery) -> Self {
        Self {
            control: pull.bits()
                | trigger.bits()
                | (1 << 11)
                | if matches!(delivery, Delivery::StatusAndDelivery) {
                    1 << 12
                } else {
                    0
                },
            ..Self::input(pin, pull)
        }
    }
    #[cfg(feature = "picasso")]
    pub const fn sci(pin: u8, pull: Pull, trigger: Trigger) -> Self {
        // SCI trigger/polarity is in the SMI bank, not the pad's trigger bits.
        Self {
            control: pull.bits() | (1 << 8),
            event: Event::Sci(trigger),
            ..Self::input(pin, pull)
        }
    }
    pub const fn native_sci(pin: u8, function: u8, pull: Pull, trigger: Trigger) -> Self {
        Self {
            event: Event::Sci(trigger),
            ..Self::native(pin, function, pull)
        }
    }
    #[cfg(feature = "picasso")]
    pub const fn wake_s3(pin: u8, pull: Pull, trigger: Trigger) -> Self {
        Self {
            control: pull.bits() | trigger.bits() | (1 << 14),
            ..Self::input(pin, pull)
        }
    }
}

/// The pads of a SoC's GPIO bank 0 and the GEVENTs they can raise.
#[derive(Debug)]
pub struct Bank {
    /// Pins `0..pins` exist, except `reserved`.
    pub pins: u8,
    pub reserved: &'static [u8],
    /// (pin, GEVENT)
    pub events: &'static [(u8, u8)],
}

/// Narrow access seam for sequencing tests; firmware uses tock register access.
pub trait Access {
    fn read_control(&mut self, pin: u8) -> u32;
    fn write_control(&mut self, pin: u8, value: u32);
    fn mux(&mut self, pin: u8, function: u8);
    fn master(&mut self, clear: u32, set: u32);
    fn route(&mut self, event: u8, config: Event);
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPad,
    MissingEvent,
    EdgeSmi,
    BothEdgesSci,
    DuplicatePad,
}

/// Validate the entire table before touching hardware. Overrides replace base
/// entries only (never append).
pub fn configure(
    io: &mut impl Access,
    bank: &Bank,
    base: &[Pad],
    overrides: &[Pad],
) -> Result<(), Error> {
    for table in [base, overrides] {
        for (index, pad) in table.iter().enumerate() {
            if pad.pin >= bank.pins
                || bank.reserved.contains(&pad.pin)
                || pad.function > 3
                || pad.control & !CONFIG_MASK != 0
            {
                return Err(Error::InvalidPad);
            }
            if table[..index].iter().any(|p| p.pin == pad.pin) {
                return Err(Error::DuplicatePad);
            }
            if !matches!(pad.event, Event::None) && !bank.events.iter().any(|(p, _)| *p == pad.pin)
            {
                return Err(Error::MissingEvent);
            }
            if matches!(pad.event, Event::Sci(Trigger::BothEdges)) {
                return Err(Error::BothEdgesSci);
            }
            if matches!(pad.event, Event::Smi(t) if !t.level()) {
                return Err(Error::EdgeSmi);
            }
        }
    }
    if base.is_empty() {
        return Ok(());
    }
    io.master((1 << 28) | (1 << 30), 0);
    for pad in base
        .iter()
        .map(|pad| overrides.iter().find(|p| p.pin == pad.pin).unwrap_or(pad))
    {
        io.mux(pad.pin, pad.function);
        let value = (io.read_control(pad.pin) & !CONFIG_MASK) | (pad.control & CONFIG_MASK);
        io.write_control(pad.pin, value);
        // Clear the W1C status bits in a separate write.
        let value = io.read_control(pad.pin) | STATUS_MASK;
        io.write_control(pad.pin, value);
        if let Some((_, event)) = bank.events.iter().find(|(p, _)| *p == pad.pin)
            && !matches!(pad.event, Event::None)
        {
            io.route(*event, pad.event);
        }
    }
    // Do not restore MASK_STATUS: kernel debounce changes otherwise lose IRQs.
    io.master(0, 1 << 30);
    Ok(())
}

pub struct Mmio {
    base: usize,
}
impl Mmio {
    /// # Safety
    /// ACPIMMIO must be decoded and UC-mapped. Caller serializes all accesses
    /// to the GPIO/IOMUX/SMI banks (including SMM and other processors).
    pub unsafe fn new() -> Self {
        Self { base: ACPI_MMIO }
    }
    fn reg32<R: tock_registers::RegisterLongName>(&self, offset: usize) -> &ReadWrite<u32, R> {
        // SAFETY: constructor contract and all offsets below are aligned registers.
        unsafe { &*((self.base + offset) as *const ReadWrite<u32, R>) }
    }
    pub fn get(&mut self, pin: u8) -> bool {
        #[cfg(feature = "picasso")]
        let _ = gpio_function(pin);
        self.reg32::<Control::Register>(0x1500 + usize::from(pin) * 4)
            .is_set(Control::PIN)
    }
    pub fn set(&mut self, pin: u8, high: bool) {
        #[cfg(feature = "picasso")]
        let _ = gpio_function(pin);
        let register = self.reg32::<Control::Register>(0x1500 + usize::from(pin) * 4);
        let value = register.get() & !(STATUS_MASK | (1 << 22));
        register.set(value | if high { 1 << 22 } else { 0 });
    }
    fn bit(&mut self, offset: usize, bit: u8, value: bool) {
        let register = self.reg32::<()>(offset);
        register.set((register.get() & !(1 << bit)) | (u32::from(value) << bit));
    }
}
impl Access for Mmio {
    fn read_control(&mut self, pin: u8) -> u32 {
        self.reg32::<Control::Register>(0x1500 + usize::from(pin) * 4)
            .get()
    }
    fn write_control(&mut self, pin: u8, value: u32) {
        self.reg32::<Control::Register>(0x1500 + usize::from(pin) * 4)
            .set(value);
    }
    fn mux(&mut self, pin: u8, function: u8) {
        let register =
            unsafe { &*((self.base + 0xd00 + usize::from(pin)) as *const ReadWrite<u8>) };
        register.set(function & 3);
        let _ = register.get(); // flush posted write
    }
    fn master(&mut self, clear: u32, set: u32) {
        let register = self.reg32::<Master::Register>(0x15fc);
        register.set((register.get() & !clear) | set);
    }
    fn route(&mut self, event: u8, config: Event) {
        match config {
            Event::None => {}
            Event::Sci(trigger) => {
                let map =
                    unsafe { &*((self.base + 0x240 + usize::from(event)) as *const ReadWrite<u8>) };
                map.set(event);
                self.bit(0x208, event, trigger.high());
                self.bit(0x20c, event, trigger.level());
            }
            Event::Smi(trigger) => {
                let register = self.reg32::<()>(0x2a0 + usize::from(event / 16) * 4);
                let shift = (event % 16) * 2;
                register.set((register.get() & !(3 << shift)) | (1 << shift));
                self.bit(0x298, event, trigger.high());
            }
        }
    }
}
