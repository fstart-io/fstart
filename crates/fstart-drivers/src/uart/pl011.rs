//! ARM PL011 UART driver.
//!
//! Used by QEMU virt (AArch64).

use crate::{Driver, DriverError};
use fstart_services::{Console, ServiceError};
use fstart_types::device::Resources;

// PL011 register offsets
const UARTDR: usize = 0x000; // Data Register
const UARTFR: usize = 0x018; // Flag Register
const UARTIBRD: usize = 0x024; // Integer Baud Rate Divisor
const UARTFBRD: usize = 0x028; // Fractional Baud Rate Divisor
const UARTLCR_H: usize = 0x02C; // Line Control Register
const UARTCR: usize = 0x030; // Control Register

// Flag register bits
const UARTFR_TXFF: u32 = 1 << 5; // TX FIFO Full
const UARTFR_RXFE: u32 = 1 << 4; // RX FIFO Empty

// LCR bits
const UARTLCR_H_WLEN_8: u32 = 0x60; // 8-bit word length
const UARTLCR_H_FEN: u32 = 0x10; // FIFO enable

// CR bits
const UARTCR_UARTEN: u32 = 1 << 0; // UART enable
const UARTCR_TXE: u32 = 1 << 8; // TX enable
const UARTCR_RXE: u32 = 1 << 9; // RX enable

/// PL011 UART driver.
pub struct Pl011 {
    base: *mut u32,
}

unsafe impl Send for Pl011 {}
unsafe impl Sync for Pl011 {}

impl Pl011 {
    pub const fn new(base_addr: u64) -> Self {
        Self {
            base: base_addr as *mut u32,
        }
    }

    /// Initialize the UART with the given clock frequency and baud rate.
    pub fn init(&self, clock_freq: u32, baud_rate: u32) {
        // Disable UART
        self.write_reg(UARTCR, 0);

        // Set baud rate
        // BRD = UARTCLK / (16 * Baud Rate)
        let brd_i = clock_freq / (16 * baud_rate);
        let brd_f = ((clock_freq % (16 * baud_rate)) * 64 + baud_rate / 2) / baud_rate;

        self.write_reg(UARTIBRD, brd_i);
        self.write_reg(UARTFBRD, brd_f);

        // 8N1, FIFO enabled
        self.write_reg(UARTLCR_H, UARTLCR_H_WLEN_8 | UARTLCR_H_FEN);

        // Enable UART, TX, RX
        self.write_reg(UARTCR, UARTCR_UARTEN | UARTCR_TXE | UARTCR_RXE);
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.base.byte_add(offset)) }
    }

    fn write_reg(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile(self.base.byte_add(offset), value) }
    }
}

impl Driver for Pl011 {
    const NAME: &'static str = "pl011";
    const COMPATIBLE: &'static [&'static str] = &["arm,pl011", "pl011"];

    fn from_resources(resources: &Resources) -> Result<Self, DriverError> {
        let base = resources
            .mmio_base
            .ok_or(DriverError::MissingResource("mmio_base"))?;
        let uart = Self::new(base);

        if let (Some(clock), Some(baud)) = (resources.clock_freq, resources.baud_rate) {
            uart.init(clock, baud);
        }

        Ok(uart)
    }
}

impl Console for Pl011 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        while self.read_reg(UARTFR) & UARTFR_TXFF != 0 {
            core::hint::spin_loop();
        }
        self.write_reg(UARTDR, byte as u32);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.read_reg(UARTFR) & UARTFR_RXFE == 0 {
            Ok(Some(self.read_reg(UARTDR) as u8))
        } else {
            Ok(None)
        }
    }
}
