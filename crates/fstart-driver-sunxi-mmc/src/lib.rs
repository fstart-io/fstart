//! Allwinner sunxi SD/MMC host controller driver (unified A20/H3/D1).
//!
//! Minimal read-only driver for booting from SD card. Implements
//! `Device` + `BlockDevice` traits. Supports SD v2.0 cards (SDHC)
//! in 4-bit mode at 25 MHz.
//!
//! Supports three SoC generations:
//!
//! - **sun4i** (A10, A20): FIFO at 0x100, AHB gate only
//! - **sun6i** (H3, H2+, A64): FIFO at 0x200, AHB gate + separate bus-reset
//! - **NCAT2** (D1, T113): FIFO at 0x200, combined gate+reset BGR register
//!
//! Ported from u-boot `drivers/mmc/sunxi_mmc.c`.

#![no_std]

use core::cell::Cell;

use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{BlockDevice, ServiceError};

use fstart_sunxi_ccu_regs::{D1_MMC_CLK, MMC_CLK};

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Driver configuration (from board RON)
// ---------------------------------------------------------------------------

/// Configuration for the Allwinner sunxi MMC controller.
///
/// The enum variant selects the SoC generation, which determines
/// FIFO offset, clock gating, and reset behaviour.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum SunxiMmcConfig {
    /// A20 (sun7i) — sun4i-generation: AHB gate only, FIFO at 0x100.
    Sun7iA20 {
        /// MMC controller base address (e.g., 0x01C0F000 for MMC0).
        base_addr: u64,
        /// CCU base address (0x01C20000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x01C20800) for GPIO pin mux.
        pio_base: u64,
        /// MMC controller index (0-3) for clock register selection.
        mmc_index: u8,
    },
    /// H3/H2+ (sun8i) — sun6i-generation: AHB gate + bus-reset, FIFO at 0x200.
    Sun8iH3 {
        /// MMC controller base address (e.g., 0x01C0F000 for MMC0).
        base_addr: u64,
        /// CCU base address (0x01C20000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x01C20800) for GPIO pin mux.
        pio_base: u64,
        /// MMC controller index (0-2) for clock register selection.
        mmc_index: u8,
    },
    /// H5 (sun50i) — same hardware as H3, sun6i-generation.
    ///
    /// Identical register layout and behaviour to `Sun8iH3`.
    /// Separate variant for board-level clarity and future-proofing.
    Sun50iH5 {
        /// MMC controller base address (e.g., 0x01C0F000 for MMC0).
        base_addr: u64,
        /// CCU base address (0x01C20000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x01C20800) for GPIO pin mux.
        pio_base: u64,
        /// MMC controller index (0-2) for clock register selection.
        mmc_index: u8,
    },
    /// D1/T113 (sun20i) — NCAT2-generation: combined gate+reset at 0x84C,
    /// module clock at 0x830, FIFO at 0x200.
    Sun20iD1 {
        /// MMC controller base address (e.g., 0x04020000 for MMC0).
        base_addr: u64,
        /// CCU base address (0x02001000) for clock gating.
        ccu_base: u64,
        /// PIO base address (0x02000000) for GPIO pin mux.
        pio_base: u64,
        /// MMC controller index (0-2) for clock register selection.
        mmc_index: u8,
    },
}

impl SunxiMmcConfig {
    /// Extract the `mmc_index` from any variant.
    pub fn mmc_index(&self) -> u8 {
        match self {
            Self::Sun7iA20 { mmc_index, .. }
            | Self::Sun8iH3 { mmc_index, .. }
            | Self::Sun50iH5 { mmc_index, .. }
            | Self::Sun20iD1 { mmc_index, .. } => *mmc_index,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal SoC generation selector
// ---------------------------------------------------------------------------

/// SoC generation — drives the hardware differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SunxiGen {
    /// sun4i-generation (A10, A20): FIFO at 0x100, AHB gate only.
    Sun4i,
    /// sun6i-generation (H3, H2+, A64): FIFO at 0x200, gate + reset.
    Sun6i,
    /// NCAT2-generation (D1/T113): FIFO at 0x200, combined gate+reset at 0x84C.
    Ncat2,
}

// ---------------------------------------------------------------------------
// Register definitions (tock-registers)
// ---------------------------------------------------------------------------

register_bitfields![u32,
    /// Global Control Register (offset 0x00).
    GCTRL [
        SOFT_RESET OFFSET(0) NUMBITS(1) [],
        FIFO_RESET OFFSET(1) NUMBITS(1) [],
        DMA_RESET OFFSET(2) NUMBITS(1) [],
        DMA_ENABLE OFFSET(5) NUMBITS(1) [],
        ACCESS_BY_AHB OFFSET(31) NUMBITS(1) [],
    ],
    /// Clock Control Register (offset 0x04).
    CLKCR [
        DIVIDER OFFSET(0) NUMBITS(8) [],
        CLK_ENABLE OFFSET(16) NUMBITS(1) [],
        CLK_POWERSAVE OFFSET(17) NUMBITS(1) [],
    ],
    /// Bus Width Register (offset 0x0C).
    WIDTH [
        BUS_WIDTH OFFSET(0) NUMBITS(2) [
            Width1 = 0,
            Width4 = 1,
            Width8 = 2,
        ],
    ],
    /// Command Register (offset 0x18).
    CMD [
        CMD_INDEX OFFSET(0) NUMBITS(6) [],
        RESP_EXPIRE OFFSET(6) NUMBITS(1) [],
        LONG_RESPONSE OFFSET(7) NUMBITS(1) [],
        CHK_RESPONSE_CRC OFFSET(8) NUMBITS(1) [],
        DATA_EXPIRE OFFSET(9) NUMBITS(1) [],
        WRITE OFFSET(10) NUMBITS(1) [],
        AUTO_STOP OFFSET(12) NUMBITS(1) [],
        WAIT_PRE_OVER OFFSET(13) NUMBITS(1) [],
        SEND_INIT_SEQ OFFSET(15) NUMBITS(1) [],
        UPCLK_ONLY OFFSET(21) NUMBITS(1) [],
        START OFFSET(31) NUMBITS(1) [],
    ],
    /// Raw Interrupt Status Register (offset 0x38).
    RINT [
        RESP_ERROR OFFSET(1) NUMBITS(1) [],
        COMMAND_DONE OFFSET(2) NUMBITS(1) [],
        DATA_OVER OFFSET(3) NUMBITS(1) [],
        RX_DATA_REQUEST OFFSET(5) NUMBITS(1) [],
        RESP_CRC_ERROR OFFSET(6) NUMBITS(1) [],
        DATA_CRC_ERROR OFFSET(7) NUMBITS(1) [],
        RESP_TIMEOUT OFFSET(8) NUMBITS(1) [],
        DATA_TIMEOUT OFFSET(9) NUMBITS(1) [],
        FIFO_RUN_ERROR OFFSET(11) NUMBITS(1) [],
        START_BIT_ERROR OFFSET(13) NUMBITS(1) [],
        AUTO_COMMAND_DONE OFFSET(14) NUMBITS(1) [],
        END_BIT_ERROR OFFSET(15) NUMBITS(1) [],
    ],
    /// Status Register (offset 0x3C).
    STATUS [
        FIFO_EMPTY OFFSET(2) NUMBITS(1) [],
        FIFO_FULL OFFSET(3) NUMBITS(1) [],
        CARD_DATA_BUSY OFFSET(9) NUMBITS(1) [],
        FIFO_LEVEL OFFSET(17) NUMBITS(14) [],
    ],
];

register_structs! {
    /// Allwinner sunxi MMC controller register block.
    SunxiMmcRegs {
        (0x00 => pub gctrl: MmioReadWrite<u32, GCTRL::Register>),
        (0x04 => pub clkcr: MmioReadWrite<u32, CLKCR::Register>),
        (0x08 => pub timeout: MmioReadWrite<u32>),
        (0x0C => pub width: MmioReadWrite<u32, WIDTH::Register>),
        (0x10 => pub blksz: MmioReadWrite<u32>),
        (0x14 => pub bytecnt: MmioReadWrite<u32>),
        (0x18 => pub cmd: MmioReadWrite<u32, CMD::Register>),
        (0x1C => pub arg: MmioReadWrite<u32>),
        (0x20 => pub resp0: MmioReadWrite<u32>),
        (0x24 => pub resp1: MmioReadWrite<u32>),
        (0x28 => pub resp2: MmioReadWrite<u32>),
        (0x2C => pub resp3: MmioReadWrite<u32>),
        (0x30 => pub imask: MmioReadWrite<u32>),
        (0x34 => pub mint: MmioReadWrite<u32>),
        (0x38 => pub rint: MmioReadWrite<u32, RINT::Register>),
        (0x3C => pub status: MmioReadWrite<u32, STATUS::Register>),
        (0x40 => @END),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Error bits in RINT register.
const RINT_ERROR_MASK: u32 = (1 << 1)  // RESP_ERROR
    | (1 << 6)  // RESP_CRC_ERROR
    | (1 << 7)  // DATA_CRC_ERROR
    | (1 << 8)  // RESP_TIMEOUT
    | (1 << 9)  // DATA_TIMEOUT
    | (1 << 11) // FIFO_RUN_ERROR
    | (1 << 13) // START_BIT_ERROR
    | (1 << 15); // END_BIT_ERROR

/// SD block size.
const BLOCK_SIZE: u32 = 512;

// GPIO pin mux: Port F, function 2 = SDC0.
/// PIO bank stride: 0x24 on sun4i/sun6i, 0x30 on NCAT2 (D1/T113).
const PIO_BANK_STRIDE_LEGACY: usize = 0x24;
const PIO_BANK_STRIDE_NCAT2: usize = 0x30;
const PIO_CFG0_OFF: usize = 0x00;
/// Drive strength register offset within a PIO bank.
/// - sun4i/sun6i: DRV0 at 0x14 within bank
/// - NCAT2: DRV0 at 0x14 within bank (same)
const PIO_DRV0_OFF: usize = 0x14;
/// Pull-up/down register offset within a PIO bank.
/// - sun4i/sun6i: PULL0 at 0x1C within bank
/// - NCAT2: PULL0 at 0x24 within bank
const PIO_PULL0_OFF_LEGACY: usize = 0x1C;
const PIO_PULL0_OFF_NCAT2: usize = 0x24;

// CCU register offsets (sun4i/sun6i — A20, H3, H5).
/// AHB gate register 0 — bit (8 + mmc_index) enables the MMC clock gate.
const CCU_AHB_GATE0_OFF: usize = 0x060;
/// MMC module clock 0 — each controller is at +4*index from this base.
const CCU_MMC_CLK0_OFF: usize = 0x088;
/// Bus soft-reset register 0 (sun6i only) — bit (8 + mmc_index) deasserts reset.
const CCU_BUS_RESET0_OFF: usize = 0x2C0;

// CCU register offsets (NCAT2 — D1/T113).
/// MMC module clock (D1): 0x830 + index*4.
const CCU_D1_MMC_CLK0_OFF: usize = 0x830;
/// MMC bus gating + reset (D1): combined register at 0x84C.
/// Gate bits [2:0], reset bits [18:16].
const CCU_D1_MMC_BGR_OFF: usize = 0x84C;

// SD command indices.
const CMD0: u32 = 0; // GO_IDLE_STATE
const CMD2: u32 = 2; // ALL_SEND_CID
const CMD3: u32 = 3; // SEND_RELATIVE_ADDR
const CMD7: u32 = 7; // SELECT_CARD
const CMD8: u32 = 8; // SEND_IF_COND
const CMD16: u32 = 16; // SET_BLOCKLEN
const CMD17: u32 = 17; // READ_SINGLE_BLOCK
const CMD18: u32 = 18; // READ_MULTIPLE_BLOCK
const CMD55: u32 = 55; // APP_CMD
const ACMD6: u32 = 6; // SET_BUS_WIDTH (app cmd)
const ACMD41: u32 = 41; // SD_SEND_OP_COND (app cmd)

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Allwinner sunxi MMC host controller driver.
///
/// Supports sun4i (A10/A20), sun6i (H3/H2+/A64), and NCAT2 (D1/T113)
/// generations. The `gen` field selects the hardware-specific code paths.
pub struct SunxiMmc {
    regs: &'static SunxiMmcRegs,
    fifo: *mut u32,
    ccu_base: usize,
    pio_base: usize,
    mmc_index: u8,
    gen: SunxiGen,
    /// Relative Card Address (assigned during init).
    rca: Cell<u16>,
    /// Whether the card is SDHC (block-addressed).
    sdhc: Cell<bool>,
    /// Card capacity in bytes (detected during init).
    capacity: Cell<u64>,
}

// SAFETY: MMC controller is a fixed MMIO peripheral, accessed from a
// single-threaded firmware context.
unsafe impl Send for SunxiMmc {}
unsafe impl Sync for SunxiMmc {}

impl Device for SunxiMmc {
    const NAME: &'static str = "sunxi-mmc";
    const COMPATIBLE: &'static [&'static str] = &[
        "allwinner,sun7i-a20-mmc",
        "allwinner,sun8i-h3-mmc",
        "allwinner,sun50i-h5-mmc",
        "allwinner,sun20i-d1-mmc",
    ];
    type Config = SunxiMmcConfig;

    fn new(config: &SunxiMmcConfig) -> Result<Self, DeviceError> {
        let (base_addr, ccu_base, pio_base, mmc_index, gen) = match *config {
            SunxiMmcConfig::Sun7iA20 {
                base_addr,
                ccu_base,
                pio_base,
                mmc_index,
            } => (base_addr, ccu_base, pio_base, mmc_index, SunxiGen::Sun4i),
            SunxiMmcConfig::Sun8iH3 {
                base_addr,
                ccu_base,
                pio_base,
                mmc_index,
            }
            | SunxiMmcConfig::Sun50iH5 {
                base_addr,
                ccu_base,
                pio_base,
                mmc_index,
            } => (base_addr, ccu_base, pio_base, mmc_index, SunxiGen::Sun6i),
            SunxiMmcConfig::Sun20iD1 {
                base_addr,
                ccu_base,
                pio_base,
                mmc_index,
            } => (base_addr, ccu_base, pio_base, mmc_index, SunxiGen::Ncat2),
        };

        let base = base_addr as usize;
        let fifo_offset = match gen {
            SunxiGen::Sun4i => 0x100,
            SunxiGen::Sun6i | SunxiGen::Ncat2 => 0x200,
        };

        // SAFETY: base_addr points to the MMC controller MMIO region.
        let regs = unsafe { &*(base as *const SunxiMmcRegs) };
        let fifo = (base + fifo_offset) as *mut u32;

        Ok(Self {
            regs,
            fifo,
            ccu_base: ccu_base as usize,
            pio_base: pio_base as usize,
            mmc_index,
            gen,
            rca: Cell::new(0),
            sdhc: Cell::new(false),
            capacity: Cell::new(0),
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        fstart_log::debug!("mmc: setup_gpio");
        self.setup_gpio();
        fstart_log::debug!("mmc: setup_clocks");
        self.setup_clocks();
        fstart_log::debug!("mmc: reset_controller");
        self.reset_controller();
        fstart_log::debug!("mmc: sd_card_init");
        self.sd_card_init().map_err(|_| {
            fstart_log::error!("mmc: sd_card_init failed");
            DeviceError::InitFailed
        })?;
        fstart_log::debug!("mmc: init complete");
        Ok(())
    }
}

impl BlockDevice for SunxiMmc {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let block_start = offset / BLOCK_SIZE as u64;
        let byte_offset = (offset % BLOCK_SIZE as u64) as usize;
        let mut bytes_read = 0usize;
        let mut block_buf = [0u8; BLOCK_SIZE as usize];

        let mut current_block = block_start;
        let mut buf_pos = 0usize;

        // Handle partial first block (single-block CMD17).
        if byte_offset != 0 {
            self.read_blocks(current_block, &mut block_buf, 1)?;
            let avail = (BLOCK_SIZE as usize) - byte_offset;
            let to_copy = avail.min(buf.len());
            buf[..to_copy].copy_from_slice(&block_buf[byte_offset..byte_offset + to_copy]);
            buf_pos += to_copy;
            bytes_read += to_copy;
            current_block += 1;
        }

        // Full blocks -- use multi-block CMD18 reads.
        // U-Boot uses up to 65535 blocks per CMD18. We cap at a
        // reasonable chunk to keep progress visible and limit the
        // impact of a single failure.
        let remaining_full = (buf.len() - buf_pos) / BLOCK_SIZE as usize;
        if remaining_full > 0 {
            // Maximum blocks per CMD18 transfer. Matches U-Boot's
            // CONFIG_SYS_MMC_MAX_BLK_COUNT default.
            const MAX_BLOCKS: u32 = 65535;

            let mut full_left = remaining_full as u32;
            while full_left > 0 {
                let chunk = full_left.min(MAX_BLOCKS);
                let chunk_bytes = chunk as usize * BLOCK_SIZE as usize;
                self.read_blocks(
                    current_block,
                    &mut buf[buf_pos..buf_pos + chunk_bytes],
                    chunk,
                )?;
                buf_pos += chunk_bytes;
                bytes_read += chunk_bytes;
                current_block += chunk as u64;
                full_left -= chunk;
            }
        }

        // Partial last block (single-block CMD17).
        let remaining = buf.len() - buf_pos;
        if remaining > 0 {
            self.read_blocks(current_block, &mut block_buf, 1)?;
            buf[buf_pos..buf_pos + remaining].copy_from_slice(&block_buf[..remaining]);
            bytes_read += remaining;
        }

        Ok(bytes_read)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, ServiceError> {
        Err(ServiceError::NotSupported) // read-only for boot
    }

    fn size(&self) -> u64 {
        self.capacity.get()
    }

    fn block_size(&self) -> u32 {
        BLOCK_SIZE
    }
}

// ---------------------------------------------------------------------------
// Private implementation
// ---------------------------------------------------------------------------

impl SunxiMmc {
    /// Get a reference to the MMC module clock register for this controller.
    ///
    /// MMC module clock register (sun4i/sun6i layout).
    ///
    /// For NCAT2 (D1), use [`d1_mmc_clk_reg`] instead — the bit layout
    /// differs.
    fn mmc_clk_reg(&self) -> &MmioReadWrite<u32, MMC_CLK::Register> {
        let addr = self.ccu_base + CCU_MMC_CLK0_OFF + (self.mmc_index as usize) * 4;
        // SAFETY: MMC clock register at known CCU MMIO address.
        unsafe { &*(addr as *const MmioReadWrite<u32, MMC_CLK::Register>) }
    }

    /// MMC module clock register (NCAT2/D1 layout).
    ///
    /// D1 has different bit positions: N at [9:8], CLK_SRC at [26:24],
    /// no OCLK_DLY/SCLK_DLY fields.
    fn d1_mmc_clk_reg(&self) -> &MmioReadWrite<u32, D1_MMC_CLK::Register> {
        let addr = self.ccu_base + CCU_D1_MMC_CLK0_OFF + (self.mmc_index as usize) * 4;
        // SAFETY: D1 MMC clock register at known CCU MMIO address.
        unsafe { &*(addr as *const MmioReadWrite<u32, D1_MMC_CLK::Register>) }
    }

    /// Configure PF0-PF5 for SDC0 function.
    ///
    /// Port F function 2 = SDC0 on all sunxi SoCs.
    /// Bank stride and pull register offset differ between generations.
    fn setup_gpio(&self) {
        let (bank_stride, pull0_off) = match self.gen {
            SunxiGen::Sun4i | SunxiGen::Sun6i => (PIO_BANK_STRIDE_LEGACY, PIO_PULL0_OFF_LEGACY),
            SunxiGen::Ncat2 => (PIO_BANK_STRIDE_NCAT2, PIO_PULL0_OFF_NCAT2),
        };
        let pf_base = self.pio_base + 5 * bank_stride; // Port F = bank 5
                                                       // SAFETY: PIO registers at known MMIO addresses.
        unsafe {
            // PF_CFG0: PF0-PF5 = function 2 (SDC0)
            fstart_mmio::write32((pf_base + PIO_CFG0_OFF) as *mut u32, 0x0022_2222);

            // PF_DRV0: PF0-PF5 = drive level 2
            fstart_mmio::write32((pf_base + PIO_DRV0_OFF) as *mut u32, 0x0000_0AAA);

            // PF_PULL0: PF0-PF5 = pull-up
            fstart_mmio::write32((pf_base + pull0_off) as *mut u32, 0x0000_0555);
        }
    }

    /// Enable AHB clock gate (and bus-reset on sun6i/NCAT2) + set initial module clock.
    ///
    /// - sun4i: AHB gate only (bit 8+index in AHB_GATE0)
    /// - sun6i: AHB gate + separate bus-reset register at CCU+0x2C0
    /// - NCAT2: combined gate+reset register at CCU+0x84C (gate bits [2:0],
    ///   reset bits [18:16])
    fn setup_clocks(&self) {
        match self.gen {
            SunxiGen::Sun4i => {
                let gate_addr = (self.ccu_base + CCU_AHB_GATE0_OFF) as *mut u32;
                let bit = 1u32 << (8 + self.mmc_index);
                // SAFETY: AHB gate register at known CCU MMIO address.
                unsafe {
                    let gate = core::ptr::read_volatile(gate_addr);
                    core::ptr::write_volatile(gate_addr, gate | bit);
                }
            }
            SunxiGen::Sun6i => {
                let gate_addr = (self.ccu_base + CCU_AHB_GATE0_OFF) as *mut u32;
                let bit = 1u32 << (8 + self.mmc_index);
                // SAFETY: AHB gate register at known CCU MMIO address.
                unsafe {
                    let gate = core::ptr::read_volatile(gate_addr);
                    core::ptr::write_volatile(gate_addr, gate | bit);
                }
                let reset_addr = (self.ccu_base + CCU_BUS_RESET0_OFF) as *mut u32;
                // SAFETY: Bus reset register at known CCU MMIO address.
                unsafe {
                    let reset = core::ptr::read_volatile(reset_addr);
                    core::ptr::write_volatile(reset_addr, reset | bit);
                }
            }
            SunxiGen::Ncat2 => {
                // D1/T113: combined BGR register at 0x84C.
                // Gate: bit mmc_index, Reset: bit (16 + mmc_index).
                let bgr_addr = (self.ccu_base + CCU_D1_MMC_BGR_OFF) as *mut u32;
                let gate_bit = 1u32 << self.mmc_index;
                let reset_bit = 1u32 << (16 + self.mmc_index);
                // SAFETY: MMC BGR register at known CCU MMIO address.
                unsafe {
                    let bgr = core::ptr::read_volatile(bgr_addr);
                    core::ptr::write_volatile(bgr_addr, bgr | gate_bit | reset_bit);
                }
            }
        }

        // Set module clock: OSC24M, N=0, M=0 -> 24 MHz.
        match self.gen {
            SunxiGen::Sun4i | SunxiGen::Sun6i => {
                self.mmc_clk_reg()
                    .write(MMC_CLK::ENABLE::SET + MMC_CLK::CLK_SRC::Osc24M);
            }
            SunxiGen::Ncat2 => {
                self.d1_mmc_clk_reg()
                    .write(D1_MMC_CLK::ENABLE::SET + D1_MMC_CLK::CLK_SRC::Osc24M);
            }
        }
    }

    /// Reset the controller.
    ///
    /// Matches U-Boot SPL `sunxi_mmc_reset()`: assert SOFT_RESET then
    /// wait for it to auto-clear.
    ///
    /// **Difference 3**: sun6i explicitly writes TMOUT to 0xFFFFFFFF
    /// after soft-reset. The H3 BROM (SD-boot path) may leave a smaller
    /// value that causes premature DATA_TIMEOUT. The A20 BROM leaves
    /// 0xFFFFFF40, which is safe.
    fn reset_controller(&self) {
        self.regs
            .gctrl
            .write(GCTRL::SOFT_RESET::SET + GCTRL::FIFO_RESET::SET + GCTRL::DMA_RESET::SET);
        udelay(1000);

        if self.gen == SunxiGen::Sun6i || self.gen == SunxiGen::Ncat2 {
            // Set hardware timeout to maximum so DATA_TIMEOUT in RINT is
            // not triggered before software polling has a chance to drain
            // the FIFO. Required on sun6i+ because the BROM may leave a
            // shorter timeout value.
            self.regs.timeout.set(0xFFFF_FFFF);
        }
    }

    /// Update the internal clock divider (required after clock changes).
    fn update_clk(&self) -> Result<(), ServiceError> {
        self.regs
            .cmd
            .write(CMD::START::SET + CMD::UPCLK_ONLY::SET + CMD::WAIT_PRE_OVER::SET);

        for _ in 0..200_000u32 {
            if !self.regs.cmd.is_set(CMD::START) {
                // Clear any interrupt bits from clock update.
                let rint_val = self.regs.rint.get();
                self.regs.rint.set(rint_val);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(ServiceError::Timeout)
    }

    /// Set the module clock to a target frequency.
    fn set_mod_clk(&self, target_hz: u32) {
        match self.gen {
            SunxiGen::Sun4i | SunxiGen::Sun6i => self.set_mod_clk_legacy(target_hz),
            SunxiGen::Ncat2 => self.set_mod_clk_d1(target_hz),
        }
    }

    /// Module clock setup for sun4i/sun6i (A20, H3, H5).
    ///
    /// Uses the `MMC_CLK` bitfield layout: N at [17:16], OCLK_DLY/SCLK_DLY
    /// phase delay fields, CLK_SRC 2-bit at [25:24].
    fn set_mod_clk_legacy(&self, target_hz: u32) {
        let (src, src_hz) = if target_hz <= 24_000_000 {
            (MMC_CLK::CLK_SRC::Osc24M, 24_000_000u32)
        } else {
            (MMC_CLK::CLK_SRC::Pll6, 600_000_000u32)
        };

        // Find N (power-of-2 pre-divider) and M.
        let mut div = src_hz.div_ceil(target_hz);
        let mut n = 0u32;
        while div > 16 {
            n += 1;
            div = div.div_ceil(2);
        }
        let m = div.max(1);

        // Phase delays based on target speed.
        let (oclk_dly, sclk_dly) = if target_hz <= 400_000 {
            (0u32, 0u32)
        } else if target_hz <= 25_000_000 {
            (0, 5)
        } else {
            (3, 4)
        };

        self.mmc_clk_reg().write(
            MMC_CLK::ENABLE::SET
                + src
                + MMC_CLK::SCLK_DLY.val(sclk_dly)
                + MMC_CLK::N.val(n)
                + MMC_CLK::OCLK_DLY.val(oclk_dly)
                + MMC_CLK::M.val(m - 1),
        );
    }

    /// Module clock setup for NCAT2 (D1, T113).
    ///
    /// Uses the `D1_MMC_CLK` bitfield layout: N at [9:8], CLK_SRC 3-bit
    /// at [26:24], no phase delay fields.
    fn set_mod_clk_d1(&self, target_hz: u32) {
        let (src, src_hz) = if target_hz <= 24_000_000 {
            (D1_MMC_CLK::CLK_SRC::Osc24M, 24_000_000u32)
        } else {
            (D1_MMC_CLK::CLK_SRC::PllPeriph0, 600_000_000u32)
        };

        // Find N (power-of-2 pre-divider) and M.
        let mut div = src_hz.div_ceil(target_hz);
        let mut n = 0u32;
        while div > 16 {
            n += 1;
            div = div.div_ceil(2);
        }
        let m = div.max(1);

        self.d1_mmc_clk_reg()
            .write(D1_MMC_CLK::ENABLE::SET + src + D1_MMC_CLK::N.val(n) + D1_MMC_CLK::M.val(m - 1));
    }

    /// Configure clock and update the card clock.
    ///
    /// Matches U-Boot's `mmc_config_clock()`: read-modify-write on CLKCR,
    /// preserving bits outside the divider mask.
    fn config_clock(&self, target_hz: u32) -> Result<(), ServiceError> {
        let mut rval = self.regs.clkcr.get();

        // Disable card clock.
        rval &= !(1 << 16); // CLK_ENABLE
        self.regs.clkcr.set(rval);
        self.update_clk()?;

        // Set module clock (CCU register).
        self.set_mod_clk(target_hz);

        // Clear internal divider (low 8 bits), preserve other bits.
        rval &= !0xFF;
        self.regs.clkcr.set(rval);

        // Re-enable card clock.
        rval |= 1 << 16; // CLK_ENABLE
        self.regs.clkcr.set(rval);
        self.update_clk()?;
        Ok(())
    }

    /// Build the command register value for an SD command.
    fn build_cmdval(cmd_index: u32, resp_type: &RespType, has_data: bool) -> u32 {
        let mut cmdval = (1u32 << 31) | cmd_index; // START + index
        if cmd_index == CMD0 {
            cmdval |= 1 << 15; // SEND_INIT_SEQ
        }
        match resp_type {
            RespType::None => {}
            RespType::R1 | RespType::R1b | RespType::R6 | RespType::R7 => {
                cmdval |= (1 << 6) | (1 << 8); // RESP_EXPIRE + CHK_CRC
            }
            RespType::R2 => {
                cmdval |= (1 << 6) | (1 << 7) | (1 << 8); // + LONG_RESPONSE
            }
            RespType::R3 => {
                cmdval |= 1 << 6; // RESP_EXPIRE only (no CRC check)
            }
        }
        if has_data {
            cmdval |= (1 << 9) | (1 << 13); // DATA_EXPIRE + WAIT_PRE_OVER
        }
        cmdval
    }

    /// Wait for RINT bit `done_bit`, checking for errors.
    /// Returns `Ok(())` or error if RINT error bits appear or timeout.
    fn rint_wait(&self, done_bit: u32, timeout: u32) -> Result<(), ServiceError> {
        for _ in 0..timeout {
            let rint = self.regs.rint.get();
            if rint & RINT_ERROR_MASK != 0 {
                fstart_log::error!("mmc: rint error={} waiting for bit={}", rint, done_bit);
                return Err(ServiceError::HardwareError);
            }
            if rint & done_bit != 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        let rint = self.regs.rint.get();
        fstart_log::error!(
            "mmc: rint timeout rint={} waiting for bit={}",
            rint,
            done_bit
        );
        Err(ServiceError::Timeout)
    }

    /// PIO read: transfer `words` 32-bit words from FIFO into `buf32`.
    /// Matches U-Boot's `mmc_trans_data_by_cpu()` for reads.
    fn pio_read_data(&self, buf: &mut [u8], words: usize) -> Result<(), ServiceError> {
        let buf32 = buf.as_mut_ptr() as *mut u32;
        let mut i = 0usize;

        while i < words {
            // Poll until FIFO is not empty.
            let mut timeout = 2_000_000u32;
            let st = loop {
                let st = self.regs.status.get();
                if st & (1 << 2) == 0 {
                    // FIFO_EMPTY bit clear -> data available
                    break st;
                }
                timeout -= 1;
                if timeout == 0 {
                    let rint = self.regs.rint.get();
                    fstart_log::error!(
                        "mmc: pio timeout at word {}/{} status={} rint={}",
                        i as u32,
                        words as u32,
                        st,
                        rint
                    );
                    return Err(ServiceError::Timeout);
                }
                core::hint::spin_loop();
            };

            // Read available words from FIFO.
            // Quirk: when FIFO is completely full, level reads as 0.
            let mut in_fifo = ((st >> 17) & 0x3FFF) as usize;
            if in_fifo == 0 && (st & (1 << 3)) != 0 {
                in_fifo = 32;
            }
            let to_read = in_fifo.min(words - i);

            for _ in 0..to_read {
                // SAFETY: FIFO register at known MMIO address.
                unsafe {
                    let word = core::ptr::read_volatile(self.fifo);
                    core::ptr::write_volatile(buf32.add(i), word);
                }
                i += 1;
            }
        }

        // Note: volatile reads already prevent compiler reordering.
        // The next RINT check via MmioReadWrite includes a barrier.

        Ok(())
    }

    /// Send an SD command and wait for completion.
    ///
    /// Follows U-Boot's `sunxi_mmc_send_cmd_common()` ordering exactly:
    /// the command register must be written first, PIO data transfer
    /// happens next, then we wait for COMMAND_DONE, then DATA_OVER.
    fn send_cmd(
        &self,
        cmd_index: u32,
        arg: u32,
        resp_type: RespType,
        has_data: bool,
    ) -> Result<u32, ServiceError> {
        let cmdval = Self::build_cmdval(cmd_index, &resp_type, has_data);

        // Clear all interrupt status bits.
        self.regs.rint.set(0xFFFF_FFFF);

        // Write argument, then start command.
        self.regs.arg.set(arg);
        self.regs.cmd.set(cmdval);

        // Wait for COMMAND_DONE (no data commands complete here).
        if let Err(e) = self.rint_wait(1 << 2, 1_000_000) {
            self.error_recovery();
            return Err(e);
        }

        // For busy responses, wait until card is not busy.
        if matches!(resp_type, RespType::R1b) {
            for _ in 0..2_000_000u32 {
                if !self.regs.status.is_set(STATUS::CARD_DATA_BUSY) {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        let resp = self.regs.resp0.get();

        // Clear RINT + reset FIFO after every command (matches U-Boot).
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.gctrl.modify(GCTRL::FIFO_RESET::SET);

        Ok(resp)
    }

    /// Read one or more contiguous 512-byte blocks using PIO (CPU).
    ///
    /// Uses CMD17 for single-block reads and CMD18 + AUTO_STOP for
    /// multi-block reads, matching U-Boot `sunxi_mmc_send_cmd_common()`.
    ///
    /// `buf` must be at least `num_blocks * 512` bytes and 4-byte aligned.
    fn read_blocks(
        &self,
        start_block: u64,
        buf: &mut [u8],
        num_blocks: u32,
    ) -> Result<(), ServiceError> {
        debug_assert!(buf.len() >= (num_blocks as usize) * (BLOCK_SIZE as usize));

        let multi = num_blocks > 1;
        let cmd_index = if multi { CMD18 } else { CMD17 };
        let resp_type = RespType::R1;

        // Build command value. For multi-block, add AUTO_STOP (bit 12)
        // so the hardware sends CMD12 automatically after the transfer.
        let mut cmdval = Self::build_cmdval(cmd_index, &resp_type, true);
        if multi {
            cmdval |= 1 << 12; // AUTO_STOP
        }

        // SDHC uses block addressing; SDSC uses byte addressing.
        let addr = if self.sdhc.get() {
            start_block as u32
        } else {
            (start_block * BLOCK_SIZE as u64) as u32
        };

        let total_bytes = num_blocks * BLOCK_SIZE;

        // Set up data transfer registers.
        self.regs.blksz.set(BLOCK_SIZE);
        self.regs.bytecnt.set(total_bytes);

        // Enable AHB access for PIO (matches U-Boot setbits_le32).
        self.regs.gctrl.modify(GCTRL::ACCESS_BY_AHB::SET);

        // Clear all interrupt status bits.
        self.regs.rint.set(0xFFFF_FFFF);

        // Write argument, then start command.
        self.regs.arg.set(addr);
        self.regs.cmd.set(cmdval);

        // PIO read FIRST -- before checking COMMAND_DONE.
        // This matches U-Boot: mmc_trans_data_by_cpu() runs between
        // cmd write and mmc_rint_wait(COMMAND_DONE).
        let words = (total_bytes / 4) as usize;
        if let Err(e) = self.pio_read_data(buf, words) {
            fstart_log::error!(
                "mmc: read PIO failed blk={} n={}",
                start_block as u32,
                num_blocks
            );
            self.error_recovery();
            return Err(e);
        }

        // Now wait for COMMAND_DONE.
        if let Err(e) = self.rint_wait(1 << 2, 1_000_000) {
            fstart_log::error!(
                "mmc: read CMD_DONE failed blk={} n={}",
                start_block as u32,
                num_blocks
            );
            self.error_recovery();
            return Err(e);
        }

        // For multi-block: wait for AUTO_COMMAND_DONE (bit 14).
        // For single-block: wait for DATA_OVER (bit 3).
        let done_bit = if multi { 1 << 14 } else { 1 << 3 };
        if let Err(e) = self.rint_wait(done_bit, 1_000_000) {
            fstart_log::error!(
                "mmc: read done-wait failed blk={} n={} bit={}",
                start_block as u32,
                num_blocks,
                done_bit
            );
            self.error_recovery();
            return Err(e);
        }

        // Clean up: clear all interrupts and reset FIFO (matches U-Boot).
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.gctrl.modify(GCTRL::FIFO_RESET::SET);
        Ok(())
    }

    /// Error recovery: full GCTRL reset + clock update (matches U-Boot).
    fn error_recovery(&self) {
        self.regs
            .gctrl
            .write(GCTRL::SOFT_RESET::SET + GCTRL::FIFO_RESET::SET + GCTRL::DMA_RESET::SET);
        let _ = self.update_clk();
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.gctrl.modify(GCTRL::FIFO_RESET::SET);
    }

    /// SD card identification and initialization sequence.
    fn sd_card_init(&self) -> Result<(), ServiceError> {
        // Start at 400 kHz for identification.
        self.config_clock(400_000)?;
        self.regs.width.write(WIDTH::BUS_WIDTH::Width1);
        fstart_log::debug!("mmc: 400kHz clock configured, 1-bit bus");

        // CMD0: GO_IDLE_STATE
        self.send_cmd(CMD0, 0, RespType::None, false)?;
        udelay(10);
        fstart_log::debug!("mmc: CMD0 ok");

        // CMD8: SEND_IF_COND (SD v2.0 check)
        let sd_v2 = self
            .send_cmd(CMD8, 0x0000_01AA, RespType::R7, false)
            .is_ok();
        fstart_log::debug!("mmc: CMD8 sd_v2={}", sd_v2 as u8);

        // ACMD41: SD_SEND_OP_COND -- poll until card is ready.
        let mut tries = 1000u32;
        let mut ocr;
        loop {
            // CMD55 (APP_CMD) prefix.
            self.send_cmd(CMD55, 0, RespType::R1, false)?;
            // ACMD41: HCS=1 (SDHC support), voltage window.
            ocr = self.send_cmd(ACMD41, 0x40FF_8000, RespType::R3, false)?;
            if ocr & (1 << 31) != 0 {
                break; // Card is ready.
            }
            tries -= 1;
            if tries == 0 {
                fstart_log::error!("mmc: ACMD41 timeout");
                return Err(ServiceError::Timeout);
            }
            udelay(1);
        }

        // Check CCS bit: SDHC if bit 30 is set.
        self.sdhc.set(ocr & (1 << 30) != 0);
        fstart_log::debug!("mmc: ACMD41 ok, sdhc={}", self.sdhc.get() as u8);

        // CMD2: ALL_SEND_CID (get card identification).
        self.send_cmd(CMD2, 0, RespType::R2, false)?;
        fstart_log::debug!("mmc: CMD2 ok");

        // CMD3: SEND_RELATIVE_ADDR (get RCA).
        let r6 = self.send_cmd(CMD3, 0, RespType::R6, false)?;
        self.rca.set((r6 >> 16) as u16);
        fstart_log::debug!("mmc: CMD3 ok, rca={}", self.rca.get() as u32);

        // Switch to 25 MHz for data transfer.
        self.config_clock(25_000_000)?;
        fstart_log::debug!("mmc: 25MHz clock configured");

        // CMD7: SELECT_CARD.
        let rca32 = (self.rca.get() as u32) << 16;
        self.send_cmd(CMD7, rca32, RespType::R1b, false)?;
        fstart_log::debug!("mmc: CMD7 ok (card selected)");

        // CMD16: SET_BLOCKLEN = 512.
        self.send_cmd(CMD16, BLOCK_SIZE, RespType::R1, false)?;
        fstart_log::debug!("mmc: CMD16 ok (blocklen=512)");

        // Switch to 4-bit bus.
        self.send_cmd(CMD55, rca32, RespType::R1, false)?;
        self.send_cmd(ACMD6, 2, RespType::R1, false)?; // 2 = 4-bit
        self.regs.width.write(WIDTH::BUS_WIDTH::Width4);
        fstart_log::debug!("mmc: 4-bit bus configured");

        // Assume 2 GB capacity for SDHC (conservative default).
        // A full CSD parse could extract the real size.
        let cap = if self.sdhc.get() {
            2 * 1024 * 1024 * 1024u64
        } else {
            // SDSC: conservative 256 MB.
            256 * 1024 * 1024
        };
        self.capacity.set(cap);

        fstart_log::info!(
            "mmc: card init complete, sdhc={}, cap={}MB",
            self.sdhc.get() as u32,
            (cap / (1024 * 1024)) as u32
        );
        Ok(())
    }
}

/// SD response type classification.
enum RespType {
    None,
    R1,
    R1b,
    R2,
    R3,
    R6,
    R7,
}
