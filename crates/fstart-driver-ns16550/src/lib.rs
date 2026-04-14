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
//! ## Typed register access
//!
//! Register bitfields are defined via `tock-registers` `register_bitfields!`.
//! Width-aware MMIO is handled by `read_reg`/`write_reg`, and typed access
//! is provided through `LocalRegisterCopy` wrappers that work with any bus
//! width.
//!
//! Init sequence is an exact match of U-Boot `ns16550_init()` +
//! `ns16550_setbrg()` (drivers/serial/ns16550.c).
//!
//! ## Register access mode (`access_mode`)
//!
//! The `access_mode` config selects between memory-mapped and port I/O access:
//!   - `Mmio` (default): Memory-mapped I/O via `read_volatile`/`write_volatile`.
//!     Used by all non-x86 platforms and MMIO-mapped x86 UARTs.
//!   - `Pio`: x86 port I/O via `in`/`out` instructions.  Used by legacy
//!     PC UARTs (COM1 at 0x3F8, COM2 at 0x2F8, etc.).  Requires the `pio`
//!     feature on this crate.
//!
//! Compatible: `"ns16550a"`, `"ns16550"`, `"snps,dw-apb-uart"`,
//!             `"allwinner,sun7i-a20-uart"`.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};
use tock_registers::register_bitfields;
use tock_registers::LocalRegisterCopy;

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
// Typed bitfield definitions for u8 registers
// ---------------------------------------------------------------------------

register_bitfields! [u8,
    /// Line Control Register (LCR).
    LCR [
        /// Word Length Select: 00=5, 01=6, 10=7, 11=8 bits.
        WLS OFFSET(0) NUMBITS(2) [],
        /// Number of Stop Bits: 0=1, 1=1.5/2.
        STB OFFSET(2) NUMBITS(1) [],
        /// Parity Enable.
        PEN OFFSET(3) NUMBITS(1) [],
        /// Divisor Latch Access Bit.
        DLAB OFFSET(7) NUMBITS(1) []
    ],
    /// Line Status Register (LSR).
    LSR [
        /// Data Ready — receiver has data.
        DR OFFSET(0) NUMBITS(1) [],
        /// Transmitter Holding Register Empty.
        THRE OFFSET(5) NUMBITS(1) [],
        /// Transmitter Empty (shift register + THR both empty).
        TEMT OFFSET(6) NUMBITS(1) []
    ],
    /// FIFO Control Register (FCR) — write-only.
    FCR [
        /// FIFO Enable.
        FIFO_EN OFFSET(0) NUMBITS(1) [],
        /// RX FIFO Reset.
        RX_RST OFFSET(1) NUMBITS(1) [],
        /// TX FIFO Reset.
        TX_RST OFFSET(2) NUMBITS(1) []
    ],
    /// Modem Control Register (MCR).
    MCR [
        /// Data Terminal Ready.
        DTR OFFSET(0) NUMBITS(1) [],
        /// Request To Send.
        RTS OFFSET(1) NUMBITS(1) []
    ]
];

// ---------------------------------------------------------------------------
// Config & driver
// ---------------------------------------------------------------------------

/// Register access mode: memory-mapped or port I/O.
///
/// Selects the bus transaction mechanism used to reach the UART registers.
/// MMIO uses `read_volatile`/`write_volatile`; PIO uses x86 `in`/`out`
/// instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AccessMode {
    /// Memory-mapped I/O (all platforms).
    Mmio,
    /// x86 port I/O via `in`/`out` instructions.
    /// Requires the `pio` feature on this crate.
    Pio,
}

impl Default for AccessMode {
    fn default() -> Self {
        Self::Mmio
    }
}

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
/// The `access_mode` field selects MMIO or port I/O:
///   - `Mmio` (default) -> memory-mapped register access
///   - `Pio` -> x86 port I/O (`in`/`out` instructions)
///
/// Serde defaults ensure backward compatibility: existing board RON
/// files without `reg_shift`/`reg_width`/`access_mode` get MMIO
/// byte-stride byte-access.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Ns16550Config {
    /// MMIO base address or I/O port base of the register block.
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
    /// Access mode: MMIO (default) or PIO (x86 port I/O).
    #[serde(default)]
    pub access_mode: AccessMode,
}

/// NS16550 UART driver — covers NS16550A, DW APB UART, sunxi UART,
/// and x86 legacy port-I/O UARTs.
///
/// Register access uses width-aware MMIO or port I/O (`read_reg`/`write_reg`)
/// with tock-registers `LocalRegisterCopy` for typed bitfield operations.
/// The `reg_shift` controls address spacing; `reg_width` controls
/// the bus transaction width (byte or 32-bit word); `access_mode` selects
/// between MMIO and PIO.
pub struct Ns16550 {
    base: usize,
    shift: u8,
    /// Effective I/O width: 1 = byte, 4 = word.
    width: u8,
    /// Access mode: MMIO or PIO.
    access_mode: AccessMode,
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

    /// Read a register (respecting `access_mode` and `reg_width`).
    ///
    /// - PIO mode: byte-width `in` from I/O port.
    /// - MMIO mode, `width == 4`: 32-bit read, returns low byte.
    /// - MMIO mode, `width == 1`: byte read.
    #[inline(always)]
    fn read_reg(&self, index: usize) -> u8 {
        let addr = self.addr(index);
        match self.access_mode {
            #[cfg(feature = "pio")]
            AccessMode::Pio => {
                // SAFETY: I/O port address provided by board config.
                unsafe { fstart_pio::inb(addr as u16) }
            }
            #[cfg(not(feature = "pio"))]
            AccessMode::Pio => {
                // PIO not compiled in — should not be reachable (constructor rejects).
                0
            }
            AccessMode::Mmio => {
                if self.width == 4 {
                    // SAFETY: self.base + offset is a valid MMIO register address
                    // provided by the board config. When width == 4, alignment is
                    // guaranteed by reg_shift >= 2 (validated in new()).
                    (unsafe { fstart_mmio::read32(addr as *const u32) }) as u8
                } else {
                    // SAFETY: self.base + offset is a valid MMIO register address
                    // provided by the board config. Byte access has no alignment
                    // requirement.
                    unsafe { fstart_mmio::read8(addr as *const u8) }
                }
            }
        }
    }

    /// Write a register (respecting `access_mode` and `reg_width`).
    ///
    /// - PIO mode: byte-width `out` to I/O port.
    /// - MMIO mode, `width == 4`: zero-extends to 32-bit, word write.
    /// - MMIO mode, `width == 1`: byte write.
    #[inline(always)]
    fn write_reg(&self, index: usize, val: u8) {
        let addr = self.addr(index);
        match self.access_mode {
            #[cfg(feature = "pio")]
            AccessMode::Pio => {
                // SAFETY: I/O port address provided by board config.
                unsafe { fstart_pio::outb(addr as u16, val) }
            }
            #[cfg(not(feature = "pio"))]
            AccessMode::Pio => {
                // PIO not compiled in — should not be reachable (constructor rejects).
            }
            AccessMode::Mmio => {
                if self.width == 4 {
                    // SAFETY: self.base + offset is a valid MMIO register address
                    // provided by the board config. When width == 4, alignment is
                    // guaranteed by reg_shift >= 2 (validated in new()).
                    unsafe { fstart_mmio::write32(addr as *mut u32, val as u32) }
                } else {
                    // SAFETY: self.base + offset is a valid MMIO register address
                    // provided by the board config. Byte access has no alignment
                    // requirement.
                    unsafe { fstart_mmio::write8(addr as *mut u8, val) }
                }
            }
        }
    }

    // -- Typed register accessors --

    /// Read LSR into a typed `LocalRegisterCopy`.
    #[inline(always)]
    fn lsr(&self) -> LocalRegisterCopy<u8, LSR::Register> {
        LocalRegisterCopy::new(self.read_reg(REG_LSR))
    }

    /// Read LCR into a typed `LocalRegisterCopy`.
    #[inline(always)]
    fn lcr(&self) -> LocalRegisterCopy<u8, LCR::Register> {
        LocalRegisterCopy::new(self.read_reg(REG_LCR))
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

        // Read-modify-write LCR to set DLAB
        let mut lcr = self.lcr();
        lcr.modify(LCR::DLAB::SET);
        self.write_reg(REG_LCR, lcr.get());

        // Write divisor latch: DLL (low byte), DLH (high byte)
        self.write_reg(REG_THR, divisor as u8);
        self.write_reg(REG_IER, (divisor >> 8) as u8);

        // Clear DLAB
        let mut lcr = self.lcr();
        lcr.modify(LCR::DLAB::CLEAR);
        self.write_reg(REG_LCR, lcr.get());
    }
}

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
        // PIO mode: always byte-width, ignore reg_width/reg_shift for
        // width resolution. Port I/O UARTs are always byte-stride.
        if config.access_mode == AccessMode::Pio {
            #[cfg(not(feature = "pio"))]
            return Err(DeviceError::ConfigError);

            #[cfg(feature = "pio")]
            return Ok(Self {
                base: config.base_addr as usize,
                shift: config.reg_shift,
                width: 1,
                access_mode: AccessMode::Pio,
                clock_freq: config.clock_freq,
                baud_rate: config.baud_rate,
            });
        }

        // MMIO mode: resolve effective I/O width.
        // 0 = auto (word if reg_shift >= 2).
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
            access_mode: AccessMode::Mmio,
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Exact match of U-Boot ns16550_init() + ns16550_setbrg().

        // Wait until transmitter completely idle.
        while !self.lsr().is_set(LSR::TEMT) {
            core::hint::spin_loop();
        }

        // 1. IER = 0 — disable all interrupts
        self.write_reg(REG_IER, 0);

        // 2. MCR = DTR + RTS
        self.write_reg(REG_MCR, (MCR::DTR::SET + MCR::RTS::SET).value);

        // 3. FCR = FIFO enable + clear both FIFOs
        self.write_reg(
            REG_FCR,
            (FCR::FIFO_EN::SET + FCR::RX_RST::SET + FCR::TX_RST::SET).value,
        );

        // 4. LCR = 8N1 (clears DLAB)
        self.write_reg(REG_LCR, LCR::WLS.val(3).value);

        // 5. Set baud rate via DLAB
        self.setbrg();

        Ok(())
    }
}

impl Console for Ns16550 {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        // Wait for THR empty
        while !self.lsr().is_set(LSR::THRE) {
            core::hint::spin_loop();
        }
        self.write_reg(REG_THR, byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.lsr().is_set(LSR::DR) {
            Ok(Some(self.read_reg(REG_THR)))
        } else {
            Ok(None)
        }
    }
}
