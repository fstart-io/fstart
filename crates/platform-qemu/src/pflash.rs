// SPDX-License-Identifier: MIT OR Apache-2.0
//! QEMU Intel CFI `pflash_cfi01` driver (`-drive if=pflash,format=raw,...`).
//!
//! Implements [`fstart_core::services::FlashRegion`] and
//! [`fstart_core::services::BootMedia`] over an 8-bit memory-mapped CFI NOR
//! flash window below 4 GiB (uncached MMIO). Supports 4 KiB sector erase,
//! buffered writes (`0xE8`, queried from CFI `QRY` table) with single-byte
//! (`0x40`) fallback, and bounded sub-windows ([`PflashSubRegion`]) for
//! CrabEFI's `NVRAM` variable store.
//!
//! All CFI command-mode transitions (`probe`, `erase`, `program`) execute from
//! `.ramcode` (copied into RAM alongside `.data`) and always return the flash
//! to Read Array mode (`0xFF`) before returning, so an XIP RO stage executing
//! from the same `pflash_cfi01` chip never fetches instructions while
//! `romd_mode` is disabled (which would fault both real CFI NOR flash and KVM).

use fstart_core::mmio::{read8, write8};
use fstart_core::services::{BootMedia, FlashRegion, ServiceError};

const CMD_READ_ARRAY: u8 = 0xff;
const CMD_CLEAR_STATUS: u8 = 0x50;
const CMD_BLOCK_ERASE: u8 = 0x20;
const CMD_CONFIRM: u8 = 0xd0;
const CMD_PROGRAM_BYTE: u8 = 0x40;
const CMD_WRITE_BUFFER: u8 = 0xe8;
const CMD_CFI_QUERY: u8 = 0x98;

const STATUS_READY: u8 = 0x80;
/// Erase error (bit 5), program error (bit 4), Vpp range error (bit 3).
const STATUS_ERROR_MASK: u8 = 0x38;

const RAM_OP_OK: u8 = 0;
const RAM_OP_HW_ERR: u8 = 1;
const RAM_OP_TIMEOUT: u8 = 2;

/// Erase sector granularity of QEMU Q35 `pflash_cfi01` (4 KiB).
pub const PFLASH_SECTOR_SIZE: u32 = 4096;

#[inline(always)]
unsafe fn raw_write_cmd(base: usize, offset: u32, cmd: u8) {
    // SAFETY: caller verified `offset < size` and `base..base+size` is MMIO.
    unsafe { write8((base + offset as usize) as *mut u8, cmd) }
}

#[inline(always)]
unsafe fn raw_read_byte(base: usize, offset: u32) -> u8 {
    // SAFETY: caller verified `offset < size` and `base..base+size` is MMIO.
    unsafe { read8((base + offset as usize) as *const u8) }
}

#[inline(always)]
unsafe fn raw_wait_ready(base: usize, offset: u32) -> u8 {
    let mut i = 0u32;
    while i < 10_000_000 {
        // SAFETY: caller validated bounds and MMIO mapping.
        let sr = unsafe { raw_read_byte(base, offset) };
        if sr & STATUS_READY != 0 {
            if sr & STATUS_ERROR_MASK != 0 {
                unsafe {
                    raw_write_cmd(base, offset, CMD_CLEAR_STATUS);
                    raw_write_cmd(base, offset, CMD_READ_ARRAY);
                }
                return RAM_OP_HW_ERR;
            }
            unsafe { raw_write_cmd(base, offset, CMD_READ_ARRAY) };
            return RAM_OP_OK;
        }
        core::hint::spin_loop();
        i += 1;
    }
    unsafe {
        raw_write_cmd(base, offset, CMD_CLEAR_STATUS);
        raw_write_cmd(base, offset, CMD_READ_ARRAY);
    }
    RAM_OP_TIMEOUT
}

/// RAM-resident CFI query leaf: never fetches from flash while out of Read Array mode.
#[cfg_attr(target_os = "none", unsafe(link_section = ".ramcode"))]
#[inline(never)]
unsafe fn ram_probe_cfi(base: usize) -> usize {
    let mut write_buf_size = 1usize;
    unsafe {
        raw_write_cmd(base, 0, CMD_CFI_QUERY);
        let q = raw_read_byte(base, 0x10);
        let r = raw_read_byte(base, 0x11);
        let y = raw_read_byte(base, 0x12);
        if q == b'Q' && r == b'R' && y == b'Y' {
            let exp_lo = raw_read_byte(base, 0x2a) as u32;
            let exp_hi = raw_read_byte(base, 0x2b) as u32;
            let exp = exp_lo | (exp_hi << 8);
            // In 8-bit mode the count-1 word is a single byte, so cap at 256 B.
            if (2..=12).contains(&exp) {
                let sz = 1usize << exp;
                write_buf_size = if sz > 256 { 256 } else { sz };
            }
        }
        raw_write_cmd(base, 0, CMD_CLEAR_STATUS);
        raw_write_cmd(base, 0, CMD_READ_ARRAY);
    }
    write_buf_size
}

/// RAM-resident 4 KiB sector erase leaf.
#[cfg_attr(target_os = "none", unsafe(link_section = ".ramcode"))]
#[inline(never)]
unsafe fn ram_erase_sector(base: usize, offset: u32) -> u8 {
    unsafe {
        raw_write_cmd(base, offset, CMD_CLEAR_STATUS);
        raw_write_cmd(base, offset, CMD_BLOCK_ERASE);
        raw_write_cmd(base, offset, CMD_CONFIRM);
        raw_wait_ready(base, offset)
    }
}

/// RAM-resident CFI Write-to-Buffer leaf (`ptr` must point into RAM, `1 <= len <= 256`).
#[cfg_attr(target_os = "none", unsafe(link_section = ".ramcode"))]
#[inline(never)]
unsafe fn ram_program_buffered(base: usize, offset: u32, ptr: *const u8, len: usize) -> u8 {
    let mut ready = false;
    let mut tries = 0u32;
    while tries < 100_000 {
        unsafe {
            raw_write_cmd(base, offset, CMD_WRITE_BUFFER);
            if raw_read_byte(base, offset) & STATUS_READY != 0 {
                ready = true;
                break;
            }
        }
        tries += 1;
    }
    if !ready {
        unsafe {
            raw_write_cmd(base, offset, CMD_CLEAR_STATUS);
            raw_write_cmd(base, offset, CMD_READ_ARRAY);
        }
        return RAM_OP_TIMEOUT;
    }
    unsafe {
        raw_write_cmd(base, offset, (len - 1) as u8);
        let mut i = 0usize;
        while i < len {
            let b = core::ptr::read(ptr.add(i));
            raw_write_cmd(base, offset + i as u32, b);
            i += 1;
        }
        raw_write_cmd(base, offset, CMD_CONFIRM);
        raw_wait_ready(base, offset)
    }
}

/// RAM-resident single-byte program leaf.
#[cfg_attr(target_os = "none", unsafe(link_section = ".ramcode"))]
#[inline(never)]
unsafe fn ram_program_byte(base: usize, offset: u32, byte: u8) -> u8 {
    unsafe {
        raw_write_cmd(base, offset, CMD_CLEAR_STATUS);
        raw_write_cmd(base, offset, CMD_PROGRAM_BYTE);
        raw_write_cmd(base, offset, byte);
        raw_wait_ready(base, offset)
    }
}

fn ram_op_result(code: u8) -> Result<(), ServiceError> {
    match code {
        RAM_OP_OK => Ok(()),
        RAM_OP_TIMEOUT => Err(ServiceError::Timeout),
        _ => Err(ServiceError::HardwareError),
    }
}

/// QEMU `pflash_cfi01` memory-mapped CFI NOR flash.
///
/// Intentionally neither `Copy` nor `Clone` so write-capable MMIO handles
/// cannot be duplicated around ownership-based write-lock guards.
pub struct Pflash {
    base: usize,
    size: u32,
    /// Minimum flash offset permitted for `erase`/`program` (set when a low
    /// sub-region such as NVRAM is split out into a disjoint [`PflashSubRegion`]).
    write_min_offset: u32,
    /// Buffered write chunk size in bytes (`1` disables `0xE8` and uses `0x40`).
    write_buf_size: usize,
}

impl Pflash {
    /// Construct and probe a `pflash_cfi01` window at `base..base+size`.
    ///
    /// Queries the CFI `QRY` table from RAM-resident code to discover
    /// buffered-write support, then leaves the device in Read Array mode (`0xFF`).
    ///
    /// # Safety
    ///
    /// `base..base+size` must be an identity-mapped, uncached MMIO window
    /// backed by a QEMU `pflash_cfi01` device (do NOT map it WP/WB via MTRRs).
    pub unsafe fn new(base: usize, size: u32) -> Self {
        let write_buf_size = if size >= 0x40 {
            // SAFETY: caller guarantees `base..base+size` is mapped pflash MMIO.
            unsafe { ram_probe_cfi(base) }
        } else {
            1
        };
        Self {
            base,
            size,
            write_min_offset: 0,
            write_buf_size,
        }
    }

    #[inline]
    fn read_byte(&self, offset: u32) -> u8 {
        // SAFETY: caller validated `offset < self.size` and MMIO window at `new`.
        unsafe { read8((self.base + offset as usize) as *const u8) }
    }

    /// Obtain a read-only [`BootMedia`] handle over this pflash window without
    /// duplicating write capability.
    #[must_use]
    pub const fn reader(&self) -> PflashReader {
        PflashReader {
            base: self.base,
            size: self.size,
        }
    }

    /// Split off the low `[0..low_size)` bytes (e.g. `NVRAM`) into a bounded
    /// [`PflashSubRegion`], restricting the remaining [`Pflash`] handle so it
    /// can no longer erase or program offsets below `low_size`.
    pub fn split_low_sub_region(
        mut self,
        low_size: u32,
    ) -> Result<(PflashSubRegion, Self), ServiceError> {
        if low_size == 0
            || low_size > self.size
            || !low_size.is_multiple_of(PFLASH_SECTOR_SIZE)
            || self.write_min_offset > 0
        {
            return Err(ServiceError::InvalidParam);
        }
        self.write_min_offset = low_size;
        let sub = PflashSubRegion {
            dev: Pflash {
                base: self.base,
                size: low_size,
                write_min_offset: 0,
                write_buf_size: self.write_buf_size,
            },
        };
        Ok((sub, self))
    }

    /// Total flash capacity in bytes.
    pub const fn capacity(&self) -> u32 {
        self.size
    }
}

/// Read-only [`BootMedia`] view of a `pflash_cfi01` window (no erase/program).
#[derive(Clone, Copy)]
pub struct PflashReader {
    base: usize,
    size: u32,
}

impl BootMedia for PflashReader {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let end = offset
            .checked_add(buf.len())
            .ok_or(ServiceError::InvalidParam)?;
        if end > self.size as usize {
            return Err(ServiceError::InvalidParam);
        }
        for (i, dst) in buf.iter_mut().enumerate() {
            // SAFETY: bounds verified against `self.size` and MMIO validated at `Pflash::new`.
            *dst = unsafe { read8((self.base + offset + i) as *const u8) };
        }
        Ok(buf.len())
    }

    fn size(&self) -> usize {
        self.size as usize
    }
}

impl FlashRegion for Pflash {
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
        let len = u32::try_from(buf.len()).map_err(|_| ServiceError::InvalidParam)?;
        let end = offset.checked_add(len).ok_or(ServiceError::InvalidParam)?;
        if end > self.size {
            return Err(ServiceError::InvalidParam);
        }
        for (i, dst) in buf.iter_mut().enumerate() {
            *dst = self.read_byte(offset + i as u32);
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        let end = offset
            .checked_add(length)
            .ok_or(ServiceError::InvalidParam)?;
        if offset < self.write_min_offset
            || !offset.is_multiple_of(PFLASH_SECTOR_SIZE)
            || !length.is_multiple_of(PFLASH_SECTOR_SIZE)
            || end > self.size
        {
            return Err(ServiceError::InvalidParam);
        }
        let mut cur = offset;
        while cur < end {
            // SAFETY: `cur < end <= self.size` and 4 KiB aligned.
            ram_op_result(unsafe { ram_erase_sector(self.base, cur) })?;
            cur += PFLASH_SECTOR_SIZE;
        }
        Ok(())
    }

    fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
        let len = u32::try_from(data.len()).map_err(|_| ServiceError::InvalidParam)?;
        let end = offset.checked_add(len).ok_or(ServiceError::InvalidParam)?;
        if offset < self.write_min_offset || end > self.size {
            return Err(ServiceError::InvalidParam);
        }
        if data.is_empty() {
            return Ok(());
        }
        if self.write_buf_size > 1 {
            let mut pos = 0usize;
            let mut ram_chunk = [0xffu8; 256];
            while pos < data.len() {
                let addr = offset + pos as u32;
                let align_rem = self.write_buf_size - (addr as usize % self.write_buf_size);
                let chunk_len = align_rem.min(data.len() - pos);
                let chunk = &data[pos..pos + chunk_len];
                if chunk_len == 1 {
                    let b = chunk[0];
                    if b != 0xff {
                        // Single-byte writes use 0x40 directly: QEMU pflash_cfi01
                        // skips the data cycle on 0xE8 when count-1 == 0.
                        // SAFETY: `addr < self.size`.
                        ram_op_result(unsafe { ram_program_byte(self.base, addr, b) })?;
                    }
                } else if chunk.iter().any(|&b| b != 0xff) {
                    // Copy to stack RAM before entering CFI command mode in case
                    // `data` itself lives inside the same pflash window.
                    ram_chunk[..chunk_len].copy_from_slice(chunk);
                    // SAFETY: `addr + chunk_len <= self.size`, `ram_chunk` is in RAM.
                    ram_op_result(unsafe {
                        ram_program_buffered(self.base, addr, ram_chunk.as_ptr(), chunk_len)
                    })?;
                }
                pos += chunk_len;
            }
        } else {
            for (i, &b) in data.iter().enumerate() {
                if b != 0xff {
                    // SAFETY: `offset + i < self.size`.
                    ram_op_result(unsafe { ram_program_byte(self.base, offset + i as u32, b) })?;
                }
            }
        }
        Ok(())
    }
}

impl BootMedia for Pflash {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.reader().read_at(offset, buf)
    }

    fn size(&self) -> usize {
        self.size as usize
    }
}

/// Bounded sub-region of [`Pflash`] implementing [`FlashRegion`], suitable for
/// CrabEFI's `FlashStorage` over `NVRAM`.
pub struct PflashSubRegion {
    dev: Pflash,
}

impl FlashRegion for PflashSubRegion {
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
        self.dev.read(offset, buf)
    }

    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        self.dev.erase(offset, length)
    }

    fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
        self.dev.program(offset, data)
    }
}
