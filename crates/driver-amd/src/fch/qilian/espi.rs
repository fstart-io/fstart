// SPDX-License-Identifier: MIT OR Apache-2.0
//! The server eSPI controllers of Qilian: their host decode windows only.
//!
//! On Qilian the ABL trains the eSPI link and programs its decodes from the
//! APCB (`FchEspiAblInitEnable`, entry `0x1706/0x2001` `EspiInit`: 16 I/O
//! and 5 MMIO ranges, Port 80, 60h/64h) and runs the board's Super I/O
//! writes (`0x2005` `EspiSioInit`) before the x86 runs. Neither vendor's x86
//! code resets the link, enables a channel or reprograms the slave (no
//! `ESPI_*` init constant in any Dell PR9XJ or GIGABYTE MD25-CG0 module;
//! FCH.md §1.1). This module therefore never sends a command: it reads the
//! decodes back ([`Espi::decode`]) and, when a board asks, adds a range in a
//! free slot ([`Espi::add_io`]), verifying the readback. The Picasso link
//! bring-up ([`crate::fch::espi::initialize`], an in-band reset and a slave
//! reconfiguration) would drop the BMC link the ABL trained and is never
//! used here.
//!
//! The decode registers are the controller's ([`crate::fch::espi`]'s
//! [`DecodeEnable`], [`io_range`], [`mmio_range`]); a Qilian controller
//! ([`CONTROLLERS`]: eSPI0 at SPI BAR plus `0x1_0000`, eSPI1 at SPI BAR plus
//! `0x2_0000`) has all 16 I/O and 5 MMIO ranges, its I/O range layout from
//! the Dell SilVenicePei range table `0xfab5b8` (GIGABYTE identical) [I].

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::fch::SPI_BASE;
pub use crate::fch::espi::IoRange;
use crate::fch::espi::{DECODE_ENABLE, DecodeEnable, io_enable_bit, io_range, mmio_range};
use crate::window::{At, Window};

/// eSPI0 and eSPI1 (SPI BAR + `0x1_0000` / `0x2_0000`).
pub const CONTROLLERS: [u32; 2] = [SPI_BASE + 0x1_0000, SPI_BASE + 0x2_0000];
/// I/O ranges per controller.
pub const IO_RANGES: usize = 16;
/// MMIO ranges per controller.
pub const MMIO_RANGES: usize = 5;

/// eSPIx40 of the controller at `base`.
const fn decode_enable(base: u32) -> At<u32, DecodeEnable::Register> {
    At::new(base + DECODE_ENABLE as u32)
}

/// Offsets of I/O range `i`'s base and size byte.
const fn io_offsets(i: usize) -> (u32, u32) {
    let (base, size) = io_range(i);
    (base as u32, size as u32)
}

/// The enable bit of MMIO range `i` and the offsets of its base and size.
const fn mmio_layout(i: usize) -> (u32, u32, u32) {
    let (bit, base, size) = mmio_range(i);
    (bit, base as u32, size as u32)
}

/// A host MMIO range forwarded to the eSPI peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRange {
    pub base: u32,
    pub size: u32,
}

/// The decodes of one controller as the ABL left them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decode {
    /// 2Eh/2Fh.
    pub sio_2e: bool,
    /// 60h/64h.
    pub kbc: bool,
    /// 80h.
    pub port80: bool,
    pub io: [Option<IoRange>; IO_RANGES],
    pub mmio: [Option<MmioRange>; MMIO_RANGES],
}

impl Decode {
    /// Every port of `[port, port + len)` reaches the peripheral, through a
    /// fixed decode or an enabled I/O range.
    pub fn covers(&self, port: u16, len: u16) -> bool {
        (u32::from(port)..u32::from(port) + u32::from(len)).all(|p| self.decodes(p))
    }

    fn decodes(&self, port: u32) -> bool {
        let fixed = (self.sio_2e && (0x2e..=0x2f).contains(&port))
            || (self.kbc && (port == 0x60 || port == 0x64))
            || (self.port80 && port == 0x80);
        fixed
            || self
                .io
                .iter()
                .flatten()
                .any(|r| (u32::from(r.base)..u32::from(r.base) + u32::from(r.size)).contains(&port))
    }

    /// An enabled I/O range starts at `port`: the vendor's eSPI/UART
    /// conflict test (`FchEspiDecodeConflictCheck`, FCH.md §2.6).
    pub fn claims(&self, port: u16) -> bool {
        self.io.iter().flatten().any(|r| r.base == port)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The controller does not decode (reads all-ones).
    Unavailable,
    /// Every I/O range is in use.
    NoRange,
    /// The range is empty, larger than 256 bytes or wraps.
    Range,
    /// The new range did not read back.
    Readback,
}

/// eSPI controller `index` (0 or 1) through the FCH's MMIO window.
pub struct Espi<'a, W> {
    mmio: &'a W,
    base: u32,
}

impl<'a, W: Window> Espi<'a, W> {
    /// Controller `index` of [`CONTROLLERS`], `None` for another index.
    pub fn new(mmio: &'a W, index: usize) -> Option<Self> {
        Some(Self {
            mmio,
            base: *CONTROLLERS.get(index)?,
        })
    }

    /// The decodes the controller has, `Unavailable` if it reads all-ones.
    pub fn decode(&self) -> Result<Decode, Error> {
        let enable = self.mmio.at(decode_enable(self.base)).extract();
        if enable.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
        let mut decode = Decode {
            sio_2e: enable.is_set(DecodeEnable::SIO_2E),
            kbc: enable.is_set(DecodeEnable::KBC_6064),
            port80: enable.is_set(DecodeEnable::PORT80),
            ..Decode::default()
        };
        for (i, slot) in decode.io.iter_mut().enumerate() {
            if enable.get() & 1 << io_enable_bit(i) == 0 {
                continue;
            }
            let (base, size) = io_offsets(i);
            *slot = Some(IoRange {
                base: self.mmio.read16(self.base + base),
                size: u16::from(self.mmio.read8(self.base + size)) + 1,
            });
        }
        for (i, slot) in decode.mmio.iter_mut().enumerate() {
            let (bit, base, size) = mmio_layout(i);
            if enable.get() & 1 << bit == 0 {
                continue;
            }
            *slot = Some(MmioRange {
                base: self.mmio.read32(self.base + base),
                size: u32::from(self.mmio.read16(self.base + size)) + 1,
            });
        }
        Ok(decode)
    }

    /// Decode `range` to the peripheral in the first free I/O range, unless
    /// the controller decodes it already; nothing else changes (the link,
    /// the channels and the other ranges stay as the ABL set them). Returns
    /// the range index used, `None` if no write was needed.
    pub fn add_io(&self, range: IoRange) -> Result<Option<usize>, Error> {
        if range.size == 0
            || range.size > 256
            || u32::from(range.base) + u32::from(range.size) > 0x1_0000
        {
            return Err(Error::Range);
        }
        let decode = self.decode()?;
        if decode.covers(range.base, range.size) {
            return Ok(None);
        }
        let i = decode
            .io
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoRange)?;
        let (base, size) = io_offsets(i);
        self.mmio.write16(self.base + base, range.base);
        self.mmio.write8(self.base + size, (range.size - 1) as u8);
        let enable = self.mmio.at(decode_enable(self.base));
        enable.set(enable.get() | 1 << io_enable_bit(i));
        match self.decode()?.io[i] {
            Some(read) if read == range => Ok(Some(i)),
            _ => Err(Error::Readback),
        }
    }

    /// Turn the fixed Port 80h decode on (POST codes to the BMC).
    pub fn enable_port80(&self) -> Result<(), Error> {
        if self.mmio.at(decode_enable(self.base)).get() == u32::MAX {
            return Err(Error::Unavailable);
        }
        self.mmio
            .at(decode_enable(self.base))
            .modify(DecodeEnable::PORT80::SET);
        Ok(())
    }
}

/// The decodes of both controllers (`None`: the controller reads
/// all-ones).
pub fn decodes<W: Window>(mmio: &W) -> [Option<Decode>; 2] {
    core::array::from_fn(|i| Espi::new(mmio, i).and_then(|espi| espi.decode().ok()))
}

/// Any controller decodes every port of `[port, port + len)`.
pub fn covers<W: Window>(mmio: &W, port: u16, len: u16) -> bool {
    decodes(mmio)
        .iter()
        .flatten()
        .any(|decode| decode.covers(port, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Mem;

    const E0: u32 = CONTROLLERS[0];

    /// The GIGABYTE MD25-CG0 APCB `EspiInit`: `0xCA2`+4, `0x2F8`+8,
    /// `0x3F8`+8, Port 80, 60h/64h (FCH.md §2.1), as the ABL would leave
    /// them in ranges 0–2.
    fn md25() -> Mem {
        let m = Mem::default();
        m.put(E0 + 0x40, 4, 0b111 << 8 | 0b110);
        for (i, (base, size)) in [(0xca2u32, 4u32), (0x2f8, 8), (0x3f8, 8)]
            .iter()
            .enumerate()
        {
            m.put(E0 + 0x44 + 2 * i as u32, 2, *base);
            m.put(E0 + 0x4c + i as u32, 1, size - 1);
        }
        m
    }

    #[test]
    fn reads_the_ranges_the_abl_programmed() {
        let m = md25();
        let decode = Espi::new(&m, 0).unwrap().decode().unwrap();
        assert!(decode.port80 && decode.kbc && !decode.sio_2e);
        assert_eq!(
            decode.io[0],
            Some(IoRange {
                base: 0xca2,
                size: 4
            })
        );
        assert!(decode.io[3..].iter().all(Option::is_none));
        assert!(decode.covers(0xca2, 2), "KCS data and status");
        assert!(decode.covers(0x3f8, 8));
        assert!(!decode.covers(0xca0, 2));
        assert!(!decode.covers(0x2e, 2));
        assert!(decode.claims(0x3f8) && !decode.claims(0x3f9));
        assert!(covers(&m, 0xca2, 2));
        // eSPI1 does not decode: all-ones.
        m.put(CONTROLLERS[1] + 0x40, 4, u32::MAX);
        assert_eq!(decodes(&m)[1], None);
        assert!(Espi::new(&m, 2).is_none());
    }

    #[test]
    fn extended_ranges_use_their_own_registers() {
        let m = Mem::default();
        // Range 4 (+0x80, enable bit 16), range 9 (+0x8E, bit 21), range 15
        // (+0xB6, bit 27), MMIO 4 (+0xBC, bit 28).
        m.put(E0 + 0x40, 4, 1 << 16 | 1 << 21 | 1 << 27 | 1 << 28);
        m.put(E0 + 0x80, 2, 0x900);
        m.put(E0 + 0x88, 1, 7);
        m.put(E0 + 0x8e, 2, 0x4e);
        m.put(E0 + 0x95, 1, 1);
        m.put(E0 + 0xb6, 2, 0x200);
        m.put(E0 + 0xbb, 1, 0xff);
        m.put(E0 + 0xbc, 4, 0xfed5_0000);
        m.put(E0 + 0xc0, 2, 0xfff);
        let d = Espi::new(&m, 0).unwrap().decode().unwrap();
        assert_eq!(
            d.io[4],
            Some(IoRange {
                base: 0x900,
                size: 8
            })
        );
        assert_eq!(
            d.io[9],
            Some(IoRange {
                base: 0x4e,
                size: 2
            })
        );
        assert_eq!(
            d.io[15],
            Some(IoRange {
                base: 0x200,
                size: 256
            })
        );
        assert_eq!(
            d.mmio[4],
            Some(MmioRange {
                base: 0xfed5_0000,
                size: 0x1000
            })
        );
        assert!(d.covers(0x2ff, 1) && d.covers(0x4e, 2));
    }

    #[test]
    fn adds_a_range_in_the_first_free_slot_and_touches_nothing_else() {
        let m = md25();
        let espi = Espi::new(&m, 0).unwrap();
        // Already decoded: no write.
        assert_eq!(
            espi.add_io(IoRange {
                base: 0x3f8,
                size: 8
            }),
            Ok(None)
        );
        assert!(m.writes().is_empty());
        assert_eq!(
            espi.add_io(IoRange {
                base: 0x2e,
                size: 2
            }),
            Ok(Some(3))
        );
        assert_eq!(m.get(E0 + 0x4a, 2), 0x2e);
        assert_eq!(m.get(E0 + 0x4f, 1), 1);
        assert_eq!(m.get(E0 + 0x40, 4), 0b1111 << 8 | 0b110);
        // Only the new range's base, size and the enable register written:
        // never the command, the slave or the channel registers.
        let offsets: Vec<u32> = m.writes().iter().map(|w| w.0 - E0).collect();
        assert_eq!(offsets, [0x4a, 0x4f, 0x40]);
        assert_eq!(espi.add_io(IoRange { base: 0, size: 0 }), Err(Error::Range));
        assert_eq!(
            espi.add_io(IoRange {
                base: 0xffff,
                size: 2
            }),
            Err(Error::Range)
        );
        // The next free slot is range 4, behind the extended enables.
        assert_eq!(
            espi.add_io(IoRange {
                base: 0x600,
                size: 16
            }),
            Ok(Some(4))
        );
        assert_eq!(m.get(E0 + 0x80, 2), 0x600);
        assert_eq!(m.get(E0 + 0x40, 4) >> 16 & 1, 1);
    }

    #[test]
    fn full_or_dead_controllers_refuse() {
        let m = Mem::default();
        m.put(E0 + 0x40, 4, 0x0fff_ff00);
        let espi = Espi::new(&m, 0).unwrap();
        assert_eq!(
            espi.add_io(IoRange {
                base: 0x2e,
                size: 2
            }),
            Err(Error::NoRange)
        );
        m.put(E0 + 0x40, 4, u32::MAX);
        assert_eq!(espi.decode(), Err(Error::Unavailable));
        assert_eq!(espi.enable_port80(), Err(Error::Unavailable));
    }
}
