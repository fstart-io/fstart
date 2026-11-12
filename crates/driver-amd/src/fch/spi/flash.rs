// SPDX-License-Identifier: MIT OR Apache-2.0
//! One reserved region of the boot flash through the SPI100 command engine
//! (rflasher's `amd_spi100` controller and SPI25 protocol), for data
//! firmware keeps across boots.
//!
//! The region is bound at construction ([`Extent`]); every access is
//! checked against it. Reads go through the command engine too, not the
//! ROM windows: those may serve stale lines after a write.
//!
//! A part larger than 16 MiB may be in 4-byte address mode (the PSP puts
//! the S8030's 32 MiB part there), where every legacy opcode takes four
//! address bytes. [`Flash::new`] reads a range the caller has from the ROM
//! window with 3- and with 4-byte addresses and uses whichever returns the
//! same bytes; if neither does, the region stays read-only.

use core::cell::Cell;

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use fstart_core::services::{FlashRegion, ServiceError};
use rflasher_core::protocol::{self as spi25, CommandAddressing};
use rflasher_core::spi::{SpiCommand, opcodes};
use rflasher_internal::controller::Controller;
use rflasher_internal::{
    HostAccess, InternalError, MmioAccess, PciAddress, PciConfigAccess, Spi100Controller,
};

use crate::fch::SPI_BASE;
use embedded_hal::delay::DelayNs;

/// Highest address a 3-byte command reaches.
const LIMIT_3BYTE: u32 = 16 << 20;
/// The 4 KiB sector erase (opcode 0x20).
pub const ERASE_SIZE: u32 = 0x1000;
/// Program chunk: the SPI100 FIFO (71 bytes) holds the opcode, a 4-byte
/// address and 66 bytes; page programs must not cross 256-byte pages.
const PROGRAM_CHUNK: usize = 64;
const PAGE: usize = 256;
/// 4 KiB sector erase timing (flashprog): poll every 10 ms, give up at 1 s.
const ERASE_POLL_US: u32 = 10_000;
const ERASE_TIMEOUT_US: u32 = 1_000_000;

/// Run an rflasher operation. The SPI100 controller completes every
/// command before returning, so its futures never wait.
fn now<T>(operation: impl Future<Output = rflasher_core::Result<T>>) -> Result<T, ServiceError> {
    match pin!(operation).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result.map_err(|_| ServiceError::HardwareError),
        Poll::Pending => Err(ServiceError::HardwareError),
    }
}
/// The SPI100 register window rflasher maps.
const WINDOW: usize = 0x100;

/// A flash region, in absolute flash offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub offset: u32,
    pub size: u32,
}

impl Extent {
    /// The absolute address of `len` bytes at region offset `offset`.
    pub fn address(self, offset: u32, len: usize) -> Result<u32, ServiceError> {
        let len = u32::try_from(len).map_err(|_| ServiceError::InvalidParam)?;
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(self.offset + offset),
            _ => Err(ServiceError::InvalidParam),
        }
    }
}

/// The SPI100 registers, forwarded to the canonical MMIO accessors.
#[derive(Clone, Copy)]
struct Registers;

impl Registers {
    fn at(offset: usize) -> usize {
        assert!(offset < WINDOW, "SPI100 register offset");
        SPI_BASE as usize + offset
    }
}

// SAFETY (all accessors): `Host::map_mmio` hands this out only for the
// SPI100 window at `SPI_BASE`, which the FCH decodes; `at` bounds offsets.
impl MmioAccess for Registers {
    fn read8(&self, offset: usize) -> u8 {
        unsafe { fstart_core::mmio::read8(Self::at(offset) as *const u8) }
    }
    fn read16(&self, offset: usize) -> u16 {
        unsafe { fstart_core::mmio::read16(Self::at(offset) as *const u16) }
    }
    fn read32(&self, offset: usize) -> u32 {
        unsafe { fstart_core::mmio::read32(Self::at(offset) as *const u32) }
    }
    fn write8(&self, offset: usize, value: u8) {
        unsafe { fstart_core::mmio::write8(Self::at(offset) as *mut u8, value) }
    }
    fn write16(&self, offset: usize, value: u16) {
        unsafe { fstart_core::mmio::write16(Self::at(offset) as *mut u16, value) }
    }
    fn write32(&self, offset: usize, value: u32) {
        unsafe { fstart_core::mmio::write32(Self::at(offset) as *mut u32, value) }
    }
}

/// rflasher's host: the SPI100 window and a delay. The SPI100 driver
/// needs no PCI config space.
struct Host<D> {
    delay: Cell<D>,
}

impl<D> PciConfigAccess for Host<D> {
    type Error = InternalError;
    fn read8(&self, _: PciAddress, _: u16) -> Result<u8, InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
    fn read16(&self, _: PciAddress, _: u16) -> Result<u16, InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
    fn read32(&self, _: PciAddress, _: u16) -> Result<u32, InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
    fn write8(&self, _: PciAddress, _: u16, _: u8) -> Result<(), InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
    fn write16(&self, _: PciAddress, _: u16, _: u16) -> Result<(), InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
    fn write32(&self, _: PciAddress, _: u16, _: u32) -> Result<(), InternalError> {
        Err(InternalError::Io("no PCI access"))
    }
}

impl<D: DelayNs + Copy> HostAccess for Host<D> {
    type MmioRegion = Registers;
    unsafe fn map_mmio(&self, address: u64, size: usize) -> Result<Registers, InternalError> {
        if address != u64::from(SPI_BASE) || size > WINDOW {
            return Err(InternalError::Io("not the FCH SPI100 window"));
        }
        Ok(Registers)
    }
    fn delay_us(&self, us: u32) {
        let mut delay = self.delay.get();
        delay.delay_us(us);
    }
}

/// One flash region through the SPI100 command engine.
pub struct Flash<D: DelayNs + Copy> {
    controller: Spi100Controller<Host<D>>,
    extent: Extent,
    /// How the part takes addresses; `None`: unknown, read-only.
    addressing: Option<CommandAddressing>,
}

impl<D: DelayNs + Copy> Flash<D> {
    /// Bind `extent`. Writes are allowed only if it is 4 KiB aligned and the
    /// command engine reads `expected` at flash offset `probe` (bytes the
    /// caller read through the ROM window) in the part's address mode.
    ///
    /// # Safety
    /// The FCH decodes the SPI100 registers at [`SPI_BASE`] and nothing else
    /// uses the command engine while this exists. The caller runs from RAM.
    pub unsafe fn new(
        delay: D,
        extent: Extent,
        probe: u32,
        expected: &[u8],
    ) -> Result<Self, ServiceError> {
        let host = Host {
            delay: Cell::new(delay),
        };
        let mut controller = Spi100Controller::new_with_host(host, u64::from(SPI_BASE), None, 0)
            .map_err(|_| ServiceError::HardwareError)?;
        let mut id = [0u8; 3];
        let identified = controller
            .execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut id))
            .is_ok();
        let aligned = extent.offset.checked_add(extent.size).is_some()
            && extent.offset.is_multiple_of(ERASE_SIZE)
            && extent.size.is_multiple_of(ERASE_SIZE);
        // Erased bytes read the same at any address: they prove nothing.
        let expected = &expected[..expected.len().min(64)];
        let meaningful = identified && aligned && expected.iter().any(|&b| b != 0xff);
        let mut readback = [0u8; 64];
        let readback = &mut readback[..expected.len()];
        let mut reads_as = |addressing| {
            now(spi25::read_with_addressing(
                &mut controller,
                opcodes::READ,
                probe,
                readback,
                addressing,
            ))
            .is_ok()
                && *readback == *expected
        };
        let addressing = if !meaningful {
            None
        } else if probe < LIMIT_3BYTE
            && extent.offset + extent.size <= LIMIT_3BYTE
            && reads_as(CommandAddressing::ThreeByte)
        {
            Some(CommandAddressing::ThreeByte)
        } else if reads_as(CommandAddressing::FourByte) {
            Some(CommandAddressing::FourByte)
        } else {
            None
        };
        fstart_log::info!(
            "spi: JEDEC {:#x}:{:#x}:{:#x}, region {:#x}+{:#x} {}",
            id[0],
            id[1],
            id[2],
            extent.offset,
            extent.size,
            match addressing {
                Some(CommandAddressing::ThreeByte) => "writable, 3-byte addresses",
                Some(_) => "writable, 4-byte addresses",
                None if !meaningful => "read-only (no probe)",
                None => "read-only (reads do not match the ROM window)",
            }
        );
        Ok(Self {
            controller,
            extent,
            addressing,
        })
    }

    /// Whether [`Self::new`] allowed writes.
    pub fn writable(&self) -> bool {
        self.addressing.is_some()
    }

    pub fn extent(&self) -> Extent {
        self.extent
    }

    fn addressing(&self) -> Result<CommandAddressing, ServiceError> {
        self.addressing.ok_or(ServiceError::NotSupported)
    }
}

impl<D: DelayNs + Copy> FlashRegion for Flash<D> {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, bytes.len())?;
        let addressing = self.addressing()?;
        now(spi25::read_with_addressing(
            &mut self.controller,
            opcodes::READ,
            address,
            bytes,
            addressing,
        ))
    }

    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, bytes.len())?;
        let addressing = self.addressing()?;
        let mut done = 0;
        while done < bytes.len() {
            let at = address + done as u32;
            let len = (bytes.len() - done)
                .min(PROGRAM_CHUNK)
                .min(PAGE - at as usize % PAGE);
            now(spi25::program_page_with_addressing(
                &mut self.controller,
                opcodes::PP,
                at,
                &bytes[done..done + len],
                addressing,
            ))?;
            done += len;
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, length as usize)?;
        let addressing = self.addressing()?;
        if !address.is_multiple_of(ERASE_SIZE) || !length.is_multiple_of(ERASE_SIZE) {
            return Err(ServiceError::InvalidParam);
        }
        for sector in (address..address + length).step_by(ERASE_SIZE as usize) {
            now(spi25::erase_block(
                &mut self.controller,
                opcodes::SE_20,
                sector,
                addressing,
                ERASE_POLL_US,
                ERASE_TIMEOUT_US,
            ))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extent_bounds_are_relative_and_closed() {
        let extent = Extent {
            offset: 0xda_0000,
            size: 0x4_0000,
        };
        assert_eq!(extent.address(0, 0x4_0000), Ok(0xda_0000));
        assert_eq!(extent.address(0x3_ffff, 1), Ok(0xdd_ffff));
        assert!(extent.address(0x4_0000, 1).is_err());
        assert!(extent.address(u32::MAX, 2).is_err());
        assert!(extent.address(0, usize::MAX).is_err());
    }
}
