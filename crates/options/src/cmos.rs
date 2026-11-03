//! Options in CMOS RAM, the battery-backed bytes of the mc146818 RTC.
//!
//! A [`Layout`] places each option at a bit range, as coreboot's
//! `cmos.layout` does, and guards them with coreboot's checksum: the sum of
//! the bytes of a range, stored big-endian in two bytes outside it. While
//! the checksum does not match (a new board, a dead battery, cleared CMOS)
//! the options are not set: the board's values boot. The first write then
//! fills every option of the layout with its default before setting one, so
//! a valid checksum never covers garbage.
//!
//! CMOS outlives a firmware update and is cleared by a jumper or the
//! battery; anything in ring 0 can write it, so it is for options whose
//! wrong value only costs a boot, not for security policy.

use crate::{Source, SourceError};

/// One byte-addressed CMOS RAM bank: index 0..0x7f behind ports 0x70/0x71
/// on every PC chipset. The first 14 bytes are the clock.
pub trait Ram {
    fn read(&self, index: u8) -> u8;
    fn write(&self, index: u8, value: u8);
}

impl<R: Ram + ?Sized> Ram for &R {
    fn read(&self, index: u8) -> u8 {
        (**self).read(index)
    }
    fn write(&self, index: u8, value: u8) {
        (**self).write(index, value)
    }
}

/// An option's place in CMOS: `width` bits from bit `bit` (LSB first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub key: &'static str,
    pub bit: u16,
    pub width: u8,
}

/// The options a board keeps in CMOS and their checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub entries: &'static [Entry],
    /// Bytes `first..=last` are summed...
    pub first: u8,
    pub last: u8,
    /// ...into the big-endian word at `checksum`, `checksum + 1`.
    pub checksum: u8,
}

/// Why a [`Layout`] is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// An entry or the checksum touches the clock or leaves bank 0.
    OutOfBank,
    /// An entry is not inside the summed range, or wider than 32 bits.
    Entry,
    /// Two entries share a bit, or the checksum overlaps the range.
    Overlap,
}

/// The clock registers, `0x00..=0x0d`.
const CLOCK_END: u8 = 0x0e;

impl Layout {
    /// Check the layout, at compile time when used in a `const`.
    pub const fn check(&self) -> Result<(), LayoutError> {
        if self.first < CLOCK_END
            || self.last > 0x7f
            || self.first > self.last
            || self.checksum < CLOCK_END
            || self.checksum > 0x7e
        {
            return Err(LayoutError::OutOfBank);
        }
        if self.checksum + 1 >= self.first && self.checksum <= self.last {
            return Err(LayoutError::Overlap);
        }
        let (low, high) = (self.first as u16 * 8, (self.last as u16 + 1) * 8);
        let mut i = 0;
        while i < self.entries.len() {
            let e = self.entries[i];
            if e.width == 0 || e.width > 32 || e.bit < low || e.bit + e.width as u16 > high {
                return Err(LayoutError::Entry);
            }
            let mut j = 0;
            while j < i {
                let o = self.entries[j];
                if e.bit < o.bit + o.width as u16 && o.bit < e.bit + e.width as u16 {
                    return Err(LayoutError::Overlap);
                }
                j += 1;
            }
            i += 1;
        }
        Ok(())
    }

    fn entry(&self, key: &str) -> Option<Entry> {
        self.entries.iter().copied().find(|e| e.key == key)
    }
}

/// A layout over a CMOS bank.
pub struct Cmos<'a, R: Ram> {
    layout: &'a Layout,
    ram: R,
}

/// Why a write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    /// The value does not fit the entry.
    TooWide,
    /// The bytes read back differ.
    Readback,
}

impl<'a, R: Ram> Cmos<'a, R> {
    pub const fn new(layout: &'a Layout, ram: R) -> Self {
        Self { layout, ram }
    }

    /// Whether the layout keeps `key`.
    pub fn has(&self, key: &str) -> bool {
        self.layout.entry(key).is_some()
    }

    fn sum(&self) -> u16 {
        (self.layout.first..=self.layout.last)
            .fold(0u16, |sum, i| sum.wrapping_add(u16::from(self.ram.read(i))))
    }

    /// Whether the stored checksum matches the options.
    pub fn valid(&self) -> bool {
        let at = self.layout.checksum;
        let stored = u16::from_be_bytes([self.ram.read(at), self.ram.read(at + 1)]);
        stored == self.sum()
    }

    fn get(&self, entry: Entry) -> u32 {
        (0..entry.width).fold(0u32, |value, k| {
            let bit = entry.bit + u16::from(k);
            let byte = self.ram.read((bit / 8) as u8);
            value | u32::from((byte >> (bit % 8)) & 1) << k
        })
    }

    fn put(&self, entry: Entry, value: u32) {
        for k in 0..entry.width {
            let bit = entry.bit + u16::from(k);
            let index = (bit / 8) as u8;
            let mask = 1u8 << (bit % 8);
            let byte = self.ram.read(index) & !mask;
            let set = if (value >> k) & 1 != 0 { mask } else { 0 };
            self.ram.write(index, byte | set);
        }
    }

    fn seal(&self) {
        let [high, low] = self.sum().to_be_bytes();
        self.ram.write(self.layout.checksum, high);
        self.ram.write(self.layout.checksum + 1, low);
    }

    /// Store `value` under `key`; `Ok(false)` if the layout has no `key`.
    /// With an invalid checksum, every other option first gets its
    /// `default`.
    pub fn write(
        &self,
        key: &str,
        value: u32,
        default: impl Fn(&str) -> u32,
    ) -> Result<bool, WriteError> {
        let Some(entry) = self.layout.entry(key) else {
            return Ok(false);
        };
        if entry.width < 32 && value >> entry.width != 0 {
            return Err(WriteError::TooWide);
        }
        if !self.valid() {
            for other in self.layout.entries {
                let fill = default(other.key);
                let fits = other.width == 32 || fill >> other.width == 0;
                self.put(*other, if fits { fill } else { 0 });
            }
        }
        self.put(entry, value);
        self.seal();
        if self.get(entry) == value && self.valid() {
            Ok(true)
        } else {
            Err(WriteError::Readback)
        }
    }
}

impl<R: Ram> Source for Cmos<'_, R> {
    fn name(&self) -> &'static str {
        if self.valid() {
            "CMOS"
        } else {
            "CMOS (checksum unset)"
        }
    }

    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        match self.layout.entry(key) {
            Some(entry) if self.valid() => Ok(Some(self.get(entry))),
            _ => Ok(None),
        }
    }
}
