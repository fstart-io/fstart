//! Lynx Point-LP pad configuration (`lp_gpio.[ch]`).
//!
//! Unlike PCH-H (which uses the shared ICH GPIO layout), each LP pad has two
//! 32-bit configuration registers and an independent ownership bit.

use super::regs::{LP_GPIO_CONF0, LP_GPIO_CONF1, lp_gpio};
use fstart_core::pio::PioRegister;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Native,
    Unused,
    Input,
    Output(bool),
    Sci,
    Smi,
    IrqEdge,
    IrqLevel,
    Pirq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    None,
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pad {
    pub mode: Mode,
    pub invert: bool,
    pub pull: Pull,
    pub blink: bool,
    pub rsmrst: bool,
}

impl Pad {
    #[must_use]
    pub const fn new(mode: Mode) -> Self {
        Self {
            mode,
            invert: false,
            pull: Pull::None,
            blink: false,
            rsmrst: false,
        }
    }

    fn encode(self) -> (u32, u32) {
        use Mode::*;
        let m = self.mode;
        (
            (LP_GPIO_CONF0::GPIO.val(u32::from(m != Native))
                + LP_GPIO_CONF0::INPUT.val(u32::from(!matches!(m, Native | Output(_))))
                + LP_GPIO_CONF0::INVERT.val(u32::from(self.invert || matches!(m, Sci | Smi)))
                + LP_GPIO_CONF0::LEVEL_IRQ.val(u32::from(m == IrqLevel))
                + LP_GPIO_CONF0::TX.val(u32::from(m == Output(true))))
            .value,
            (LP_GPIO_CONF1::PULL.val(self.pull as u32)
                + LP_GPIO_CONF1::SENSE_DISABLE.val(u32::from(matches!(m, Unused | Output(_)))))
            .value,
        )
    }
}

const fn pirq(pin: usize) -> Option<usize> {
    match pin {
        8..=10 => Some(pin - 8),
        13..=14 => Some(pin - 10),
        45..=55 => Some(pin - 40),
        _ => None,
    }
}

/// Indexed by physical pad `0..94`; no sentinel or implicit pin omissions.
pub type PadTable = [Pad; lp_gpio::PAD_COUNT];

#[derive(Default)]
struct Banks {
    owner: [u32; 3],
    smi: [u32; 3],
    irq: [u32; 3],
    reset: [u32; 3],
    blink: u32,
    pirq: u32,
}

impl Banks {
    fn add(&mut self, pin: usize, p: Pad) {
        let i = pin / 32;
        let b = 1 << (pin % 32);
        use Mode::*;
        if !matches!(p.mode, Native | Sci | Smi) {
            self.owner[i] |= b;
        }
        if p.mode == Smi {
            self.smi[i] |= b;
        }
        if matches!(p.mode, IrqEdge | IrqLevel) {
            self.irq[i] |= b;
        }
        if p.rsmrst {
            self.reset[i] |= b;
        }
        if i == 0 && p.blink {
            self.blink |= b;
        }
        if p.mode == Pirq {
            self.pirq |= 1 << pirq(pin).expect("pin is not PIRQ capable");
        }
    }
}

/// Lynx Point-LP GPIO controller bound to its configured `GPIOBASE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LpGpio(u16);

impl LpGpio {
    /// Default LP `GPIOBASE` I/O port address.
    pub const DEFAULT_BASE: u16 = 0x1400;

    /// Bind to the configured LP `GPIOBASE` I/O port address.
    #[must_use]
    pub const fn new(base: u16) -> Self {
        Self(base)
    }

    /// Bind to the default LP `GPIOBASE` (`0x1400`).
    #[must_use]
    pub const fn default_base() -> Self {
        Self(Self::DEFAULT_BASE)
    }

    /// Configured `GPIOBASE` I/O port address.
    #[must_use]
    pub const fn base(self) -> u16 {
        self.0
    }

    #[inline]
    fn try_conf0(base: u16, pin: u8) -> Option<PioRegister<u32, LP_GPIO_CONF0::Register>> {
        (usize::from(pin) < lp_gpio::PAD_COUNT)
            .then(|| PioRegister::new(base + lp_gpio::CONF_BASE + 8 * u16::from(pin)))
    }

    #[inline]
    fn conf0(base: u16, pin: u8) -> PioRegister<u32, LP_GPIO_CONF0::Register> {
        assert!(usize::from(pin) < lp_gpio::PAD_COUNT);
        PioRegister::new(base + lp_gpio::CONF_BASE + 8 * u16::from(pin))
    }

    #[inline]
    fn conf1(base: u16, pin: u8) -> PioRegister<u32, LP_GPIO_CONF1::Register> {
        assert!(usize::from(pin) < lp_gpio::PAD_COUNT);
        PioRegister::new(base + lp_gpio::CONF_BASE + 4 + 8 * u16::from(pin))
    }

    pub(super) fn native_at(base: u16, pin: u8) -> bool {
        Self::try_conf0(base, pin).is_some_and(|reg| !reg.is_set(LP_GPIO_CONF0::GPIO))
    }

    /// Sample the input level of `pin` at the given `GPIOBASE`.
    pub fn get_at(base: u16, pin: u8) -> bool {
        Self::try_conf0(base, pin).is_some_and(|reg| reg.is_set(LP_GPIO_CONF0::RX))
    }

    /// Drive output `pin` at the given `GPIOBASE`.
    #[inline]
    pub fn set_at(base: u16, pin: u8, high: bool) {
        if let Some(reg) = Self::try_conf0(base, pin) {
            reg.modify(LP_GPIO_CONF0::TX.val(u32::from(high)));
        }
    }

    /// Sample the input level of `pin` at this controller's `GPIOBASE`.
    pub fn get(self, pin: u8) -> bool {
        Self::get_at(self.0, pin)
    }

    /// Drive output `pin` at this controller's `GPIOBASE`.
    #[inline]
    pub fn set(self, pin: u8, high: bool) {
        Self::set_at(self.0, pin, high);
    }

    pub(crate) fn setup_at(base: u16, pads: &PadTable) {
        let mut banks = Banks::default();
        for (pin, p) in pads.iter().copied().enumerate() {
            let (a, b) = p.encode();
            Self::conf0(base, pin as u8).set(a);
            Self::conf1(base, pin as u8).set(b);
            banks.add(pin, p);
        }
        for (offset, words) in [
            (lp_gpio::OWNER_BASE, banks.owner),
            (lp_gpio::SMI_ROUT_BASE, banks.smi),
            (lp_gpio::IRQ_EN_BASE, banks.irq),
            (lp_gpio::RESET_BASE, banks.reset),
        ] {
            for (i, w) in words.into_iter().enumerate() {
                PioRegister::<u32>::new(base + offset + 4 * i as u16).set(w);
            }
        }
        PioRegister::<u32>::new(base + lp_gpio::BLINK).set(banks.blink);
        PioRegister::<u32>::new(base + lp_gpio::PIRQ_TO_IOAPIC).set(banks.pirq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippy_modes_and_sparse_pirq() {
        assert_eq!(Pad::new(Mode::Unused).encode(), (5, 4));
        assert_eq!(Pad::new(Mode::Smi).encode(), (13, 0));
        assert_eq!(Pad::new(Mode::Output(true)).encode(), (0x8000_0001, 4));
        let mut b = Banks::default();
        b.add(34, Pad::new(Mode::Smi));
        b.add(47, Pad::new(Mode::Pirq));
        assert_eq!(b.smi, [0, 4, 0]);
        assert_eq!(b.owner, [0, 1 << 15, 0]);
        assert_eq!(b.pirq, 1 << 7);
        b.add(51, Pad::new(Mode::Pirq)); // PIRQT, IOAPIC 24+11
        assert_eq!(b.pirq, (1 << 7) | (1 << 11));
        assert_eq!(pirq(12), None);
    }
}
