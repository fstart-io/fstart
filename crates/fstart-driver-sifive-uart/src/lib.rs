//! SiFive UART driver.
//!
//! Covers the SiFive-proprietary UART found on FU540 and FU740 SoCs
//! (HiFive Unleashed / Unmatched). This is NOT an NS16550-compatible
//! UART — it has its own register layout with TX/RX FIFOs, a simple
//! baud rate divisor, and FIFO full/empty flags in bit 31.
//!
//! ## Register map (7 x 32-bit registers, 32-bit aligned)
//!
//! | Offset | Name    | Description                              |
//! |--------|---------|------------------------------------------|
//! | 0x00   | txdata  | TX data; bit 31 = FIFO full              |
//! | 0x04   | rxdata  | RX data; bit 31 = FIFO empty             |
//! | 0x08   | txctrl  | TX control: enable, stop bits, watermark |
//! | 0x0C   | rxctrl  | RX control: enable, watermark            |
//! | 0x10   | ie      | Interrupt enable                         |
//! | 0x14   | ip      | Interrupt pending                        |
//! | 0x18   | div     | Baud rate divisor                        |
//!
//! ## Baud rate
//!
//! `f_baud = f_in / (div + 1)`, so `div = ceil(f_in / f_baud) - 1`.
//!
//! Reference implementations:
//! - coreboot `src/drivers/uart/sifive.c`
//! - U-Boot `drivers/serial/serial_sifive.c`
//!
//! Compatible: `"sifive,fu740-c000-uart"`, `"sifive,uart0"`.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};
use tock_registers::register_bitfields;
use tock_registers::LocalRegisterCopy;

// ---------------------------------------------------------------------------
// Register offsets (byte offsets, all 32-bit aligned)
// ---------------------------------------------------------------------------

/// Transmit data register (write byte to [7:0], bit 31 = FIFO full).
const REG_TXDATA: usize = 0x00;
/// Receive data register (read byte from [7:0], bit 31 = FIFO empty).
const REG_RXDATA: usize = 0x04;
/// Transmit control register.
const REG_TXCTRL: usize = 0x08;
/// Receive control register.
const REG_RXCTRL: usize = 0x0C;
/// Interrupt enable register.
const REG_IE: usize = 0x10;
/// Baud rate divisor register.
const REG_DIV: usize = 0x18;

// ---------------------------------------------------------------------------
// Typed bitfield definitions for 32-bit registers
// ---------------------------------------------------------------------------

register_bitfields! [u32,
    /// Transmit data register.
    TXDATA [
        /// Transmit data (write byte here).
        DATA OFFSET(0) NUMBITS(8) [],
        /// TX FIFO full flag (read-only). 1 = FIFO full, cannot accept data.
        FULL OFFSET(31) NUMBITS(1) []
    ],
    /// Receive data register.
    RXDATA [
        /// Received data byte.
        DATA OFFSET(0) NUMBITS(8) [],
        /// RX FIFO empty flag (read-only). 1 = FIFO empty, no data available.
        EMPTY OFFSET(31) NUMBITS(1) []
    ],
    /// Transmit control register.
    TXCTRL [
        /// Transmit enable.
        TXEN OFFSET(0) NUMBITS(1) [],
        /// Number of stop bits: 0 = 1 stop bit, 1 = 2 stop bits.
        NSTOP OFFSET(1) NUMBITS(1) [],
        /// TX FIFO interrupt watermark level.
        TXCNT OFFSET(16) NUMBITS(3) []
    ],
    /// Receive control register.
    RXCTRL [
        /// Receive enable.
        RXEN OFFSET(0) NUMBITS(1) [],
        /// RX FIFO interrupt watermark level.
        RXCNT OFFSET(16) NUMBITS(3) []
    ],
    /// Interrupt enable register.
    IE [
        /// TX watermark interrupt enable.
        TXWM OFFSET(0) NUMBITS(1) [],
        /// RX watermark interrupt enable.
        RXWM OFFSET(1) NUMBITS(1) []
    ]
];

// ---------------------------------------------------------------------------
// Config & driver
// ---------------------------------------------------------------------------

/// Typed configuration for the SiFive UART driver.
///
/// The `clock_freq` is the input clock to the UART peripheral. On the
/// FU740, this is the peripheral clock (HFPCLK), typically ~260 MHz
/// after PRCI initialization. On QEMU sifive_u, it is the `clock-frequency`
/// property from the device tree.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SifiveUartConfig {
    /// MMIO base address of the register block.
    pub base_addr: u64,
    /// Input clock frequency in Hz (peripheral clock).
    pub clock_freq: u32,
    /// Desired baud rate.
    pub baud_rate: u32,
}

/// SiFive UART driver.
///
/// Supports the SiFive-proprietary UART found on FU540/FU740 SoCs.
/// Register access is 32-bit word-aligned MMIO.
pub struct SifiveUart {
    base: usize,
    clock_freq: u32,
    baud_rate: u32,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for SifiveUart {}
unsafe impl Sync for SifiveUart {}

impl SifiveUart {
    /// Read a 32-bit register at the given byte offset.
    #[inline(always)]
    fn read_reg(&self, offset: usize) -> u32 {
        let addr = self.base + offset;
        // SAFETY: self.base + offset is a valid MMIO register address provided
        // by the board config. All SiFive UART registers are 32-bit aligned.
        unsafe { fstart_mmio::read32(addr as *const u32) }
    }

    /// Write a 32-bit register at the given byte offset.
    #[inline(always)]
    fn write_reg(&self, offset: usize, val: u32) {
        let addr = self.base + offset;
        // SAFETY: self.base + offset is a valid MMIO register address provided
        // by the board config. All SiFive UART registers are 32-bit aligned.
        unsafe { fstart_mmio::write32(addr as *mut u32, val) }
    }
}

impl Device for SifiveUart {
    const NAME: &'static str = "sifive-uart";
    const COMPATIBLE: &'static [&'static str] = &["sifive,fu740-c000-uart", "sifive,uart0"];
    type Config = SifiveUartConfig;

    fn new(config: &SifiveUartConfig) -> Result<Self, DeviceError> {
        if config.baud_rate == 0 || config.clock_freq == 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self {
            base: config.base_addr as usize,
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Baud rate divisor: div = ceil(f_in / f_baud) - 1
        // Using integer ceiling division: (a + b - 1) / b
        let div = (self.clock_freq as u64).div_ceil(self.baud_rate as u64) - 1;
        self.write_reg(REG_DIV, div as u32);

        // Enable TX: 1 stop bit, watermark at 1 (matches coreboot/U-Boot)
        let txctrl: LocalRegisterCopy<u32, TXCTRL::Register> =
            LocalRegisterCopy::new((TXCTRL::TXEN::SET + TXCTRL::TXCNT.val(1)).value);
        self.write_reg(REG_TXCTRL, txctrl.get());

        // Enable RX: watermark at 0
        let rxctrl: LocalRegisterCopy<u32, RXCTRL::Register> =
            LocalRegisterCopy::new((RXCTRL::RXEN::SET + RXCTRL::RXCNT.val(0)).value);
        self.write_reg(REG_RXCTRL, rxctrl.get());

        // Disable all interrupts (polled mode)
        self.write_reg(REG_IE, 0);

        Ok(())
    }
}

impl Console for SifiveUart {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Spin until TX FIFO is not full (bit 31 == 0)
        loop {
            let txdata: LocalRegisterCopy<u32, TXDATA::Register> =
                LocalRegisterCopy::new(self.read_reg(REG_TXDATA));
            if !txdata.is_set(TXDATA::FULL) {
                break;
            }
            core::hint::spin_loop();
        }
        self.write_reg(REG_TXDATA, byte as u32);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        let rxdata: LocalRegisterCopy<u32, RXDATA::Register> =
            LocalRegisterCopy::new(self.read_reg(REG_RXDATA));
        if rxdata.is_set(RXDATA::EMPTY) {
            Ok(None)
        } else {
            Ok(Some(rxdata.read(RXDATA::DATA) as u8))
        }
    }
}
