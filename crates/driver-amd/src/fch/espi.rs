// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD eSPI controller: its decode registers ([`DecodeEnable`],
//! [`io_range`], [`mmio_range`]), and its bring-up by the x86 on Picasso,
//! a four-window controller without the newer extended windows or
//! ALERT_ENABLE bits.
//!
//! [`initialize`] resets the link in band and reconfigures the slave: only
//! for SoCs whose x86 firmware owns the link. On the server FCHs the ABL
//! trains it from the APCB, and the x86 only reads and adds decode windows
//! ([`super::qilian::espi`]).
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::Timer;
use heapless::Vec;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// eSPIx40 (32-bit): DecodeEnable.
    pub DecodeEnable [
        /// Super I/O config ports 2Eh/2Fh.
        SIO_2E OFFSET(0) NUMBITS(1) [],
        /// KBC ports 60h/64h.
        KBC_6064 OFFSET(1) NUMBITS(1) [],
        /// POST code port 80h.
        PORT80 OFFSET(2) NUMBITS(1) [],
        /// I/O ranges 0–3.
        IO_0 OFFSET(8) NUMBITS(4) [],
        /// MMIO ranges 0–3.
        MMIO_0 OFFSET(12) NUMBITS(4) [],
        /// I/O ranges 4–7.
        IO_4 OFFSET(16) NUMBITS(4) [],
        /// I/O ranges 8–11.
        IO_8 OFFSET(20) NUMBITS(4) [],
        /// I/O ranges 12–15.
        IO_12 OFFSET(24) NUMBITS(4) [],
        /// MMIO range 4.
        MMIO_4 OFFSET(28) NUMBITS(1) []
    ]
];

/// eSPIx40: the decode enables.
pub const DECODE_ENABLE: u16 = 0x40;

/// The enable bit of I/O range `i` in eSPIx40.
pub const fn io_enable_bit(i: usize) -> u32 {
    match i {
        0..=3 => 8 + i as u32,
        4..=7 => 16 + (i as u32 - 4),
        8..=11 => 20 + (i as u32 - 8),
        _ => 24 + (i as u32 - 12),
    }
}

/// Offsets of I/O range `i`'s base (16 bit) and size byte (`size - 1`):
/// ranges 0–3 at `+0x44`/`+0x4C`, the extended ones 4–7 at `+0x80`/`+0x88`,
/// 8–11 at `+0x8C`/`+0x94`, 12–15 at `+0xB0`/`+0xB8`.
pub const fn io_range(i: usize) -> (u16, u16) {
    let (base, size, k) = match i {
        0..=3 => (0x44, 0x4c, i),
        4..=7 => (0x80, 0x88, i - 4),
        8..=11 => (0x8c, 0x94, i - 8),
        _ => (0xb0, 0xb8, i - 12),
    };
    (base + 2 * k as u16, size + k as u16)
}

/// The enable bit of MMIO range `i` and the offsets of its base (32 bit)
/// and size (16 bit, `size - 1`): ranges 0–3 at `+0x50`/`+0x60`, the
/// extended range 4 at `+0xBC`/`+0xC0` (coreboot's `espi_def.h`).
pub const fn mmio_range(i: usize) -> (u32, u16, u16) {
    match i {
        0..=3 => (12 + i as u32, 0x50 + 4 * i as u16, 0x60 + 2 * i as u16),
        _ => (28, 0xbc, 0xc0),
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Mode {
    Single,
    Dual,
    Quad,
}
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Frequency {
    Mhz16,
    Mhz33,
    Mhz66,
}
#[derive(Debug, Clone, Copy)]
pub enum Alert {
    InBand,
    PushPull,
    OpenDrain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoRange {
    pub base: u16,
    pub size: u16,
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub io: [Option<IoRange>; 4],
    pub port80: bool,
    pub keyboard: bool,
    pub superio: bool,
    pub mode: Mode,
    pub frequency: Frequency,
    pub alert: Alert,
    pub crc: bool,
    pub peripheral: bool,
    pub virtual_wire: bool,
    pub oob: bool,
    pub flash: bool,
    pub subtractive: bool,
    pub irq_polarity: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Window,
    Unavailable,
    BusyTimeout,
    CompletionTimeout,
    Status(u32),
    Capability,
    ChannelTimeout,
}
pub trait Registers {
    fn read32(&mut self, offset: u16) -> u32;
    fn write32(&mut self, offset: u16, value: u32);
    fn read16(&mut self, offset: u16) -> u16;
    fn write16(&mut self, offset: u16, value: u16);
    fn write8(&mut self, offset: u16, value: u8);
}
const COMPLETE: u32 = 1 << 28;
fn available(value: u32) -> Result<u32, Error> {
    if value == u32::MAX {
        Err(Error::Unavailable)
    } else {
        Ok(value)
    }
}
fn update(r: &mut impl Registers, offset: u16, clear: u32, set: u32) -> Result<(), Error> {
    let old = available(r.read32(offset))?;
    r.write32(offset, (old & !clear) | set);
    Ok(())
}
fn ready(r: &mut impl Registers, timer: &impl Timer) -> Result<(), Error> {
    let start = timer.timestamp_us();
    for _ in 0..=100 {
        if available(r.read32(0))? & 8 == 0 {
            return Ok(());
        }
        if timer.timestamp_us().wrapping_sub(start) >= 100 {
            break;
        }
        timer.delay_us(1);
    }
    Err(Error::BusyTimeout)
}
fn clear_status(r: &mut impl Registers) -> Result<(), Error> {
    let status = available(r.read32(0x70))?;
    if status != 0 {
        r.write32(0x70, status);
    } // W1C, never plain zero.
    Ok(())
}
fn command(
    r: &mut impl Registers,
    timer: &impl Timer,
    header: u32,
    data: u32,
    payload: u32,
    allowed: u32,
) -> Result<(), Error> {
    ready(r, timer)?;
    clear_status(r)?;
    r.write32(4, data);
    r.write32(8, 0);
    r.write32(0xc, payload);
    r.write32(0, header | 8); // Trigger LAST.
    ready(r, timer)?;
    let start = timer.timestamp_us();
    for _ in 0..=100 {
        let status = available(r.read32(0x70))?;
        if status != 0 {
            if status & COMPLETE == 0 || status & !(COMPLETE | allowed) != 0 {
                return Err(Error::Status(status));
            }
            r.write32(0x70, status);
            return Ok(());
        }
        if timer.timestamp_us().wrapping_sub(start) >= 100 {
            break;
        }
        timer.delay_us(1);
    }
    Err(Error::CompletionTimeout)
}
fn header(kind: u32, address: u16) -> u32 {
    kind | (u32::from(address >> 8) << 8) | (u32::from(address & 255) << 16)
}
fn get(r: &mut impl Registers, timer: &impl Timer, address: u16) -> Result<u32, Error> {
    command(r, timer, header(1, address), 0, 0, 0)?;
    available(r.read32(4))
}
fn set(r: &mut impl Registers, timer: &impl Timer, address: u16, value: u32) -> Result<(), Error> {
    command(r, timer, header(0, address), value, 0, 0)
}
/// Expand board resource ranges into physical 256-byte windows before reset.
/// Reject wrapping, overlapping ranges or exhaustion before any hardware write.
pub fn windows(config: &Config) -> Result<Vec<IoRange, 4>, Error> {
    let mut windows = Vec::new();
    for range in config.io.iter().flatten() {
        if range.size == 0 || u32::from(range.base) + u32::from(range.size) > 65536 {
            return Err(Error::Window);
        }
        let mut base = u32::from(range.base);
        let mut remaining = range.size;
        while remaining != 0 {
            let size = remaining.min(256);
            if windows.iter().any(|other: &IoRange| {
                base < u32::from(other.base) + u32::from(other.size)
                    && u32::from(other.base) < base + u32::from(size)
            }) {
                return Err(Error::Window);
            }
            windows
                .push(IoRange {
                    base: base as u16,
                    size,
                })
                .map_err(|_| Error::Window)?;
            remaining -= size;
            base += u32::from(size);
        }
    }
    Ok(windows)
}
/// Native capability negotiation can reduce requested rate/lane width. Return
/// the negotiated host configuration for diagnostics, not a silent success.
fn general(config: &Config, caps: u32) -> Result<(u32, u32), Error> {
    let modes = caps >> 24 & 3;
    let mode = match config.mode {
        Mode::Quad if modes & 2 != 0 => 2,
        Mode::Quad | Mode::Dual if modes & 1 != 0 => 1,
        _ => 0,
    };
    let maximum = match caps >> 16 & 7 {
        0 => 20,
        1 => 25,
        2 => 33,
        3 => 50,
        4 => 66,
        _ => return Err(Error::Capability),
    };
    let (slave_rate, host_rate) = match config.frequency {
        Frequency::Mhz66 if maximum >= 66 => (4, 2),
        Frequency::Mhz66 | Frequency::Mhz33 if maximum >= 33 => (2, 1),
        _ => (0, 0),
    };
    let (slave_alert, host_alert) = match config.alert {
        Alert::InBand => (0, 0),
        Alert::PushPull => (1 << 28, 1 << 30),
        Alert::OpenDrain if caps & (1 << 19) != 0 => ((1 << 28) | (1 << 23), 1 << 30),
        Alert::OpenDrain => return Err(Error::Capability),
    };
    let crc = if config.crc { 1 << 31 } else { 0 };
    Ok((
        crc | slave_alert | mode << 26 | slave_rate << 20,
        crc | host_alert | mode << 28 | host_rate << 25,
    ))
}
fn channel(
    r: &mut impl Registers,
    timer: &impl Timer,
    address: u16,
    config: u32,
    host_bit: u32,
) -> Result<(), Error> {
    set(r, timer, address, config)?;
    if config & 1 == 0 {
        return Ok(());
    }
    let start = timer.timestamp_us();
    for _ in 0..=10000 {
        if get(r, timer, address)? & 2 != 0 {
            return update(r, 0x68, 0, host_bit);
        }
        if timer.timestamp_us().wrapping_sub(start) >= 10000 {
            break;
        }
        timer.delay_us(1);
    }
    Err(Error::ChannelTimeout)
}
/// Cold connection setup. Board eSPI GPIOs and SPI BAR must be ready first.
/// Any submission error requires stop/reset, never partial setup/retry. Do not
/// reset an already-active PSP/EC connection merely to rediscover CBI facts.
pub fn initialize(
    r: &mut impl Registers,
    timer: &impl Timer,
    config: &Config,
) -> Result<u32, Error> {
    let ranges = windows(config)?;
    r.write32(0x30, 8);
    r.write32(0x34, (23 << 13) | (31 << 8));
    r.write32(0x6c, 0);
    clear_status(r)?;
    r.write16(DECODE_ENABLE, 0);
    for i in 0..4 {
        let (base, size) = io_range(i);
        r.write16(base, 0);
        r.write8(size, 0);
    }
    for i in 0..4 {
        let (_, base, size) = mmio_range(i);
        r.write32(base, 0);
        r.write16(size, 0);
    }
    let initial = match config.alert {
        Alert::InBand => 0,
        _ => 1 << 30,
    };
    r.write32(0x68, initial);
    // Native workaround tolerates NO_RESPONSE only when reset COMPLETE is set.
    command(r, timer, 2, 0, 0, 1 << 4)?;
    r.write32(0x68, initial);
    let caps = get(r, timer, 8)?;
    let (slave, host) = general(config, caps)?;
    set(r, timer, 8, slave)?;
    r.write32(0x68, host);
    r.write32(0xac, config.irq_polarity); // BEFORE enabling VW.
    if config.virtual_wire {
        if caps & 2 == 0 {
            return Err(Error::Capability);
        }
        let slave_vw = get(r, timer, 0x20)?;
        let master = available(r.read32(0x2c))?;
        let count = (slave_vw >> 8 & 63).min(master >> 13 & 63);
        channel(r, timer, 0x20, 1 | count << 16, 4)?;
        command(r, timer, 5, 0, 3 | (0x20 << 8), 0)?; // Assert PLTRST
        command(r, timer, 5, 0, 3 | (0x22 << 8), 0)?; // Deassert PLTRST
    }
    let mut peripheral = get(r, timer, 0x10)?;
    if config.peripheral {
        if caps & 1 == 0 {
            return Err(Error::Capability);
        }
        peripheral |= 5;
    } else {
        peripheral &= !5;
    }
    channel(r, timer, 0x10, peripheral, 8)?;
    for (enabled, capability, address, bit) in
        [(config.oob, 4, 0x30, 2), (config.flash, 8, 0x40, 1)]
    {
        if !enabled {
            continue;
        }
        if caps & capability == 0 {
            return Err(Error::Capability);
        }
        let old = get(r, timer, address)?;
        channel(r, timer, address, old | 1, bit)?;
    }
    let standard = DecodeEnable::PORT80.val(config.port80.into())
        + DecodeEnable::KBC_6064.val(config.keyboard.into())
        + DecodeEnable::SIO_2E.val(config.superio.into());
    update(r, DECODE_ENABLE, 0, standard.value)?;
    for (i, range) in ranges.iter().enumerate() {
        let (base, size) = io_range(i);
        r.write16(base, range.base);
        r.write8(size, (range.size - 1) as u8);
        update(r, DECODE_ENABLE, 0, 1 << io_enable_bit(i))?;
    }
    update(
        r,
        0x34,
        if config.subtractive { 3 << 3 } else { 4 },
        if config.subtractive { 4 } else { 0 },
    )?;
    update(r, 0x34, 0, 2)?;
    available(r.read32(0x68))
}

pub struct Native {
    base: usize,
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl Native {
    /// # Safety
    /// Full 0x100-byte controller aperture decoded UC/exclusive, SPI BAR valid.
    pub unsafe fn new(spi_base: usize) -> Self {
        Self {
            base: spi_base + 0x10000,
            _exclusive: core::marker::PhantomData,
        }
    }
    fn register8(&self, offset: u16) -> &MmioReadWrite<u8> {
        assert!(offset < 0x100);
        unsafe { &*((self.base + usize::from(offset)) as *const MmioReadWrite<u8>) }
    }
    fn register16(&self, offset: u16) -> &MmioReadWrite<u16> {
        assert!(offset < 0xff && offset.is_multiple_of(2));
        unsafe { &*((self.base + usize::from(offset)) as *const MmioReadWrite<u16>) }
    }
    fn register32(&self, offset: u16) -> &MmioReadWrite<u32> {
        assert!(offset < 0xfd && offset.is_multiple_of(4));
        unsafe { &*((self.base + usize::from(offset)) as *const MmioReadWrite<u32>) }
    }
}
impl Registers for Native {
    fn read32(&mut self, offset: u16) -> u32 {
        self.register32(offset).get()
    }
    fn write32(&mut self, offset: u16, value: u32) {
        self.register32(offset).set(value);
    }
    fn read16(&mut self, offset: u16) -> u16 {
        self.register16(offset).get()
    }
    fn write16(&mut self, offset: u16, value: u16) {
        self.register16(offset).set(value);
    }
    fn write8(&mut self, offset: u16, value: u8) {
        self.register8(offset).set(value);
    }
}
