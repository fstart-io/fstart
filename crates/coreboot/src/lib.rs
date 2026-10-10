//! The coreboot payload interface.
//!
//! coreboot payloads (SeaBIOS, GRUB, libpayload programs, ...) learn about
//! the platform from the coreboot table: an `LBIO` header followed by tagged
//! records, found through a forwarding header in the first page of memory.
//! [`TableWriter`] writes the subset fstart has to offer.
//!
//! Files coreboot would keep in CBFS are handed over in RAM instead, one
//! [`tag::FSTART_FILE`] record each (see [`TableWriter::file`]), so neither
//! fstart nor its payloads need CBFS. Only payloads taught to read that
//! record see the files.
//!
//! Layouts follow coreboot's `commonlib/coreboot_tables.h`. 64-bit fields
//! there are only 4-byte aligned, so every record here is a packed
//! little-endian struct.

#![no_std]

pub mod manifest;

use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Record tags.
pub mod tag {
    pub const MEMORY: u32 = 0x01;
    pub const MAINBOARD: u32 = 0x03;
    pub const SERIAL: u32 = 0x0f;
    pub const FORWARD: u32 = 0x11;
    pub const FRAMEBUFFER: u32 = 0x12;
    pub const ACPI_RSDP: u32 = 0x43;
    /// fstart extension: one file handed to the payload in RAM. Outside
    /// coreboot's tag range ("FS" in the upper half), so readers that do not
    /// know it skip it.
    pub const FSTART_FILE: u32 = 0x4653_0001;
}

/// Memory range types of the [`tag::MEMORY`] record.
pub mod mem {
    pub const RAM: u32 = 1;
    pub const RESERVED: u32 = 2;
    pub const ACPI: u32 = 3;
    pub const NVS: u32 = 4;
    pub const UNUSABLE: u32 = 5;
    /// Firmware tables payloads may scan (coreboot's CBMEM). Payloads report
    /// it as reserved to the OS.
    pub const TABLE: u32 = 16;
}

/// Where coreboot leaves the forwarding header, and where payloads look for
/// it: they scan the first 4 KiB for an `LBIO` header.
pub const FORWARD_ADDR: u64 = 0x500;

/// [`Serial::kind`] of a port-I/O UART.
pub const SERIAL_IO_MAPPED: u32 = 1;
/// [`Serial::kind`] of an MMIO UART.
pub const SERIAL_MEMORY_MAPPED: u32 = 2;

const SIGNATURE: [u8; 4] = *b"LBIO";
/// Size of the table header that precedes the records.
pub const HEADER_SIZE: usize = core::mem::size_of::<Header>();
const RECORD_HEADER_SIZE: usize = core::mem::size_of::<RecordHeader>();
/// Records start and end on this boundary.
const RECORD_ALIGN: usize = 4;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    signature: [u8; 4],
    header_bytes: U32,
    header_checksum: U32,
    table_bytes: U32,
    table_checksum: U32,
    table_entries: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RecordHeader {
    tag: U32,
    size: U32,
}

/// One range of the [`tag::MEMORY`] record.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct MemoryRange {
    pub start: U64,
    pub size: U64,
    pub kind: U32,
}

impl MemoryRange {
    #[must_use]
    pub fn new(start: u64, size: u64, kind: u32) -> Self {
        Self {
            start: start.into(),
            size: size.into(),
            kind: kind.into(),
        }
    }
}

/// Body of the [`tag::SERIAL`] record.
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Serial {
    /// [`SERIAL_IO_MAPPED`] or [`SERIAL_MEMORY_MAPPED`].
    pub kind: U32,
    pub base: U32,
    pub baud: U32,
    /// Register stride in bytes.
    pub regwidth: U32,
    /// UART input clock.
    pub input_hertz: U32,
}

/// Body of the [`tag::FRAMEBUFFER`] record.
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Framebuffer {
    pub physical_address: U64,
    pub x_resolution: U32,
    pub y_resolution: U32,
    pub bytes_per_line: U32,
    pub bits_per_pixel: u8,
    pub red_mask_pos: u8,
    pub red_mask_size: u8,
    pub green_mask_pos: u8,
    pub green_mask_size: u8,
    pub blue_mask_pos: u8,
    pub blue_mask_size: u8,
    pub reserved_mask_pos: u8,
    pub reserved_mask_size: u8,
    pub orientation: u8,
    pub flags: u8,
    pub pad: u8,
}

impl Framebuffer {
    /// Describe the pixel bits above the colour channels as the reserved
    /// mask, as coreboot does for 32 bpp XRGB. SeaVGABIOS takes the sum of
    /// all mask sizes as the pixel depth, so 8:8:8 without it reads as 24 bpp.
    #[must_use]
    pub fn with_reserved_mask(mut self) -> Self {
        let channels = [
            (self.red_mask_pos, self.red_mask_size),
            (self.green_mask_pos, self.green_mask_size),
            (self.blue_mask_pos, self.blue_mask_size),
        ];
        let used: u8 = channels.iter().map(|(_, size)| size).sum();
        let top = channels
            .iter()
            .map(|(pos, size)| pos + size)
            .max()
            .unwrap_or(0);
        if self.bits_per_pixel > used && top + (self.bits_per_pixel - used) <= self.bits_per_pixel {
            self.reserved_mask_pos = top;
            self.reserved_mask_size = self.bits_per_pixel - used;
        }
        self
    }
}

/// Body of the [`tag::FSTART_FILE`] record, followed by the NUL-terminated
/// file name.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FileHeader {
    address: U64,
    size: U32,
    reserved: U32,
}

/// The output buffer cannot hold the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// Appends records to a coreboot table in a caller-owned buffer.
pub struct TableWriter<'a> {
    buf: &'a mut [u8],
    used: usize,
    entries: u32,
}

impl<'a> TableWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Result<Self, Full> {
        if buf.len() < HEADER_SIZE {
            return Err(Full);
        }
        Ok(Self {
            buf,
            used: HEADER_SIZE,
            entries: 0,
        })
    }

    /// Reserve a record of `body_len` bytes, write its header and return
    /// its zeroed body.
    fn record(&mut self, tag: u32, body_len: usize) -> Result<&mut [u8], Full> {
        let size = (RECORD_HEADER_SIZE + body_len).next_multiple_of(RECORD_ALIGN);
        let end = self.used.checked_add(size).ok_or(Full)?;
        let record = self.buf.get_mut(self.used..end).ok_or(Full)?;
        record.fill(0);
        let header = RecordHeader {
            tag: tag.into(),
            size: (size as u32).into(),
        };
        record[..RECORD_HEADER_SIZE].copy_from_slice(header.as_bytes());
        self.used = end;
        self.entries += 1;
        Ok(&mut record[RECORD_HEADER_SIZE..])
    }

    fn fixed(&mut self, tag: u32, body: &[u8]) -> Result<(), Full> {
        self.record(tag, body.len())?[..body.len()].copy_from_slice(body);
        Ok(())
    }

    /// The memory map: `ranges` (start, size, e820 type) with every part
    /// that overlaps one of `tables` reported as [`mem::TABLE`] instead.
    /// Payloads scan table memory for ACPI/SMBIOS entry points.
    pub fn memory(
        &mut self,
        ranges: impl IntoIterator<Item = (u64, u64, u32)>,
        tables: &[(u64, u64)],
    ) -> Result<(), Full> {
        let mut sorted = [(0u64, 0u64); 8];
        let sorted = sorted.get_mut(..tables.len()).ok_or(Full)?;
        sorted.copy_from_slice(tables);
        sorted.sort_unstable();

        let mut pieces = [MemoryRange::new(0, 0, 0); 64];
        let mut count = 0;
        let mut push = |start: u64, end: u64, kind: u32| {
            if end > start {
                *pieces.get_mut(count).ok_or(Full)? = MemoryRange::new(start, end - start, kind);
                count += 1;
            }
            Ok(())
        };
        for (start, size, kind) in ranges {
            let end = start.saturating_add(size);
            let mut pos = start;
            for &(table, table_size) in sorted.iter() {
                let table_end = table.saturating_add(table_size);
                if table_end <= pos || table >= end {
                    continue;
                }
                let low = table.max(pos);
                let high = table_end.min(end);
                push(pos, low, kind)?;
                push(low, high, mem::TABLE)?;
                pos = high;
            }
            push(pos, end, kind)?;
        }
        let pieces = pieces[..count].as_bytes();
        self.fixed(tag::MEMORY, pieces)
    }

    pub fn mainboard(&mut self, vendor: &str, part: &str) -> Result<(), Full> {
        // Two index bytes, then both strings NUL-terminated.
        let len = 2 + vendor.len() + 1 + part.len() + 1;
        let body = self.record(tag::MAINBOARD, len)?;
        body[0] = 0;
        body[1] = (vendor.len() + 1) as u8;
        body[2..2 + vendor.len()].copy_from_slice(vendor.as_bytes());
        let part_at = 2 + vendor.len() + 1;
        body[part_at..part_at + part.len()].copy_from_slice(part.as_bytes());
        Ok(())
    }

    pub fn serial(&mut self, serial: &Serial) -> Result<(), Full> {
        self.fixed(tag::SERIAL, serial.as_bytes())
    }

    pub fn framebuffer(&mut self, framebuffer: &Framebuffer) -> Result<(), Full> {
        self.fixed(tag::FRAMEBUFFER, framebuffer.as_bytes())
    }

    pub fn acpi_rsdp(&mut self, rsdp: u64) -> Result<(), Full> {
        self.fixed(tag::ACPI_RSDP, U64::new(rsdp).as_bytes())
    }

    /// Point at the real table from a forwarding header.
    pub fn forward(&mut self, table: u64) -> Result<(), Full> {
        self.fixed(tag::FORWARD, U64::new(table).as_bytes())
    }

    /// A file of `size` bytes at physical `address`, named `name`.
    pub fn file(&mut self, name: &str, address: u64, size: u32) -> Result<(), Full> {
        let header = FileHeader {
            address: address.into(),
            size: size.into(),
            reserved: 0.into(),
        };
        let header = header.as_bytes();
        let body = self.record(tag::FSTART_FILE, header.len() + name.len() + 1)?;
        body[..header.len()].copy_from_slice(header);
        body[header.len()..header.len() + name.len()].copy_from_slice(name.as_bytes());
        Ok(())
    }

    /// Write the header and its checksums; return the complete table.
    pub fn finish(self) -> &'a [u8] {
        let (header, records) = self.buf[..self.used].split_at_mut(HEADER_SIZE);
        let mut value = Header {
            signature: SIGNATURE,
            header_bytes: (HEADER_SIZE as u32).into(),
            header_checksum: 0.into(),
            table_bytes: (records.len() as u32).into(),
            table_checksum: u32::from(checksum(records)).into(),
            table_entries: self.entries.into(),
        };
        value.header_checksum = u32::from(checksum(value.as_bytes())).into();
        header.copy_from_slice(value.as_bytes());
        &self.buf[..self.used]
    }
}

/// coreboot's `compute_ip_checksum`: the one's complement of the
/// one's-complement sum of little-endian 16-bit words.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u16 {
    let sum = bytes
        .chunks(2)
        .map(|word| u32::from(word[0]) | u32::from(*word.get(1).unwrap_or(&0)) << 8)
        .fold(0u32, |sum, word| {
            let sum = sum + word;
            (sum & 0xffff) + (sum >> 16)
        });
    !(sum as u16)
}

#[cfg(test)]
mod tests;
