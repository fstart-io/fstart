//! ARM PL011 UART driver.
//!
//! Used by QEMU virt (AArch64).
//! Register access uses barrier-aware MMIO types from `fstart-mmio`.

#![no_std]

use fstart_mmio::MmioReadOnly;
use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};

register_bitfields! [u32,
    /// Flag Register
    FR [
        /// Transmit FIFO full
        TXFF OFFSET(5) NUMBITS(1) [],
        /// Receive FIFO empty
        RXFE OFFSET(4) NUMBITS(1) []
    ],
    /// Line Control Register
    LCR_H [
        /// Word length (bits 5:6)
        WLEN OFFSET(5) NUMBITS(2) [
            Bits5 = 0b00,
            Bits6 = 0b01,
            Bits7 = 0b10,
            Bits8 = 0b11
        ],
        /// FIFO enable
        FEN OFFSET(4) NUMBITS(1) []
    ],
    /// Control Register
    CR [
        /// UART enable
        UARTEN OFFSET(0) NUMBITS(1) [],
        /// Transmit enable
        TXE OFFSET(8) NUMBITS(1) [],
        /// Receive enable
        RXE OFFSET(9) NUMBITS(1) []
    ]
];

register_structs! {
    /// PL011 register block (32-bit word-addressable registers).
    Pl011Regs {
        /// Data Register
        (0x000 => pub dr: MmioReadWrite<u32>),
        /// Reserved
        (0x004 => _reserved0),
        /// Flag Register
        (0x018 => pub fr: MmioReadOnly<u32, FR::Register>),
        /// Reserved
        (0x01C => _reserved1),
        /// Integer Baud Rate Divisor
        (0x024 => pub ibrd: MmioReadWrite<u32>),
        /// Fractional Baud Rate Divisor
        (0x028 => pub fbrd: MmioReadWrite<u32>),
        /// Line Control Register
        (0x02C => pub lcr_h: MmioReadWrite<u32, LCR_H::Register>),
        /// Control Register
        (0x030 => pub cr: MmioReadWrite<u32, CR::Register>),
        (0x034 => @END),
    }
}

/// Typed configuration for the PL011 driver.
///
/// Contains exactly the fields this driver needs — no optional grab-bag.
/// Serializable with both RON (build-time validation) and postcard
/// (runtime config from FFS).
///
/// ACPI fields are always present (`Option<T>` with `#[serde(default)]`)
/// but only used when the `acpi` feature is active.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pl011Config {
    /// MMIO base address of the register block.
    pub base_addr: u64,
    /// Input clock frequency in Hz.
    pub clock_freq: u32,
    /// Desired baud rate.
    pub baud_rate: u32,

    // -- ACPI fields (board-specific, from RON) --
    /// ACPI namespace name (e.g., "COM0").
    /// Only used on ACPI-capable platforms.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
    /// GIC System Interrupt Vector for this UART.
    /// ARM-specific; ignored on non-ARM platforms.
    #[serde(default)]
    pub acpi_gsiv: Option<u32>,
    /// Emit a DBG2 (Debug Port Table 2) for this UART.
    ///
    /// When `true`, the driver's ACPI `extra_tables()` emits a DBG2
    /// table alongside the SPCR table.  Required by SBSA for the
    /// primary debug port.
    #[serde(default)]
    pub acpi_dbg2: bool,
}

#[cfg(feature = "acpi")]
mod acpi_support;

/// PL011 UART driver.
pub struct Pl011 {
    regs: &'static Pl011Regs,
    clock_freq: u32,
    baud_rate: u32,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for Pl011 {}
unsafe impl Sync for Pl011 {}

impl Device for Pl011 {
    const NAME: &'static str = "pl011";
    const COMPATIBLE: &'static [&'static str] = &["arm,pl011", "pl011"];
    type Config = Pl011Config;

    fn new(config: &Pl011Config) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: base_addr comes from the board RON and is validated
            // by codegen at build time.
            regs: unsafe { &*(config.base_addr as *const Pl011Regs) },
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Disable UART
        self.regs.cr.set(0);

        // Set baud rate using u64 to avoid overflow in intermediate calculations.
        // BRD = UARTCLK / (16 * Baud Rate)
        let clk = self.clock_freq as u64;
        let baud = self.baud_rate as u64;
        let divisor = 16 * baud;
        let brd_i = (clk / divisor) as u32;
        let brd_f = (((clk % divisor) * 64 + baud / 2) / baud) as u32;

        self.regs.ibrd.set(brd_i);
        self.regs.fbrd.set(brd_f);

        // 8N1, FIFO enabled
        self.regs.lcr_h.write(LCR_H::WLEN::Bits8 + LCR_H::FEN::SET);

        // Enable UART, TX, RX
        self.regs
            .cr
            .write(CR::UARTEN::SET + CR::TXE::SET + CR::RXE::SET);

        Ok(())
    }
}

impl Console for Pl011 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Wait for TX FIFO not full
        while self.regs.fr.is_set(FR::TXFF) {
            core::hint::spin_loop();
        }
        self.regs.dr.set(byte as u32);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if !self.regs.fr.is_set(FR::RXFE) {
            Ok(Some(self.regs.dr.get() as u8))
        } else {
            Ok(None)
        }
    }
}
