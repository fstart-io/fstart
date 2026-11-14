// SPDX-License-Identifier: MIT OR Apache-2.0
//! Banked FCH GPIO. Configuration
//! preserves reserved/drive-strength bits and clears W1C interrupt/wake
//! status deliberately, not via an accidental RMW.
use crate::fch::acpimmio::{gpio_bank, iomux};
use crate::fch::smi::{self, Route, SCI_LEVEL, SCI_TRIGGER, TRIGGER0, sci_map};
use crate::window::{At, Reg, Window};
use tock_registers::{
    fields::{Field, FieldValue},
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    pub Control [
        DEBOUNCE OFFSET(0) NUMBITS(8) [],
        LEVEL OFFSET(8) NUMBITS(1) [
            Edge = 0,
            Level = 1
        ],
        POLARITY OFFSET(9) NUMBITS(2) [
            High = 0,
            Low = 1,
            Both = 2
        ],
        STATUS_ENABLE OFFSET(11) NUMBITS(1) [],
        DELIVERY_ENABLE OFFSET(12) NUMBITS(1) [],
        WAKE_S0I3 OFFSET(13) NUMBITS(1) [],
        WAKE_S3 OFFSET(14) NUMBITS(1) [],
        WAKE_S4_S5 OFFSET(15) NUMBITS(1) [],
        PIN OFFSET(16) NUMBITS(1) [],
        DRIVE_STRENGTH OFFSET(17) NUMBITS(2) [],
        /// Reserved in public PPR 55570; CPM setting bit 2 controls it
        /// when the pull field is enabled (RXM100CB `ffd79a92`).
        CPM_BIT19 OFFSET(19) NUMBITS(1) [],
        PULL_UP OFFSET(20) NUMBITS(1) [],
        PULL_DOWN OFFSET(21) NUMBITS(1) [],
        OUTPUT OFFSET(22) NUMBITS(1) [],
        OUTPUT_ENABLE OFFSET(23) NUMBITS(1) [],
        INTERRUPT_STATUS OFFSET(28) NUMBITS(1) [],
        WAKE_STATUS OFFSET(29) NUMBITS(1) []
    ],
    pub Master [
        MASK_STATUS OFFSET(28) NUMBITS(1) [],
        INTERRUPTS OFFSET(30) NUMBITS(1) []
    ]
];
/// The W1C status bits of a pad.
pub const STATUS_MASK: u32 = (1 << 28) | (1 << 29);

/// First pin of the remote bank (Huashan's pins 256 and up).
pub const REMOTE_FIRST: u16 = 256;
/// Pins in the remote bank (`0x1200..0x12C0` holds 48 control dwords).
pub const REMOTE_PINS: u16 = 48;

/// An FCH GPIO pin: bank 0 (0-255) or, on Huashan, the remote bank
/// (256-303): control at `0x1200 + 4(n - 256)`, IOMUX at
/// `0x12C0 + (n - 256)` (FCH_SP3 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pin(pub u16);
impl Pin {
    /// Offset of the control dword in the ACPI-MMIO block.
    pub const fn control(self) -> u32 {
        match self.0 {
            n if n < REMOTE_FIRST => gpio_bank::BASE + 4 * n as u32,
            n => REMOTE_GPIO + 4 * (n - REMOTE_FIRST) as u32,
        }
    }
    /// Offset of the IOMUX byte in the ACPI-MMIO block.
    pub const fn iomux(self) -> u32 {
        match self.0 {
            n if n < REMOTE_FIRST => iomux::BASE + n as u32,
            n => REMOTE_GPIO + 0xC0 + (n - REMOTE_FIRST) as u32,
        }
    }
    pub const fn is_valid(self) -> bool {
        self.0 < REMOTE_FIRST + REMOTE_PINS
    }
    /// The pin of bank 0, if it is one.
    const fn local(self) -> Option<u8> {
        if self.0 < REMOTE_FIRST {
            Some(self.0 as u8)
        } else {
            None
        }
    }
}

/// The remote GPIO bank of the ACPI-MMIO block (Huashan).
const REMOTE_GPIO: u32 = 0x1200;
/// The pad configuration a [`Pad`] sets: everything but the pin state, the
/// drive strength and the reserved bits.
pub const CONFIG_MASK: u32 =
    0xff | (7 << 8) | (3 << 11) | (7 << 13) | (3 << 20) | (3 << 22) | STATUS_MASK;

const fn control(pin: u8) -> At<u32, Control::Register> {
    gpio_bank::at(4 * pin as u32)
}
const MASTER: At<u32, Master::Register> = gpio_bank::at(0xfc);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub pin: Pin,
    /// The IOMUX function, `None` to leave it.
    pub function: Option<Function>,
    /// The [`Control`] bits the pad sets, within [`Self::mask`].
    pub control: u32,
    /// The control bits the pad replaces; the others stay.
    pub mask: u32,
    pub event: Event,
}
impl Pad {
    /// A pad whose configuration replaces the whole control register but
    /// its status bits ([`CONFIG_MASK`]).
    const fn with(pin: u8, function: Function, control: u32, event: Event) -> Self {
        Self {
            pin: Pin(pin as u16),
            function: Some(function),
            control,
            mask: CONFIG_MASK,
            event,
        }
    }

    /// Only IOMUX `function` for `pin`; the control register stays.
    pub const fn mux(pin: Pin, function: u8) -> Self {
        Self {
            pin,
            function: Some(Function::Native(function)),
            control: 0,
            mask: 0,
            event: Event::None,
        }
    }

    /// Also set the pull to `pull`.
    pub const fn pull(self, pull: Pull) -> Self {
        let bits = Control::PULL_UP::SET.value | Control::PULL_DOWN::SET.value;
        let value = match pull {
            Pull::None => 0,
            Pull::Up => Control::PULL_UP::SET.value,
            Pull::Down => Control::PULL_DOWN::SET.value,
        };
        self.set(bits, value)
    }

    /// Also make the pad an input.
    pub const fn as_input(self) -> Self {
        self.set(Control::OUTPUT_ENABLE::SET.value, 0)
    }

    /// Also drive the pad `high` or low.
    pub const fn driving(self, high: bool) -> Self {
        let bits = Control::OUTPUT_ENABLE::SET.value | Control::OUTPUT::SET.value;
        self.set(bits, Self::output_fields(high))
    }

    /// Replace the control `bits` with `value`.
    const fn set(self, bits: u32, value: u32) -> Self {
        Self {
            control: (self.control & !bits) | (value & bits),
            mask: self.mask | bits,
            ..self
        }
    }

    /// Decode an AMD CPM `$A10` setting word for `pin` (BOARD_S8030 §3.4):
    /// IOMUX = bits 10:8 when bit 11 is set; bit 7 enables the pull field
    /// (bit 5 up, bit 6 down, bit 2 control bit 19); with bit 3 clear, bit
    /// 1 selects input, else output = bit 0; bit 3 set keeps the direction.
    pub const fn from_cpm(pin: Pin, setting: u16) -> Self {
        let mut pad = Self {
            function: None,
            ..Self::mux(pin, 0)
        };
        if setting & 0x800 != 0 {
            pad.function = Some(Function::Native(((setting >> 8) & 7) as u8));
        }
        if setting & 0x80 != 0 {
            pad = pad.pull(if setting & 0x20 != 0 {
                Pull::Up
            } else if setting & 0x40 != 0 {
                Pull::Down
            } else {
                Pull::None
            });
            pad = pad.set(
                Control::CPM_BIT19::SET.value,
                if setting & 4 != 0 {
                    Control::CPM_BIT19::SET.value
                } else {
                    0
                },
            );
        }
        if setting & 0x8 == 0 {
            pad = if setting & 0x2 != 0 {
                pad.as_input()
            } else {
                pad.driving(setting & 1 != 0)
            };
        }
        pad
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
        Self::with(
            pin,
            Function::Native(function),
            fields(&[pull.field()]),
            Event::None,
        )
    }
    /// `native` with the pad configuration `control` (debounce and the like).
    pub const fn native_with(
        pin: u8,
        function: u8,
        control: FieldValue<u32, Control::Register>,
    ) -> Self {
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
        let control =
            fields(&[pull.field(), Control::STATUS_ENABLE::SET, delivery]) | trigger.fields();
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
    fn has(&self, pin: Pin) -> bool {
        pin.local().is_some_and(|pin| self.exists(pin))
    }
    /// The IOMUX value of `pad`: `Some(None)` leaves the IOMUX, `None` if
    /// the bank doesn't know the pad's GPIO function.
    fn function(&self, pad: &Pad) -> Option<Option<u8>> {
        match pad.function {
            None => Some(None),
            Some(Function::Gpio) => pad.pin.local().and_then(self.gpio_function).map(Some),
            Some(Function::Native(function)) => Some(Some(function)),
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

/// A GPIO bank of one FCH, in its ACPI-MMIO block `acpi`.
pub struct Gpio<W> {
    acpi: W,
    bank: &'static Bank,
}
impl<W: Window> Gpio<W> {
    /// The caller serializes all accesses to the GPIO, IOMUX and SMI banks
    /// of `acpi` (including SMM and other processors).
    pub const fn new(acpi: W, bank: &'static Bank) -> Self {
        Self { acpi, bank }
    }

    fn validate(&self, table: &[Pad]) -> Result<(), Error> {
        for (index, pad) in table.iter().enumerate() {
            if !self.bank.has(pad.pin)
                || matches!(pad.function, Some(Function::Native(function)) if function > 3)
                || pad.control & !pad.mask != 0
                || pad.mask & !CONFIG_MASK != 0
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
                && !self
                    .bank
                    .events
                    .iter()
                    .any(|(p, _)| Pin((*p).into()) == pad.pin)
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
        let master = self.acpi.at(MASTER);
        master.modify(Master::MASK_STATUS::CLEAR + Master::INTERRUPTS::CLEAR);
        for pad in base
            .iter()
            .map(|pad| overrides.iter().find(|p| p.pin == pad.pin).unwrap_or(pad))
        {
            let pin = pad.pin.0 as u8;
            if let Some(function) = self.bank.function(pad).unwrap_or(Some(0)) {
                let mux = self.acpi.at(iomux::at::<u8, ()>(pin.into()));
                mux.set(function & 3);
                mux.get(); // flush posted write
            }
            let control = self.acpi.at(control(pin));
            control.set((control.get() & !pad.mask) | pad.control);
            // Clear the W1C status bits in a separate write.
            control.modify(Control::INTERRUPT_STATUS::SET + Control::WAKE_STATUS::SET);
            if let Some((_, event)) = self.bank.events.iter().find(|(p, _)| *p == pin) {
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
                self.acpi.at(sci_map(event)).set(event);
                self.acpi
                    .at(SCI_TRIGGER)
                    .modify(gevent_bit(event, trigger.high()));
                self.acpi
                    .at(SCI_LEVEL)
                    .modify(gevent_bit(event, trigger.level()));
            }
            Event::Smi(trigger) => {
                smi::set_route(&self.acpi, smi::source::gevent(event), Route::Smi);
                self.acpi
                    .at(TRIGGER0)
                    .modify(gevent_bit(event, trigger.high()));
            }
        }
    }

    /// Select IOMUX `function` for `pin`, nothing else.
    pub fn set_function(&self, pin: u8, function: u8) -> Result<(), Error> {
        if !self.bank.exists(pin) || function > 3 {
            return Err(Error::InvalidPad);
        }
        self.acpi.at(iomux::at::<u8, ()>(pin.into())).set(function);
        Ok(())
    }
    /// The control register of `pin`, for vendor sequences that program it
    /// field by field.
    pub fn control(&self, pin: u8) -> Result<Reg<&W, u32, Control::Register>, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(self.acpi.at(control(pin)))
    }
    /// The level of `pin`.
    pub fn get(&self, pin: u8) -> Result<bool, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(self.acpi.at(control(pin)).is_set(Control::PIN))
    }
    /// Whether `pin` latched an interrupt, clearing the latch. A pending
    /// wake status stays.
    pub fn take_interrupt_status(&self, pin: u8) -> Result<bool, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        let control = self.acpi.at(control(pin));
        let value = control.extract();
        if !value.is_set(Control::INTERRUPT_STATUS) {
            return Ok(false);
        }
        // Status bits are write-1-to-clear.
        control.set((value.get() & !STATUS_MASK) | Control::INTERRUPT_STATUS::SET.value);
        Ok(true)
    }
    /// Drive output `pin`; pending status stays.
    pub fn set(&self, pin: u8, high: bool) -> Result<(), Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        self.acpi.at(control(pin)).modify(
            Control::OUTPUT.val(high.into())
                + Control::INTERRUPT_STATUS::CLEAR
                + Control::WAKE_STATUS::CLEAR,
        );
        Ok(())
    }
}

/// One pad of a bank as an embedded-hal output, borrowed from the bank so a
/// board can drive several pads of it.
pub struct Output<'a, W> {
    gpio: &'a Gpio<W>,
    pin: u8,
}

impl<W: Window> Gpio<W> {
    /// Drive `pin` through embedded-hal. Its pad must already be configured
    /// as an output.
    pub fn output(&self, pin: u8) -> Result<Output<'_, W>, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(Output { gpio: self, pin })
    }
}

impl<W> embedded_hal::digital::ErrorType for Output<'_, W> {
    type Error = Error;
}

impl<W: Window> embedded_hal::digital::OutputPin for Output<'_, W> {
    fn set_low(&mut self) -> Result<(), Error> {
        self.gpio.set(self.pin, false)
    }

    fn set_high(&mut self) -> Result<(), Error> {
        self.gpio.set(self.pin, true)
    }
}

/// One pad of a bank as an embedded-hal input.
pub struct Input<W> {
    gpio: Gpio<W>,
    pin: u8,
}

impl<W: Window> Gpio<W> {
    /// Read `pin` through embedded-hal. Its pad must already be configured.
    pub fn input(self, pin: u8) -> Result<Input<W>, Error> {
        if !self.bank.exists(pin) {
            return Err(Error::InvalidPad);
        }
        Ok(Input { gpio: self, pin })
    }
}

impl embedded_hal::digital::Error for Error {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

impl<W> embedded_hal::digital::ErrorType for Input<W> {
    type Error = Error;
}

impl<W: Window> embedded_hal::digital::InputPin for Input<W> {
    fn is_high(&mut self) -> Result<bool, Error> {
        self.gpio.get(self.pin)
    }

    fn is_low(&mut self) -> Result<bool, Error> {
        self.is_high().map(|high| !high)
    }
}

// ---- Any FCH's pads through its ACPI-MMIO window ----

/// Apply `pad` to the FCH whose ACPI-MMIO block is `acpi`: the control
/// bits of the pad in one read-modify-write, then the IOMUX (the order of
/// AMD CPM). A pad for this path names its function
/// ([`Function::Native`]); [`Function::Gpio`] needs the bank. IOMUX is
/// byte-only on MMIO (PPR 55570 §9.1.10); over SMN the window writes the
/// containing dword, so neighbouring pins' IOMUX bytes are rewritten with
/// their read values.
pub fn configure<W: Window>(acpi: &W, pad: &Pad) {
    debug_assert!(pad.pin.is_valid());
    if pad.mask != 0 {
        let ctl = acpi.reg::<u32, Control::Register>(pad.pin.control());
        ctl.set((ctl.get() & !pad.mask) | pad.control);
    }
    if let Some(Function::Native(function)) = pad.function {
        acpi.reg::<u8, ()>(pad.pin.iomux()).set(function);
    }
}

/// Apply a pad table in order.
pub fn configure_all<W: Window>(acpi: &W, pads: &[Pad]) {
    pads.iter().for_each(|pad| configure(acpi, pad));
}

/// The level of `pin` (`PinSts`).
pub fn read<W: Window>(acpi: &W, pin: Pin) -> bool {
    acpi.reg::<u32, Control::Register>(pin.control())
        .is_set(Control::PIN)
}

/// Drive output `pin`.
pub fn write<W: Window>(acpi: &W, pin: Pin, high: bool) {
    acpi.reg::<u32, Control::Register>(pin.control())
        .modify(Control::OUTPUT.val(high.into()));
}
