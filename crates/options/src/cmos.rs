//! Option overrides in CMOS RAM, the battery-backed bytes of the mc146818
//! RTC.
//!
//! Nothing about the options is declared: an override is a record of its
//! key's hash and its value, in a window of bank 0 the platform does not
//! use otherwise. Any knob can be stored, only the values the user changed
//! take space, and a later firmware with more knobs reads the same records.
//!
//! ```text
//! first                                                last-1  last
//! | tag | hash lo | hash hi | value (LE) | ... | 0 | ... | sum hi | sum lo |
//! ```
//!
//! A record's `tag` says where its value is: `0x80 | value` holds a value
//! below 128 itself, 1 to 4 is the number of value bytes after the hash; 0
//! ends the records. An on/off or a choice is three bytes. The window's
//! last two bytes hold the byte sum of the others, big-endian, as coreboot
//! sums its CMOS. While the sum does not match (a new board, a dead
//! battery, cleared CMOS) the window holds no overrides and the board's
//! values boot; the first write starts it afresh.
//!
//! The window only has room for what differs from the board's values:
//! whoever stores an option [`Cmos::remove`]s it when it is the board's
//! value again.
//!
//! CMOS outlives a firmware update and is cleared by a jumper or the
//! battery; anything in ring 0 can write it, so it is for options whose
//! wrong value only costs a boot, not for security policy.

use crate::{Meta, Source, SourceError};

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

/// The bank-0 bytes `first..=last` the platform leaves to the options; the
/// last two hold the checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub first: u8,
    pub last: u8,
}

/// The clock registers, `0x00..=0x0d`.
const CLOCK_END: u8 = 0x0e;
/// The tag, the hash's two bytes.
const HEADER: usize = 3;
/// The longest value.
const VALUE: usize = 4;
/// The tag of a value it holds itself.
const INLINE: u8 = 0x80;

/// The bytes the record of `value` takes.
pub const fn record_size(value: u32) -> usize {
    if value < INLINE as u32 {
        HEADER
    } else {
        HEADER + (4 - value.leading_zeros() as usize / 8)
    }
}

impl Window {
    /// Whether the window lies in bank 0 past the clock and holds at least
    /// one record of each width.
    pub const fn check(&self) -> bool {
        self.first >= CLOCK_END
            && self.last <= 0x7f
            && self.last >= self.first
            && (self.last - self.first + 1) as usize >= HEADER + VALUE + 2
    }

    /// The record bytes, before the checksum.
    const fn records(&self) -> core::ops::Range<u8> {
        self.first..self.last - 1
    }

    /// The bytes the window has for records ([`record_size`]).
    pub const fn capacity(&self) -> usize {
        (self.last - 1 - self.first) as usize
    }
}

/// The record name of `key`: 16-bit FNV-1a, folded.
pub const fn hash(key: &str) -> u16 {
    let bytes = key.as_bytes();
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    ((h >> 16) ^ h) as u16
}

/// The first two of `metas` whose keys share a [`hash`], for a board test
/// over the knobs it offers with CMOS storage.
pub fn collision<'a, I>(metas: I) -> Option<(&'static str, &'static str)>
where
    I: IntoIterator<Item = &'a Meta> + Clone,
{
    for (i, meta) in metas.clone().into_iter().enumerate() {
        if let Some(other) = metas
            .clone()
            .into_iter()
            .take(i)
            .find(|o| o.key != meta.key && hash(o.key) == hash(meta.key))
        {
            return Some((other.key, meta.key));
        }
    }
    None
}

/// Why a write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    /// The records no longer fit the window.
    Full,
    /// The bytes read back differ.
    Readback,
}

/// The option records in a window of a CMOS bank.
pub struct Cmos<R: Ram> {
    window: Window,
    ram: R,
}

impl<R: Ram> Cmos<R> {
    pub const fn new(window: Window, ram: R) -> Self {
        Self { window, ram }
    }

    fn sum(&self) -> u16 {
        self.window
            .records()
            .fold(0u16, |sum, i| sum.wrapping_add(u16::from(self.ram.read(i))))
    }

    /// Whether the stored checksum matches the records.
    pub fn valid(&self) -> bool {
        let at = self.window.last - 1;
        u16::from_be_bytes([self.ram.read(at), self.ram.read(at + 1)]) == self.sum()
    }

    /// Each `(hash, value, offset, len)` record, in order, until `each`
    /// returns `false`; `Corrupt` for a malformed record. A window with an
    /// unset checksum has none.
    fn records(
        &self,
        mut each: impl FnMut(u16, u32, u8, usize) -> bool,
    ) -> Result<(), SourceError> {
        if !self.valid() {
            return Ok(());
        }
        let range = self.window.records();
        let mut at = range.start;
        while at < range.end {
            let tag = self.ram.read(at);
            if tag == 0 {
                break;
            }
            let (len, inline) = if tag & INLINE != 0 {
                (0, u32::from(tag & !INLINE))
            } else {
                (usize::from(tag), 0)
            };
            if len > VALUE || usize::from(range.end - at) < HEADER + len {
                return Err(SourceError::Corrupt);
            }
            let hash = u16::from_le_bytes([self.ram.read(at + 1), self.ram.read(at + 2)]);
            let value = (0..len).fold(inline, |v, k| {
                v | u32::from(self.ram.read(at + (HEADER + k) as u8)) << (8 * k)
            });
            if !each(hash, value, at, len) {
                break;
            }
            at += (HEADER + len) as u8;
        }
        Ok(())
    }

    /// Store `value` under `key`, keeping the other records. A window with
    /// an unset or corrupt checksum starts empty.
    pub fn write(&self, key: &str, value: u32) -> Result<(), WriteError> {
        self.rewrite(key, Some(value))
    }

    /// Drop the record of `key`, so the board's value boots and the record's
    /// bytes are free again. A window without overrides is left as it is.
    pub fn remove(&self, key: &str) -> Result<(), WriteError> {
        match self.read(key) {
            Ok(None) => Ok(()),
            _ => self.rewrite(key, None),
        }
    }

    /// The window's records without `key`'s, then `key`'s if it has a
    /// `value`: built in a window-sized buffer, then written back with the
    /// sum.
    fn rewrite(&self, key: &str, value: Option<u32>) -> Result<(), WriteError> {
        let name = hash(key);
        let mut bytes = [0u8; 0x80];
        let mut used = 0;
        let keep = |hash: u16, value: u32, bytes: &mut [u8; 0x80], used: &mut usize| {
            let record = record_size(value);
            let Some(slot) = bytes.get_mut(*used..*used + record) else {
                return false;
            };
            let len = record - HEADER;
            slot[0] = if len == 0 {
                INLINE | value as u8
            } else {
                len as u8
            };
            slot[1..HEADER].copy_from_slice(&hash.to_le_bytes());
            slot[HEADER..].copy_from_slice(&value.to_le_bytes()[..len]);
            *used += record;
            true
        };
        let mut fits = true;
        if self
            .records(|hash, value, _, _| {
                if hash != name {
                    fits &= keep(hash, value, &mut bytes, &mut used);
                }
                true
            })
            .is_err()
        {
            used = 0;
        }
        if let Some(value) = value {
            fits &= keep(name, value, &mut bytes, &mut used);
        }
        if !fits || used > self.window.capacity() {
            return Err(WriteError::Full);
        }
        for (i, at) in self.window.records().enumerate() {
            self.ram.write(at, bytes[i]);
        }
        let [high, low] = self.sum().to_be_bytes();
        self.ram.write(self.window.last - 1, high);
        self.ram.write(self.window.last, low);
        match self.read(key) {
            Ok(stored) if stored == value => Ok(()),
            _ => Err(WriteError::Readback),
        }
    }
}

impl<R: Ram> Source for Cmos<R> {
    fn name(&self) -> &'static str {
        if self.valid() {
            "CMOS"
        } else {
            "CMOS (checksum unset)"
        }
    }

    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        let name = hash(key);
        let mut found = None;
        self.records(|hash, value, _, _| {
            if hash == name {
                found = Some(value);
            }
            found.is_none()
        })?;
        Ok(found)
    }
}
