//! NS16550(A) UART driver.
//!
//! Used by QEMU virt (RISC-V), many x86 platforms, and others.

use crate::{Driver, DriverError};
use fstart_services::{Console, ServiceError};
use fstart_types::device::Resources;

// Register offsets
const THR: usize = 0; // Transmitter Holding Register (write)
const RBR: usize = 0; // Receiver Buffer Register (read)
const IER: usize = 1; // Interrupt Enable Register
const FCR: usize = 2; // FIFO Control Register
const LCR: usize = 3; // Line Control Register
const LSR: usize = 5; // Line Status Register

// LSR bits
const LSR_DATA_READY: u8 = 0x01;
const LSR_THR_EMPTY: u8 = 0x20;

// LCR bits
const LCR_8N1: u8 = 0x03; // 8 data bits, no parity, 1 stop bit
const LCR_DLAB: u8 = 0x80; // Divisor Latch Access Bit

/// NS16550A UART driver (MMIO variant).
pub struct Ns16550 {
    base: *mut u8,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for Ns16550 {}
unsafe impl Sync for Ns16550 {}

impl Ns16550 {
    /// Create a new driver with the given MMIO base address.
    pub const fn new(base_addr: u64) -> Self {
        Self {
            base: base_addr as *mut u8,
        }
    }

    /// Initialize the UART with the given clock frequency and baud rate.
    pub fn init(&self, clock_freq: u32, baud_rate: u32) {
        let divisor = clock_freq / (16 * baud_rate);

        // Disable interrupts
        self.write_reg(IER, 0x00);

        // Set baud rate via divisor latch
        self.write_reg(LCR, LCR_DLAB);
        self.write_reg(0, (divisor & 0xFF) as u8); // DLL
        self.write_reg(1, ((divisor >> 8) & 0xFF) as u8); // DLM

        // 8N1, disable DLAB
        self.write_reg(LCR, LCR_8N1);

        // Enable and reset FIFOs
        self.write_reg(FCR, 0x07);
    }

    fn read_reg(&self, offset: usize) -> u8 {
        unsafe { core::ptr::read_volatile(self.base.add(offset)) }
    }

    fn write_reg(&self, offset: usize, value: u8) {
        unsafe { core::ptr::write_volatile(self.base.add(offset), value) }
    }
}

impl Driver for Ns16550 {
    const NAME: &'static str = "ns16550";
    const COMPATIBLE: &'static [&'static str] = &["ns16550a", "ns16550"];

    fn from_resources(resources: &Resources) -> Result<Self, DriverError> {
        let base = resources
            .mmio_base
            .ok_or(DriverError::MissingResource("mmio_base"))?;
        let uart = Self::new(base);

        // Initialize if clock and baud are specified
        if let (Some(clock), Some(baud)) = (resources.clock_freq, resources.baud_rate) {
            uart.init(clock, baud);
        }

        Ok(uart)
    }
}

impl Console for Ns16550 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Wait for THR empty
        while self.read_reg(LSR) & LSR_THR_EMPTY == 0 {
            core::hint::spin_loop();
        }
        self.write_reg(THR, byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.read_reg(LSR) & LSR_DATA_READY != 0 {
            Ok(Some(self.read_reg(RBR)))
        } else {
            Ok(None)
        }
    }
}
