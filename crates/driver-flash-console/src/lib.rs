//! A debug console appended to an erased, reserved flash region, after
//! coreboot `drivers/spi/flashconsole.c` (`CONSOLE_SPI_FLASH`).
//!
//! Each stage looks for the end of the log the previous stages left, the
//! first erased (0xff) byte, and appends after it. Like coreboot, the console
//! never erases the region: an erase would drop the earlier stages' output.
//! Output is buffered per line and programmed on a newline, a full buffer, an
//! explicit flush, or at the end of the region; once the region is full or a
//! write fails, the console goes offline for the rest of the stage. Read the
//! log back with a programmer, or erase the region to reuse it.
//!
//! The flash access is the platform's: a [`Medium`] reads and programs the
//! region through the chipset's SPI controller.
//!
//! A stage that executes in place from the chip it programs must not fetch
//! from it while a program cycle runs. With an [`XipImage`], the console
//! reads the whole running image through its (cacheable) mapping before
//! every write, so the program cycle and its status polling hit the cache,
//! the same protection coreboot's flash console has in XIP stages. Anything
//! else the stage reads from the chip during a write (page tables, say) is
//! the platform's to move out of the way first.

#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::services::{Console, ServiceError};

/// coreboot `LINE_BUFFER_SIZE`.
const LINE_BUFFER_SIZE: usize = 128;
const CACHE_LINE: usize = 64;

/// The flash region a [`FlashConsole`] writes; offsets are within it.
pub trait Medium {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError>;
    /// Program erased bytes; returns once the chip is idle again.
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError>;
}

/// A stage image executing in place from the flash mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XipImage {
    pub base: usize,
    pub size: usize,
}

/// The append position and line buffer; `medium` is `None` once offline.
struct Log<M> {
    medium: Option<M>,
    size: u32,
    offset: u32,
    line: [u8; LINE_BUFFER_SIZE],
    length: usize,
}

impl<M: Medium> Log<M> {
    const fn offline() -> Self {
        Self {
            medium: None,
            size: 0,
            offset: 0,
            line: [0; LINE_BUFFER_SIZE],
            length: 0,
        }
    }

    /// coreboot `flashconsole_init()`: continue after the first erased byte.
    /// Earlier stages only append, so the region is a log without 0xff
    /// followed by erased bytes, and a binary search finds the end.
    fn open(mut medium: M, size: u32) -> Self {
        let (mut low, mut high) = (0, size);
        while low < high {
            let middle = low + (high - low) / 2;
            let mut byte = [0];
            if medium.read(middle, &mut byte).is_err() {
                return Self::offline();
            }
            if byte[0] == 0xff {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        if low >= size {
            // No space left in the console region.
            return Self::offline();
        }
        Self {
            medium: Some(medium),
            size,
            offset: low,
            ..Self::offline()
        }
    }

    /// coreboot `flashconsole_tx_byte()`.
    fn push(&mut self, byte: u8, before_write: impl Fn()) -> Result<(), ServiceError> {
        if self.medium.is_none() {
            return Err(ServiceError::NotInitialized);
        }
        self.line[self.length] = byte;
        self.length += 1;
        if self.length == LINE_BUFFER_SIZE
            || self.offset as usize + self.length >= self.size as usize
            || byte == b'\n'
        {
            return self.flush(before_write);
        }
        Ok(())
    }

    /// coreboot `flashconsole_tx_flush()`.
    fn flush(&mut self, before_write: impl Fn()) -> Result<(), ServiceError> {
        let Some(medium) = self.medium.as_mut() else {
            return Ok(());
        };
        let length = self.length.min((self.size - self.offset) as usize);
        self.length = 0;
        if length == 0 {
            return Ok(());
        }
        before_write();
        if let Err(error) = medium.program(self.offset, &self.line[..length]) {
            self.medium = None;
            return Err(error);
        }
        self.offset += length as u32;
        if self.offset >= self.size {
            // The region is full: stop future write attempts.
            self.medium = None;
        }
        Ok(())
    }
}

pub struct FlashConsole<M> {
    xip: Option<XipImage>,
    busy: AtomicBool,
    log: UnsafeCell<Log<M>>,
}

// SAFETY: every access to `log` holds `busy`.
unsafe impl<M: Send> Send for FlashConsole<M> {}
// SAFETY: as for Send.
unsafe impl<M: Send> Sync for FlashConsole<M> {}

impl<M: Medium> FlashConsole<M> {
    /// Continue the log in the `size`-byte region `medium` reaches. Offline
    /// if the region cannot be read or is full.
    pub fn open(medium: M, size: u32, xip: Option<XipImage>) -> Self {
        Self {
            xip,
            busy: AtomicBool::new(false),
            log: UnsafeCell::new(Log::open(medium, size)),
        }
    }

    /// Whether the console still writes.
    pub fn online(&self) -> bool {
        let mut online = false;
        let _ = self.with_log(|log, _| {
            online = log.medium.is_some();
            Ok(())
        });
        online
    }

    /// Bring the running XIP image into the cache before the chip turns busy.
    fn warm_xip(xip: Option<XipImage>) {
        let Some(image) = xip else {
            return;
        };
        for offset in (0..image.size).step_by(CACHE_LINE) {
            // SAFETY: the running image is mapped and readable.
            unsafe { fstart_core::mmio::read8((image.base + offset) as *const u8) };
        }
    }

    fn with_log(
        &self,
        f: impl FnOnce(&mut Log<M>, &dyn Fn()) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        // A recursive call (the medium logging) must not spin or re-enter.
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(ServiceError::IoError);
        }
        let xip = self.xip;
        // SAFETY: busy exclusively guards the cell for the closure's lifetime.
        let result = f(unsafe { &mut *self.log.get() }, &|| Self::warm_xip(xip));
        self.busy.store(false, Ordering::Release);
        result
    }
}

impl<M: Medium + Send> Console for FlashConsole<M> {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        self.with_log(|log, warm| log.push(byte, warm))
    }
    fn write_bytes(&self, bytes: &[u8]) -> Result<(), ServiceError> {
        self.with_log(|log, warm| bytes.iter().try_for_each(|&byte| log.push(byte, warm)))
    }
    fn flush(&self) -> Result<(), ServiceError> {
        self.with_log(|log, warm| log.flush(warm))
    }
    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    struct Ram {
        bytes: Vec<u8>,
        fail: bool,
    }
    impl Medium for Ram {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
            let o = offset as usize;
            bytes.copy_from_slice(&self.bytes[o..o + bytes.len()]);
            Ok(())
        }
        fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
            if self.fail {
                return Err(ServiceError::HardwareError);
            }
            for (cell, byte) in self.bytes[offset as usize..].iter_mut().zip(bytes) {
                // NOR programming only clears bits.
                *cell &= *byte;
            }
            Ok(())
        }
    }
    fn ram(written: &[u8], size: usize) -> Ram {
        let mut bytes = std::vec![0xff; size];
        bytes[..written.len()].copy_from_slice(written);
        Ram { bytes, fail: false }
    }
    fn write(log: &mut Log<Ram>, text: &[u8]) {
        for &byte in text {
            let _ = log.push(byte, || {});
        }
    }

    #[test]
    fn appends_after_the_previous_stage() {
        for previous in [&b""[..], b"a", b"bootblock\n", &[b'x'; 999]] {
            let mut log = Log::open(ram(previous, 1024), 1024);
            assert_eq!(log.offset as usize, previous.len());
            write(&mut log, b"postcar\n");
            let bytes = &log.medium.as_ref().unwrap().bytes;
            assert_eq!(&bytes[..previous.len()], previous);
            assert_eq!(&bytes[previous.len()..][..8], b"postcar\n");
            assert_eq!(bytes[previous.len() + 8], 0xff);
        }
    }

    #[test]
    fn programs_lines_full_buffers_and_explicit_flushes() {
        let mut log = Log::open(ram(b"", 4096), 4096);
        write(&mut log, b"partial");
        assert_eq!(log.medium.as_ref().unwrap().bytes[0], 0xff);
        let _ = log.flush(|| {});
        assert_eq!(&log.medium.as_ref().unwrap().bytes[..7], b"partial");
        write(&mut log, &[b'y'; LINE_BUFFER_SIZE]);
        assert_eq!(log.offset as usize, 7 + LINE_BUFFER_SIZE);
    }

    #[test]
    fn a_full_region_or_failed_write_goes_offline() {
        assert!(Log::open(ram(&[b'z'; 64], 64), 64).medium.is_none());
        let mut log = Log::open(ram(b"0123456789", 16), 16);
        write(&mut log, b"abcdefgh");
        assert!(log.medium.is_none());
        let mut failing = Log::open(ram(b"", 64), 64);
        failing.medium.as_mut().unwrap().fail = true;
        assert!(failing.push(b'\n', || {}).is_err());
        assert!(failing.medium.is_none());
        assert_eq!(failing.push(b'a', || {}), Err(ServiceError::NotInitialized));
    }

    #[test]
    fn warms_before_each_write() {
        let calls = core::cell::Cell::new(0);
        let mut log = Log::open(ram(b"", 4096), 4096);
        for &byte in b"one\ntwo\n" {
            let _ = log.push(byte, || calls.set(calls.get() + 1));
        }
        assert_eq!(calls.get(), 2);
    }
}
