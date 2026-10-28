//! FCH GPIO and IOMUX.
//!
//! Local pins 0–255: control dword at GPIO bank `0x1500 + 4n`, IOMUX byte
//! at `0xD00 + n`. Huashan also has a remote bank for pins 256 and up:
//! control at `0x1200 + 4(n − 256)`, IOMUX at `0x12C0 + (n − 256)` (FCH_SP3
//! §3.2 af57: the SATA1 SGPIO pads 260–263 are `fed812c4..c7` /
//! `fed81210..18`; the S8030 uses 256–267, BOARD_S8030 §3.4).
//!
//! On Naples every die has its own FCH; a pin on die n is reached through
//! that die's ACPI-MMIO over SMN (the vendor board code writes SMN
//! `2D02500h`/`2D01D00h` + pin, BOARD_S8030 §5). IOMUX is byte-only on
//! MMIO (PPR 55570 §9.1.10); over SMN this module writes the containing
//! dword, so neighbouring pins' IOMUX bytes are rewritten with their read
//! values.

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::Window;
use super::acpimmio::{GPIO, IOMUX, REMOTE_GPIO};

register_bitfields![u32,
    /// FCH::GPIO::GPIOBank0Ctl (PPR 55570).
    pub CONTROL [
        ACTIVE_LEVEL OFFSET(9) NUMBITS(2) [],
        INTERRUPT_STATUS_EN OFFSET(11) NUMBITS(1) [],
        INTERRUPT_EN OFFSET(12) NUMBITS(1) [],
        WAKE_CONTROL OFFSET(13) NUMBITS(3) [],
        PIN_STS OFFSET(16) NUMBITS(1) [],
        DRIVE_STRENGTH OFFSET(17) NUMBITS(2) [],
        /// Reserved in public PPR 55570; CPM setting bit 2 controls it
        /// when the pull field is enabled (RXM100CB `ffd79a92`).
        CPM_BIT19 OFFSET(19) NUMBITS(1) [],
        PULL_UP OFFSET(20) NUMBITS(1) [],
        PULL_DOWN OFFSET(21) NUMBITS(1) [],
        OUTPUT_VALUE OFFSET(22) NUMBITS(1) [],
        OUTPUT_ENABLE OFFSET(23) NUMBITS(1) [],
        INTERRUPT_STS OFFSET(28) NUMBITS(1) [],
        WAKE_STS OFFSET(29) NUMBITS(1) []
    ]
];

/// First pin of the remote bank.
pub const REMOTE_FIRST: u16 = 256;
/// Pins in the remote bank (`0x1200..0x12C0` holds 48 control dwords).
pub const REMOTE_PINS: u16 = 48;

/// An FCH GPIO pin number (local 0–255, remote 256–303).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pin(pub u16);

impl Pin {
    /// Offset of the control dword in the ACPI-MMIO block.
    pub const fn control(self) -> u32 {
        match self.0 {
            n if n < REMOTE_FIRST => GPIO + 4 * n as u32,
            n => REMOTE_GPIO + 4 * (n - REMOTE_FIRST) as u32,
        }
    }

    /// Offset of the IOMUX byte in the ACPI-MMIO block.
    pub const fn iomux(self) -> u32 {
        match self.0 {
            n if n < REMOTE_FIRST => IOMUX + n as u32,
            n => REMOTE_GPIO + 0xC0 + (n - REMOTE_FIRST) as u32,
        }
    }

    pub const fn is_valid(self) -> bool {
        self.0 < REMOTE_FIRST + REMOTE_PINS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    None,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Leave output enable and value alone.
    Keep,
    Input,
    Output(bool),
}

/// One pad setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub pin: Pin,
    /// IOMUX function, `None` = keep.
    pub function: Option<u8>,
    /// `None` = keep.
    pub pull: Option<Pull>,
    /// Only CPM assigns this otherwise unnamed control bit. `None` keeps
    /// it unchanged, as direct UART/I2C/SGPIO pad programming does.
    pub cpm_bit19: Option<bool>,
    pub direction: Direction,
}

impl Pad {
    /// Decode an AMD CPM `$A10` setting word for `pin` (BOARD_S8030 §3.4):
    /// IOMUX = bits 10:8 when bit 11 is set; bit 7 enables the pull field
    /// (bit 5 up, bit 6 down, bit 2 control bit 19); with bit 3 clear,
    /// bit 1 selects input, else
    /// output = bit 0; bit 3 set keeps the direction.
    pub const fn from_cpm(pin: Pin, setting: u16) -> Self {
        let function = if setting & 0x800 != 0 {
            Some(((setting >> 8) & 7) as u8)
        } else {
            None
        };
        let pull = if setting & 0x80 == 0 {
            None
        } else if setting & 0x20 != 0 {
            Some(Pull::Up)
        } else if setting & 0x40 != 0 {
            Some(Pull::Down)
        } else {
            Some(Pull::None)
        };
        let direction = if setting & 0x8 != 0 {
            Direction::Keep
        } else if setting & 0x2 != 0 {
            Direction::Input
        } else {
            Direction::Output(setting & 1 != 0)
        };
        Self {
            pin,
            function,
            pull,
            cpm_bit19: if setting & 0x80 != 0 {
                Some(setting & 4 != 0)
            } else {
                None
            },
            direction,
        }
    }
}

/// Apply `pad` to the FCH whose ACPI-MMIO block is `acpi`.
pub fn configure<W: Window>(acpi: &W, pad: &Pad) {
    debug_assert!(pad.pin.is_valid());
    let ctl = acpi.reg::<u32, CONTROL::Register>(pad.pin.control());
    if let Some(value) = pad.cpm_bit19 {
        ctl.modify(CONTROL::CPM_BIT19.val(value.into()));
    }
    if let Some(pull) = pad.pull {
        ctl.modify(match pull {
            Pull::None => CONTROL::PULL_UP::CLEAR + CONTROL::PULL_DOWN::CLEAR,
            Pull::Up => CONTROL::PULL_UP::SET + CONTROL::PULL_DOWN::CLEAR,
            Pull::Down => CONTROL::PULL_UP::CLEAR + CONTROL::PULL_DOWN::SET,
        });
    }
    match pad.direction {
        Direction::Keep => {}
        Direction::Input => ctl.modify(CONTROL::OUTPUT_ENABLE::CLEAR),
        Direction::Output(high) => {
            ctl.modify(CONTROL::OUTPUT_VALUE.val(high.into()) + CONTROL::OUTPUT_ENABLE::SET)
        }
    }
    // CPM configures the control byte before switching the pad's function.
    if let Some(function) = pad.function {
        acpi.reg::<u8, ()>(pad.pin.iomux()).set(function);
    }
}

/// Apply a pad table in order.
pub fn configure_all<W: Window>(acpi: &W, pads: &[Pad]) {
    pads.iter().for_each(|pad| configure(acpi, pad));
}

/// The pin level (`PinSts`).
pub fn read<W: Window>(acpi: &W, pin: Pin) -> bool {
    acpi.reg::<u32, CONTROL::Register>(pin.control())
        .is_set(CONTROL::PIN_STS)
}

/// Drive an output pin.
pub fn write<W: Window>(acpi: &W, pin: Pin, high: bool) {
    acpi.reg::<u32, CONTROL::Register>(pin.control())
        .modify(CONTROL::OUTPUT_VALUE.val(high.into()));
}

#[cfg(test)]
mod tests {
    use super::super::model::Mem;
    use super::*;

    #[test]
    fn local_and_remote_addresses() {
        // FCH_SP3 §3.2 af57: SGPIO pads of SATA0 (9/10/23) and SATA1 (260..262).
        assert_eq!(Pin(9).control(), 0x1524);
        assert_eq!(Pin(23).control(), 0x155c);
        assert_eq!(Pin(40).iomux(), 0xd28);
        assert_eq!(Pin(260).control(), 0x1210);
        assert_eq!(Pin(263).iomux(), 0x12c7);
        assert!(!Pin(304).is_valid());
    }

    #[test]
    fn cpm_settings() {
        // Rome M.2 detect pins (BOARD_S8030 §3.3): 982h = IOMUX 1, input, no pull.
        let pad = Pad::from_cpm(Pin(144), 0x982);
        assert_eq!(pad.function, Some(1));
        assert_eq!(pad.pull, Some(Pull::None));
        assert_eq!(pad.direction, Direction::Input);
        // GPIO4/5 output high with pull-up (880h | out | pull).
        let pad = Pad::from_cpm(Pin(4), 0x880 | 0x20 | 1);
        assert_eq!(pad.function, Some(0));
        assert_eq!(pad.pull, Some(Pull::Up));
        assert_eq!(pad.direction, Direction::Output(true));
    }

    #[test]
    fn cpm_bit19_is_explicit_and_does_not_change_kept_outputs() {
        let acpi = Mem::default();
        let pin = Pin(89);
        let seed = 0x00ff_4321;
        // RXM100CB GPIO89 setting 0x0988 clears 19..21, not OE/value.
        acpi.put(pin.control(), 4, seed);
        configure(&acpi, &Pad::from_cpm(pin, 0x988));
        assert_eq!(acpi.get(pin.control(), 4), seed & !0x0038_0000);
        assert_eq!(acpi.get(pin.iomux(), 1), 1);
        configure(&acpi, &Pad::from_cpm(pin, 0x98c));
        assert_eq!(
            acpi.get(pin.control(), 4),
            (seed & !0x0038_0000) | 0x0008_0000
        );
        assert_eq!(Pad::from_cpm(pin, 0x908).cpm_bit19, None);
        configure(&acpi, &Pad::from_cpm(pin, 0x908));
        assert_eq!(acpi.get(pin.control(), 4) & 0x0008_0000, 0x0008_0000);
    }

    #[test]
    fn configure_matches_the_sgpio_pad_writes() {
        let acpi = Mem::default();
        acpi.put(0x1524, 4, 0x0020_0000);
        configure(
            &acpi,
            &Pad {
                pin: Pin(9),
                function: Some(1),
                pull: Some(Pull::Up),
                cpm_bit19: None,
                direction: Direction::Keep,
            },
        );
        assert_eq!(acpi.get(0xd09, 1), 1);
        assert_eq!(acpi.get(0x1524, 4), 0x0010_0000);
    }
}
