//! Allwinner sunxi SPI0 controller driver (A20 sun4i, H3/H2+/H5 sun6i).
//!
//! Minimal boot driver: burst engine implementing
//! [`SpiBus`](embedded_hal::spi::SpiBus) plus board-policy construction.
//! NOR framing (opcodes, chunking) lives in [`crate::spi_nor`]; per-SoC
//! differences (register layout, clock gating/reset, CDR1 passthrough, and
//! chip-select pin mux) are captured in [`SpiVariant`].
//!
//! Ported from U-Boot `arch/arm/mach-sunxi/spl_spi_sunxi.c`.

use core::marker::PhantomData;

use embedded_hal::spi::{ErrorType, SpiBus};
use fstart_arch::udelay;
use fstart_core::mmio::{self, MmioReadWrite};
use fstart_core::mmio32;
use fstart_core::services::ServiceError;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

use crate::a20_ccu::{A20_CCU_BASE, A20_PIO_BASE, A20_SPI0_BASE};
use crate::h3_ccu::{H3_CCU_BASE, H3_PIO_BASE, H3_SPI0_BASE};
use crate::pio::{PORT_C, PioGen, SunxiPio};
use crate::spi_nor::{
    NOR_FAST_READ_THRESHOLD_HZ, NOR_MAX_3BYTE_SIZE, SpiClockReport, SpiNorFlash, SunxiSpiError,
};

// ---------------------------------------------------------------------------
// Board policy
// ---------------------------------------------------------------------------

/// Sunxi SPI0 NOR flash policy. Fixed controller/CCU/PIO addresses are not
/// configuration.
#[derive(Debug, Clone, Copy)]
pub struct SunxiSpiConfig {
    /// Desired SPI bus clock in Hz. The achieved frequency is at or below
    /// this; above 25 MHz the NOR layer switches to Fast Read.
    pub spi_freq: u32,
    /// SPI NOR flash capacity in bytes (3-byte addressing: max 16 MiB).
    pub flash_size: u32,
}

impl SunxiSpiConfig {
    /// Construct the SPI0 policy for the board's flash and speed.
    #[must_use]
    pub const fn new(spi_freq: u32, flash_size: u32) -> Self {
        Self {
            spi_freq,
            flash_size,
        }
    }

    /// Validate the SPI0 policy.
    #[must_use]
    pub const fn build(self) -> Self {
        if self.spi_freq == 0 {
            panic!("sunxi SPI frequency must be non-zero");
        }
        if self.flash_size == 0 || self.flash_size > NOR_MAX_3BYTE_SIZE {
            panic!("sunxi SPI flash size must fit 3-byte addressing");
        }
        self
    }
}

/// A20 SPI0 defaults: 6 MHz on OSC24M (plain Read), 16 MiB max window.
pub const A20_SPI_DEFAULT_FREQ: u32 = 6_000_000;
/// H3 SPI0 defaults: 24 MHz on OSC24M (plain Read, CDR1 passthrough),
/// 16 MiB max window.
pub const H3_SPI_DEFAULT_FREQ: u32 = 24_000_000;
/// Default 3-byte-addressed SPI NOR window (16 MiB).
pub const SUNXI_SPI_DEFAULT_SIZE: u32 = NOR_MAX_3BYTE_SIZE;

// ---------------------------------------------------------------------------
// Sun4i register definitions (A10/A20)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Sun4i Control Register (offset 0x08, 32-bit): global + transfer + FIFO control.
    SUN4I_CTL [
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
        TP OFFSET(18) NUMBITS(1) []
    ],
    /// Sun4i FIFO Status Register (offset 0x28, 32-bit).
    SUN4I_FIFO_STA [
        /// RX FIFO byte count (bits 6:0).
        RF_CNT OFFSET(0) NUMBITS(7) []
    ]
];

register_structs! {
    /// Sun4i SPI controller register block (only SPL-needed registers).
    pub Sun4iSpiRegs {
        /// RX data register (offset 0x00, 32-bit).
        (0x00 => pub rxdata: MmioReadWrite<u32>),
        /// TX data register (offset 0x04, 32-bit).
        (0x04 => pub txdata: MmioReadWrite<u32>),
        /// Control register (offset 0x08, 32-bit).
        (0x08 => pub ctl: MmioReadWrite<u32, SUN4I_CTL::Register>),
        /// Interrupt/DMA/wait registers (unused).
        (0x0C => _reserved: [u8; 0x10]),
        /// Clock control (offset 0x1C, 32-bit): bit 12 (DRS) selects CDR2 (bits 7:0);
        /// SPI_CLK = MOD_CLK / (2 * (CDR2 + 1)).
        (0x1C => pub clk_ctl: MmioReadWrite<u32>),
        /// Burst count (offset 0x20, 32-bit) — total bytes in the transfer (TX + RX).
        (0x20 => pub burst_cnt: MmioReadWrite<u32>),
        /// Transmit count (offset 0x24, 32-bit) — bytes actually transmitted.
        (0x24 => pub xmit_cnt: MmioReadWrite<u32>),
        /// FIFO status register (offset 0x28, 32-bit).
        (0x28 => pub fifo_sta: MmioReadWrite<u32, SUN4I_FIFO_STA::Register>),
        (0x2C => @END),
    }
}

// ---------------------------------------------------------------------------
// Sun6i register definitions (H3/H2+/A64)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Sun6i Global Control Register (offset 0x04, 32-bit).
    SUN6I_GCR [
        /// SPI controller enable.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Master mode.
        MASTER OFFSET(1) NUMBITS(1) [],
        /// Soft reset (self-clearing).
        SRST OFFSET(31) NUMBITS(1) []
    ],
    /// Sun6i Transfer Control Register (offset 0x08, 32-bit).
    SUN6I_TCR [
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
        XCH OFFSET(31) NUMBITS(1) []
    ],
    /// Sun6i FIFO Control Register (offset 0x18, 32-bit).
    SUN6I_FIFO_CTL [
        /// RX FIFO reset (self-clearing).
        RF_RST OFFSET(15) NUMBITS(1) [],
        /// TX FIFO reset (self-clearing).
        TF_RST OFFSET(31) NUMBITS(1) []
    ],
    /// Sun6i FIFO Status Register (offset 0x1C, 32-bit, 8-bit count on sun6i).
    SUN6I_FIFO_STA [
        /// RX FIFO byte count (bits 7:0).
        RF_CNT OFFSET(0) NUMBITS(8) []
    ]
];

register_structs! {
    /// Sun6i SPI controller register block (only SPL-needed registers).
    pub Sun6iSpiRegs {
        (0x000 => _res0: [u8; 0x04]),
        /// Global control (offset 0x004, 32-bit): enable, master, soft reset.
        (0x004 => pub gcr: MmioReadWrite<u32, SUN6I_GCR::Register>),
        /// Transfer control (offset 0x008, 32-bit): CS, polarity, exchange.
        (0x008 => pub tcr: MmioReadWrite<u32, SUN6I_TCR::Register>),
        (0x00C => _res1: [u8; 0x0C]),
        /// FIFO control (offset 0x018, 32-bit): FIFO resets.
        (0x018 => pub fifo_ctl: MmioReadWrite<u32, SUN6I_FIFO_CTL::Register>),
        /// FIFO status (offset 0x01C, 32-bit): RX count.
        (0x01C => pub fifo_sta: MmioReadWrite<u32, SUN6I_FIFO_STA::Register>),
        (0x020 => _res2: [u8; 0x04]),
        /// Clock control (offset 0x024, 32-bit): same CDR format as sun4i.
        (0x024 => pub clk_ctl: MmioReadWrite<u32>),
        (0x028 => _res3: [u8; 0x08]),
        /// Master burst count (offset 0x030, 32-bit) — total bytes (TX + RX).
        (0x030 => pub mbc: MmioReadWrite<u32>),
        /// Master transmit count (offset 0x034, 32-bit) — bytes transmitted.
        (0x034 => pub mtc: MmioReadWrite<u32>),
        /// Burst control count (offset 0x038, 32-bit).
        (0x038 => pub bcc: MmioReadWrite<u32>),
        (0x03C => _res4: [u8; 0x1C4]),
        /// TX data FIFO window (offset 0x200, 32-bit).
        (0x200 => pub txd: MmioReadWrite<u32>),
        (0x204 => _res5: [u8; 0xFC]),
        /// RX data FIFO window (offset 0x300, 32-bit).
        (0x300 => pub rxd: MmioReadWrite<u32>),
        (0x304 => @END),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// SPI FIFO depth (64 bytes).
const SPI_FIFO_DEPTH: usize = 64;

/// OSC24M crystal frequency (always available, no PLL dependency).
const OSC24M_FREQ: u32 = 24_000_000;

/// PLL_PERIPH frequency. The CCU driver enables it before this driver runs.
const PLL_PERIPH_FREQ: u32 = 600_000_000;

/// Poll iterations for FIFO completion and soft reset (~5 us per 64-byte
/// transfer at 100 MHz; 1M iterations is ample margin on a 1 GHz core).
const SPI_POLL_TIMEOUT: u32 = 1_000_000;

/// AHB gate bit for SPI0 (CCU + 0x060) and bus-reset bit (CCU + 0x2C0).
const AHB_GATE_SPI0: u32 = 1 << 20;
const BUS_RESET_SPI0: u32 = 1 << 20;

/// CCU register offsets.
const CCU_AHB_GATE_OFFSET: usize = 0x060;
const CCU_SPI0_CLK_OFFSET: usize = 0x0A0;
const CCU_BUS_RESET0_OFFSET: usize = 0x2C0;

/// SPI0 alternate function on port C.
const SPI0_PIN_FUNC: u8 = 3;

// ---------------------------------------------------------------------------
// Per-SoC variant parameterization (sealed)
// ---------------------------------------------------------------------------

mod sealed {
    use fstart_core::services::ServiceError;

    pub trait Sealed {
        /// SPI0 register block physical base address.
        const BASE: u64;
        /// CCU physical base address.
        const CCU_BASE: u64;
        /// PIO physical base address.
        const PIO_BASE: u64;
        /// Chip-select 0 pin on Port C (`23` on A20 sun4i, `3` on H3 sun6i).
        const CS_PIN: u8;
        /// Whether the CCU has a separate bus-reset register at `0x2C0`.
        const HAS_BUS_RESET: bool;
        /// Whether `clk_ctl = 0` provides a 1:1 CDR1 passthrough.
        const HAS_CDR1_PASSTHROUGH: bool;

        /// Program the controller clock-divider register.
        fn set_clk_ctl(clk_ctl: u32);
        /// Enable the controller (and perform soft reset on sun6i).
        fn enable_controller(clk_ctl: u32) -> Result<(), ServiceError>;
        /// Drive CS0 active (low).
        fn cs_assert();
        /// Drive CS0 inactive (high).
        fn cs_deassert();
        /// Reset TX and RX FIFOs.
        fn reset_fifos();
        /// Program transfer counts, load TX bytes into the FIFO, and kick `XCH`.
        ///
        /// # Safety
        ///
        /// Caller must ensure `tx.len() <= rx_len <= SPI_FIFO_DEPTH` and own the
        /// SPI0 controller.
        unsafe fn start_burst(tx: &[u8], rx_len: usize);
        /// Current number of bytes available in the RX FIFO.
        fn rx_fifo_count() -> u32;
        /// Pop a single byte from the RX FIFO.
        fn pop_rx_byte() -> u8;
    }
}

/// Hardware parameters that vary across Allwinner SPI0 generations.
///
/// Sealed so downstream crates cannot implement it with arbitrary MMIO
/// addresses or invoke low-level register methods directly.
pub trait SpiVariant: sealed::Sealed {}

/// Allwinner A20 (sun7i / sun4i SPI IP) variant.
pub enum A20 {}

impl A20 {
    #[inline(always)]
    fn regs() -> &'static Sun4iSpiRegs {
        // SAFETY: fixed A20 SPI0 MMIO register block.
        unsafe { &*(A20_SPI0_BASE as usize as *const Sun4iSpiRegs) }
    }
}

impl sealed::Sealed for A20 {
    const BASE: u64 = A20_SPI0_BASE;
    const CCU_BASE: u64 = A20_CCU_BASE;
    const PIO_BASE: u64 = A20_PIO_BASE;
    const CS_PIN: u8 = 23;
    const HAS_BUS_RESET: bool = false;
    const HAS_CDR1_PASSTHROUGH: bool = false;

    fn set_clk_ctl(clk_ctl: u32) {
        Self::regs().clk_ctl.set(clk_ctl);
    }

    fn enable_controller(_clk_ctl: u32) -> Result<(), ServiceError> {
        Self::regs().ctl.write(
            SUN4I_CTL::ENABLE::SET
                + SUN4I_CTL::MASTER::SET
                + SUN4I_CTL::TF_RST::SET
                + SUN4I_CTL::RF_RST::SET
                + SUN4I_CTL::CS_MANUAL::SET
                + SUN4I_CTL::CS_ACTIVE_LOW::SET
                + SUN4I_CTL::CS_LEVEL::SET
                + SUN4I_CTL::TP::SET,
        );
        Ok(())
    }

    fn cs_assert() {
        Self::regs().ctl.modify(SUN4I_CTL::CS_LEVEL::CLEAR);
    }

    fn cs_deassert() {
        Self::regs().ctl.modify(SUN4I_CTL::CS_LEVEL::SET);
    }

    fn reset_fifos() {
        Self::regs()
            .ctl
            .modify(SUN4I_CTL::TF_RST::SET + SUN4I_CTL::RF_RST::SET);
    }

    unsafe fn start_burst(tx: &[u8], rx_len: usize) {
        let regs = Self::regs();
        regs.burst_cnt.set(rx_len as u32);
        regs.xmit_cnt.set(tx.len() as u32);
        // Byte-width writes: one FIFO byte per write (a 32-bit write would
        // push 4 bytes and corrupt the frame).
        for &byte in tx {
            // SAFETY: fixed A20 SPI0 TX FIFO address.
            unsafe { mmio::write8((A20_SPI0_BASE as usize + 0x04) as *mut u8, byte) };
        }
        regs.ctl.modify(SUN4I_CTL::XCH::SET);
    }

    fn rx_fifo_count() -> u32 {
        Self::regs().fifo_sta.read(SUN4I_FIFO_STA::RF_CNT)
    }

    fn pop_rx_byte() -> u8 {
        // SAFETY: fixed A20 SPI0 RX FIFO address; one byte popped per read.
        unsafe { mmio::read8(A20_SPI0_BASE as usize as *const u8) }
    }
}

impl SpiVariant for A20 {}

/// Allwinner H3/H2+/H5 (sun8i / sun6i SPI IP) variant.
pub enum H3 {}

impl H3 {
    #[inline(always)]
    fn regs() -> &'static Sun6iSpiRegs {
        // SAFETY: fixed H3 SPI0 MMIO register block.
        unsafe { &*(H3_SPI0_BASE as usize as *const Sun6iSpiRegs) }
    }
}

impl sealed::Sealed for H3 {
    const BASE: u64 = H3_SPI0_BASE;
    const CCU_BASE: u64 = H3_CCU_BASE;
    const PIO_BASE: u64 = H3_PIO_BASE;
    const CS_PIN: u8 = 3;
    const HAS_BUS_RESET: bool = true;
    const HAS_CDR1_PASSTHROUGH: bool = true;

    fn set_clk_ctl(clk_ctl: u32) {
        Self::regs().clk_ctl.set(clk_ctl);
    }

    fn enable_controller(clk_ctl: u32) -> Result<(), ServiceError> {
        let regs = Self::regs();
        regs.gcr
            .write(SUN6I_GCR::ENABLE::SET + SUN6I_GCR::MASTER::SET + SUN6I_GCR::SRST::SET);
        // Wait for the soft reset to self-clear.
        let mut timeout = SPI_POLL_TIMEOUT;
        while regs.gcr.is_set(SUN6I_GCR::SRST) {
            timeout -= 1;
            if timeout == 0 {
                fstart_log::error!("spi0: soft reset timeout");
                return Err(ServiceError::HardwareError);
            }
            core::hint::spin_loop();
        }
        // Soft reset clears the clock divider; re-apply it.
        regs.clk_ctl.set(clk_ctl);
        // Manual CS, active-low, deasserted (pin HIGH).
        regs.tcr.write(
            SUN6I_TCR::CS_MANUAL::SET + SUN6I_TCR::CS_ACTIVE_LOW::SET + SUN6I_TCR::CS_LEVEL::SET,
        );
        Ok(())
    }

    fn cs_assert() {
        Self::regs().tcr.modify(SUN6I_TCR::CS_LEVEL::CLEAR);
    }

    fn cs_deassert() {
        Self::regs().tcr.modify(SUN6I_TCR::CS_LEVEL::SET);
    }

    fn reset_fifos() {
        // Preserve trigger-level bits; only reset the FIFOs.
        Self::regs()
            .fifo_ctl
            .modify(SUN6I_FIFO_CTL::TF_RST::SET + SUN6I_FIFO_CTL::RF_RST::SET);
    }

    unsafe fn start_burst(tx: &[u8], rx_len: usize) {
        let regs = Self::regs();
        regs.mbc.set(rx_len as u32);
        regs.mtc.set(tx.len() as u32);
        regs.bcc.set(tx.len() as u32);
        // Byte-width writes: one FIFO byte per write.
        for &byte in tx {
            // SAFETY: fixed H3 SPI0 TX FIFO window.
            unsafe { mmio::write8((H3_SPI0_BASE as usize + 0x200) as *mut u8, byte) };
        }
        regs.tcr.modify(SUN6I_TCR::XCH::SET);
    }

    fn rx_fifo_count() -> u32 {
        // Mask to 7 bits like U-Boot: only the count matters and bit 7
        // may be a status flag on some sun6i revisions.
        Self::regs().fifo_sta.read(SUN6I_FIFO_STA::RF_CNT) & 0x7F
    }

    fn pop_rx_byte() -> u8 {
        // SAFETY: fixed H3 SPI0 RX FIFO window; one byte popped per read.
        unsafe { mmio::read8((H3_SPI0_BASE as usize + 0x300) as *const u8) }
    }
}

impl SpiVariant for H3 {}

/// Allwinner A20 SPI0 controller.
pub type A20Spi = SunxiSpi<A20>;
/// Allwinner H3/H2+/H5 SPI0 controller.
pub type H3Spi = SunxiSpi<H3>;
/// NOR flash behind an A20 SPI0 controller.
pub type A20SpiFlash = SpiNorFlash<A20Spi>;
/// NOR flash behind an H3 SPI0 controller.
pub type H3SpiFlash = SpiNorFlash<H3Spi>;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Parameterized sunxi SPI0 controller.
pub struct SunxiSpi<V: SpiVariant> {
    spi_clk_ctl: u32,
    ccu_spi_clk: u32,
    achieved_hz: u32,
    _variant: PhantomData<V>,
}

impl<V: SpiVariant> SunxiSpi<V> {
    /// Construct without touching hardware.
    #[must_use]
    pub fn new_from_config(config: &'static SunxiSpiConfig) -> Self {
        let (ccu_spi_clk, spi_clk_ctl, achieved_hz) =
            compute_clock(config.spi_freq, V::HAS_CDR1_PASSTHROUGH);
        Self {
            spi_clk_ctl,
            ccu_spi_clk,
            achieved_hz,
            _variant: PhantomData,
        }
    }

    /// Achieved bus clock in Hz (at or below the configured frequency).
    #[must_use]
    pub const fn achieved_hz(&self) -> u32 {
        self.achieved_hz
    }

    /// Whether the bus runs fast enough to require Fast Read (0x0B).
    #[must_use]
    pub const fn fast_read(&self) -> bool {
        self.achieved_hz > NOR_FAST_READ_THRESHOLD_HZ
    }

    /// Configure pins/clocks and enable the controller.
    pub fn init(&mut self) -> Result<(), ServiceError> {
        self.setup_gpio();
        self.setup_clocks();
        V::enable_controller(self.spi_clk_ctl)?;
        fstart_log::info!("spi0: {} MHz", self.achieved_hz / 1_000_000);
        Ok(())
    }

    #[inline(always)]
    fn ccu_read(offset: usize) -> u32 {
        // SAFETY: fixed sunxi CCU mapping for this sealed variant.
        unsafe { mmio::read32((V::CCU_BASE as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn ccu_write(offset: usize, val: u32) {
        // SAFETY: fixed sunxi CCU mapping for this sealed variant.
        unsafe { mmio::write32((V::CCU_BASE as usize + offset) as *mut u32, val) }
    }

    /// PC0 (MOSI), PC1 (MISO), PC2 (CLK), and variant CS0 pin to SPI0 func 3.
    fn setup_gpio(&self) {
        let pio = SunxiPio::new(mmio32(V::PIO_BASE), PioGen::Legacy);
        pio.set_function(PORT_C, 0, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 1, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 2, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, V::CS_PIN, SPI0_PIN_FUNC);
    }

    /// Deassert bus reset (if present), open the AHB gate, and program dividers.
    fn setup_clocks(&self) {
        if V::HAS_BUS_RESET {
            Self::ccu_write(
                CCU_BUS_RESET0_OFFSET,
                Self::ccu_read(CCU_BUS_RESET0_OFFSET) | BUS_RESET_SPI0,
            );
        }
        Self::ccu_write(
            CCU_AHB_GATE_OFFSET,
            Self::ccu_read(CCU_AHB_GATE_OFFSET) | AHB_GATE_SPI0,
        );
        V::set_clk_ctl(self.spi_clk_ctl);
        Self::ccu_write(CCU_SPI0_CLK_OFFSET, self.ccu_spi_clk);
    }

    fn cs_assert(&self) {
        V::cs_assert();
    }

    fn cs_deassert(&self) {
        V::cs_deassert();
        // tSHSL: chip-select high time between operations.
        udelay(1);
    }

    /// One FIFO-sized burst: clock `rx.len()` bytes while transmitting the
    /// `tx.len()` prefix (`tx.len() <= rx.len() <= 64`).
    fn burst(&self, tx: &[u8], rx: &mut [u8]) -> Result<(), SunxiSpiError> {
        if tx.len() > rx.len() || rx.len() > SPI_FIFO_DEPTH {
            return Err(SunxiSpiError::TooLong);
        }
        V::reset_fifos();
        // SAFETY: `tx.len() <= rx.len() <= SPI_FIFO_DEPTH` checked above and `self` owns SPI0.
        unsafe { V::start_burst(tx, rx.len()) };
        let mut timeout = SPI_POLL_TIMEOUT;
        loop {
            if V::rx_fifo_count() >= rx.len() as u32 {
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                // Quiesce the FIFOs so a later re-init starts clean. The
                // controller still needs re-init() after a timeout; the
                // bootblock halts on error, so no retry path exists today.
                V::reset_fifos();
                return Err(SunxiSpiError::Timeout);
            }
            core::hint::spin_loop();
        }
        for slot in rx.iter_mut() {
            *slot = V::pop_rx_byte();
        }
        Ok(())
    }

    /// Full-duplex transfer, chunked to the FIFO, CS held throughout.
    fn transact_in_place(&self, words: &mut [u8]) -> Result<(), SunxiSpiError> {
        let mut mirror = [0u8; SPI_FIFO_DEPTH];
        for chunk in words.chunks_mut(SPI_FIFO_DEPTH) {
            mirror[..chunk.len()].copy_from_slice(chunk);
            self.burst(&mirror[..chunk.len()], chunk)?;
        }
        Ok(())
    }

    /// Unequal-length full-duplex under one CS assertion: chunk over the
    /// longer side, pad short TX with `0xFF`, keep only fitting RX bytes.
    fn duplex_bursts(&self, read: &mut [u8], write: &[u8]) -> Result<(), SunxiSpiError> {
        let mut tx = [0xFFu8; SPI_FIFO_DEPTH];
        let mut rx = [0u8; SPI_FIFO_DEPTH];
        let total = read.len().max(write.len());
        let mut pos = 0;
        while pos < total {
            let n = (total - pos).min(SPI_FIFO_DEPTH);
            let w_avail = write.len().saturating_sub(pos).min(n);
            if w_avail > 0 {
                tx[..w_avail].copy_from_slice(&write[pos..pos + w_avail]);
            }
            tx[w_avail..n].fill(0xFF);
            self.burst(&tx[..n], &mut rx[..n])?;
            let r_avail = read.len().saturating_sub(pos).min(n);
            if r_avail > 0 {
                read[pos..pos + r_avail].copy_from_slice(&rx[..r_avail]);
            }
            pos += n;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Clock computation (pure — unit tested)
// ---------------------------------------------------------------------------

/// Compute CCU source/N/M dividers and the controller CDR divider for a
/// bus frequency at or below `target`. Returns
/// `(ccu_spi_clk_reg, spi_clk_ctl_reg, actual_hz)`.
///
/// The result is safe but not necessarily closest: the smallest CCU divider
/// is tried first, so CDR2 may undershoot where a larger divider would land
/// nearer the target. Never exceeds `target`.
///
/// - **<= 24 MHz**: OSC24M (no PLL dependency).
/// - **> 24 MHz**: PLL_PERIPH (600 MHz) via CCU N/M plus CDR2.
/// - When `cdr1_passthrough` is true (sun6i), `clk_ctl = 0` passes the
///   module clock through 1:1 whenever `target >= mod_clk`.
fn compute_clock(target: u32, cdr1_passthrough: bool) -> (u32, u32, u32) {
    // Rejected by SunxiSpiConfig::build(); 0 would divide by zero below.
    debug_assert!(target > 0);
    // u64 math: validated policies stay far below u32::MAX, but the
    // divider search must not wrap on absurd inputs either.
    if target <= OSC24M_FREQ {
        let ccu_val = 1u32 << 31;
        if cdr1_passthrough && target >= OSC24M_FREQ {
            return (ccu_val, 0, OSC24M_FREQ);
        }
        let (clk_ctl, actual) = compute_cdr2(u64::from(OSC24M_FREQ), u64::from(target));
        (ccu_val, clk_ctl, actual)
    } else {
        // Smallest CCU divider d = 2^N * (M+1) with PLL/d/2 <= target.
        let min_ccu_div = (u64::from(PLL_PERIPH_FREQ)).div_ceil(2 * u64::from(target));
        let (n, m) = find_ccu_nm(min_ccu_div);
        let mod_clk = u64::from(PLL_PERIPH_FREQ) / (1 << n) / (u64::from(m) + 1);
        let ccu_val = (1u32 << 31) | (1u32 << 24) | (n << 16) | m;
        if cdr1_passthrough && u64::from(target) >= mod_clk {
            return (ccu_val, 0, mod_clk as u32);
        }
        let (clk_ctl, actual) = compute_cdr2(mod_clk, u64::from(target));
        (ccu_val, clk_ctl, actual)
    }
}

/// CDR2 divider: `SPI_CLK = mod_clk / (2 * (CDR2 + 1))`, never above target.
fn compute_cdr2(mod_clk: u64, target: u64) -> (u32, u32) {
    let div = mod_clk.div_ceil(2 * target);
    let cdr2 = div.saturating_sub(1).min(255) as u32;
    (
        1 << 12 | cdr2,
        (mod_clk / (2 * (u64::from(cdr2) + 1))) as u32,
    )
}

/// Smallest `(N, M)` with `(2^N) * (M + 1) >= min_div`.
/// N: 0-3 (divider 1, 2, 4, 8); M: 0-15 (divider 1-16).
fn find_ccu_nm(min_div: u64) -> (u32, u32) {
    for n in 0..=3u32 {
        let n_div = 1u64 << n;
        if n_div >= min_div {
            return (n, 0);
        }
        let m = min_div.div_ceil(n_div) - 1;
        if m <= 15 {
            return (n, m as u32);
        }
    }
    (3, 15)
}

// ---------------------------------------------------------------------------
// embedded-hal SpiBus — dumb full-duplex bursts, no NOR knowledge
// ---------------------------------------------------------------------------

impl<V: SpiVariant> ErrorType for SunxiSpi<V> {
    type Error = SunxiSpiError;
}

impl<V: SpiVariant> SpiBus<u8> for SunxiSpi<V> {
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        if words.is_empty() {
            return Ok(());
        }
        const FF: [u8; SPI_FIFO_DEPTH] = [0xFF; SPI_FIFO_DEPTH];
        self.cs_assert();
        let mut result = Ok(());
        for chunk in words.chunks_mut(SPI_FIFO_DEPTH) {
            if let Err(e) = self.burst(&FF[..chunk.len()], chunk) {
                result = Err(e);
                break;
            }
        }
        self.cs_deassert();
        result
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        if words.is_empty() {
            return Ok(());
        }
        let mut discard = [0u8; SPI_FIFO_DEPTH];
        self.cs_assert();
        let mut result = Ok(());
        for chunk in words.chunks(SPI_FIFO_DEPTH) {
            if let Err(e) = self.burst(chunk, &mut discard[..chunk.len()]) {
                result = Err(e);
                break;
            }
        }
        self.cs_deassert();
        result
    }

    /// Full-duplex over `max(read.len(), write.len())` clocks per the
    /// embedded-hal contract: the short TX side is padded with `0xFF` and
    /// RX beyond `read.len()` is discarded.
    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        if read.is_empty() {
            return self.write(write);
        }
        if write.is_empty() {
            return self.read(read);
        }
        if read.len() == write.len() {
            read.copy_from_slice(write);
            return self.transfer_in_place(read);
        }
        self.cs_assert();
        let result = self.duplex_bursts(read, write);
        self.cs_deassert();
        result
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        if words.is_empty() {
            return Ok(());
        }
        self.cs_assert();
        let result = self.transact_in_place(words);
        self.cs_deassert();
        result
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<V: SpiVariant> SpiClockReport for SunxiSpi<V> {
    fn fast_read(&self) -> bool {
        self.fast_read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a20_clock_stays_at_or_below_target_on_osc24m() {
        // 6 MHz: div = ceil(24/12) = 2 -> CDR2 = 1 -> exactly 6 MHz.
        let (ccu, ctl, actual) = compute_clock(6_000_000, false);
        assert_eq!(ccu, 1 << 31);
        assert_eq!(ctl, (1 << 12) | 1);
        assert_eq!(actual, 6_000_000);
    }

    #[test]
    fn a20_clock_rounds_down_to_cdr2_steps() {
        // 10 MHz is not reachable: div = ceil(24/20) = 2 -> 6 MHz.
        let (_, _, actual) = compute_clock(10_000_000, false);
        assert_eq!(actual, 6_000_000);
        // sun4i has no 1:1 passthrough: CDR2 = 0 still divides by 2,
        // so OSC24M tops out at 12 MHz.
        let (_, ctl, actual) = compute_clock(24_000_000, false);
        assert_eq!(ctl, 1 << 12);
        assert_eq!(actual, 12_000_000);
    }

    #[test]
    fn h3_clock_prefers_cdr1_passthrough_at_24mhz() {
        let (ccu, ctl, actual) = compute_clock(24_000_000, true);
        assert_eq!(ccu, 1 << 31);
        assert_eq!(ctl, 0);
        assert_eq!(actual, 24_000_000);
    }

    #[test]
    fn h3_clock_passes_through_mod_clock() {
        // 600 MHz: CCU divider 1 -> MOD 600 MHz, CDR1 passthrough exact.
        let (ccu, ctl, actual) = compute_clock(600_000_000, true);
        assert_eq!(ccu, (1 << 31) | (1 << 24));
        assert_eq!(ctl, 0);
        assert_eq!(actual, 600_000_000);
    }

    #[test]
    fn clock_uses_pll_above_24mhz() {
        // 50 MHz: CCU div 6 (N=0, M=5) -> MOD 100 MHz -> CDR2 = 0 -> 50 MHz.
        for cdr1 in [false, true] {
            let (ccu, ctl, actual) = compute_clock(50_000_000, cdr1);
            assert_eq!(ccu, (1 << 31) | (1 << 24) | 5);
            assert_eq!(ctl, 1 << 12);
            assert_eq!(actual, 50_000_000);
        }
    }

    #[test]
    fn clock_never_exceeds_target() {
        for cdr1 in [false, true] {
            let mut khz = 1_000u32;
            while khz <= 600_000 {
                let target = khz * 1_000;
                let (_, _, actual) = compute_clock(target, cdr1);
                assert!(
                    actual > 0 && actual <= target,
                    "cdr1={cdr1} target {target} Hz -> {actual} Hz"
                );
                khz += 1_000;
            }
        }
    }

    #[test]
    #[should_panic(expected = "sunxi SPI frequency must be non-zero")]
    fn config_rejects_zero_freq() {
        let _ = SunxiSpiConfig::new(0, 0x1000).build();
    }

    #[test]
    #[should_panic(expected = "sunxi SPI flash size must fit 3-byte addressing")]
    fn config_rejects_oversize_flash() {
        let _ = SunxiSpiConfig::new(1_000_000, 0x0200_0000).build();
    }

    #[test]
    #[should_panic(expected = "sunxi SPI flash size must fit 3-byte addressing")]
    fn config_rejects_zero_size_flash() {
        let _ = SunxiSpiConfig::new(1_000_000, 0).build();
    }
}
