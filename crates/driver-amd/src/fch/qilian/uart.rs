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
/// and its PCI_INTR source. The vendor clears every pad's output enable
/// (GPIO control bit 23): UART0 `0x171C..0x172C`, UART1 `0x1734/0x1738`,
/// UART2 `0x171C/0x1724` (DEVICES.md §2.5).
struct Uart {
    base: u64,
    device: u8,
    pads: &'static [u16],
    function: u8,
    intr: u8,
}

const UARTS: [Uart; 3] = [
    Uart {
        base: 0xfedc_9000,
        device: 0x0b,
        pads: &[135, 136, 137, 138, 139],
        function: 0,
        intr: 0x74,
    },
    Uart {
        base: 0xfedc_a000,
        device: 0x0c,
        pads: &[141, 142],
        function: 0,
        intr: 0x75,
    },
    Uart {
        base: 0xfedc_e000,
        device: 0x10,
        pads: &[135, 137],
        function: 1,
        intr: 0x78,
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

/// The PCI_INTR source of UART `n` (PIC index; `| 0x80` is the IOAPIC
/// one, the vendor's `0xF4/0xF5/0xF8` routes, FCH.md §2.8).
pub const fn intr_source(n: u8) -> Option<u8> {
    if n < COUNT {
        Some(UARTS[n as usize].intr)
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
        // The W1C status bits written as 0: a latched status stays.
        control(pin).modify(
            Control::OUTPUT_ENABLE::CLEAR
                + Control::INTERRUPT_STATUS::CLEAR
                + Control::WAKE_STATUS::CLEAR,
        );
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

/// Two UARTs asked for the same legacy range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shared(pub LegacyPort);

/// `FEDC_0020h` and the PMxFF bits for the board's legacy decodes, as the
/// vendor's host code builds them (`FchDataInitClass`, Dell `0xf0d490` [D]):
/// range `k` behind UART `n` sets bit `k` (the range's enable) and puts `n`
/// in bits `[9 + 2k : 8 + 2k]` (the UART behind the range); PMxFF\[4 + n\]
/// marks UART `n` legacy. [`crate::fch::aml::FchAcpi::read`] reads the
/// same layout back.
pub fn legacy_decode(legacy: [Option<LegacyPort>; 4]) -> Result<(u16, u8), Shared> {
    let (mut map, mut enables) = (0u16, 0u8);
    for (n, port) in legacy.iter().enumerate() {
        let Some(port) = *port else { continue };
        let k = port as u16;
        if map & (1 << k) != 0 {
            return Err(Shared(port));
        }
        map |= 1 << k | (n as u16) << (8 + 2 * k);
        enables |= 1 << n;
    }
    Ok((map, enables))
}

/// Program the UARTs' legacy decodes (`FchUartInitV80`'s tail): the
/// [`legacy_decode`] map into `FEDC_0020h` and `PMxFF = (x & 0x0F) | v`,
/// each only if not zero, as the vendor. Resolve eSPI conflicts first
/// ([`without_espi_conflicts`]).
pub fn set_legacy(fch: &impl FchHw, legacy: [Option<LegacyPort>; 4]) -> Result<(), Shared> {
    let (map, enables) = legacy_decode(legacy)?;
    if map != 0 {
        fch.mmio().reg::<u16, ()>(LEGACY_DECODE).set(map);
    }
    if enables != 0 {
        fch.acpi()
            .at(PM_LEGACY_UART)
            .modify(LegacyEnable::UART.val(enables));
    }
    Ok(())
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
    fn uart2_takes_pads_135_and_137_at_function_1_and_clears_their_output_enable() {
        let hw = fch();
        up(&hw, 0x10);
        hw.acpi.put(AOAC + 0x40 + 0x20, 1, 0x83);
        hw.acpi.put(0x171c, 4, 1 << 23);
        enable(&hw, 2).unwrap();
        assert_eq!(hw.acpi.get(0x171c, 4), 0);
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
        hw.acpi.put(PM + 0xff, 1, 0xcf);
        set_legacy(&hw, legacy).unwrap();
        // Range 1 (2F8h) on, UART1 in bits 11:10.
        assert_eq!(hw.mmio.get(LEGACY_DECODE, 2), 1 << 1 | 1 << 10);
        assert_eq!(hw.acpi.get(PM + 0xff, 1), 0x2f);
    }

    /// The vendor's encoding (`(1 << (v - 1)) | n << (2v + 6)` for
    /// `FchUart{n}LegacyEnable = v`) and `FchAcpi::read`'s decoding agree.
    #[test]
    fn legacy_map_is_what_fch_acpi_reads() {
        use crate::fch::aml::FchAcpi;
        use crate::fch::model::Mem;
        for (n, port) in [(0usize, LegacyPort::P3f8), (2, LegacyPort::P2e8)] {
            let mut legacy = [None; 4];
            legacy[n] = Some(port);
            let (map, enables) = legacy_decode(legacy).unwrap();
            let v = port as u16 + 1;
            assert_eq!(map, 1 << (v - 1) | (n as u16) << (2 * v + 6));
            assert_eq!(enables, 1 << n);
            let mmio = Mem::default();
            mmio.put(LEGACY_DECODE, 2, map.into());
            mmio.put(
                [0xfedc_9000u32, 0xfedc_a000, 0xfedc_e000][n] + 0xfc,
                4,
                0x4457_0110,
            );
            let io = Mem::ports();
            (0..=255u32).for_each(|i| io.set_at(crate::fch::irq::DATA, i, 4));
            let read = FchAcpi::read(&mmio, &io);
            assert!(read.uarts.is_empty());
            assert_eq!(read.legacy[0].port, port.port());
        }
        let both = [Some(LegacyPort::P3f8), Some(LegacyPort::P3f8), None, None];
        assert_eq!(legacy_decode(both), Err(Shared(LegacyPort::P3f8)));
        assert_eq!(intr_source(2), Some(0x78));
    }
}
