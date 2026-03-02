//! NS16550(A) UART driver — unified, supports both byte-stride and
//! word-stride register layouts.
//!
//! Covers classic NS16550A (byte-stride, reg-shift=0), Synopsys
//! DesignWare APB UART (`snps,dw-apb-uart`, word-stride, reg-shift=2),
//! and Allwinner sunxi UARTs (NS16550-compatible, word-stride).
//!
//! **Register access is always byte-width** (`strb`/`ldrb` on ARM,
//! `sb`/`lb` on RISC-V).  This matches U-Boot's `writeb()`/`readb()`
//! for NS16550 — even when registers sit at 4-byte boundaries, only the
//! low byte matters.  The `reg_shift` config controls the address stride
//! between registers (0 = packed, 2 = 4-byte spacing).
//!
//! Register access uses `MmioReadWrite<u8>`/`MmioReadOnly<u8>` from
//! `fstart-mmio`, giving barrier-correct typed access with tock-registers
//! bitfield definitions.  The runtime `reg_shift` is handled by computing
//! the register address per-access rather than a fixed `register_structs!`
//! layout.
//!
//! Init sequence is an exact match of U-Boot `ns16550_init()` +
//! `ns16550_setbrg()` (drivers/serial/ns16550.c).
//!
//! Compatible: `"ns16550a"`, `"ns16550"`, `"snps,dw-apb-uart"`,
//!             `"allwinner,sun7i-a20-uart"`.

#![no_std]

use fstart_mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Console, ServiceError};

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

/// Typed configuration for the NS16550 driver.
///
/// The `reg_shift` field controls the address stride between registers:
///   - `0` -> byte-packed (offset = reg_index), classic NS16550A
///   - `2` -> 4-byte spacing (offset = reg_index << 2), DW APB / sunxi
///
/// Serde defaults ensure backward compatibility: existing board RON
/// files without `reg_shift` get `0` (byte-stride, no change).
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
}

/// NS16550 UART driver — covers NS16550A, DW APB UART, and sunxi UART.
///
/// Register access is **byte-width** via `MmioReadWrite<u8>` with
/// tock-registers bitfield definitions.  The `reg_shift` value controls
/// only the address spacing between registers.
pub struct Ns16550 {
    base: usize,
    shift: u8,
    clock_freq: u32,
    baud_rate: u32,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for Ns16550 {}
unsafe impl Sync for Ns16550 {}

impl Ns16550 {
    /// Get a typed reference to a read-write register at the given index.
    ///
    /// # Safety contract
    /// The returned reference is valid for the lifetime of `self` because
    /// the MMIO address is hardware-fixed.  `MmioReadWrite<u8>` is
    /// `#[repr(transparent)]` over `UnsafeCell<u8>`, matching the single
    /// byte at the computed address.
    #[inline(always)]
    fn rw<R: tock_registers::RegisterLongName>(&self, index: usize) -> &MmioReadWrite<u8, R> {
        unsafe { &*((self.base + (index << self.shift)) as *const MmioReadWrite<u8, R>) }
    }

    /// Get a typed reference to a read-only register at the given index.
    #[inline(always)]
    fn ro<R: tock_registers::RegisterLongName>(&self, index: usize) -> &MmioReadOnly<u8, R> {
        unsafe { &*((self.base + (index << self.shift)) as *const MmioReadOnly<u8, R>) }
    }

    // -- Named register accessors --

    /// THR/RBR/DLL — transmit/receive/divisor-low (context-dependent).
    #[inline(always)]
    fn thr(&self) -> &MmioReadWrite<u8> {
        self.rw(REG_THR)
    }

    /// IER/DLH — interrupt enable / divisor-high (context-dependent).
    #[inline(always)]
    fn ier(&self) -> &MmioReadWrite<u8> {
        self.rw(REG_IER)
    }

    /// FCR — FIFO Control Register (write-only in hardware).
    #[inline(always)]
    fn fcr(&self) -> &MmioReadWrite<u8, FCR::Register> {
        self.rw(REG_FCR)
    }

    /// LCR — Line Control Register.
    #[inline(always)]
    fn lcr(&self) -> &MmioReadWrite<u8, LCR::Register> {
        self.rw(REG_LCR)
    }

    /// MCR — Modem Control Register.
    #[inline(always)]
    fn mcr(&self) -> &MmioReadWrite<u8, MCR::Register> {
        self.rw(REG_MCR)
    }

    /// LSR — Line Status Register (read-only).
    #[inline(always)]
    fn lsr(&self) -> &MmioReadOnly<u8, LSR::Register> {
        self.ro(REG_LSR)
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
        self.lcr().modify(LCR::DLAB::SET);

        // Write divisor latch: DLL (low byte), DLH (high byte)
        self.thr().set(divisor as u8);
        self.ier().set((divisor >> 8) as u8);

        // Clear DLAB
        self.lcr().modify(LCR::DLAB::CLEAR);
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
        Ok(Self {
            base: config.base_addr as usize,
            shift: config.reg_shift,
            clock_freq: config.clock_freq,
            baud_rate: config.baud_rate,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Exact match of U-Boot ns16550_init() + ns16550_setbrg().

        // Wait until transmitter completely idle.
        while !self.lsr().is_set(LSR::TEMT) {
            core::hint::spin_loop();
        }

        // 1. IER = 0 — disable all interrupts
        self.ier().set(0);

        // 2. MCR = DTR + RTS
        self.mcr().write(MCR::DTR::SET + MCR::RTS::SET);

        // 3. FCR = FIFO enable + clear both FIFOs
        self.fcr()
            .write(FCR::FIFO_EN::SET + FCR::RX_RST::SET + FCR::TX_RST::SET);

        // 4. LCR = 8N1 (clears DLAB)
        self.lcr().write(LCR::WLS.val(3));

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
        self.thr().set(byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.lsr().is_set(LSR::DR) {
            Ok(Some(self.thr().get()))
        } else {
            Ok(None)
        }
    }
}
