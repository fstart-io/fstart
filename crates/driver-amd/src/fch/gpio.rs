// SPDX-License-Identifier: MIT OR Apache-2.0
//! Banked FCH GPIO. Configuration
//! preserves reserved/drive-strength bits and clears W1C interrupt/wake
//! status deliberately, not via an accidental RMW.
use crate::fch::{
    AcpiReg, Bus, BusRegister, Fch, SMI_TRIGGER0, SmiMode, gpio_bank, iomux, smi_bank, smi_route,
};
use tock_registers::{
    LocalRegisterCopy,
    fields::{Field, FieldValue},
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    pub Control [
        DEBOUNCE OFFSET(0) NUMBITS(8) [],
        LEVEL OFFSET(8) NUMBITS(1) [Edge = 0, Level = 1],
        POLARITY OFFSET(9) NUMBITS(2) [High = 0, Low = 1, Both = 2],
        STATUS_ENABLE OFFSET(11) NUMBITS(1) [],
        DELIVERY_ENABLE OFFSET(12) NUMBITS(1) [],
        WAKE_S0I3 OFFSET(13) NUMBITS(1) [],
        WAKE_S3 OFFSET(14) NUMBITS(1) [],
        WAKE_S4_S5 OFFSET(15) NUMBITS(1) [],
        PIN OFFSET(16) NUMBITS(1) [],
        DRIVE_STRENGTH OFFSET(17) NUMBITS(2) [],
        PULL_UP OFFSET(20) NUMBITS(1) [],
        PULL_DOWN OFFSET(21) NUMBITS(1) [],
        OUTPUT OFFSET(22) NUMBITS(1) [],
        OUTPUT_ENABLE OFFSET(23) NUMBITS(1) [],
        INTERRUPT_STATUS OFFSET(28) NUMBITS(1) [],
        WAKE_STATUS OFFSET(29) NUMBITS(1) []
    ],
    pub Master [MASK_STATUS OFFSET(28) NUMBITS(1) [], INTERRUPTS OFFSET(30) NUMBITS(1) []],
    /// SMIx08 and SMIx0C: SCI trigger polarity and level, bit per GEVENT.
    pub SciTrigger [GEVENTS OFFSET(0) NUMBITS(24) []],
    pub SciLevel [GEVENTS OFFSET(0) NUMBITS(24) []]
];
/// The W1C status bits of a pad.
pub const STATUS_MASK: u32 = (1 << 28) | (1 << 29);
/// The pad configuration a [`Pad`] sets: everything but the pin state, the
/// drive strength and the reserved bits.
pub const CONFIG_MASK: u32 =
    0xff | (7 << 8) | (3 << 11) | (7 << 13) | (3 << 20) | (3 << 22) | STATUS_MASK;

const fn control(pin: u8) -> AcpiReg<u32, Control::Register> {
    gpio_bank::at(4 * pin as u32)
}
const MASTER: AcpiReg<u32, Master::Register> = gpio_bank::at(0xfc);
const SCI_TRIGGER: AcpiReg<u32, SciTrigger::Register> = smi_bank::at(0x08);
const SCI_LEVEL: AcpiReg<u32, SciLevel::Register> = smi_bank::at(0x0c);
/// SMIx40 + n: the SCI a GEVENT raises.
const fn sci_map(event: u8) -> AcpiReg<u8> {
    smi_bank::at(0x40 + event as u32)
}

/// The bit of GEVENT `event` in a per-GEVENT register.
fn gevent_bit<R: tock_registers::RegisterLongName>(event: u8, set: bool) -> FieldValue<u32, R> {
    Field::<u32, R>::new(1, event.into()).val(set.into())
}

/// Field values combined in const context, where `+` isn't available.
const fn fields(values: &[FieldValue<u32, Control::Register>]) -> u32 {
    let mut value = 0;
    let mut i = 0;
    while i < values.len() {
        value |= values[i].value;
        i += 1;
    }
    value
}

#[derive(Debug, Clone, Copy)]
pub enum Pull {
    None,
    Up,
    Down,
}
impl Pull {
    const fn field(self) -> FieldValue<u32, Control::Register> {
        match self {
            Self::None => Control::PULL_UP::CLEAR,
            Self::Up => Control::PULL_UP::SET,
            Self::Down => Control::PULL_DOWN::SET,
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
    const fn fields(self) -> u32 {
        use Control::{LEVEL, POLARITY};
        match self {
            Self::EdgeHigh => fields(&[LEVEL::Edge, POLARITY::High]),
            Self::EdgeLow => fields(&[LEVEL::Edge, POLARITY::Low]),
            Self::LevelHigh => fields(&[LEVEL::Level, POLARITY::High]),
            Self::LevelLow => fields(&[LEVEL::Level, POLARITY::Low]),
            Self::BothEdges => fields(&[LEVEL::Edge, POLARITY::Both]),
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
/// The IOMUX function of a pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    /// The bank's GPIO function of the pin.
    Gpio,
    /// An IOMUX function by number, as the vendor tables name them.
    Native(u8),
}
#[derive(Debug, Clone, Copy)]
pub struct Pad {
    pub pin: u8,
    pub function: Function,
    pub control: LocalRegisterCopy<u32, Control::Register>,
    pub event: Event,
}
impl Pad {
    const fn with(pin: u8, function: Function, control: u32, event: Event) -> Self {
        Self {
            pin,
            function,
            control: LocalRegisterCopy::new(control),
            event,
        }
    }
    const fn output_fields(high: bool) -> u32 {
        fields(&[
            Control::OUTPUT_ENABLE::SET,
            if high {
                Control::OUTPUT::SET
            } else {
                Control::OUTPUT::CLEAR
            },
        ])
    }
    /// IOMUX `function` driven as an output: the form of the vendor CPM
    /// tables, which name functions by number.
    pub const fn driven(pin: u8, function: u8, high: bool, pull: Pull) -> Self {
        let control = fields(&[pull.field()]) | Self::output_fields(high);
        Self::with(pin, Function::Native(function), control, Event::None)
    }
    pub const fn native(pin: u8, function: u8, pull: Pull) -> Self {
        assert!(function < 4);
        Self::with(pin, Function::Native(function), fields(&[pull.field()]), Event::None)
    }
    /// `native` with the pad configuration `control` (debounce and the like).
    pub const fn native_with(pin: u8, function: u8, control: FieldValue<u32, Control::Register>) -> Self {
        Self::with(pin, Function::Native(function), control.value, Event::None)
    }
    pub const fn input(pin: u8, pull: Pull) -> Self {
        Self::with(pin, Function::Gpio, fields(&[pull.field()]), Event::None)
    }
    pub const fn output(pin: u8, high: bool) -> Self {
        Self::with(pin, Function::Gpio, Self::output_fields(high), Event::None)
    }
    pub const fn nc(pin: u8) -> Self {
        Self::input(pin, Pull::Down)
    }
    pub const fn interrupt(pin: u8, pull: Pull, trigger: Trigger, delivery: Delivery) -> Self {
        let delivery = match delivery {
            Delivery::Status => Control::DELIVERY_ENABLE::CLEAR,
            Delivery::StatusAndDelivery => Control::DELIVERY_ENABLE::SET,
        };
        let control = fields(&[pull.field(), Control::STATUS_ENABLE::SET, delivery]) | trigger.fields();
        Self::with(pin, Function::Gpio, control, Event::None)
    }
    pub const fn sci(pin: u8, pull: Pull, trigger: Trigger) -> Self {
        // SCI trigger/polarity is in the SMI bank, not the pad's trigger bits.
        let control = fields(&[pull.field(), Control::LEVEL::Level]);
        Self::with(pin, Function::Gpio, control, Event::Sci(trigger))
    }
    pub const fn native_sci(pin: u8, function: u8, pull: Pull, trigger: Trigger) -> Self {
        Self {
            event: Event::Sci(trigger),
            ..Self::native(pin, function, pull)
        }
    }
    pub const fn wake_s3(pin: u8, pull: Pull, trigger: Trigger) -> Self {
        let control = fields(&[pull.field(), Control::WAKE_S3::SET]) | trigger.fields();
        Self::with(pin, Function::Gpio, control, Event::None)
    }
}

/// The GEVENTs of the Family 17h FCHs (Picasso and Zeppelin): (pin,
/// GEVENT).
pub const FAMILY17H_EVENTS: &[(u8, u8)] = &[
    (0, 21),
    (1, 19),
    (2, 8),
    (3, 2),
    (4, 4),
    (5, 7),
    (6, 10),
    (7, 11),
    (8, 23),
    (9, 22),
    (16, 12),
    (17, 13),
    (18, 14),
    (21, 5),
    (22, 3),
    (23, 16),
    (24, 15),
    (40, 20),
    (84, 18),
    (86, 9),
    (89, 0),
    (90, 1),
    (91, 6),
    (129, 17),
];

/// The pads of a SoC's GPIO bank 0, the IOMUX function that makes each a
/// GPIO and the GEVENTs they can raise.
#[derive(Debug)]
pub struct Bank {
    /// Pins `0..pins` exist, except `reserved`.
    pub pins: u8,
    pub reserved: &'static [u8],
    /// The GPIO function of a pin, `None` if it has none or it isn't known.
    pub gpio_function: fn(u8) -> Option<u8>,
    /// (pin, GEVENT)
    pub events: &'static [(u8, u8)],
}
impl Bank {
    fn exists(&self, pin: u8) -> bool {
        pin < self.pins && !self.reserved.contains(&pin)
    }
    fn function(&self, pad: &Pad) -> Option<u8> {
        match pad.function {
            Function::Gpio => (self.gpio_function)(pad.pin),
            Function::Native(function) => Some(function),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPad,
    /// The pad wants its GPIO function, which the bank doesn't know.
    NoGpioFunction,
    MissingEvent,
    EdgeSmi,
    BothEdgesSci,
    DuplicatePad,
}

/// A GPIO bank of one FCH.
pub struct Gpio<B> {
    fch: Fch<B>,
    bank: &'static Bank,
}
impl<B: Bus> Gpio<B> {
    /// The caller serializes all accesses to the GPIO, IOMUX and SMI banks
    /// of `fch` (including SMM and other processors).
    pub const fn new(fch: Fch<B>, bank: &'static Bank) -> Self {
        Self { fch, bank }
    }

    fn validate(&self, table: &[Pad]) -> Result<(), Error> {
        for (index, pad) in table.iter().enumerate() {
            if !self.bank.exists(pad.pin)
                || matches!(pad.function, Function::Native(function) if function > 3)
                || pad.control.get() & !CONFIG_MASK != 0
            {
                return Err(Error::InvalidPad);
            }
            if self.bank.function(pad).is_none() {
                return Err(Error::NoGpioFunction);
            }
            if table[..index].iter().any(|p| p.pin == pad.pin) {
                return Err(Error::DuplicatePad);
            }
            if !matches!(pad.event, Event::None)
                && !self.bank.events.iter().any(|(p, _)| *p == pad.pin)
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
        Ok(())
    }

    /// Validate the entire table before touching hardware. Overrides replace
    /// base entries only (never append).
    pub fn configure(&self, base: &[Pad], overrides: &[Pad]) -> Result<(), Error> {
        self.validate(base)?;
        self.validate(overrides)?;
        if base.is_empty() {
            return Ok(());
        }
        let master = self.fch.reg(MASTER);
        master.modify(Master::MASK_STATUS::CLEAR + Master::INTERRUPTS::CLEAR);
        for pad in base
            .iter()
            .map(|pad| overrides.iter().find(|p| p.pin == pad.pin).unwrap_or(pad))
        {
            let mux = self.fch.reg(iomux::at::<u8, ()>(pad.pin.into()));
            mux.set(self.bank.function(pad).unwrap_or(0) & 3);
            mux.get(); // flush posted write
            let control = self.fch.reg(control(pad.pin));
            control.set((control.get() & !CONFIG_MASK) | pad.control.get());
            // Clear the W1C status bits in a separate write.
            control.modify(Control::INTERRUPT_STATUS::SET + Control::WAKE_STATUS::SET);
            if let Some((_, event)) = self.bank.events.iter().find(|(p, _)| *p == pad.pin) {
                self.route(*event, pad.event);
            }
        }
        // Do not restore MASK_STATUS: kernel debounce changes otherwise lose IRQs.
        master.modify(Master::INTERRUPTS::SET);
        Ok(())
    }

    fn route(&self, event: u8, config: Event) {
        match config {
            Event::None => {}
            Event::Sci(trigger) => {
                self.fch.reg(sci_map(event)).set(event);
                self.fch
                    .reg(SCI_TRIGGER)
                    .modify(gevent_bit(event, trigger.high()));
                self.fch
                    .reg(SCI_LEVEL)
                    .modify(gevent_bit(event, trigger.level()));
            }
            Event::Smi(trigger) => {
                smi_route(&self.fch, event, SmiMode::Smi);
                self.fch
                    .reg(SMI_TRIGGER0)
                    .modify(gevent_bit(event, trigger.high()));
            }
        }
    }

    /// Select IOMUX `function` for `pin`, nothing else.
    pub fn set_function(&self, pin: u8, function: u8) -> Result<(), Error> {
        if !self.bank.exists(pin) || function > 3 {
            return Err(Error::InvalidPad);
        }
        self.fch.reg(iomux::at::<u8, ()>(pin.into())).set(function);
        Ok(())
    }
    /// The control register of `pin`, for vendor sequences that program it
    /// field by field.
    pub fn control(&self, pin: u8) -> Result<BusRegister<'_, B, u32, Control::Register>, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(self.fch.reg(control(pin)))
    }
    /// The level of `pin`.
    pub fn get(&self, pin: u8) -> Result<bool, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(self.fch.reg(control(pin)).is_set(Control::PIN))
    }
    /// Drive output `pin`; pending status stays.
    pub fn set(&self, pin: u8, high: bool) -> Result<(), Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        self.fch.reg(control(pin)).modify(
            Control::OUTPUT.val(high.into())
                + Control::INTERRUPT_STATUS::CLEAR
                + Control::WAKE_STATUS::CLEAR,
        );
        Ok(())
    }
}
