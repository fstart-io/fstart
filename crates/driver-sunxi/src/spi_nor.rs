//! Generic SPI NOR flash read layer.
//!
//! Coreboot splits SPI flash into a controller driver (full-duplex bursts
//! with chip-select handling, cf. `spi-generic.c`) and a NOR framing layer
//! (opcodes, addressing, vendor quirks, cf. `spi_flash.c`). This module is
//! the framing layer: it issues Read (0x03) / Fast Read (0x0B) commands over
//! any [`SpiBus`](embedded_hal::spi::SpiBus) controller and presents the chip
//! as a read-only [`BlockDevice`], which plugs into `BlockDeviceMedia` for
//! stage loading without touching the platform boot code.
//!
//! Only reads are implemented: the SRAM bootblock never writes flash.
//! Erase/write, 4-byte addressing, SFDP and vendor quirks are follow-ups;
//! `rflasher-core` is the opcode/quirk reference when they are needed.

use core::cell::RefCell;

use embedded_hal::spi::SpiBus;
use fstart_core::services::{BlockDevice, ServiceError};

/// SPI NOR Read Data Bytes: `[0x03, A23:A16, A15:A8, A7:A0, data...]`.
pub const NOR_CMD_READ: u8 = 0x03;
/// SPI NOR Fast Read: `[0x0B, A23:A16, A15:A8, A7:A0, dummy, data...]`.
/// The extra dummy byte gives the flash setup time above ~25 MHz.
pub const NOR_CMD_FAST_READ: u8 = 0x0B;
/// SPI NOR Read JEDEC ID: `[0x9F, manufacturer, memory-type, capacity]`.
pub const NOR_CMD_RDID: u8 = 0x9F;

/// Largest single chip-selected transfer. Matches the 64-byte sunxi FIFO;
/// every `SpiBus` controller used here must support at least this much.
pub const NOR_MAX_TRANSFER: usize = 64;

/// Clock rate above which Fast Read is required. Most NOR parts support
/// plain Read only to ~25-33 MHz.
pub const NOR_FAST_READ_THRESHOLD_HZ: u32 = 25_000_000;

/// Largest flash reachable with 3-byte addressing (16 MiB).
pub const NOR_MAX_3BYTE_SIZE: u32 = 0x0100_0000;

/// Controller failure, shared by the sunxi [`SpiBus`] implementations so
/// this layer can map errors without knowing the controller type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SunxiSpiError {
    /// Hardware did not complete the transfer in time.
    Timeout,
    /// FIFO overrun/underrun or other controller protocol error.
    Hardware,
    /// A single transfer exceeded the FIFO depth.
    TooLong,
}

impl embedded_hal::spi::Error for SunxiSpiError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        embedded_hal::spi::ErrorKind::Other
    }
}

fn service_error(error: SunxiSpiError) -> ServiceError {
    match error {
        SunxiSpiError::Timeout => ServiceError::Timeout,
        SunxiSpiError::TooLong => ServiceError::InvalidParam,
        SunxiSpiError::Hardware => ServiceError::HardwareError,
    }
}

/// Controller capability reporting whether the bus clock requires Fast Read.
///
/// Implemented by the sunxi [`SpiBus`] controllers; lets [`SpiNorFlash`]
/// take its framing flag from the controller instead of trusting the caller
/// to pair it correctly (plain Read above ~25 MHz clocks in garbage).
pub trait SpiClockReport: SpiBus<u8> {
    /// Whether the achieved bus clock exceeds [`NOR_FAST_READ_THRESHOLD_HZ`].
    fn fast_read(&self) -> bool;
}

/// Read-only SPI NOR flash on any [`SpiBus`] controller.
///
/// Commands are framed in a stack buffer and executed with a single
/// `transfer_in_place`: the command bytes clock out while the data bytes
/// clock in, so the controller stays a dumb full-duplex burst engine with
/// no NOR knowledge. Large reads are split into FIFO-sized chunks, each
/// with its own chip-select assertion.
pub struct SpiNorFlash<B> {
    bus: RefCell<B>,
    size: u32,
    fast_read: bool,
}

impl<B> SpiNorFlash<B> {
    /// Wrap an initialized controller. `fast_read` must match the bus
    /// clock (see [`NOR_FAST_READ_THRESHOLD_HZ`]); prefer
    /// [`from_controller`](Self::from_controller), which takes the flag
    /// from the controller's own clock report.
    #[must_use]
    pub fn new(bus: B, size: u32, fast_read: bool) -> Self {
        Self {
            bus: RefCell::new(bus),
            size,
            fast_read,
        }
    }

    /// Wrap an initialized controller, taking the Fast Read flag from its
    /// reported bus clock. This is the safe constructor for platform flows.
    #[must_use]
    pub fn from_controller(bus: B, size: u32) -> Self
    where
        B: SpiClockReport,
    {
        let fast_read = bus.fast_read();
        Self::new(bus, size, fast_read)
    }

    /// Command overhead: opcode + 3 address bytes (+ dummy for fast read).
    const fn cmd_len(&self) -> usize {
        if self.fast_read { 5 } else { 4 }
    }

    /// Read the JEDEC ID and reject absent/broken flash (all-zero/all-FF).
    /// Logs the ID for board bringup; size stays board-configured (RDID
    /// alone does not encode capacity — that needs SFDP).
    pub fn probe(&self) -> Result<[u8; 3], ServiceError>
    where
        B: SpiBus<u8, Error = SunxiSpiError>,
    {
        let mut frame = [NOR_CMD_RDID, 0, 0, 0];
        self.bus
            .borrow_mut()
            .transfer_in_place(&mut frame)
            .map_err(service_error)?;
        let id = [frame[1], frame[2], frame[3]];
        if id == [0, 0, 0] || id == [0xFF, 0xFF, 0xFF] {
            fstart_log::error!("spi-nor: no flash detected");
            return Err(ServiceError::HardwareError);
        }
        let jedec = (u32::from(id[0]) << 16) | (u32::from(id[1]) << 8) | u32::from(id[2]);
        fstart_log::info!(
            "spi-nor: JEDEC {}{}",
            fstart_log::Hex(u64::from(jedec)),
            if self.fast_read { " (fast read)" } else { "" }
        );
        Ok(id)
    }

    /// Single NOR read command, up to FIFO capacity minus the overhead.
    fn read_chunk(&self, addr: u32, buf: &mut [u8]) -> Result<usize, ServiceError>
    where
        B: SpiBus<u8, Error = SunxiSpiError>,
    {
        let cmd_len = self.cmd_len();
        let len = buf.len().min(NOR_MAX_TRANSFER - cmd_len);
        if len == 0 {
            return Ok(0);
        }
        let mut frame = [0u8; NOR_MAX_TRANSFER];
        frame[0] = if self.fast_read {
            NOR_CMD_FAST_READ
        } else {
            NOR_CMD_READ
        };
        frame[1] = (addr >> 16) as u8;
        frame[2] = (addr >> 8) as u8;
        frame[3] = addr as u8;
        if self.fast_read {
            frame[4] = 0xFF;
        }
        let total = cmd_len + len;
        self.bus
            .borrow_mut()
            .transfer_in_place(&mut frame[..total])
            .map_err(service_error)?;
        buf[..len].copy_from_slice(&frame[cmd_len..total]);
        Ok(len)
    }
}

// SAFETY: firmware is single-threaded here; the RefCell is never shared
// across execution contexts. Send-ness follows the owned controller.
unsafe impl<B: Send> Sync for SpiNorFlash<B> {}

/// Bound to [`SunxiSpiError`] so timeout diagnostics survive as
/// [`ServiceError::Timeout`] instead of collapsing into `HardwareError`.
impl<B> BlockDevice for SpiNorFlash<B>
where
    B: SpiBus<u8, Error = SunxiSpiError> + Send,
{
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset >= self.size as u64 {
            return Err(ServiceError::InvalidParam);
        }
        // Clamp to the flash end; offset < size (u32) so this fits in u32.
        let read_len = buf.len().min((self.size as u64 - offset) as usize);
        let mut pos = 0;
        let mut addr = offset as u32;
        while pos < read_len {
            let read = self.read_chunk(addr, &mut buf[pos..read_len])?;
            if read == 0 {
                return Err(ServiceError::HardwareError);
            }
            pos += read;
            addr += read as u32;
        }
        Ok(pos)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, ServiceError> {
        Err(ServiceError::NotSupported)
    }

    fn size(&self) -> u64 {
        self.size as u64
    }

    fn block_size(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::spi::{ErrorKind, ErrorType};

    /// In-memory NOR: backing store answers Read/RDID commands.
    struct MockBus {
        backing: [u8; 256],
        transfers: usize,
        last_opcode: u8,
        last_dummy: u8,
    }

    impl MockBus {
        fn new() -> Self {
            let mut backing = [0u8; 256];
            for (i, b) in backing.iter_mut().enumerate() {
                *b = (i & 0xFF) as u8;
            }
            Self {
                backing,
                transfers: 0,
                last_opcode: 0,
                last_dummy: 0,
            }
        }
    }

    impl ErrorType for MockBus {
        type Error = SunxiSpiError;
    }

    impl SpiBus<u8> for MockBus {
        fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
            words.fill(0xFF);
            Ok(())
        }

        fn write(&mut self, _words: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn transfer(&mut self, read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
            read.fill(0xFF);
            Ok(())
        }

        fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
            self.transfers += 1;
            self.last_opcode = words[0];
            match words[0] {
                NOR_CMD_RDID => {
                    words[1..4].copy_from_slice(&[0xEF, 0x40, 0x18]);
                }
                NOR_CMD_READ | NOR_CMD_FAST_READ => {
                    let fast = words[0] == NOR_CMD_FAST_READ;
                    let cmd_len = if fast { 5 } else { 4 };
                    if fast {
                        self.last_dummy = words[4];
                    }
                    let addr = (u32::from(words[1]) << 16)
                        | (u32::from(words[2]) << 8)
                        | u32::from(words[3]);
                    // Wipe the command echo like real full-duplex RX would.
                    words[..cmd_len].fill(0);
                    let data_len = words.len() - cmd_len;
                    for (i, slot) in words[cmd_len..].iter_mut().enumerate() {
                        *slot = self.backing[(addr as usize + i) % self.backing.len()];
                    }
                    let _ = data_len;
                }
                _ => return Err(SunxiSpiError::Hardware),
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn chunks_large_reads_to_fifo_size() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, false);
        let mut buf = [0u8; 100];
        assert_eq!(flash.read(0, &mut buf), Ok(100));
        // 60 + 40 data bytes per 64-byte transfer.
        assert_eq!(flash.bus.borrow().transfers, 2);
        let mut expect = [0u8; 100];
        for (i, slot) in expect.iter_mut().enumerate() {
            *slot = (i & 0xFF) as u8;
        }
        assert_eq!(buf, expect);
    }

    #[test]
    fn fast_read_uses_dummy_byte() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, true);
        let mut buf = [0u8; 10];
        assert_eq!(flash.read(16, &mut buf), Ok(10));
        assert_eq!(flash.bus.borrow().transfers, 1);
        assert_eq!(flash.bus.borrow().last_opcode, NOR_CMD_FAST_READ);
        assert_eq!(flash.bus.borrow().last_dummy, 0xFF);
        let mut expect = [0u8; 10];
        for (i, slot) in expect.iter_mut().enumerate() {
            *slot = ((16 + i) & 0xFF) as u8;
        }
        assert_eq!(buf, expect);
    }

    #[test]
    fn empty_read_returns_zero() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, false);
        assert_eq!(flash.read(0, &mut []), Ok(0));
        assert_eq!(flash.bus.borrow().transfers, 0);
    }

    #[test]
    fn clamps_reads_at_flash_end() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, false);
        let mut buf = [0u8; 32];
        assert_eq!(flash.read(240, &mut buf), Ok(16));
        assert_eq!(flash.read(256, &mut buf), Err(ServiceError::InvalidParam));
        assert_eq!(
            flash.read(u64::from(u32::MAX) + 1, &mut buf),
            Err(ServiceError::InvalidParam)
        );
    }

    #[test]
    fn probe_accepts_id_and_rejects_absent_flash() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, false);
        assert_eq!(flash.probe(), Ok([0xEF, 0x40, 0x18]));

        struct AbsentBus;
        impl ErrorType for AbsentBus {
            type Error = SunxiSpiError;
        }
        impl SpiBus<u8> for AbsentBus {
            fn read(&mut self, w: &mut [u8]) -> Result<(), Self::Error> {
                w.fill(0xFF);
                Ok(())
            }
            fn write(&mut self, _: &[u8]) -> Result<(), Self::Error> {
                Ok(())
            }
            fn transfer(&mut self, r: &mut [u8], _: &[u8]) -> Result<(), Self::Error> {
                r.fill(0xFF);
                Ok(())
            }
            fn transfer_in_place(&mut self, w: &mut [u8]) -> Result<(), Self::Error> {
                w.fill(0xFF);
                Ok(())
            }
            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        let absent = SpiNorFlash::new(AbsentBus, 256, false);
        assert_eq!(absent.probe(), Err(ServiceError::HardwareError));
        let _ = ErrorKind::Other;
    }

    #[test]
    fn write_is_not_supported() {
        let flash = SpiNorFlash::new(MockBus::new(), 256, false);
        assert_eq!(flash.write(0, &[1, 2, 3]), Err(ServiceError::NotSupported));
        assert_eq!(flash.block_size(), 1);
        assert_eq!(flash.size(), 256);
    }
}
