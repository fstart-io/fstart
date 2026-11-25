// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's MMIO UARTs: their pads, AOAC devices and legacy I/O decode,
//! as `FchUartInitV80 0xf4ad60` (Dell SilVenicePei; GIGABYTE `0x709aa4af`
//! identical) programs them [D] (FCH.md §1.6/12, §2.6). The pads differ from
//! Songshan's ([`crate::fch::songshan::uart`]): UART2 takes pads 135/137 at
//! IOMUX function 1, and UART0 takes all five of 135–139.
//!
//! A legacy port an eSPI I/O range starts at stays with eSPI: the vendor
//! drops the UART's legacy decode then (`FchEspiDecodeConflictCheck
//! 0xf4ac00`, [`without_espi_conflicts`]).

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::espi::Decode;
use crate::fch::FchHw;
use crate::fch::acpimmio::{AOAC, PM};
use crate::fch::gpio::{self, Control, Pad, Pin};
use crate::fch::uart::LEGACY_DECODE;
use crate::window::{At, Window};
use fstart_core::services::Timer;

/// A UART: its MMIO, its AOAC device, its pads and their IOMUX function,
/// and whether the pads' output enable is cleared (UART0's GPIO controls
/// `0x171C..0x172C` bit 23).
struct Uart {
    base: u64,
    device: u8,
    pads: &'static [u16],
    function: u8,
    clear_output_enable: bool,
}

const UARTS: [Uart; 3] = [
    Uart {
        base: 0xfedc_9000,
        device: 0x0b,
        pads: &[135, 136, 137, 138, 139],
        function: 0,
        clear_output_enable: true,
    },
    Uart {
        base: 0xfedc_a000,
        device: 0x0c,
        pads: &[141, 142],
        function: 0,
        clear_output_enable: false,
    },
    Uart {
        base: 0xfedc_e000,
        device: 0x10,
        pads: &[135, 137],
        function: 1,
        clear_output_enable: false,
    },
];

/// The UARTs Qilian has.
pub const COUNT: u8 = UARTS.len() as u8;

/// The MMIO base of UART `n`, `None` if Qilian has no such UART.
pub const fn base(n: u8) -> Option<u64> {
    if n < COUNT {
        Some(UARTS[n as usize].base)
    } else {
        None
    }
}

register_bitfields![u8,
    /// AOACx40 + 2n as the Qilian UART code writes it: `(x & 0x7C) | 8`.
    AoacUart [
        TARGET OFFSET(0) NUMBITS(2) [],
        POWER OFFSET(3) NUMBITS(1) [],
        IS_SW_CONTROL OFFSET(7) NUMBITS(1) []
    ],
    /// AOACx41 + 2n: the device is up when the low three bits are set.
    AoacUartState [
        UP OFFSET(0) NUMBITS(3) []
    ],
    /// PMxFF: the legacy I/O enables of UART 0..3.
    pub LegacyEnable [
        UART OFFSET(4) NUMBITS(4) []
    ]
];

/// PMxFF.
pub const PM_LEGACY_UART: At<u8, LegacyEnable::Register> = At::new(PM + 0xff);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No such UART.
    Uart,
    /// A pad or the AOAC device reads all-ones.
    Unavailable,
    /// The device did not come up within 1 ms.
    PowerTimeout,
}

/// Power UART `n` (AOAC, as `0xf76a60`: at most 100 polls 10 us apart) and
/// give it its pads.
pub fn enable(fch: &impl FchHw, n: u8) -> Result<(), Error> {
    let uart = UARTS.get(usize::from(n)).ok_or(Error::Uart)?;
    let acpi = fch.acpi();
    let control = |pin: u16| acpi.reg::<u32, Control::Register>(Pin(pin).control());
    if uart.pads.iter().any(|&pin| control(pin).get() == u32::MAX) {
        return Err(Error::Unavailable);
    }
    let power = acpi.reg::<u8, AoacUart::Register>(AOAC + 0x40 + 2 * u32::from(uart.device));
    let state = acpi.reg::<u8, AoacUartState::Register>(AOAC + 0x41 + 2 * u32::from(uart.device));
    if power.get() == u8::MAX {
        return Err(Error::Unavailable);
    }
    power.modify(AoacUart::TARGET.val(0) + AoacUart::IS_SW_CONTROL::CLEAR + AoacUart::POWER::SET);
    let mut polls = 0;
    while state.read(AoacUartState::UP) != 7 {
        if polls == 100 {
            return Err(Error::PowerTimeout);
        }
        fch.timer().delay_us(10);
        polls += 1;
    }
    for &pin in uart.pads {
        gpio::configure(acpi, &Pad::mux(Pin(pin), uart.function));
        if uart.clear_output_enable {
            // The W1C status bits written as 0: a latched status stays.
            control(pin).modify(
                Control::OUTPUT_ENABLE::CLEAR
                    + Control::INTERRUPT_STATUS::CLEAR
                    + Control::WAKE_STATUS::CLEAR,
            );
        }
    }
    Ok(())
}

/// The legacy I/O ranges a UART can decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LegacyPort {
    P2e8 = 0,
    P2f8 = 1,
    P3e8 = 2,
    P3f8 = 3,
}

impl LegacyPort {
    /// The range's first port (the vendor's table `0xfab638` [I]).
    pub const fn port(self) -> u16 {
        match self {
            Self::P2e8 => 0x2e8,
            Self::P2f8 => 0x2f8,
            Self::P3e8 => 0x3e8,
            Self::P3f8 => 0x3f8,
        }
    }
}

/// The board's legacy decode of each UART, without the ones an eSPI I/O
/// range starts at (`FchEspiDecodeConflictCheck`: eSPI keeps the port; the
/// vendor compares range bases only).
pub fn without_espi_conflicts(
    legacy: [Option<LegacyPort>; 4],
    espi: &Decode,
) -> [Option<LegacyPort>; 4] {
    legacy.map(|port| port.filter(|p| !espi.claims(p.port())))
}

/// Program the UARTs' legacy decodes: the range select of UART `n` in
/// `FEDC_0020h[9 + 2n : 8 + 2n]` and its enable in PMxFF\[4 + n\] [D].
/// Resolve eSPI conflicts first ([`without_espi_conflicts`]).
pub fn set_legacy(fch: &impl FchHw, legacy: [Option<LegacyPort>; 4]) {
    let map = fch.mmio().reg::<u16, ()>(LEGACY_DECODE);
    let (mut value, mut enables) = (map.get(), 0u8);
    for (n, port) in legacy.iter().enumerate() {
        let shift = 8 + 2 * n;
        value &= !(3 << shift);
        if let Some(port) = port {
            value |= (*port as u16) << shift;
            enables |= 1 << n;
        }
    }
    map.set(value);
    fch.acpi()
        .at(PM_LEGACY_UART)
        .modify(LegacyEnable::UART.val(enables));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::IOMUX;
    use crate::fch::model::fch;
    use crate::fch::qilian::espi::IoRange;

    fn up(hw: &crate::fch::model::TestFch, device: u8) {
        hw.acpi.put(AOAC + 0x41 + 2 * u32::from(device), 1, 7);
    }

    #[test]
    fn uart2_takes_pads_135_and_137_at_function_1() {
        let hw = fch();
        up(&hw, 0x10);
        hw.acpi.put(AOAC + 0x40 + 0x20, 1, 0x83);
        enable(&hw, 2).unwrap();
        assert_eq!(hw.acpi.get(AOAC + 0x40 + 0x20, 1), 0x08);
        assert_eq!(hw.acpi.get(IOMUX + 135, 1), 1);
        assert_eq!(hw.acpi.get(IOMUX + 137, 1), 1);
        assert_eq!(hw.acpi.get(IOMUX + 136, 1), 0);
        assert_eq!(base(2), Some(0xfedc_e000));
        assert_eq!(base(3), None);
        assert_eq!(enable(&hw, 3), Err(Error::Uart));
    }

    #[test]
    fn uart0_clears_its_pads_output_enable() {
        let hw = fch();
        up(&hw, 0x0b);
        for pin in 135..=139u32 {
            hw.acpi
                .put(0x1500 + 4 * pin, 4, 1 << 23 | 1 << 28 | 1 << 22);
            hw.acpi.put(IOMUX + pin, 1, 2);
        }
        enable(&hw, 0).unwrap();
        for pin in 135..=139u32 {
            assert_eq!(hw.acpi.get(0x1500 + 4 * pin, 4), 1 << 22, "pin {pin}");
            assert_eq!(hw.acpi.get(IOMUX + pin, 1), 0);
        }
        assert_eq!(
            hw.acpi.get(0x171c, 4),
            1 << 22,
            "0x171C is pad 135's control"
        );
    }

    #[test]
    fn power_times_out_and_dead_pads_refuse() {
        let hw = fch();
        assert_eq!(enable(&hw, 1), Err(Error::PowerTimeout));
        assert_eq!(hw.timer.timestamp_us(), 1000);
        let hw = fch();
        hw.acpi.put(0x1500 + 4 * 141, 4, u32::MAX);
        assert_eq!(enable(&hw, 1), Err(Error::Unavailable));
        assert!(hw.acpi.writes().is_empty());
    }

    #[test]
    fn espi_keeps_its_ports_and_legacy_decode_is_programmed() {
        let mut espi = Decode::default();
        espi.io[0] = Some(IoRange {
            base: 0x3f8,
            size: 8,
        });
        let board = [Some(LegacyPort::P3f8), Some(LegacyPort::P2f8), None, None];
        let legacy = without_espi_conflicts(board, &espi);
        assert_eq!(legacy, [None, Some(LegacyPort::P2f8), None, None]);
        let hw = fch();
        hw.mmio.put(LEGACY_DECODE, 2, 0x0f0f);
        hw.acpi.put(PM + 0xff, 1, 0x0f);
        set_legacy(&hw, legacy);
        assert_eq!(hw.mmio.get(LEGACY_DECODE, 2), 0x040f);
        assert_eq!(hw.acpi.get(PM + 0xff, 1), 0x2f);
    }
}
