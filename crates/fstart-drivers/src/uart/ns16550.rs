//! NS16550(A) UART driver.
//!
//! Used by QEMU virt (RISC-V), many x86 platforms, and others.
//! Register access uses the `tock-registers` crate for type-safe MMIO.

// `tock-registers` `register_structs!` macro triggers `modulo_one` inside
// its generated alignment test.  This is a false positive.
#![allow(clippy::modulo_one)]

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;
use tock_registers::registers::{ReadOnly, ReadWrite, WriteOnly};

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};

register_bitfields! [u8,
    /// Interrupt Enable Register
    IER [
        /// Received Data Available Interrupt
        ERBFI OFFSET(0) NUMBITS(1) [],
        /// Transmitter Holding Register Empty Interrupt
        ETBEI OFFSET(1) NUMBITS(1) [],
        /// Receiver Line Status Interrupt
        ELSI OFFSET(2) NUMBITS(1) [],
        /// Modem Status Interrupt
        EDSSI OFFSET(3) NUMBITS(1) []
    ],
    /// FIFO Control Register
    FCR [
        /// FIFO Enable
        FIFOE OFFSET(0) NUMBITS(1) [],
        /// Receiver FIFO Reset
        RFIFOR OFFSET(1) NUMBITS(1) [],
        /// Transmitter FIFO Reset
        XFIFOR OFFSET(2) NUMBITS(1) []
    ],
    /// Line Control Register
    LCR [
        /// Word Length Select
        WLS OFFSET(0) NUMBITS(2) [
            Bits5 = 0b00,
            Bits6 = 0b01,
            Bits7 = 0b10,
            Bits8 = 0b11
        ],
        /// Number of Stop Bits
        STB OFFSET(2) NUMBITS(1) [],
        /// Parity Enable
        PEN OFFSET(3) NUMBITS(1) [],
        /// Divisor Latch Access Bit
        DLAB OFFSET(7) NUMBITS(1) []
    ],
    /// Line Status Register
    LSR [
        /// Data Ready
        DR OFFSET(0) NUMBITS(1) [],
        /// Transmitter Holding Register Empty
        THRE OFFSET(5) NUMBITS(1) []
    ]
];

register_structs! {
    /// NS16550 register block (byte-addressable registers).
    Ns16550Regs {
        /// Transmit Holding / Receive Buffer / Divisor Latch Low
        (0x00 => pub thr_rbr_dll: ReadWrite<u8>),
        /// Interrupt Enable / Divisor Latch High
        (0x01 => pub ier_dlh: ReadWrite<u8, IER::Register>),
        /// FIFO Control (write-only) / Interrupt Identification (read-only)
        (0x02 => pub fcr_iir: WriteOnly<u8, FCR::Register>),
        /// Line Control Register
        (0x03 => pub lcr: ReadWrite<u8, LCR::Register>),
        /// Modem Control Register
        (0x04 => pub mcr: ReadWrite<u8>),
        /// Line Status Register
        (0x05 => pub lsr: ReadOnly<u8, LSR::Register>),
        /// Modem Status Register
        (0x06 => pub msr: ReadOnly<u8>),
        /// Scratch Register
        (0x07 => pub scr: ReadWrite<u8>),
        (0x08 => @END),
    }
}

/// Typed configuration for the NS16550 driver.
///
/// Contains exactly the fields this driver needs — no optional grab-bag.
/// Serializable with both RON (build-time validation) and postcard
/// (runtime config from FFS).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Ns16550Config {
    /// MMIO base address of the register block.
    pub base_addr: u64,
    /// Input clock frequency in Hz.
    pub clock_freq: u32,
    /// Desired baud rate.
    pub baud_rate: u32,
}

/// NS16550A UART driver (MMIO variant).
pub struct Ns16550 {
    regs: &'static Ns16550Regs,
    clock_freq: u32,
    baud_rate: u32,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for Ns16550 {}
unsafe impl Sync for Ns16550 {}

impl Device for Ns16550 {
    const NAME: &'static str = "ns16550";
    const COMPATIBLE: &'static [&'static str] = &["ns16550a", "ns16550"];
    type Config = Ns16550Config;

    fn new(config: &Ns16550Config) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: base_addr comes from the board RON and is validated
            // by codegen at build time.
            regs: unsafe { &*(config.base_addr as *const Ns16550Regs) },
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Use u64 to avoid overflow in intermediate calculations
        let divisor = (self.clock_freq as u64 / (16 * self.baud_rate as u64)) as u8;

        // Disable interrupts
        self.regs.ier_dlh.set(0);

        // Set baud rate via divisor latch
        self.regs.lcr.write(LCR::DLAB::SET);
        self.regs.thr_rbr_dll.set(divisor); // DLL
        self.regs.ier_dlh.set(0); // DLH

        // 8N1, disable DLAB
        self.regs.lcr.write(LCR::WLS::Bits8 + LCR::DLAB::CLEAR);

        // Enable and reset FIFOs
        self.regs
            .fcr_iir
            .write(FCR::FIFOE::SET + FCR::RFIFOR::SET + FCR::XFIFOR::SET);

        Ok(())
    }
}

impl Console for Ns16550 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Wait for THR empty
        while !self.regs.lsr.is_set(LSR::THRE) {
            core::hint::spin_loop();
        }
        self.regs.thr_rbr_dll.set(byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.regs.lsr.is_set(LSR::DR) {
            Ok(Some(self.regs.thr_rbr_dll.get()))
        } else {
            Ok(None)
        }
    }
}
