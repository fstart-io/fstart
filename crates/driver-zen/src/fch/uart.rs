//! FCH UARTs (DesignWare, `FEDC_9000h` / `FEDC_A000h`) and their legacy
//! I/O decode.
//!
//! The S8030 console is the BMC's SUART at `3F8h` on LPC; the FCH UART0
//! only reaches the COM header with jumper J15 = 1-2 (BOARD_S8030 §4), so
//! nothing here runs by default.

use tock_registers::interfaces::Writeable;

use super::acpimmio::{self, AoacDevice};
use super::gpio::{self, Direction, Pad, Pin};
use super::{FchHw, Window, update};

/// MMIO base of UART `n` (sp4r2 ACPI `FUR0`/`FUR1`).
pub const fn base(n: u8) -> u64 {
    0xFEDC_9000 + 0x1000 * n as u64
}

/// The FCH UART legacy I/O decode register: the donor clears it in the late
/// task on Huashan (FCH_SP3 §3.3 af70, `fedc0020` = 0).
pub const LEGACY_DECODE: u32 = 0xFEDC_0020;
/// Two registers in the same block get bit 1 set by the env task on all
/// generations (FCH_SP3 §3.2 af50, `fedc0010`/`fedc0030`; unnamed).
pub const MISC_10: u32 = 0xFEDC_0010;
pub const MISC_30: u32 = 0xFEDC_0030;

/// UART0/1: AOAC devices 11/12 and pads 135..139 / 140..144 at function 0
/// (Taishan sp4r2 FCH_TAISHAN §7, inference; the S8030 UART0 pads are
/// 135–138, BOARD_S8030 §3.4).
const UART: [(u8, u16); 2] = [(11, 135), (12, 140)];

/// Power UART `n` and route its pads to the UART (CTS/RXD/RTS/TXD).
pub fn enable<H: FchHw>(hw: &H, n: u8) {
    let (dev, first) = UART[usize::from(n)];
    (first..first + 4).for_each(|pin| {
        gpio::configure(
            hw.acpi(),
            &Pad {
                pin: Pin(pin),
                function: Some(0),
                pull: None,
                direction: Direction::Keep,
            },
        )
    });
    acpimmio::aoac_power(hw.acpi(), AoacDevice(dev), true);
}

/// Set the legacy decode register (0 turns all legacy UART ranges off).
pub fn set_legacy_decode<H: FchHw>(hw: &H, value: u16) {
    hw.mmio().reg::<u16, ()>(LEGACY_DECODE).set(value);
}

/// The af50 bits in the `FEDC_0000h` block.
pub(crate) fn env<H: FchHw>(hw: &H) {
    update::<u8, _>(hw.mmio(), MISC_10, 0, 0x02);
    update::<u8, _>(hw.mmio(), MISC_30, 0, 0x02);
}
