//! NS16550(A) UART driver — unified, supports byte-stride, word-stride,
//! and word-width register layouts.
//!
//! Covers classic NS16550A (byte-stride, reg-shift=0), Synopsys
//! DesignWare APB UART (`snps,dw-apb-uart`, word-stride, reg-shift=2),
//! and Allwinner sunxi UARTs (NS16550-compatible, word-stride).
//!
//! ## Register access width (`reg_width`)
//!
//! The `reg_width` config controls the **bus transaction width**:
//!
//! - `1` (default): Byte access (`sb`/`lb` on RISC-V, `strb`/`ldrb` on
//!   ARM).  Classic NS16550A and most PC-style UARTs.
//! - `4`: 32-bit word access (`sw`/`lw` on RISC-V, `str`/`ldr` on ARM).
//!   Required by some APB-connected UARTs (e.g., Allwinner D1's DW APB
//!   UART).  Matches U-Boot's `reg-io-width = <4>` / `writel()`/`readl()`.
//!
//! When `reg_width = 4`, only the low 8 bits of the 32-bit value are
//! significant (NS16550 is inherently 8-bit), matching U-Boot's behavior.
//!
//! ## Register spacing (`reg_shift`)
//!
//! The `reg_shift` config controls the address stride between registers:
//! - `0` -> byte-packed (offset = reg_index), classic NS16550A
//! - `2` -> 4-byte spacing (offset = reg_index << 2), DW APB / sunxi
//!
//! Init sequence is an exact match of U-Boot `ns16550_init()` +
//! `ns16550_setbrg()` (drivers/serial/ns16550.c).
//!
//! Compatible: `"ns16550a"`, `"ns16550"`, `"snps,dw-apb-uart"`,
//!             `"allwinner,sun7i-a20-uart"`.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};
use tock_registers::register_bitfields;

// ---------------------------------------------------------------------------
// Register indices (not byte offsets — multiply by `1 << reg_shift` for
// the actual MMIO address offset).
// ---------------------------------------------------------------------------

/// Transmit Holding Register / Receive Buffer Register / Divisor Latch Low.
const REG_THR: usize = 0;
/// Interrupt Enable Register / Divisor Latch High.
const REG_IER: usize = 1;
/// FIFO Control Register (write-only).
const REG_FCR: usize = 2;
/// Line Control Register.
const REG_LCR: usize = 3;
/// Modem Control Register.
const REG_MCR: usize = 4;
/// Line Status Register (read-only).
const REG_LSR: usize = 5;

// ---------------------------------------------------------------------------
// Bitfield definitions — self-documenting reference for the register layout.
// Actual MMIO access uses `read_reg`/`write_reg` with raw u8 constants
// below, because tock-registers typed references are u8-only and cannot
// express the u32 bus width some hardware variants require.
// ---------------------------------------------------------------------------

register_bitfields! [u8,
    /// Line Control Register
    LCR [
        /// Word Length Select: 0b11 = 8 bits
        WLS OFFSET(0) NUMBITS(2) [],
        /// Divisor Latch Access Bit
        DLAB OFFSET(7) NUMBITS(1) []
    ],
    /// Line Status Register
    LSR [
        /// Data Ready
        DR OFFSET(0) NUMBITS(1) [],
        /// Transmitter Holding Register Empty
        THRE OFFSET(5) NUMBITS(1) [],
        /// Transmitter Empty
        TEMT OFFSET(6) NUMBITS(1) []
    ],
    /// FIFO Control Register
    FCR [
        /// FIFO Enable
        FIFO_EN OFFSET(0) NUMBITS(1) [],
        /// Receiver FIFO Reset
        RX_RST OFFSET(1) NUMBITS(1) [],
        /// Transmitter FIFO Reset
        TX_RST OFFSET(2) NUMBITS(1) []
    ],
    /// Modem Control Register
    MCR [
        /// Data Terminal Ready
        DTR OFFSET(0) NUMBITS(1) [],
        /// Request To Send
        RTS OFFSET(1) NUMBITS(1) []
    ]
];

// ---------------------------------------------------------------------------
// Config & driver
// ---------------------------------------------------------------------------

/// Typed configuration for the NS16550 driver.
///
/// The `reg_shift` field controls the address stride between registers:
///   - `0` -> byte-packed (offset = reg_index), classic NS16550A
///   - `2` -> 4-byte spacing (offset = reg_index << 2), DW APB / sunxi
///
/// The `reg_width` field controls the bus transaction width:
///   - `1` (default) -> byte access (`sb`/`lb`)
///   - `4` -> 32-bit word access (`sw`/`lw`), for DW APB UARTs
///
/// Serde defaults ensure backward compatibility: existing board RON
/// files without `reg_shift`/`reg_width` get byte-stride byte-access.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Ns16550Config {
    /// MMIO base address of the register block.
    pub base_addr: u64,
    /// Input clock frequency in Hz.
    pub clock_freq: u32,
    /// Desired baud rate.
    pub baud_rate: u32,
    /// Register address shift (0 = byte-stride, 2 = 4-byte stride).
    #[serde(default)]
    pub reg_shift: u8,
    /// Register I/O width in bytes (1 = byte, 4 = 32-bit word).
    /// Corresponds to U-Boot's `reg-io-width` DTS property.
    /// Default 0 means "auto": uses 4 when reg_shift >= 2, else 1.
    #[serde(default)]
    pub reg_width: u8,
}

/// NS16550 UART driver — covers NS16550A, DW APB UART, and sunxi UART.
///
/// Supports both byte-width and 32-bit-word register access, selected
/// by `reg_width`.  The `reg_shift` controls address spacing.
pub struct Ns16550 {
    base: usize,
    shift: u8,
    /// Effective I/O width: 1 = byte, 4 = word.
    width: u8,
    clock_freq: u32,
    baud_rate: u32,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for Ns16550 {}
unsafe impl Sync for Ns16550 {}

impl Ns16550 {
    /// Compute the MMIO address for register at `index`.
    #[inline(always)]
    fn addr(&self, index: usize) -> usize {
        self.base + (index << self.shift)
    }

    /// Read a register (respecting `reg_width`).
    ///
    /// When `width == 4`, performs a 32-bit read and returns the low byte.
    /// When `width == 1`, performs a byte read.
    #[inline(always)]
    fn read_reg(&self, index: usize) -> u8 {
        let addr = self.addr(index);
        if self.width == 4 {
            // SAFETY: self.base + offset is a valid MMIO register address provided
            // by the board config. When width == 4, alignment is guaranteed by
            // reg_shift >= 2 (validated in new()).
            (unsafe { fstart_mmio::read32(addr as *const u32) }) as u8
        } else {
            // SAFETY: self.base + offset is a valid MMIO register address provided
            // by the board config. Byte access has no alignment requirement.
            unsafe { fstart_mmio::read8(addr as *const u8) }
        }
    }

    /// Write a register (respecting `reg_width`).
    ///
    /// When `width == 4`, zero-extends to 32-bit and performs a word write.
    /// When `width == 1`, performs a byte write.
    #[inline(always)]
    fn write_reg(&self, index: usize, val: u8) {
        let addr = self.addr(index);
        if self.width == 4 {
            // SAFETY: self.base + offset is a valid MMIO register address provided
            // by the board config. When width == 4, alignment is guaranteed by
            // reg_shift >= 2 (validated in new()).
            unsafe { fstart_mmio::write32(addr as *mut u32, val as u32) }
        } else {
            // SAFETY: self.base + offset is a valid MMIO register address provided
            // by the board config. Byte access has no alignment requirement.
            unsafe { fstart_mmio::write8(addr as *mut u8, val) }
        }
    }

    /// Read-modify-write helper for a register.
    #[inline(always)]
    fn modify_reg(&self, index: usize, clear: u8, set: u8) {
        let val = self.read_reg(index);
        self.write_reg(index, (val & !clear) | set);
    }

    /// Set baud rate — exact match of U-Boot `ns16550_setbrg()`.
    ///
    /// Uses read-modify-write on LCR to set/clear DLAB, matching the
    /// U-Boot binary.  Divisor uses `DIV_ROUND_CLOSEST`:
    /// `(clock + baud*8) / (baud*16)`.
    fn setbrg(&self) {
        let baud16 = (self.baud_rate as u64) * 16;
        let divisor = ((self.clock_freq as u64) + (self.baud_rate as u64) * 8) / baud16;
        let divisor = divisor as u16;

        // Read-modify-write LCR to set DLAB (bit 7)
        self.modify_reg(REG_LCR, 0, LCR_DLAB);

        // Write divisor latch: DLL (low byte), DLH (high byte)
        self.write_reg(REG_THR, divisor as u8);
        self.write_reg(REG_IER, (divisor >> 8) as u8);

        // Clear DLAB
        self.modify_reg(REG_LCR, LCR_DLAB, 0);
    }
}

// ---------------------------------------------------------------------------
// Raw bit constants for register manipulation (avoids tock-registers
// typed references, which are u8-only and incompatible with u32 MMIO).
// ---------------------------------------------------------------------------

/// LCR: Word Length Select = 8 bits (WLS=3).
const LCR_8N1: u8 = 0x03;
/// LCR: Divisor Latch Access Bit.
const LCR_DLAB: u8 = 1 << 7;
/// MCR: DTR + RTS asserted.
const MCR_DTR_RTS: u8 = 0x03;
/// FCR: FIFO enable + clear RX + clear TX.
const FCR_FIFO_ENABLE: u8 = 0x07;
/// LSR: Transmitter Empty (shift register + THR both empty).
const LSR_TEMT: u8 = 1 << 6;
/// LSR: Transmitter Holding Register Empty.
const LSR_THRE: u8 = 1 << 5;
/// LSR: Data Ready.
const LSR_DR: u8 = 1 << 0;

impl Device for Ns16550 {
    const NAME: &'static str = "ns16550";
    const COMPATIBLE: &'static [&'static str] = &[
        "ns16550a",
        "ns16550",
        "snps,dw-apb-uart",
        "allwinner,sun7i-a20-uart",
    ];
    type Config = Ns16550Config;

    fn new(config: &Ns16550Config) -> Result<Self, DeviceError> {
        // Resolve effective I/O width: 0 = auto (word if reg_shift >= 2).
        let width = match config.reg_width {
            0 => {
                if config.reg_shift >= 2 {
                    4
                } else {
                    1
                }
            }
            1 | 4 => config.reg_width,
            _ => return Err(DeviceError::ConfigError),
        };

        // 32-bit accesses require 4-byte alignment: reg_shift >= 2
        // guarantees offsets are multiples of 4.
        if width == 4 && config.reg_shift < 2 {
            return Err(DeviceError::ConfigError);
        }

        Ok(Self {
            base: config.base_addr as usize,
            shift: config.reg_shift,
            width,
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Exact match of U-Boot ns16550_init() + ns16550_setbrg().

        // Wait until transmitter completely idle.
        while (self.read_reg(REG_LSR) & LSR_TEMT) == 0 {
            core::hint::spin_loop();
        }

        // 1. IER = 0 — disable all interrupts
        self.write_reg(REG_IER, 0);

        // 2. MCR = DTR + RTS
        self.write_reg(REG_MCR, MCR_DTR_RTS);

        // 3. FCR = FIFO enable + clear both FIFOs
        self.write_reg(REG_FCR, FCR_FIFO_ENABLE);

        // 4. LCR = 8N1 (clears DLAB)
        self.write_reg(REG_LCR, LCR_8N1);

        // 5. Set baud rate via DLAB
        self.setbrg();

        Ok(())
    }
}

impl Console for Ns16550 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Wait for THR empty
        while (self.read_reg(REG_LSR) & LSR_THRE) == 0 {
            core::hint::spin_loop();
        }
        self.write_reg(REG_THR, byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if (self.read_reg(REG_LSR) & LSR_DR) != 0 {
            Ok(Some(self.read_reg(REG_THR)))
        } else {
            Ok(None)
        }
    }
}
