//! Allwinner H3/H2+ (sun8i) SD/MMC host controller driver.
//!
//! Minimal read-only driver for booting from SD card. Implements
//! `Device` + `BlockDevice` traits. Supports SD v2.0 cards (SDHC)
//! in 4-bit mode at 25 MHz.
//!
//! The H3 uses the same MMC controller hardware as the A20 but has
//! different clock gating and reset register layouts (sun6i-generation
//! with separate bus-reset registers).
//!
//! Hardware: Allwinner SD/MMC controller at 0x01C0F000 (MMC0).
//! Ported from u-boot `drivers/mmc/sunxi_mmc.c` (sun6i clock paths).

#![no_std]

use core::cell::Cell;

use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{BlockDevice, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiH3CcuRegs, MMC_CLK};

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Driver configuration (from board RON)
// ---------------------------------------------------------------------------

/// Configuration for the Allwinner H3 MMC controller.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiH3MmcConfig {
    /// MMC controller base address (e.g., 0x01C0F000 for MMC0).
    pub base_addr: u64,
    /// CCU base address (0x01C20000) for clock gating.
    pub ccu_base: u64,
    /// PIO base address (0x01C20800) for GPIO pin mux.
    pub pio_base: u64,
    /// MMC controller index (0-2) for clock register selection.
    pub mmc_index: u8,
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
    /// Allwinner H3 MMC controller register block (same hardware as A20).
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

// FIFO is at offset 0x200 on sun8i/H3 (CONFIG_SUNXI_GEN_SUN6I).
// On A20/sun7i it is at 0x100. On H3 the range 0x100-0x1FF holds
// the threshold-control (thldc), sample-delay, and padding registers,
// and the FIFO data port starts at 0x200.
// Reference: U-Boot drivers/mmc/sunxi_mmc.h `struct sunxi_mmc`,
// conditional on CONFIG_SUNXI_GEN_SUN6I.
const FIFO_OFFSET: usize = 0x200;

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

// GPIO pin mux: Port F, function 2 = SDC0 (same as A20).
const PIO_PF_OFFSET: usize = 5 * 0x24; // Port F = bank 5
const PIO_CFG0_OFF: usize = 0x00;
const PIO_DRV0_OFF: usize = 0x14;
const PIO_PULL0_OFF: usize = 0x1C;

// SD command indices.
const CMD0: u32 = 0;
const CMD2: u32 = 2;
const CMD3: u32 = 3;
const CMD7: u32 = 7;
const CMD8: u32 = 8;
const CMD16: u32 = 16;
const CMD17: u32 = 17;
const CMD18: u32 = 18;
const CMD55: u32 = 55;
const ACMD6: u32 = 6;
const ACMD41: u32 = 41;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Allwinner H3/H2+ MMC host controller driver.
pub struct SunxiH3Mmc {
    regs: &'static SunxiMmcRegs,
    fifo: *mut u32,
    ccu: &'static SunxiH3CcuRegs,
    pio_base: usize,
    mmc_index: u8,
    rca: Cell<u16>,
    sdhc: Cell<bool>,
    capacity: Cell<u64>,
}

// SAFETY: MMC controller is a fixed MMIO peripheral, accessed from a
// single-threaded firmware context.
unsafe impl Send for SunxiH3Mmc {}
unsafe impl Sync for SunxiH3Mmc {}

impl Device for SunxiH3Mmc {
    const NAME: &'static str = "sunxi-h3-mmc";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun8i-h3-mmc"];
    type Config = SunxiH3MmcConfig;

    fn new(config: &SunxiH3MmcConfig) -> Result<Self, DeviceError> {
        let base = config.base_addr as usize;
        let regs = unsafe { &*(base as *const SunxiMmcRegs) };
        let fifo = (base + FIFO_OFFSET) as *mut u32;
        let ccu = unsafe { &*(config.ccu_base as *const SunxiH3CcuRegs) };
        let pio_base = config.pio_base as usize;

        Ok(Self {
            regs,
            fifo,
            ccu,
            pio_base,
            mmc_index: config.mmc_index,
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

impl BlockDevice for SunxiH3Mmc {
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

        // Handle partial first block
        if byte_offset != 0 {
            self.read_blocks(current_block, &mut block_buf, 1)?;
            let avail = (BLOCK_SIZE as usize) - byte_offset;
            let to_copy = avail.min(buf.len());
            buf[..to_copy].copy_from_slice(&block_buf[byte_offset..byte_offset + to_copy]);
            buf_pos += to_copy;
            bytes_read += to_copy;
            current_block += 1;
        }

        // Full blocks via multi-block CMD18
        let remaining_full = (buf.len() - buf_pos) / BLOCK_SIZE as usize;
        if remaining_full > 0 {
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

        // Partial last block
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

impl SunxiH3Mmc {
    /// Configure PF0-PF5 for SDC0 function (same pin mux as A20).
    fn setup_gpio(&self) {
        let pf_base = self.pio_base + PIO_PF_OFFSET;
        unsafe {
            fstart_mmio::write32((pf_base + PIO_CFG0_OFF) as *mut u32, 0x0022_2222);
            fstart_mmio::write32((pf_base + PIO_DRV0_OFF) as *mut u32, 0x0000_0AAA);
            fstart_mmio::write32((pf_base + PIO_PULL0_OFF) as *mut u32, 0x0000_0555);
        }
    }

    /// Enable AHB1 clock gate, deassert reset, and set module clock.
    ///
    /// H3 has separate bus-reset registers (unlike A20 which only has gates).
    fn setup_clocks(&self) {
        // Enable AHB1 gate for this MMC controller (bit 8 + index)
        let gate = self.ccu.bus_gate0.get();
        self.ccu.bus_gate0.set(gate | (1 << (8 + self.mmc_index)));

        // Deassert bus reset for this MMC controller (bit 8 + index)
        let reset = self.ccu.bus_reset0.get();
        self.ccu.bus_reset0.set(reset | (1 << (8 + self.mmc_index)));

        // Set module clock: OSC24M, N=0, M=0 -> 24 MHz
        self.ccu
            .mmc_clk(self.mmc_index)
            .write(MMC_CLK::ENABLE::SET + MMC_CLK::CLK_SRC::Osc24M);
    }

    /// Reset the controller.
    ///
    /// Writes TMOUT to `0xFFFFFFFF` after soft-reset to ensure the
    /// hardware data-timeout counter does not fire prematurely.  The
    /// A20 BROM leaves this at `0xFFFFFF40`, which is safe.  The H3
    /// BROM (SD-boot path) may leave a smaller value, so we set it
    /// explicitly to match U-Boot SPL behaviour.
    fn reset_controller(&self) {
        self.regs
            .gctrl
            .write(GCTRL::SOFT_RESET::SET + GCTRL::FIFO_RESET::SET + GCTRL::DMA_RESET::SET);
        udelay(1000);
        // Set hardware timeout to maximum so DATA_TIMEOUT in RINT is
        // not triggered before software polling has a chance to drain
        // the FIFO.
        self.regs.timeout.set(0xFFFF_FFFF);
    }

    /// Update the internal clock divider.
    fn update_clk(&self) -> Result<(), ServiceError> {
        self.regs
            .cmd
            .write(CMD::START::SET + CMD::UPCLK_ONLY::SET + CMD::WAIT_PRE_OVER::SET);

        for _ in 0..200_000u32 {
            if !self.regs.cmd.is_set(CMD::START) {
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
        let (src, src_hz) = if target_hz <= 24_000_000 {
            (MMC_CLK::CLK_SRC::Osc24M, 24_000_000u32)
        } else {
            (MMC_CLK::CLK_SRC::Pll6, 600_000_000u32)
        };

        let mut div = src_hz.div_ceil(target_hz);
        let mut n = 0u32;
        while div > 16 {
            n += 1;
            div = div.div_ceil(2);
        }
        let m = div.max(1);

        let (oclk_dly, sclk_dly) = if target_hz <= 400_000 {
            (0u32, 0u32)
        } else if target_hz <= 25_000_000 {
            (0, 5)
        } else {
            (3, 4)
        };

        self.ccu.mmc_clk(self.mmc_index).write(
            MMC_CLK::ENABLE::SET
                + src
                + MMC_CLK::SCLK_DLY.val(sclk_dly)
                + MMC_CLK::N.val(n)
                + MMC_CLK::OCLK_DLY.val(oclk_dly)
                + MMC_CLK::M.val(m - 1),
        );
    }

    /// Configure clock and update the card clock.
    fn config_clock(&self, target_hz: u32) -> Result<(), ServiceError> {
        let mut rval = self.regs.clkcr.get();
        rval &= !(1 << 16);
        self.regs.clkcr.set(rval);
        self.update_clk()?;
        self.set_mod_clk(target_hz);
        rval &= !0xFF;
        self.regs.clkcr.set(rval);
        rval |= 1 << 16;
        self.regs.clkcr.set(rval);
        self.update_clk()?;
        Ok(())
    }

    /// Build command register value.
    fn build_cmdval(cmd_index: u32, resp_type: &RespType, has_data: bool) -> u32 {
        let mut cmdval = (1u32 << 31) | cmd_index;
        if cmd_index == CMD0 {
            cmdval |= 1 << 15;
        }
        match resp_type {
            RespType::None => {}
            RespType::R1 | RespType::R1b | RespType::R6 | RespType::R7 => {
                cmdval |= (1 << 6) | (1 << 8);
            }
            RespType::R2 => {
                cmdval |= (1 << 6) | (1 << 7) | (1 << 8);
            }
            RespType::R3 => {
                cmdval |= 1 << 6;
            }
        }
        if has_data {
            cmdval |= (1 << 9) | (1 << 13);
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

    /// PIO read: transfer `words` 32-bit words from FIFO into `buf`.
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
                    // FIFO_EMPTY bit clear → data available
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
        Ok(())
    }

    /// Send an SD command.
    fn send_cmd(
        &self,
        cmd_index: u32,
        arg: u32,
        resp_type: RespType,
        has_data: bool,
    ) -> Result<u32, ServiceError> {
        let cmdval = Self::build_cmdval(cmd_index, &resp_type, has_data);
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.arg.set(arg);
        self.regs.cmd.set(cmdval);

        if let Err(e) = self.rint_wait(1 << 2, 1_000_000) {
            self.error_recovery();
            return Err(e);
        }

        if matches!(resp_type, RespType::R1b) {
            for _ in 0..2_000_000u32 {
                if !self.regs.status.is_set(STATUS::CARD_DATA_BUSY) {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        let resp = self.regs.resp0.get();
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.gctrl.modify(GCTRL::FIFO_RESET::SET);
        Ok(resp)
    }

    /// Read one or more contiguous 512-byte blocks using PIO (CPU).
    ///
    /// Uses CMD17 for single-block reads and CMD18 + AUTO_STOP for
    /// multi-block reads. `buf` must be at least `num_blocks * 512`
    /// bytes and 4-byte aligned.
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
        let mut cmdval = Self::build_cmdval(cmd_index, &resp_type, true);
        if multi {
            cmdval |= 1 << 12;
        }

        let addr = if self.sdhc.get() {
            start_block as u32
        } else {
            (start_block * BLOCK_SIZE as u64) as u32
        };

        let total_bytes = num_blocks * BLOCK_SIZE;
        self.regs.blksz.set(BLOCK_SIZE);
        self.regs.bytecnt.set(total_bytes);
        self.regs.gctrl.modify(GCTRL::ACCESS_BY_AHB::SET);
        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.arg.set(addr);
        self.regs.cmd.set(cmdval);

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

        if let Err(e) = self.rint_wait(1 << 2, 1_000_000) {
            fstart_log::error!(
                "mmc: read CMD_DONE failed blk={} n={}",
                start_block as u32,
                num_blocks
            );
            self.error_recovery();
            return Err(e);
        }

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

        self.regs.rint.set(0xFFFF_FFFF);
        self.regs.gctrl.modify(GCTRL::FIFO_RESET::SET);
        Ok(())
    }

    /// Error recovery.
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
        self.config_clock(400_000)?;
        self.regs.width.write(WIDTH::BUS_WIDTH::Width1);

        self.send_cmd(CMD0, 0, RespType::None, false)?;
        udelay(10);

        let sd_v2 = self
            .send_cmd(CMD8, 0x0000_01AA, RespType::R7, false)
            .is_ok();
        let _ = sd_v2;

        let mut tries = 1000u32;
        let mut ocr;
        loop {
            self.send_cmd(CMD55, 0, RespType::R1, false)?;
            ocr = self.send_cmd(ACMD41, 0x40FF_8000, RespType::R3, false)?;
            if ocr & (1 << 31) != 0 {
                break;
            }
            tries -= 1;
            if tries == 0 {
                return Err(ServiceError::Timeout);
            }
            udelay(1);
        }

        self.sdhc.set(ocr & (1 << 30) != 0);
        self.send_cmd(CMD2, 0, RespType::R2, false)?;

        let r6 = self.send_cmd(CMD3, 0, RespType::R6, false)?;
        self.rca.set((r6 >> 16) as u16);

        self.config_clock(25_000_000)?;

        let rca32 = (self.rca.get() as u32) << 16;
        self.send_cmd(CMD7, rca32, RespType::R1b, false)?;
        self.send_cmd(CMD16, BLOCK_SIZE, RespType::R1, false)?;

        self.send_cmd(CMD55, rca32, RespType::R1, false)?;
        self.send_cmd(ACMD6, 2, RespType::R1, false)?;
        self.regs.width.write(WIDTH::BUS_WIDTH::Width4);

        let cap = if self.sdhc.get() {
            2 * 1024 * 1024 * 1024u64
        } else {
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
