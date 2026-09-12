//! Allwinner H3/H2+ (sun8i) SPI0 controller driver.
//!
//! Minimal boot driver: burst engine implementing
//! [`SpiBus`](embedded_hal::spi::SpiBus) plus board-policy construction.
//! NOR framing (opcodes, chunking) lives in [`crate::spi_nor`]; this file
//! only knows the sun6i register layout, clock gating/reset and pin mux.
//!
//! The sun6i IP splits control into GCR/TCR/FIFO_CTL (vs. sun4i's single
//! CTL), moves TX/RX to FIFO windows at 0x200/0x300, and needs a bus-reset
//! deassert plus soft reset. CS is on PC3 (vs. PC23 on sun4i).
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

use crate::h3_ccu::{H3_CCU_BASE, H3_PIO_BASE, H3_SPI0_BASE};
use crate::pio::{PORT_C, PioGen, SunxiPio};
use crate::spi_nor::{
    NOR_FAST_READ_THRESHOLD_HZ, NOR_MAX_3BYTE_SIZE, SpiClockReport, SpiNorFlash, SunxiSpiError,
};

// ---------------------------------------------------------------------------
// Board policy
// ---------------------------------------------------------------------------

/// H3 SPI0 NOR flash policy. Fixed controller/CCU/PIO addresses are not
/// configuration.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H3SpiConfig {
    /// Desired SPI bus clock in Hz. The achieved frequency is at or below
    /// this; above 25 MHz the NOR layer switches to Fast Read.
    pub spi_freq: u32,
    /// SPI NOR flash capacity in bytes (3-byte addressing: max 16 MiB).
    pub flash_size: u32,
}

impl H3SpiConfig {
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
            panic!("H3 SPI frequency must be non-zero");
        }
        if self.flash_size == 0 || self.flash_size > NOR_MAX_3BYTE_SIZE {
            panic!("H3 SPI flash size must fit 3-byte addressing");
        }
        self
    }
}

/// H3 SPI0 defaults: 24 MHz on OSC24M (plain Read, CDR1 passthrough),
/// 16 MiB max window. Boards with SPI flash override the size; the BROM
/// boot device selects whether this policy is used at all.
pub const H3_SPI_DEFAULT_FREQ: u32 = 24_000_000;
pub const H3_SPI_DEFAULT_SIZE: u32 = NOR_MAX_3BYTE_SIZE;

/// NOR flash behind an H3 SPI0 controller.
pub type H3SpiFlash = SpiNorFlash<H3Spi>;

// ---------------------------------------------------------------------------
// Sun6i register definitions (H3/H2+/A64)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Global Control Register (offset 0x04).
    GCR [
        /// SPI controller enable.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Master mode.
        MASTER OFFSET(1) NUMBITS(1) [],
        /// Soft reset (self-clearing).
        SRST OFFSET(31) NUMBITS(1) [],
    ],
    /// Transfer Control Register (offset 0x08).
    TCR [
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
    /// FIFO Control Register (offset 0x18).
    FIFO_CTL [
        /// RX FIFO reset (self-clearing).
        RF_RST OFFSET(15) NUMBITS(1) [],
        /// TX FIFO reset (self-clearing).
        TF_RST OFFSET(31) NUMBITS(1) [],
    ],
    /// FIFO Status Register (offset 0x1C, 8-bit count on sun6i).
    FIFO_STA [
        /// RX FIFO byte count (bits 7:0).
        RF_CNT OFFSET(0) NUMBITS(8) [],
    ]
];

register_structs! {
    /// Sun6i SPI controller register block (only SPL-needed registers).
    pub Sun6iSpiRegs {
        (0x000 => _res0: [u8; 0x04]),
        /// Global control (enable, master, soft reset).
        (0x004 => pub gcr: MmioReadWrite<u32, GCR::Register>),
        /// Transfer control (CS, polarity, exchange).
        (0x008 => pub tcr: MmioReadWrite<u32, TCR::Register>),
        (0x00C => _res1: [u8; 0x0C]),
        /// FIFO control (FIFO resets).
        (0x018 => pub fifo_ctl: MmioReadWrite<u32, FIFO_CTL::Register>),
        /// FIFO status (RX count).
        (0x01C => pub fifo_sta: MmioReadWrite<u32, FIFO_STA::Register>),
        (0x020 => _res2: [u8; 0x04]),
        /// Clock control (same CDR format as sun4i).
        (0x024 => pub clk_ctl: MmioReadWrite<u32>),
        (0x028 => _res3: [u8; 0x08]),
        /// Master burst count — total bytes (TX + RX).
        (0x030 => pub mbc: MmioReadWrite<u32>),
        /// Master transmit count — bytes actually transmitted.
        (0x034 => pub mtc: MmioReadWrite<u32>),
        /// Burst control count.
        (0x038 => pub bcc: MmioReadWrite<u32>),
        (0x03C => _res4: [u8; 0x1C4]),
        /// TX data FIFO window.
        (0x200 => pub txd: MmioReadWrite<u32>),
        (0x204 => _res5: [u8; 0xFC]),
        /// RX data FIFO window.
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

/// Poll iterations for FIFO completion and soft reset.
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
// Driver struct
// ---------------------------------------------------------------------------

/// H3 SPI0 controller.
pub struct H3Spi {
    base: usize,
    spi_clk_ctl: u32,
    ccu_spi_clk: u32,
    achieved_hz: u32,
}

// SAFETY: fixed MMIO peripheral used during single-threaded boot.
unsafe impl Send for H3Spi {}
// SAFETY: fixed MMIO peripheral used during single-threaded boot.
unsafe impl Sync for H3Spi {}

impl H3Spi {
    /// Construct without touching hardware.
    #[must_use]
    pub fn new_from_config(config: &'static H3SpiConfig) -> Self {
        let (ccu_spi_clk, spi_clk_ctl, achieved_hz) = compute_clock(config.spi_freq);
        Self {
            base: H3_SPI0_BASE as usize,
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

    fn regs(&self) -> &'static Sun6iSpiRegs {
        // SAFETY: fixed H3 SPI0 mapping.
        unsafe { &*(self.base as *const Sun6iSpiRegs) }
    }

    /// Configure pins/clocks and enable the controller.
    pub fn init(&mut self) -> Result<(), ServiceError> {
        self.setup_gpio();
        self.setup_clocks();
        let regs = self.regs();
        regs.gcr
            .write(GCR::ENABLE::SET + GCR::MASTER::SET + GCR::SRST::SET);
        // Wait for the soft reset to self-clear.
        let mut timeout = SPI_POLL_TIMEOUT;
        while regs.gcr.is_set(GCR::SRST) {
            timeout -= 1;
            if timeout == 0 {
                fstart_log::error!("spi0: soft reset timeout");
                return Err(ServiceError::HardwareError);
            }
            core::hint::spin_loop();
        }
        // Soft reset clears the clock divider; re-apply it.
        regs.clk_ctl.set(self.spi_clk_ctl);
        // Manual CS, active-low, deasserted (pin HIGH).
        regs.tcr
            .write(TCR::CS_MANUAL::SET + TCR::CS_ACTIVE_LOW::SET + TCR::CS_LEVEL::SET);
        fstart_log::info!("spi0: {} MHz", self.achieved_hz / 1_000_000);
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
/// is tried first, so CDR2 may undershoot where a larger divider plus the
/// CDR1 passthrough would land nearer the target. Never exceeds `target`.
///
/// Unlike sun4i, sun6i offers a CDR1 1:1 passthrough (`clk_ctl` = 0), used
/// when the module clock already matches the target.
fn compute_clock(target: u32) -> (u32, u32, u32) {
    // Rejected by H3SpiConfig::build(); 0 would divide by zero below.
    debug_assert!(target > 0);
    // u64 math: validated policies stay far below u32::MAX, but the
    // divider search must not wrap on absurd inputs either.
    if target <= OSC24M_FREQ {
        let ccu_val = 1u32 << 31;
        if target >= OSC24M_FREQ {
            return (ccu_val, 0, OSC24M_FREQ);
        }
        let (clk_ctl, actual) = compute_cdr2(u64::from(OSC24M_FREQ), u64::from(target));
        (ccu_val, clk_ctl, actual)
    } else {
        let min_ccu_div = (u64::from(PLL_PERIPH_FREQ)).div_ceil(2 * u64::from(target));
        let (n, m) = find_ccu_nm(min_ccu_div);
        let mod_clk = u64::from(PLL_PERIPH_FREQ) / (1 << n) / (u64::from(m) + 1);
        let ccu_val = (1u32 << 31) | (1u32 << 24) | (n << 16) | m;
        if u64::from(target) >= mod_clk {
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

impl H3Spi {
    #[inline(always)]
    fn ccu_read(offset: usize) -> u32 {
        // SAFETY: fixed H3 CCU mapping.
        unsafe { mmio::read32((H3_CCU_BASE as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn ccu_write(offset: usize, val: u32) {
        // SAFETY: fixed H3 CCU mapping.
        unsafe { mmio::write32((H3_CCU_BASE as usize + offset) as *mut u32, val) }
    }

    /// PC0 (MOSI), PC1 (MISO), PC2 (CLK), PC3 (CS0) to SPI0 function 3.
    fn setup_gpio(&self) {
        let pio = SunxiPio::new(mmio32(H3_PIO_BASE), PioGen::Legacy);
        pio.set_function(PORT_C, 0, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 1, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 2, SPI0_PIN_FUNC);
        pio.set_function(PORT_C, 3, SPI0_PIN_FUNC);
    }

    /// Deassert bus reset, open the AHB gate, program clock dividers.
    fn setup_clocks(&self) {
        Self::ccu_write(
            CCU_BUS_RESET0_OFFSET,
            Self::ccu_read(CCU_BUS_RESET0_OFFSET) | BUS_RESET_SPI0,
        );
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

impl H3Spi {
    fn cs_assert(&self) {
        self.regs().tcr.modify(TCR::CS_LEVEL::CLEAR);
    }

    fn cs_deassert(&self) {
        self.regs().tcr.modify(TCR::CS_LEVEL::SET);
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
        // Preserve trigger-level bits; only reset the FIFOs.
        regs.fifo_ctl
            .modify(FIFO_CTL::TF_RST::SET + FIFO_CTL::RF_RST::SET);
        regs.mbc.set(rx.len() as u32);
        regs.mtc.set(tx.len() as u32);
        regs.bcc.set(tx.len() as u32);
        // Byte-width writes: one FIFO byte per write.
        for &byte in tx {
            // SAFETY: fixed SPI0 TX FIFO window.
            unsafe { mmio::write8((self.base + 0x200) as *mut u8, byte) };
        }
        regs.tcr.modify(TCR::XCH::SET);
        let mut timeout = SPI_POLL_TIMEOUT;
        loop {
            // Mask to 7 bits like U-Boot: only the count matters and bit 7
            // may be a status flag on some sun6i revisions.
            if regs.fifo_sta.read(FIFO_STA::RF_CNT) & 0x7F >= rx.len() as u32 {
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                // Quiesce the FIFOs so a later re-init starts clean. The
                // controller still needs re-init() after a timeout; the
                // bootblock halts on error, so no retry path exists today.
                regs.fifo_ctl
                    .modify(FIFO_CTL::TF_RST::SET + FIFO_CTL::RF_RST::SET);
                return Err(SunxiSpiError::Timeout);
            }
            core::hint::spin_loop();
        }
        for slot in rx.iter_mut() {
            // SAFETY: fixed SPI0 RX FIFO window; one byte popped per read.
            *slot = unsafe { mmio::read8((self.base + 0x300) as *const u8) };
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

impl ErrorType for H3Spi {
    type Error = SunxiSpiError;
}

impl SpiBus<u8> for H3Spi {
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

impl H3Spi {
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

impl SpiClockReport for H3Spi {
    fn fast_read(&self) -> bool {
        self.fast_read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_prefers_cdr1_passthrough_at_24mhz() {
        let (ccu, ctl, actual) = compute_clock(24_000_000);
        assert_eq!(ccu, 1 << 31);
        assert_eq!(ctl, 0);
        assert_eq!(actual, 24_000_000);
    }

    #[test]
    fn clock_rounds_down_on_osc24m() {
        // 10 MHz is not reachable: div = ceil(24/20) = 2 -> 6 MHz.
        let (_, _, actual) = compute_clock(10_000_000);
        assert_eq!(actual, 6_000_000);
    }

    #[test]
    fn clock_uses_pll_above_24mhz() {
        // 50 MHz: CCU div 6 -> MOD 100 MHz; 100 > 50 so CDR2 = 0 -> 50 MHz.
        let (ccu, ctl, actual) = compute_clock(50_000_000);
        assert_eq!(ccu, (1 << 31) | (1 << 24) | 5);
        assert_eq!(ctl, 1 << 12);
        assert_eq!(actual, 50_000_000);
    }

    #[test]
    fn clock_passes_through_mod_clock() {
        // 600 MHz: CCU divider 1 -> MOD 600 MHz, CDR1 passthrough exact.
        let (ccu, ctl, actual) = compute_clock(600_000_000);
        assert_eq!(ccu, (1 << 31) | (1 << 24));
        assert_eq!(ctl, 0);
        assert_eq!(actual, 600_000_000);
    }

    #[test]
    #[should_panic(expected = "H3 SPI flash size must fit 3-byte addressing")]
    fn config_rejects_zero_size_flash() {
        let _ = H3SpiConfig::new(1_000_000, 0).build();
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

    #[test]
    #[should_panic(expected = "H3 SPI frequency must be non-zero")]
    fn config_rejects_zero_freq() {
        let _ = H3SpiConfig::new(0, 0x1000).build();
    }

    #[test]
    #[should_panic(expected = "H3 SPI flash size must fit 3-byte addressing")]
    fn config_rejects_oversize_flash() {
        let _ = H3SpiConfig::new(1_000_000, 0x0200_0000).build();
    }
}
