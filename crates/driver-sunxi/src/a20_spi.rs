//! Allwinner A20 (sun7i) SPI0 controller driver.
//!
//! Minimal boot driver: burst engine implementing
//! [`SpiBus`](embedded_hal::spi::SpiBus) plus board-policy construction.
//! NOR framing (opcodes, chunking) lives in [`crate::spi_nor`]; this file
//! only knows the sun4i register layout, clock gating and pin mux.
//!
//! Ported from U-Boot `arch/arm/mach-sunxi/spl_spi_sunxi.c`.

use embedded_hal::spi::{ErrorType, SpiBus};
use fstart_arch::udelay;
use fstart_core::mmio::{self, MmioReadWrite};
use fstart_core::mmio32;
use fstart_core::services::ServiceError;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use crate::a20_ccu::{A20_CCU_BASE, A20_PIO_BASE, A20_SPI0_BASE};
use crate::pio::{PORT_C, PioGen, SunxiPio};
use crate::spi_nor::{
    NOR_FAST_READ_THRESHOLD_HZ, NOR_MAX_3BYTE_SIZE, SpiClockReport, SpiNorFlash, SunxiSpiError,
};

// ---------------------------------------------------------------------------
// Board policy
// ---------------------------------------------------------------------------

/// A20 SPI0 NOR flash policy. Fixed controller/CCU/PIO addresses are not
/// configuration.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A20SpiConfig {
    /// Desired SPI bus clock in Hz. The achieved frequency is at or below
    /// this; above 25 MHz the NOR layer switches to Fast Read.
    pub spi_freq: u32,
    /// SPI NOR flash capacity in bytes (3-byte addressing: max 16 MiB).
    pub flash_size: u32,
}

impl A20SpiConfig {
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
            panic!("A20 SPI frequency must be non-zero");
        }
        if self.flash_size == 0 || self.flash_size > NOR_MAX_3BYTE_SIZE {
            panic!("A20 SPI flash size must fit 3-byte addressing");
        }
        self
    }
}

/// A20 SPI0 defaults: 6 MHz on OSC24M (plain Read), 16 MiB max window.
/// Boards with SPI flash override the size; the BROM boot device selects
/// whether this policy is used at all.
pub const A20_SPI_DEFAULT_FREQ: u32 = 6_000_000;
pub const A20_SPI_DEFAULT_SIZE: u32 = NOR_MAX_3BYTE_SIZE;

/// NOR flash behind an A20 SPI0 controller.
pub type A20SpiFlash = SpiNorFlash<A20Spi>;

// ---------------------------------------------------------------------------
// Sun4i register definitions (A10/A20)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Control Register (offset 0x08): global + transfer + FIFO control.
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
    /// FIFO Status Register (offset 0x28).
    FIFO_STA [
        /// RX FIFO byte count (bits 6:0).
        RF_CNT OFFSET(0) NUMBITS(7) [],
    ]
];

register_structs! {
    /// Sun4i SPI controller register block (only SPL-needed registers).
    pub Sun4iSpiRegs {
        /// RX data register.
        (0x00 => pub rxdata: MmioReadWrite<u32>),
        /// TX data register.
        (0x04 => pub txdata: MmioReadWrite<u32>),
        /// Control register.
        (0x08 => pub ctl: MmioReadWrite<u32, CTL::Register>),
        /// Interrupt/DMA/wait registers (unused).
        (0x0C => _reserved: [u8; 0x10]),
        /// Clock control: bit 12 (DRS) selects CDR2 (bits 7:0);
        /// SPI_CLK = MOD_CLK / (2 * (CDR2 + 1)).
        (0x1C => pub clk_ctl: MmioReadWrite<u32>),
        /// Burst count — total bytes in the transfer (TX + RX).
        (0x20 => pub burst_cnt: MmioReadWrite<u32>),
        /// Transmit count — bytes actually transmitted (rest is clock-only).
        (0x24 => pub xmit_cnt: MmioReadWrite<u32>),
        /// FIFO status register.
        (0x28 => pub fifo_sta: MmioReadWrite<u32, FIFO_STA::Register>),
        (0x2C => @END),
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

/// Poll iterations for FIFO completion (~5 us per 64-byte transfer at
/// 100 MHz; 1M iterations is ample margin on a 1 GHz core).
const SPI_POLL_TIMEOUT: u32 = 1_000_000;

/// AHB gate bit for SPI0 (CCU + 0x060).
const AHB_GATE_SPI0: u32 = 1 << 20;

/// CCU register offsets.
const CCU_AHB_GATE_OFFSET: usize = 0x060;
const CCU_SPI0_CLK_OFFSET: usize = 0x0A0;

/// SPI0 alternate function on port C.
const SPI0_PIN_FUNC: u8 = 3;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// A20 SPI0 controller.
pub struct A20Spi {
    base: usize,
    spi_clk_ctl: u32,
    ccu_spi_clk: u32,
    achieved_hz: u32,
}

// SAFETY: fixed MMIO peripheral used during single-threaded boot.
unsafe impl Send for A20Spi {}
// SAFETY: fixed MMIO peripheral used during single-threaded boot.
unsafe impl Sync for A20Spi {}

impl A20Spi {
    /// Construct without touching hardware.
    #[must_use]
    pub fn new_from_config(config: &'static A20SpiConfig) -> Self {
        let (ccu_spi_clk, spi_clk_ctl, achieved_hz) = compute_clock(config.spi_freq);
        Self {
            base: A20_SPI0_BASE as usize,
            spi_clk_ctl,
            ccu_spi_clk,
            achieved_hz,
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

    fn regs(&self) -> &'static Sun4iSpiRegs {
        // SAFETY: fixed A20 SPI0 mapping.
        unsafe { &*(self.base as *const Sun4iSpiRegs) }
    }

    /// Configure pins/clocks and enable the controller.
    pub fn init(&mut self) -> Result<(), ServiceError> {
        self.setup_gpio();
        self.setup_clocks();
        self.regs().ctl.write(
            CTL::ENABLE::SET
                + CTL::MASTER::SET
                + CTL::TF_RST::SET
                + CTL::RF_RST::SET
                + CTL::CS_MANUAL::SET
                + CTL::CS_ACTIVE_LOW::SET
                + CTL::CS_LEVEL::SET
                + CTL::TP::SET,
        );
        fstart_log::info!("spi0: {} MHz", self.achieved_hz / 1_000_000);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Clock computation (pure — unit tested)
// ---------------------------------------------------------------------------

/// Compute CCU source/N/M dividers and the controller CDR2 divider for a
/// bus frequency at or below `target`. Returns
/// `(ccu_spi_clk_reg, spi_clk_ctl_reg, actual_hz)`.
///
/// The result is safe but not necessarily closest: the smallest CCU divider
/// is tried first, so CDR2 may undershoot where a larger divider would land
/// nearer the target. Never exceeds `target`.
///
/// - **<= 24 MHz**: OSC24M (no PLL dependency).
/// - **> 24 MHz**: PLL_PERIPH (600 MHz) via CCU N/M plus CDR2.
fn compute_clock(target: u32) -> (u32, u32, u32) {
    // Rejected by A20SpiConfig::build(); 0 would divide by zero below.
    debug_assert!(target > 0);
    // u64 math: validated policies stay far below u32::MAX, but the
    // divider search must not wrap on absurd inputs either.
    if target <= OSC24M_FREQ {
        // Enable, CLK_SRC=00 (OSC24M), N=0, M=0.
        let (clk_ctl, actual) = compute_cdr2(u64::from(OSC24M_FREQ), u64::from(target));
        ((1 << 31), clk_ctl, actual)
    } else {
        // Smallest CCU divider d = 2^N * (M+1) with PLL/d/2 <= target.
        let min_ccu_div = (u64::from(PLL_PERIPH_FREQ)).div_ceil(2 * u64::from(target));
        let (n, m) = find_ccu_nm(min_ccu_div);
        let mod_clk = u64::from(PLL_PERIPH_FREQ) / (1 << n) / (u64::from(m) + 1);
        let ccu_val = (1u32 << 31) | (1u32 << 24) | (n << 16) | m;
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
// Initialisation helpers
// ---------------------------------------------------------------------------

impl A20Spi {
    #[inline(always)]
    fn ccu_read(offset: usize) -> u32 {
        // SAFETY: fixed A20 CCU mapping.
        unsafe { mmio::read32((A20_CCU_BASE as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn ccu_write(offset: usize, val: u32) {
        // SAFETY: fixed A20 CCU mapping.
        unsafe { mmio::write32((A20_CCU_BASE as usize + offset) as *mut u32, val) }
    }

    /// PC0 (MOSI), PC1 (MISO), PC2 (CLK), PC23 (CS0) to SPI0 function 3.
    fn setup_gpio(&self) {
        let pio = SunxiPio::new(mmio32(A20_PIO_BASE), PioGen::Legacy);
        pio.set_function(PORT_C, 0, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 1, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 2, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 23, SPI0_PIN_FUNC);
    }

    /// Open the AHB gate and program the module clock dividers.
    fn setup_clocks(&self) {
        Self::ccu_write(
            CCU_AHB_GATE_OFFSET,
            Self::ccu_read(CCU_AHB_GATE_OFFSET) | AHB_GATE_SPI0,
        );
        self.regs().clk_ctl.set(self.spi_clk_ctl);
        Self::ccu_write(CCU_SPI0_CLK_OFFSET, self.ccu_spi_clk);
    }
}

// ---------------------------------------------------------------------------
// Burst engine
// ---------------------------------------------------------------------------

impl A20Spi {
    fn cs_assert(&self) {
        self.regs().ctl.modify(CTL::CS_LEVEL::CLEAR);
    }

    fn cs_deassert(&self) {
        self.regs().ctl.modify(CTL::CS_LEVEL::SET);
        // tSHSL: chip-select high time between operations.
        udelay(1);
    }

    /// One FIFO-sized burst: clock `rx.len()` bytes while transmitting the
    /// `tx.len()` prefix (`tx.len() <= rx.len() <= 64`).
    fn burst(&self, tx: &[u8], rx: &mut [u8]) -> Result<(), SunxiSpiError> {
        if tx.len() > rx.len() || rx.len() > SPI_FIFO_DEPTH {
            return Err(SunxiSpiError::TooLong);
        }
        let regs = self.regs();
        regs.ctl.modify(CTL::TF_RST::SET + CTL::RF_RST::SET);
        regs.burst_cnt.set(rx.len() as u32);
        regs.xmit_cnt.set(tx.len() as u32);
        // Byte-width writes: one FIFO byte per write (a 32-bit write would
        // push 4 bytes and corrupt the frame).
        for &byte in tx {
            // SAFETY: fixed SPI0 TX FIFO address.
            unsafe { mmio::write8((self.base + 0x04) as *mut u8, byte) };
        }
        regs.ctl.modify(CTL::XCH::SET);
        let mut timeout = SPI_POLL_TIMEOUT;
        loop {
            if regs.fifo_sta.read(FIFO_STA::RF_CNT) >= rx.len() as u32 {
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                // Quiesce the FIFOs so a later re-init starts clean. The
                // controller still needs re-init() after a timeout; the
                // bootblock halts on error, so no retry path exists today.
                regs.ctl.modify(CTL::TF_RST::SET + CTL::RF_RST::SET);
                return Err(SunxiSpiError::Timeout);
            }
            core::hint::spin_loop();
        }
        for slot in rx.iter_mut() {
            // SAFETY: fixed SPI0 RX FIFO address; one byte popped per read.
            *slot = unsafe { mmio::read8(self.base as *const u8) };
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
}

// ---------------------------------------------------------------------------
// embedded-hal SpiBus — dumb full-duplex bursts, no NOR knowledge
// ---------------------------------------------------------------------------

impl ErrorType for A20Spi {
    type Error = SunxiSpiError;
}

impl SpiBus<u8> for A20Spi {
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

impl A20Spi {
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

impl SpiClockReport for A20Spi {
    fn fast_read(&self) -> bool {
        self.fast_read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_stays_at_or_below_target_on_osc24m() {
        // 6 MHz: div = ceil(24/12) = 2 -> CDR2 = 1 -> exactly 6 MHz.
        let (ccu, ctl, actual) = compute_clock(6_000_000);
        assert_eq!(ccu, 1 << 31);
        assert_eq!(ctl, (1 << 12) | 1);
        assert_eq!(actual, 6_000_000);
    }

    #[test]
    fn clock_rounds_down_to_cdr2_steps() {
        // 10 MHz is not reachable: div = ceil(24/20) = 2 -> 6 MHz.
        let (_, _, actual) = compute_clock(10_000_000);
        assert_eq!(actual, 6_000_000);
        // sun4i has no 1:1 passthrough: CDR2 = 0 still divides by 2,
        // so OSC24M tops out at 12 MHz.
        let (_, ctl, actual) = compute_clock(24_000_000);
        assert_eq!(ctl, 1 << 12);
        assert_eq!(actual, 12_000_000);
    }

    #[test]
    fn clock_uses_pll_above_24mhz() {
        // 50 MHz: CCU div 6 (N=0, M=5) -> MOD 100 MHz -> CDR2 = 0 -> 50 MHz.
        let (ccu, ctl, actual) = compute_clock(50_000_000);
        assert_eq!(ccu, (1 << 31) | (1 << 24) | 5);
        assert_eq!(ctl, 1 << 12);
        assert_eq!(actual, 50_000_000);
    }

    #[test]
    #[should_panic(expected = "A20 SPI frequency must be non-zero")]
    fn config_rejects_zero_freq() {
        let _ = A20SpiConfig::new(0, 0x1000).build();
    }

    #[test]
    #[should_panic(expected = "A20 SPI flash size must fit 3-byte addressing")]
    fn config_rejects_oversize_flash() {
        let _ = A20SpiConfig::new(1_000_000, 0x0200_0000).build();
    }

    #[test]
    #[should_panic(expected = "A20 SPI flash size must fit 3-byte addressing")]
    fn config_rejects_zero_size_flash() {
        let _ = A20SpiConfig::new(1_000_000, 0).build();
    }

    #[test]
    fn clock_never_exceeds_target() {
        let mut khz = 1_000u32;
        while khz <= 600_000 {
            let target = khz * 1_000;
            let (_, _, actual) = compute_clock(target);
            assert!(
                actual > 0 && actual <= target,
                "target {target} Hz -> {actual} Hz"
            );
            khz += 1_000;
        }
    }
}
