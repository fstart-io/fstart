//! Allwinner sunxi MMC0 host controller driver (A20, H3/H2+/H5, D1/T113).
//!
//! Minimal read-only driver for booting from SD card. Implements
//! [`BlockDevice`] and supports SD v2.0 cards (SDHC) in 4-bit mode at 25 MHz.
//!
//! Per-SoC differences (MMIO base, FIFO offset, PIO generation, CCU clock
//! gating/reset, and module clock register layout) are captured in
//! [`MmcVariant`].
//!
//! Ported from U-Boot `drivers/mmc/sunxi_mmc.c`.

use core::cell::Cell;
use core::marker::PhantomData;

use fstart_arch::udelay;
use fstart_core::mmio::{MmioReadWrite, read32, write32};
use fstart_core::{
    mmio32,
    services::{BlockDevice, ServiceError},
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

use crate::a20_ccu::{A20_CCU_BASE, A20_MMC0_BASE, A20_PIO_BASE};
use crate::ccu_regs::{D1_MMC_CLK, MMC_CLK, SunxiA20CcuRegs, SunxiD1CcuRegs, SunxiH3CcuRegs};
use crate::d1_ccu::{D1_CCU_BASE, D1_MMC0_BASE, D1_PIO_BASE};
use crate::h3_ccu::{H3_CCU_BASE, H3_PIO_BASE};
use crate::pio::{PORT_F, PioGen, SunxiPio};

/// H3/H2+/H5 MMC0 register block base address.
pub const H3_MMC0_BASE: u64 = 0x01c0_f000;

// ---------------------------------------------------------------------------
// Board policy
// ---------------------------------------------------------------------------

/// Sunxi MMC controllers supported by the early-boot flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SunxiMmcController {
    /// SD/MMC controller zero.
    Mmc0,
}

/// Board-owned MMC policy. Fixed SoC addresses are not configuration.
#[derive(Debug, Clone, Copy)]
pub struct SunxiMmcConfig {
    pub controller: SunxiMmcController,
}

impl SunxiMmcConfig {
    /// Construct policy for the supported boot controller.
    #[must_use]
    pub const fn new(controller: SunxiMmcController) -> Self {
        Self { controller }
    }

    /// Validate the boot-media closure.
    #[must_use]
    pub const fn build(self) -> Self {
        match self.controller {
            SunxiMmcController::Mmc0 => self,
        }
    }
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
        ACCESS_BY_AHB OFFSET(31) NUMBITS(1) []
    ],
    /// Clock Control Register (offset 0x04).
    CLKCR [
        DIVIDER OFFSET(0) NUMBITS(8) [],
        CLK_ENABLE OFFSET(16) NUMBITS(1) [],
        CLK_POWERSAVE OFFSET(17) NUMBITS(1) []
    ],
    /// Bus Width Register (offset 0x0C).
    WIDTH [
        BUS_WIDTH OFFSET(0) NUMBITS(2) [
            Width1 = 0,
            Width4 = 1,
            Width8 = 2
        ]
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
        START OFFSET(31) NUMBITS(1) []
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
        END_BIT_ERROR OFFSET(15) NUMBITS(1) []
    ],
    /// Status Register (offset 0x3C).
    STATUS [
        FIFO_EMPTY OFFSET(2) NUMBITS(1) [],
        FIFO_FULL OFFSET(3) NUMBITS(1) [],
        CARD_DATA_BUSY OFFSET(9) NUMBITS(1) [],
        FIFO_LEVEL OFFSET(17) NUMBITS(14) []
    ]
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

/// AHB gate register 0 (sun4i/sun6i) — bit 8 enables MMC0.
const CCU_AHB_GATE0_OFF: usize = 0x060;
/// Bus soft-reset register 0 (sun6i/H3) — bit 8 deasserts MMC0 reset.
const CCU_H3_BUS_RESET0_OFF: usize = 0x2c0;
/// Combined bus gating + reset register (NCAT2/D1) — bit 0 gate, bit 16 reset.
const CCU_D1_MMC_BGR_OFF: usize = 0x84c;

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
// Per-SoC variant hooks
// ---------------------------------------------------------------------------

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::A20 {}
    impl Sealed for super::H3 {}
    impl Sealed for super::D1 {}
}

/// Per-SoC parameterization of the sunxi MMC0 controller (sealed to the
/// built-in sunxi variants whose fixed MMIO addresses are known valid).
pub trait MmcVariant: sealed::Sealed {
    const MMC0_BASE: u64;
    const FIFO_OFFSET: usize;
    const PIO_BASE: u64;
    const PIO_GEN: PioGen;

    /// Enable the bus clock gate (and deassert reset where separate) and
    /// select the initial 24 MHz module clock.
    ///
    /// # Safety
    ///
    /// Caller must own MMC0 initialization on this SoC with the CCU mapped.
    unsafe fn setup_clocks();

    /// Configure the CCU module clock for `target_hz` (must be non-zero).
    ///
    /// # Safety
    ///
    /// Caller must own MMC0 initialization on this SoC with the CCU mapped.
    unsafe fn set_mod_clk(target_hz: u32);
}

/// Shared module-clock divider calculation for sun4i/sun6i (`MMC_CLK` layout).
fn set_mod_clk_legacy(reg: &MmioReadWrite<u32, MMC_CLK::Register>, target_hz: u32) {
    assert!(target_hz > 0, "MMC clock target_hz must be non-zero");
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

    reg.write(
        MMC_CLK::ENABLE::SET
            + src
            + MMC_CLK::SCLK_DLY.val(sclk_dly)
            + MMC_CLK::N.val(n)
            + MMC_CLK::OCLK_DLY.val(oclk_dly)
            + MMC_CLK::M.val(m - 1),
    );
}

/// Allwinner A20 (sun7i) MMC0 variant.
pub struct A20;

impl MmcVariant for A20 {
    const MMC0_BASE: u64 = A20_MMC0_BASE;
    const FIFO_OFFSET: usize = 0x100;
    const PIO_BASE: u64 = A20_PIO_BASE;
    const PIO_GEN: PioGen = PioGen::Legacy;

    unsafe fn setup_clocks() {
        let gate_addr = (A20_CCU_BASE as usize + CCU_AHB_GATE0_OFF) as *mut u32;
        // SAFETY: AHB gate register at the fixed A20 CCU mapping.
        unsafe {
            let gate = read32(gate_addr);
            write32(gate_addr, gate | (1 << 8));
        }
        // SAFETY: fixed A20 CCU MMIO mapping.
        let ccu = unsafe { &*(A20_CCU_BASE as *const SunxiA20CcuRegs) };
        ccu.mmc_clk(0)
            .write(MMC_CLK::ENABLE::SET + MMC_CLK::CLK_SRC::Osc24M);
    }

    unsafe fn set_mod_clk(target_hz: u32) {
        // SAFETY: fixed A20 CCU MMIO mapping.
        let ccu = unsafe { &*(A20_CCU_BASE as *const SunxiA20CcuRegs) };
        set_mod_clk_legacy(ccu.mmc_clk(0), target_hz);
    }
}

/// Allwinner H3/H2+/H5 (sun8i/sun50i) MMC0 variant.
pub struct H3;

impl MmcVariant for H3 {
    const MMC0_BASE: u64 = H3_MMC0_BASE;
    const FIFO_OFFSET: usize = 0x200;
    const PIO_BASE: u64 = H3_PIO_BASE;
    const PIO_GEN: PioGen = PioGen::Legacy;

    unsafe fn setup_clocks() {
        let gate_addr = (H3_CCU_BASE as usize + CCU_AHB_GATE0_OFF) as *mut u32;
        let reset_addr = (H3_CCU_BASE as usize + CCU_H3_BUS_RESET0_OFF) as *mut u32;
        // SAFETY: AHB gate and bus-reset registers at the fixed H3 CCU mapping.
        unsafe {
            write32(gate_addr, read32(gate_addr) | (1 << 8));
            write32(reset_addr, read32(reset_addr) | (1 << 8));
        }
        // SAFETY: fixed H3 CCU MMIO mapping.
        let ccu = unsafe { &*(H3_CCU_BASE as *const SunxiH3CcuRegs) };
        ccu.mmc_clk(0)
            .write(MMC_CLK::ENABLE::SET + MMC_CLK::CLK_SRC::Osc24M);
    }

    unsafe fn set_mod_clk(target_hz: u32) {
        // SAFETY: fixed H3 CCU MMIO mapping.
        let ccu = unsafe { &*(H3_CCU_BASE as *const SunxiH3CcuRegs) };
        set_mod_clk_legacy(ccu.mmc_clk(0), target_hz);
    }
}

/// Allwinner D1/T113 (sun20i / NCAT2) MMC0 variant.
pub struct D1;

impl MmcVariant for D1 {
    const MMC0_BASE: u64 = D1_MMC0_BASE;
    const FIFO_OFFSET: usize = 0x200;
    const PIO_BASE: u64 = D1_PIO_BASE;
    const PIO_GEN: PioGen = PioGen::Ncat2;

    unsafe fn setup_clocks() {
        let bgr_addr = (D1_CCU_BASE as usize + CCU_D1_MMC_BGR_OFF) as *mut u32;
        // SAFETY: MMC BGR register at the fixed D1 CCU mapping.
        unsafe {
            write32(bgr_addr, read32(bgr_addr) | (1 << 0) | (1 << 16));
        }
        // SAFETY: fixed D1 CCU MMIO mapping.
        let ccu = unsafe { &*(D1_CCU_BASE as *const SunxiD1CcuRegs) };
        ccu.mmc_clk(0)
            .write(D1_MMC_CLK::ENABLE::SET + D1_MMC_CLK::CLK_SRC::Osc24M);
    }

    unsafe fn set_mod_clk(target_hz: u32) {
        assert!(target_hz > 0, "MMC clock target_hz must be non-zero");
        let (src, src_hz) = if target_hz <= 24_000_000 {
            (D1_MMC_CLK::CLK_SRC::Osc24M, 24_000_000u32)
        } else {
            (D1_MMC_CLK::CLK_SRC::PllPeriph0, 600_000_000u32)
        };

        let mut div = src_hz.div_ceil(target_hz);
        let mut n = 0u32;
        while div > 16 {
            n += 1;
            div = div.div_ceil(2);
        }
        let m = div.max(1);

        // SAFETY: fixed D1 CCU MMIO mapping.
        let ccu = unsafe { &*(D1_CCU_BASE as *const SunxiD1CcuRegs) };
        ccu.mmc_clk(0)
            .write(D1_MMC_CLK::ENABLE::SET + src + D1_MMC_CLK::N.val(n) + D1_MMC_CLK::M.val(m - 1));
    }
}

/// Convenience aliases matching each SoC's platform module.
pub type A20Mmc = SunxiMmc<A20>;
pub type H3Mmc = SunxiMmc<H3>;
pub type D1Mmc = SunxiMmc<D1>;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// Parameterized Allwinner sunxi MMC0 host controller.
pub struct SunxiMmc<V: MmcVariant> {
    regs: &'static SunxiMmcRegs,
    fifo: *const u32,
    _config: &'static SunxiMmcConfig,
    rca: Cell<u16>,
    sdhc: Cell<bool>,
    capacity: Cell<u64>,
    _variant: PhantomData<V>,
}

// SAFETY: the controller is a fixed MMIO peripheral used during single-threaded boot.
unsafe impl<V: MmcVariant> Send for SunxiMmc<V> {}
// SAFETY: the controller is a fixed MMIO peripheral used during single-threaded boot.
unsafe impl<V: MmcVariant> Sync for SunxiMmc<V> {}

impl<V: MmcVariant> SunxiMmc<V> {
    /// Construct without touching hardware.
    #[must_use]
    pub fn new_from_config(config: &'static SunxiMmcConfig) -> Self {
        match config.controller {
            SunxiMmcController::Mmc0 => Self {
                // SAFETY: MMC0 has a fixed physical MMIO mapping for `V`.
                regs: unsafe { &*(V::MMC0_BASE as *const SunxiMmcRegs) },
                fifo: (V::MMC0_BASE as usize + V::FIFO_OFFSET) as *const u32,
                _config: config,
                rca: Cell::new(0),
                sdhc: Cell::new(false),
                capacity: Cell::new(0),
                _variant: PhantomData,
            },
        }
    }

    /// Configure pins/clocks and initialize the SD card.
    pub fn init(&mut self) -> Result<(), ServiceError> {
        fstart_log::debug!("mmc: setup_gpio");
        self.setup_gpio();
        fstart_log::debug!("mmc: setup_clocks");
        // SAFETY: `self` owns MMC0 initialization on this SoC.
        unsafe { V::setup_clocks() };
        fstart_log::debug!("mmc: reset_controller");
        self.reset_controller();
        fstart_log::debug!("mmc: sd_card_init");
        self.sd_card_init()?;
        fstart_log::debug!("mmc: init complete");
        Ok(())
    }
}

impl<V: MmcVariant> BlockDevice for SunxiMmc<V> {
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
        Err(ServiceError::NotSupported)
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

impl<V: MmcVariant> SunxiMmc<V> {
    /// Configure PF0-PF5 for the MMC0 function.
    fn setup_gpio(&self) {
        let pio = SunxiPio::new(mmio32(V::PIO_BASE), V::PIO_GEN);
        pio.write_cfg_raw(PORT_F, 0, 0x0022_2222);
        pio.write_drv_raw(PORT_F, 0, 0x0000_0aaa);
        pio.write_pull_raw(PORT_F, 0, 0x0000_0555);
    }

    /// Reset the controller (matches U-Boot SPL `sunxi_mmc_reset()`).
    fn reset_controller(&self) {
        self.regs
            .gctrl
            .write(GCTRL::SOFT_RESET::SET + GCTRL::FIFO_RESET::SET + GCTRL::DMA_RESET::SET);
        udelay(1000);
    }

    /// Update the internal clock divider (required after clock changes).
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

    /// Configure clock and update the card clock.
    fn config_clock(&self, target_hz: u32) -> Result<(), ServiceError> {
        let mut rval = self.regs.clkcr.get();

        // Disable card clock.
        rval &= !(1 << 16);
        self.regs.clkcr.set(rval);
        self.update_clk()?;

        // Set module clock (CCU register).
        // SAFETY: `self` owns MMC0 and `target_hz` is non-zero (400 kHz / 25 MHz).
        unsafe { V::set_mod_clk(target_hz) };

        // Clear internal divider (low 8 bits), preserve other bits.
        rval &= !0xFF;
        self.regs.clkcr.set(rval);

        // Re-enable card clock.
        rval |= 1 << 16;
        self.regs.clkcr.set(rval);
        self.update_clk()?;
        Ok(())
    }

    /// Build the command register value for an SD command.
    fn build_cmdval(cmd_index: u32, resp_type: &RespType, has_data: bool) -> u32 {
        let mut cmdval = (1u32 << 31) | cmd_index;
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
    fn pio_read_data(&self, buf: &mut [u8], words: usize) -> Result<(), ServiceError> {
        debug_assert!(buf.len() >= words * 4);
        let mut i = 0usize;

        while i < words {
            let mut timeout = 2_000_000u32;
            let st = loop {
                let st = self.regs.status.get();
                if st & (1 << 2) == 0 {
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

            // Quirk: when FIFO is completely full, level reads as 0.
            let mut in_fifo = ((st >> 17) & 0x3FFF) as usize;
            if in_fifo == 0 && (st & (1 << 3)) != 0 {
                in_fifo = 32;
            }
            let to_read = in_fifo.min(words - i);

            for _ in 0..to_read {
                // SAFETY: FIFO register at the fixed MMIO address for `V`.
                let word = unsafe { read32(self.fifo) };
                buf[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
                i += 1;
            }
        }

        Ok(())
    }

    /// Send an SD command and wait for completion.
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
            cmdval |= 1 << 12; // AUTO_STOP
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
        self.config_clock(400_000)?;
        self.regs.width.write(WIDTH::BUS_WIDTH::Width1);
        fstart_log::debug!("mmc: 400kHz clock configured, 1-bit bus");

        self.send_cmd(CMD0, 0, RespType::None, false)?;
        udelay(10);
        fstart_log::debug!("mmc: CMD0 ok");

        let sd_v2 = self
            .send_cmd(CMD8, 0x0000_01AA, RespType::R7, false)
            .is_ok();
        fstart_log::debug!("mmc: CMD8 sd_v2={}", sd_v2 as u8);

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
                fstart_log::error!("mmc: ACMD41 timeout");
                return Err(ServiceError::Timeout);
            }
            udelay(1);
        }

        self.sdhc.set(ocr & (1 << 30) != 0);
        fstart_log::debug!("mmc: ACMD41 ok, sdhc={}", self.sdhc.get() as u8);

        self.send_cmd(CMD2, 0, RespType::R2, false)?;
        fstart_log::debug!("mmc: CMD2 ok");

        let r6 = self.send_cmd(CMD3, 0, RespType::R6, false)?;
        self.rca.set((r6 >> 16) as u16);
        fstart_log::debug!("mmc: CMD3 ok, rca={}", self.rca.get() as u32);

        self.config_clock(25_000_000)?;
        fstart_log::debug!("mmc: 25MHz clock configured");

        let rca32 = (self.rca.get() as u32) << 16;
        self.send_cmd(CMD7, rca32, RespType::R1b, false)?;
        fstart_log::debug!("mmc: CMD7 ok (card selected)");

        self.send_cmd(CMD16, BLOCK_SIZE, RespType::R1, false)?;
        fstart_log::debug!("mmc: CMD16 ok (blocklen=512)");

        self.send_cmd(CMD55, rca32, RespType::R1, false)?;
        self.send_cmd(ACMD6, 2, RespType::R1, false)?;
        self.regs.width.write(WIDTH::BUS_WIDTH::Width4);
        fstart_log::debug!("mmc: 4-bit bus configured");

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
