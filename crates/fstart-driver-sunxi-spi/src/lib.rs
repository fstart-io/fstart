//! Allwinner A20 (sun7i) SPI NOR flash boot driver.
//!
//! Minimal read-only driver for booting from SPI NOR flash. Implements
//! `Device` + `BlockDevice` traits. Uses the sun4i-variant SPI controller
//! at 0x01C05000 (SPI0) to read from an attached SPI NOR flash chip via
//! the standard Read Data Bytes command (0x03).
//!
//! Ported from U-Boot `arch/arm/mach-sunxi/spl_spi_sunxi.c` (sun4i
//! paths only). The driver talks directly to SPI hardware registers
//! without using a full SPI framework — same approach as U-Boot's SPL
//! SPI loader for minimal code size in the bootblock.
//!
//! Hardware: Allwinner sun4i-variant SPI controller.
//!
//! The driver handles its own clock gating (AHB + module clock) and
//! GPIO pin mux (PC0-PC2, PC23) during `init()`.

#![no_std]

use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{BlockDevice, ServiceError};

use fstart_sunxi_ccu_regs::SunxiA20CcuRegs;

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Driver configuration (from board RON)
// ---------------------------------------------------------------------------

/// Configuration for the Allwinner A20 SPI NOR flash boot driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiA20SpiConfig {
    /// SPI controller base address (0x01C05000 for SPI0 on A20).
    pub base_addr: u64,
    /// CCU base address (0x01C20000) for clock gating.
    pub ccu_base: u64,
    /// PIO base address (0x01C20800) for GPIO pin mux.
    pub pio_base: u64,
    /// SPI NOR flash capacity in bytes (e.g., 0x01000000 for 16 MiB).
    pub flash_size: u32,
}

// ---------------------------------------------------------------------------
// Register definitions (tock-registers)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Control Register (offset 0x08) — sun4i variant.
    CTL [
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
    FIFO_STA [
        /// RX FIFO byte count (bits 6:0).
        RF_CNT OFFSET(0) NUMBITS(7) [],
    ]
];

register_structs! {
    /// Sun4i SPI controller register block.
    ///
    /// Only the registers needed for SPL-style SPI NOR flash reads
    /// are included; interrupt/DMA registers are omitted.
    pub SunxiSpiRegs {
        /// RX data register. Read received bytes here.
        (0x00 => pub rxdata: MmioReadWrite<u32>),
        /// TX data register. Write bytes to transmit here.
        (0x04 => pub txdata: MmioReadWrite<u32>),
        /// Control register.
        (0x08 => pub ctl: MmioReadWrite<u32, CTL::Register>),
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
        (0x28 => pub fifo_sta: MmioReadWrite<u32, FIFO_STA::Register>),
        (0x2C => @END),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// SPI NOR flash Read Data Bytes command (single-IO, 3-byte address).
const SPI_CMD_READ: u8 = 0x03;

/// SPI FIFO depth on sun4i variant (64 bytes).
const SPI_FIFO_DEPTH: usize = 64;

/// Command overhead: 1 byte opcode + 3 bytes address = 4 bytes.
const SPI_CMD_LEN: usize = 4;

/// Maximum data payload per SPI transfer (FIFO depth minus command).
const SPI_MAX_XFER: usize = SPI_FIFO_DEPTH - SPI_CMD_LEN;

/// Clock divider value: DRS=1 (bit 12), CDR2=1 (bits 7:0).
/// SPI_CLK = OSC24M / (2 * (1+1)) = 6 MHz.
const SPI0_CLK_DIV_BY_4: u32 = 0x1001;

/// AHB gate bit for SPI0.
const AHB_GATE_SPI0: u32 = 1 << 20;

/// GPIO port C configuration register 0 (controls PC0-PC7).
/// Offset from PIO base.
const PIO_PC_CFG0: usize = 0x48;
/// GPIO port C configuration register 2 (controls PC16-PC23).
const PIO_PC_CFG2: usize = 0x50;
/// SPI0 function number for port C pins on A10/A20.
const SPI0_PIN_FUNC: u32 = 3;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Allwinner A20 SPI NOR flash boot driver.
///
/// Provides read-only block device access to an SPI NOR flash chip
/// connected to SPI0. Designed for bootblock use where code size must
/// be minimal.
pub struct SunxiA20Spi {
    /// SPI controller registers.
    regs: &'static SunxiSpiRegs,
    /// CCU registers for clock control.
    ccu: &'static SunxiA20CcuRegs,
    /// PIO base address for GPIO configuration.
    pio_base: usize,
    /// Flash capacity in bytes.
    flash_size: u32,
}

// SAFETY: SunxiA20Spi contains only MMIO register references and config
// values. MMIO accesses are inherently ordered by volatile semantics.
// Firmware is single-threaded at this point.
unsafe impl Send for SunxiA20Spi {}
unsafe impl Sync for SunxiA20Spi {}

impl Device for SunxiA20Spi {
    const NAME: &'static str = "sunxi-a20-spi";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun4i-a10-spi"];

    type Config = SunxiA20SpiConfig;

    fn new(config: &Self::Config) -> Result<Self, DeviceError> {
        if config.base_addr == 0 {
            return Err(DeviceError::MissingResource("base_addr"));
        }
        // 3-byte addressing (SPI command 0x03) can only reach 16 MiB.
        if config.flash_size > 0x0100_0000 {
            return Err(DeviceError::ConfigError);
        }

        // SAFETY: MMIO addresses are valid hardware register blocks.
        let regs = unsafe { &*(config.base_addr as *const SunxiSpiRegs) };
        let ccu = unsafe { &*(config.ccu_base as *const SunxiA20CcuRegs) };

        Ok(Self {
            regs,
            ccu,
            pio_base: config.pio_base as usize,
            flash_size: config.flash_size,
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

impl SunxiA20Spi {
    /// Configure PC0 (MOSI), PC1 (MISO), PC2 (CLK), PC23 (CS0) as
    /// SPI0 function (function 3 on A10/A20).
    fn setup_gpio(&self) {
        // PC0-PC2: bits [11:0] in PC_CFG0, 4 bits per pin.
        // SAFETY: PIO registers are valid MMIO.
        unsafe {
            let cfg0 = (self.pio_base + PIO_PC_CFG0) as *mut u32;
            let val = core::ptr::read_volatile(cfg0);
            // Clear bits [11:0] (PC0-PC2 function select), set to function 3.
            let val = (val & !0xFFF)
                | SPI0_PIN_FUNC          // PC0 = SPI0_MOSI
                | (SPI0_PIN_FUNC << 4)   // PC1 = SPI0_MISO
                | (SPI0_PIN_FUNC << 8); // PC2 = SPI0_CLK
            core::ptr::write_volatile(cfg0, val);

            // PC23: bits [31:28] in PC_CFG2 (PC16-PC23, 4 bits per pin).
            // PC23 is at offset (23 - 16) * 4 = 28 bits.
            let cfg2 = (self.pio_base + PIO_PC_CFG2) as *mut u32;
            let val = core::ptr::read_volatile(cfg2);
            let val = (val & !(0xF << 28)) | (SPI0_PIN_FUNC << 28); // PC23 = SPI0_CS0
            core::ptr::write_volatile(cfg2, val);
        }
    }

    /// Enable AHB gate and SPI module clock.
    ///
    /// Sets SPI0 module clock source to OSC24M with divide-by-4
    /// (resulting in 6 MHz SPI clock — same as what the BROM uses).
    fn setup_clocks(&self) {
        // Open AHB gate for SPI0 (bit 20 of AHB_GATE0).
        let gate = self.ccu.ahb_gate0.get();
        self.ccu.ahb_gate0.set(gate | AHB_GATE_SPI0);

        // Enable SPI module clock: bit 31 = enable, source = OSC24M (default).
        self.ccu.spi0_clk.set(1 << 31);

        // Set clock divider in the SPI controller.
        self.regs.clk_ctl.set(SPI0_CLK_DIV_BY_4);
    }

    /// Enable the SPI controller in master mode and reset FIFOs.
    fn enable_controller(&self) {
        self.regs.ctl.write(
            CTL::ENABLE::SET
                + CTL::MASTER::SET
                + CTL::TF_RST::SET
                + CTL::RF_RST::SET
                + CTL::CS_MANUAL::SET
                + CTL::CS_ACTIVE_LOW::SET
                + CTL::TP::SET,
        );
    }

    /// Perform a single SPI NOR flash read transfer.
    ///
    /// Reads up to `SPI_MAX_XFER` (60) bytes from the given 24-bit
    /// flash address. Returns the number of bytes actually read.
    fn spi_read_chunk(&self, addr: u32, buf: &mut [u8]) -> usize {
        let len = buf.len().min(SPI_MAX_XFER);
        if len == 0 {
            return 0;
        }

        // Reset FIFOs.
        self.regs.ctl.modify(CTL::TF_RST::SET + CTL::RF_RST::SET);

        // Set burst count (total bytes = command + data).
        let total = (SPI_CMD_LEN + len) as u32;
        self.regs.burst_cnt.set(total);

        // Set transmit count (only the 4-byte command is TX).
        self.regs.xmit_cnt.set(SPI_CMD_LEN as u32);

        // Assert chip select (CS low).
        self.regs.ctl.modify(CTL::CS_LEVEL::CLEAR);

        // Write the Read command + 3-byte address to TX FIFO.
        self.regs.txdata.set(SPI_CMD_READ as u32);
        self.regs.txdata.set((addr >> 16) & 0xFF);
        self.regs.txdata.set((addr >> 8) & 0xFF);
        self.regs.txdata.set(addr & 0xFF);

        // Start the exchange.
        self.regs.ctl.modify(CTL::XCH::SET);

        // Wait for the transfer to complete: poll until RX FIFO has
        // all expected bytes (command echo + data).
        let expected = total;
        loop {
            let rx_count = self.regs.fifo_sta.read(FIFO_STA::RF_CNT);
            if rx_count >= expected {
                break;
            }
            core::hint::spin_loop();
        }

        // Discard the 4 command echo bytes from RX FIFO.
        for _ in 0..SPI_CMD_LEN {
            let _ = self.regs.rxdata.get();
        }

        // Read the actual data bytes.
        for byte in buf.iter_mut().take(len) {
            *byte = self.regs.rxdata.get() as u8;
        }

        // Deassert chip select (CS high).
        self.regs.ctl.modify(CTL::CS_LEVEL::SET);

        // tSHSL: chip select high time between operations.
        udelay(1);

        len
    }
}

impl BlockDevice for SunxiA20Spi {
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
