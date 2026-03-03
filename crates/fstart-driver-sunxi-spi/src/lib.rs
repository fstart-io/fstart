//! Allwinner sunxi SPI NOR flash boot driver (unified A20/H3).
//!
//! Minimal read-only driver for booting from SPI NOR flash. Implements
//! `Device` + `BlockDevice` traits. Uses the SPI0 controller to read
//! from an attached SPI NOR flash chip via the standard Read Data Bytes
//! command (0x03).
//!
//! Supports both sun4i-generation (A10/A20) and sun6i-generation
//! (H3/H2+/A64) SPI controllers. These are **different IP blocks** with
//! incompatible register layouts:
//!
//! 1. **Register offsets**: sun4i has a compact layout (RX=0x00, TX=0x04,
//!    CTL=0x08); sun6i splits control into GCR/TCR/FIFO_CTL and moves
//!    TX/RX to dedicated FIFO windows at 0x200/0x300
//! 2. **Control bits**: sun4i packs global, transfer, and FIFO control
//!    into a single 0x08 register; sun6i separates them (0x04/0x08/0x18)
//! 3. **Clock gating**: sun4i has AHB gate only; sun6i additionally
//!    deasserts a separate bus-reset register and performs a soft reset
//! 4. **GPIO pin mux**: sun4i uses PC23 for CS0; sun6i uses PC3
//!
//! The enum-based config ([`SunxiSpiConfig`]) selects the SoC variant
//! at board-config time; the driver dispatches internally.
//!
//! Ported from U-Boot `arch/arm/mach-sunxi/spl_spi_sunxi.c`.

#![no_std]

use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{BlockDevice, ServiceError};

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Driver configuration (from board RON)
// ---------------------------------------------------------------------------

/// Configuration for the Allwinner sunxi SPI NOR flash boot driver.
///
/// The enum variant selects the SoC generation, which determines
/// register layout, clock gating, and GPIO pin mux.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum SunxiSpiConfig {
    /// A20 (sun7i) — sun4i-generation SPI IP.
    ///
    /// Compact register layout, AHB gate only, CS on PC23.
    Sun7iA20 {
        /// SPI controller base address (0x01C05000 for SPI0 on A20).
        base_addr: u64,
        /// CCU base address (0x01C20000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x01C20800) for GPIO pin mux.
        pio_base: u64,
        /// SPI NOR flash capacity in bytes (e.g., 0x01000000 for 16 MiB).
        flash_size: u32,
    },
    /// H3/H2+ (sun8i) — sun6i-generation SPI IP.
    ///
    /// Split register layout, AHB gate + bus-reset, CS on PC3.
    Sun8iH3 {
        /// SPI controller base address (0x01C68000 for SPI0 on H3).
        base_addr: u64,
        /// CCU base address (0x01C20000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x01C20800) for GPIO pin mux.
        pio_base: u64,
        /// SPI NOR flash capacity in bytes (e.g., 0x01000000 for 16 MiB).
        flash_size: u32,
    },
}

// ---------------------------------------------------------------------------
// Internal SoC generation selector
// ---------------------------------------------------------------------------

/// SoC generation — drives the hardware differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SunxiGen {
    /// sun4i-generation (A10, A20): compact registers, AHB gate only.
    Sun4i,
    /// sun6i-generation (H3, H2+, A64): split registers, gate + reset.
    Sun6i,
}

// ---------------------------------------------------------------------------
// Sun4i register definitions (A10/A20)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Control Register (offset 0x08) — sun4i variant.
    ///
    /// This single register combines global control, transfer control,
    /// and FIFO control — all in one 32-bit word.
    CTL4 [
        /// SPI controller enable.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Master mode.
        MASTER OFFSET(1) NUMBITS(1) [],
        /// Clock phase (CPHA).
        CPHA OFFSET(2) NUMBITS(1) [],
        /// Clock polarity (CPOL).
        CPOL OFFSET(3) NUMBITS(1) [],
        /// Chip select active low.
        CS_ACTIVE_LOW OFFSET(4) NUMBITS(1) [],
        /// TX FIFO reset (self-clearing).
        TF_RST OFFSET(8) NUMBITS(1) [],
        /// RX FIFO reset (self-clearing).
        RF_RST OFFSET(9) NUMBITS(1) [],
        /// Exchange burst — start transfer.
        XCH OFFSET(10) NUMBITS(1) [],
        /// Chip select index (0-3).
        CS_SEL OFFSET(12) NUMBITS(2) [],
        /// Manual chip select control.
        CS_MANUAL OFFSET(16) NUMBITS(1) [],
        /// Chip select level (when CS_MANUAL=1).
        CS_LEVEL OFFSET(17) NUMBITS(1) [],
        /// Transmit pause enable.
        TP OFFSET(18) NUMBITS(1) [],
    ],
    /// FIFO Status Register (offset 0x28) — sun4i variant.
    FIFO_STA4 [
        /// RX FIFO byte count (bits 6:0).
        RF_CNT OFFSET(0) NUMBITS(7) [],
    ]
];

register_structs! {
    /// Sun4i SPI controller register block (A10/A20).
    ///
    /// Only the registers needed for SPL-style SPI NOR flash reads
    /// are included; interrupt/DMA registers are omitted.
    pub Sun4iSpiRegs {
        /// RX data register. Read received bytes here.
        (0x00 => pub rxdata: MmioReadWrite<u32>),
        /// TX data register. Write bytes to transmit here.
        (0x04 => pub txdata: MmioReadWrite<u32>),
        /// Control register (global + transfer + FIFO control combined).
        (0x08 => pub ctl: MmioReadWrite<u32, CTL4::Register>),
        /// Interrupt control, interrupt status, DMA control, wait clock.
        (0x0C => _reserved: [u8; 0x10]),
        /// Clock control register.
        ///
        /// Bit 12 (DRS): 0 = use CDR1 (bits 11:8), 1 = use CDR2 (bits 7:0).
        /// CDR2 divider: SPI_CLK = MOD_CLK / (2 * (CDR2 + 1)).
        /// Value 0x1001 = DRS=1, CDR2=1 → divide by 4.
        (0x1C => pub clk_ctl: MmioReadWrite<u32>),
        /// Burst count — total number of bytes in the transfer (TX + RX).
        (0x20 => pub burst_cnt: MmioReadWrite<u32>),
        /// Transmit count — number of bytes to actually transmit.
        /// Remaining bytes in the burst are clock-only (for receiving).
        (0x24 => pub xmit_cnt: MmioReadWrite<u32>),
        /// FIFO status register.
        (0x28 => pub fifo_sta: MmioReadWrite<u32, FIFO_STA4::Register>),
        (0x2C => @END),
    }
}

// ---------------------------------------------------------------------------
// Sun6i register definitions (H3/H2+/A64)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Global Control Register (offset 0x04) — sun6i variant.
    GCR6 [
        /// SPI controller enable.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Master mode.
        MASTER OFFSET(1) NUMBITS(1) [],
        /// Soft reset (self-clearing). Must wait until cleared after set.
        SRST OFFSET(31) NUMBITS(1) [],
    ],
    /// Transfer Control Register (offset 0x08) — sun6i variant.
    TCR6 [
        /// Clock phase (CPHA).
        CPHA OFFSET(0) NUMBITS(1) [],
        /// Clock polarity (CPOL).
        CPOL OFFSET(1) NUMBITS(1) [],
        /// Chip select active low.
        CS_ACTIVE_LOW OFFSET(2) NUMBITS(1) [],
        /// Chip select index (0-1).
        CS_SEL OFFSET(4) NUMBITS(2) [],
        /// Manual chip select control.
        CS_MANUAL OFFSET(6) NUMBITS(1) [],
        /// Chip select level (when CS_MANUAL=1).
        CS_LEVEL OFFSET(7) NUMBITS(1) [],
        /// Exchange burst — start transfer.
        XCH OFFSET(31) NUMBITS(1) [],
    ],
    /// FIFO Control Register (offset 0x18) — sun6i variant.
    ///
    /// Separate from GCR/TCR, unlike sun4i which packs FIFO reset
    /// bits into the control register.
    FIFO_CTL6 [
        /// RX FIFO reset (self-clearing).
        RF_RST OFFSET(15) NUMBITS(1) [],
        /// TX FIFO reset (self-clearing).
        TF_RST OFFSET(31) NUMBITS(1) [],
    ],
    /// FIFO Status Register (offset 0x1C) — sun6i variant.
    FIFO_STA6 [
        /// RX FIFO byte count (bits 7:0 — 8 bits on sun6i).
        RF_CNT OFFSET(0) NUMBITS(8) [],
    ]
];

register_structs! {
    /// Sun6i SPI controller register block (H3/H2+/A64).
    ///
    /// The sun6i variant separates control into GCR, TCR, and FIFO_CTL.
    /// TX/RX data ports are at dedicated FIFO memory windows (0x200/0x300).
    pub Sun6iSpiRegs {
        (0x000 => _res0: [u8; 0x04]),
        /// Global control register (enable, master, soft reset).
        (0x004 => pub gcr: MmioReadWrite<u32, GCR6::Register>),
        /// Transfer control register (CS, polarity, exchange).
        (0x008 => pub tcr: MmioReadWrite<u32, TCR6::Register>),
        (0x00C => _res1: [u8; 0x0C]),
        /// FIFO control register (FIFO resets).
        (0x018 => pub fifo_ctl: MmioReadWrite<u32, FIFO_CTL6::Register>),
        /// FIFO status register (RX count).
        (0x01C => pub fifo_sta: MmioReadWrite<u32, FIFO_STA6::Register>),
        (0x020 => _res2: [u8; 0x04]),
        /// Clock control register (same format as sun4i).
        (0x024 => pub clk_ctl: MmioReadWrite<u32>),
        (0x028 => _res3: [u8; 0x08]),
        /// Master burst count — total bytes in the transfer (TX + RX).
        (0x030 => pub mbc: MmioReadWrite<u32>),
        /// Master transmit count — number of bytes to transmit.
        (0x034 => pub mtc: MmioReadWrite<u32>),
        /// Burst control count — SPI-specific burst length.
        (0x038 => pub bcc: MmioReadWrite<u32>),
        (0x03C => _res4: [u8; 0x1C4]),
        /// TX data register (FIFO window).
        (0x200 => pub txd: MmioReadWrite<u32>),
        (0x204 => _res5: [u8; 0xFC]),
        /// RX data register (FIFO window).
        (0x300 => pub rxd: MmioReadWrite<u32>),
        (0x304 => @END),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// SPI NOR flash Read Data Bytes command (single-IO, 3-byte address).
const SPI_CMD_READ: u8 = 0x03;

/// SPI FIFO depth (64 bytes on both sun4i and sun6i-H3 variants).
const SPI_FIFO_DEPTH: usize = 64;

/// Command overhead: 1 byte opcode + 3 bytes address = 4 bytes.
const SPI_CMD_LEN: usize = 4;

/// Maximum data payload per SPI transfer (FIFO depth minus command).
const SPI_MAX_XFER: usize = SPI_FIFO_DEPTH - SPI_CMD_LEN;

/// Clock divider value: DRS=1 (bit 12), CDR2=1 (bits 7:0).
/// SPI_CLK = OSC24M / (2 * (1+1)) = 6 MHz.
const SPI0_CLK_DIV_BY_4: u32 = 0x1001;

/// AHB gate bit for SPI0 (bit 20 in both A20 and H3 gate registers).
const AHB_GATE_SPI0: u32 = 1 << 20;

/// Bus reset bit for SPI0 on H3 (bit 20 in bus_reset0 at CCU+0x2C0).
const BUS_RESET_SPI0: u32 = 1 << 20;

/// CCU register offsets (same on both A20 and H3).
const CCU_AHB_GATE_OFFSET: usize = 0x060;
const CCU_SPI0_CLK_OFFSET: usize = 0x0A0;
/// Bus soft-reset register 0 (H3 / sun6i only).
const CCU_BUS_RESET0_OFFSET: usize = 0x2C0;

/// GPIO port C configuration register 0 (controls PC0-PC7).
const PIO_PC_CFG0: usize = 0x48;
/// GPIO port C configuration register 2 (controls PC16-PC23).
/// Used by sun4i for CS on PC23.
const PIO_PC_CFG2: usize = 0x50;
/// SPI0 function number for port C pins.
const SPI0_PIN_FUNC: u32 = 3;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Allwinner sunxi SPI NOR flash boot driver (unified A20/H3).
///
/// Provides read-only block device access to an SPI NOR flash chip
/// connected to SPI0. Designed for bootblock use where code size must
/// be minimal.
pub struct SunxiSpi {
    /// SPI controller base address.
    base: usize,
    /// CCU base address for clock control.
    ccu_base: usize,
    /// PIO base address for GPIO configuration.
    pio_base: usize,
    /// Flash capacity in bytes.
    flash_size: u32,
    /// SoC generation selector.
    gen: SunxiGen,
}

// SAFETY: SunxiSpi contains only MMIO base addresses and config values.
// MMIO accesses are inherently ordered by volatile semantics.
// Firmware is single-threaded at this point.
unsafe impl Send for SunxiSpi {}
unsafe impl Sync for SunxiSpi {}

impl Device for SunxiSpi {
    const NAME: &'static str = "sunxi-spi";
    const COMPATIBLE: &'static [&'static str] =
        &["allwinner,sun4i-a10-spi", "allwinner,sun8i-h3-spi"];

    type Config = SunxiSpiConfig;

    fn new(config: &Self::Config) -> Result<Self, DeviceError> {
        let (base_addr, ccu_base, pio_base, flash_size, gen) = match *config {
            SunxiSpiConfig::Sun7iA20 {
                base_addr,
                ccu_base,
                pio_base,
                flash_size,
            } => (base_addr, ccu_base, pio_base, flash_size, SunxiGen::Sun4i),
            SunxiSpiConfig::Sun8iH3 {
                base_addr,
                ccu_base,
                pio_base,
                flash_size,
            } => (base_addr, ccu_base, pio_base, flash_size, SunxiGen::Sun6i),
        };

        if base_addr == 0 {
            return Err(DeviceError::MissingResource("base_addr"));
        }
        // 3-byte addressing (SPI command 0x03) can only reach 16 MiB.
        if flash_size > 0x0100_0000 {
            return Err(DeviceError::ConfigError);
        }

        Ok(Self {
            base: base_addr as usize,
            ccu_base: ccu_base as usize,
            pio_base: pio_base as usize,
            flash_size,
            gen,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        self.setup_gpio();
        self.setup_clocks();
        self.enable_controller();

        fstart_log::info!(
            "SPI0: init complete (6 MHz, flash {}KB)",
            self.flash_size / 1024
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Register accessors
// ---------------------------------------------------------------------------

impl SunxiSpi {
    /// Get the sun4i register block. Only valid when `self.gen == Sun4i`.
    fn sun4i_regs(&self) -> &'static Sun4iSpiRegs {
        // SAFETY: MMIO base address validated in `new()`.
        unsafe { &*(self.base as *const Sun4iSpiRegs) }
    }

    /// Get the sun6i register block. Only valid when `self.gen == Sun6i`.
    fn sun6i_regs(&self) -> &'static Sun6iSpiRegs {
        // SAFETY: MMIO base address validated in `new()`.
        unsafe { &*(self.base as *const Sun6iSpiRegs) }
    }
}

// ---------------------------------------------------------------------------
// Initialisation helpers
// ---------------------------------------------------------------------------

impl SunxiSpi {
    /// Read a PIO register with proper MMIO barriers.
    #[inline(always)]
    fn pio_read(&self, offset: usize) -> u32 {
        unsafe { fstart_mmio::read32((self.pio_base + offset) as *const u32) }
    }

    /// Write a PIO register with proper MMIO barriers.
    #[inline(always)]
    fn pio_write(&self, offset: usize, val: u32) {
        unsafe { fstart_mmio::write32((self.pio_base + offset) as *mut u32, val) }
    }

    /// Read a CCU register with proper MMIO barriers.
    #[inline(always)]
    fn ccu_read(&self, offset: usize) -> u32 {
        unsafe { fstart_mmio::read32((self.ccu_base + offset) as *const u32) }
    }

    /// Write a CCU register with proper MMIO barriers.
    #[inline(always)]
    fn ccu_write(&self, offset: usize, val: u32) {
        unsafe { fstart_mmio::write32((self.ccu_base + offset) as *mut u32, val) }
    }

    /// Configure SPI0 GPIO pins on port C.
    ///
    /// Both generations use PC0 (MOSI), PC1 (MISO), PC2 (CLK) as
    /// SPI0 function 3. The CS0 pin differs:
    /// - sun4i: PC23 (bits [31:28] of PC_CFG2)
    /// - sun6i: PC3  (bits [15:12] of PC_CFG0)
    fn setup_gpio(&self) {
        let val = self.pio_read(PIO_PC_CFG0);

        match self.gen {
            SunxiGen::Sun4i => {
                // PC0-PC2: bits [11:0] in PC_CFG0, 4 bits per pin.
                let val = (val & !0xFFF)
                    | SPI0_PIN_FUNC         // PC0 = SPI0_MOSI
                    | (SPI0_PIN_FUNC << 4)  // PC1 = SPI0_MISO
                    | (SPI0_PIN_FUNC << 8); // PC2 = SPI0_CLK
                self.pio_write(PIO_PC_CFG0, val);

                // PC23: bits [31:28] in PC_CFG2 (PC16-PC23, 4 bits per pin).
                let val = self.pio_read(PIO_PC_CFG2);
                let val = (val & !(0xF << 28)) | (SPI0_PIN_FUNC << 28);
                self.pio_write(PIO_PC_CFG2, val);
            }
            SunxiGen::Sun6i => {
                // PC0-PC3: bits [15:0] in PC_CFG0, 4 bits per pin.
                let val = (val & !0xFFFF)
                    | SPI0_PIN_FUNC          // PC0 = SPI0_MOSI
                    | (SPI0_PIN_FUNC << 4)   // PC1 = SPI0_MISO
                    | (SPI0_PIN_FUNC << 8)   // PC2 = SPI0_CLK
                    | (SPI0_PIN_FUNC << 12); // PC3 = SPI0_CS0
                self.pio_write(PIO_PC_CFG0, val);
            }
        }
    }

    /// Enable AHB gate, bus reset (sun6i), and SPI module clock.
    ///
    /// Sets SPI0 module clock source to OSC24M with divide-by-4
    /// (resulting in 6 MHz SPI clock — same as what the BROM uses).
    fn setup_clocks(&self) {
        // Sun6i: deassert bus reset for SPI0 before opening gate.
        if self.gen == SunxiGen::Sun6i {
            let val = self.ccu_read(CCU_BUS_RESET0_OFFSET);
            self.ccu_write(CCU_BUS_RESET0_OFFSET, val | BUS_RESET_SPI0);
        }

        // Open AHB gate for SPI0 (bit 20).
        let val = self.ccu_read(CCU_AHB_GATE_OFFSET);
        self.ccu_write(CCU_AHB_GATE_OFFSET, val | AHB_GATE_SPI0);

        // Set clock divider in the SPI controller.
        match self.gen {
            SunxiGen::Sun4i => self.sun4i_regs().clk_ctl.set(SPI0_CLK_DIV_BY_4),
            SunxiGen::Sun6i => self.sun6i_regs().clk_ctl.set(SPI0_CLK_DIV_BY_4),
        }

        // Enable SPI module clock: bit 31 = enable, source = OSC24M.
        self.ccu_write(CCU_SPI0_CLK_OFFSET, 1 << 31);
    }

    /// Enable the SPI controller and prepare for transfers.
    ///
    /// - sun4i: enable + master + FIFO reset + CS manual in one write
    /// - sun6i: enable + master + soft reset, wait for reset, then
    ///   re-apply clock divider and configure transfer control
    fn enable_controller(&self) {
        match self.gen {
            SunxiGen::Sun4i => {
                self.sun4i_regs().ctl.write(
                    CTL4::ENABLE::SET
                        + CTL4::MASTER::SET
                        + CTL4::TF_RST::SET
                        + CTL4::RF_RST::SET
                        + CTL4::CS_MANUAL::SET
                        + CTL4::CS_ACTIVE_LOW::SET
                        + CTL4::CS_LEVEL::SET
                        + CTL4::TP::SET,
                );
            }
            SunxiGen::Sun6i => {
                let regs = self.sun6i_regs();

                // Enable controller in master mode and trigger soft reset.
                regs.gcr
                    .write(GCR6::ENABLE::SET + GCR6::MASTER::SET + GCR6::SRST::SET);

                // Wait for soft reset to complete (SRST self-clears).
                while regs.gcr.is_set(GCR6::SRST) {
                    core::hint::spin_loop();
                }

                // Re-apply clock divider — soft reset may have cleared it
                // back to the power-on default (0x0002 → CDR1 mode, 12 MHz).
                regs.clk_ctl.set(SPI0_CLK_DIV_BY_4);

                // Configure transfer control: manual CS, active-low,
                // CS starts deasserted (CS_LEVEL=1 → pin HIGH).
                regs.tcr
                    .write(TCR6::CS_MANUAL::SET + TCR6::CS_ACTIVE_LOW::SET + TCR6::CS_LEVEL::SET);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FIFO helpers — byte-width MMIO access
// ---------------------------------------------------------------------------

impl SunxiSpi {
    /// Write a single byte to the TX FIFO.
    ///
    /// Uses byte-width MMIO access.  The Allwinner SPI FIFO pushes one
    /// byte per byte-write; a 32-bit write would push 4 bytes and
    /// corrupt the SPI command frame.  (U-Boot uses `writeb` here.)
    #[inline]
    unsafe fn txd_write_byte(&self, byte: u8) {
        let addr = match self.gen {
            SunxiGen::Sun4i => (self.base + 0x04) as *mut u8,
            SunxiGen::Sun6i => (self.base + 0x200) as *mut u8,
        };
        fstart_mmio::write8(addr, byte);
    }

    /// Read a single byte from the RX FIFO.
    ///
    /// Uses byte-width MMIO access (pops one byte per read).
    /// U-Boot uses `readb` for individual data bytes.
    #[inline]
    unsafe fn rxd_read_byte(&self) -> u8 {
        let addr = match self.gen {
            SunxiGen::Sun4i => self.base as *const u8,
            SunxiGen::Sun6i => (self.base + 0x300) as *const u8,
        };
        fstart_mmio::read8(addr)
    }

    /// Read a 32-bit word from the RX FIFO (drains 4 bytes at once).
    ///
    /// Used to discard the 4-byte command echo in a single read.
    /// U-Boot uses `readl` for this exact purpose.
    #[inline]
    unsafe fn rxd_drain_word(&self) {
        let addr = match self.gen {
            SunxiGen::Sun4i => self.base as *const u32,
            SunxiGen::Sun6i => (self.base + 0x300) as *const u32,
        };
        let _ = fstart_mmio::read32(addr);
    }
}

// ---------------------------------------------------------------------------
// SPI NOR flash read
// ---------------------------------------------------------------------------

impl SunxiSpi {
    /// Perform a single SPI NOR flash read transfer.
    ///
    /// Reads up to `SPI_MAX_XFER` (60) bytes from the given 24-bit
    /// flash address. Returns the number of bytes actually read.
    fn spi_read_chunk(&self, addr: u32, buf: &mut [u8]) -> usize {
        match self.gen {
            SunxiGen::Sun4i => self.spi_read_chunk_sun4i(addr, buf),
            SunxiGen::Sun6i => self.spi_read_chunk_sun6i(addr, buf),
        }
    }

    /// Sun4i (A10/A20) SPI read chunk implementation.
    fn spi_read_chunk_sun4i(&self, addr: u32, buf: &mut [u8]) -> usize {
        let len = buf.len().min(SPI_MAX_XFER);
        if len == 0 {
            return 0;
        }
        let regs = self.sun4i_regs();

        // Reset FIFOs.
        regs.ctl.modify(CTL4::TF_RST::SET + CTL4::RF_RST::SET);

        // Set burst count (total bytes = command + data).
        let total = (SPI_CMD_LEN + len) as u32;
        regs.burst_cnt.set(total);

        // Set transmit count (only the 4-byte command is TX).
        regs.xmit_cnt.set(SPI_CMD_LEN as u32);

        // Assert chip select (CS low).
        regs.ctl.modify(CTL4::CS_LEVEL::CLEAR);

        // Write the Read command + 3-byte address to TX FIFO.
        // CRITICAL: use byte-width writes.  A 32-bit write pushes
        // 4 bytes into the FIFO, corrupting the command.
        unsafe {
            self.txd_write_byte(SPI_CMD_READ);
            self.txd_write_byte((addr >> 16) as u8);
            self.txd_write_byte((addr >> 8) as u8);
            self.txd_write_byte(addr as u8);
        }

        // Start the exchange.
        regs.ctl.modify(CTL4::XCH::SET);

        // Wait for the transfer to complete: poll until RX FIFO has
        // all expected bytes (command echo + data).
        loop {
            let rx_count = regs.fifo_sta.read(FIFO_STA4::RF_CNT);
            if rx_count >= total {
                break;
            }
            core::hint::spin_loop();
        }

        // Discard the 4 command echo bytes (one 32-bit read drains 4).
        unsafe {
            self.rxd_drain_word();
        }

        // Read the actual data bytes (byte-width reads).
        for byte in buf.iter_mut().take(len) {
            *byte = unsafe { self.rxd_read_byte() };
        }

        // Deassert chip select (CS high).
        regs.ctl.modify(CTL4::CS_LEVEL::SET);

        // tSHSL: chip select high time between operations.
        udelay(1);

        len
    }

    /// Sun6i (H3/H2+/A64) SPI read chunk implementation.
    fn spi_read_chunk_sun6i(&self, addr: u32, buf: &mut [u8]) -> usize {
        let len = buf.len().min(SPI_MAX_XFER);
        if len == 0 {
            return 0;
        }
        let regs = self.sun6i_regs();

        // Reset FIFOs (separate FIFO control register on sun6i).
        regs.fifo_ctl
            .write(FIFO_CTL6::TF_RST::SET + FIFO_CTL6::RF_RST::SET);

        // Set burst count (total bytes = command + data).
        let total = (SPI_CMD_LEN + len) as u32;
        regs.mbc.set(total);

        // Set transmit count (only the 4-byte command is TX).
        regs.mtc.set(SPI_CMD_LEN as u32);

        // Sun6i also needs the burst control count set.
        regs.bcc.set(SPI_CMD_LEN as u32);

        // Assert chip select (CS low) — via TCR on sun6i.
        regs.tcr.modify(TCR6::CS_LEVEL::CLEAR);

        // Write the Read command + 3-byte address to TX FIFO.
        // CRITICAL: use byte-width writes.  A 32-bit write pushes
        // 4 bytes into the FIFO, corrupting the command.
        unsafe {
            self.txd_write_byte(SPI_CMD_READ);
            self.txd_write_byte((addr >> 16) as u8);
            self.txd_write_byte((addr >> 8) as u8);
            self.txd_write_byte(addr as u8);
        }

        // Start the exchange (XCH is in TCR on sun6i, not GCR).
        regs.tcr.modify(TCR6::XCH::SET);

        // Wait for the transfer to complete: poll until RX FIFO has
        // all expected bytes.
        loop {
            let rx_count = regs.fifo_sta.read(FIFO_STA6::RF_CNT);
            if rx_count >= total {
                break;
            }
            core::hint::spin_loop();
        }

        // Discard the 4 command echo bytes (one 32-bit read drains 4).
        unsafe {
            self.rxd_drain_word();
        }

        // Read the actual data bytes (byte-width reads).
        for byte in buf.iter_mut().take(len) {
            *byte = unsafe { self.rxd_read_byte() };
        }

        // Deassert chip select (CS high).
        regs.tcr.modify(TCR6::CS_LEVEL::SET);

        // tSHSL: chip select high time between operations.
        udelay(1);

        len
    }
}

// ---------------------------------------------------------------------------
// BlockDevice implementation (shared between generations)
// ---------------------------------------------------------------------------

impl BlockDevice for SunxiSpi {
    /// Read data from SPI NOR flash.
    ///
    /// Breaks large reads into 60-byte chunks (64-byte FIFO minus
    /// 4-byte command overhead) using the SPI Read (0x03) command.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let mut pos = 0usize;
        let mut addr = offset as u32;

        while pos < buf.len() {
            let remaining = buf.len() - pos;
            let chunk = remaining.min(SPI_MAX_XFER);
            let read = self.spi_read_chunk(addr, &mut buf[pos..pos + chunk]);
            if read == 0 {
                return Err(ServiceError::HardwareError);
            }
            pos += read;
            addr += read as u32;
        }

        Ok(pos)
    }

    /// Write is not supported (read-only boot driver).
    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, ServiceError> {
        Err(ServiceError::NotSupported)
    }

    /// Total flash capacity in bytes.
    fn size(&self) -> u64 {
        self.flash_size as u64
    }

    /// SPI NOR flash is byte-addressable.
    fn block_size(&self) -> u32 {
        1
    }
}
