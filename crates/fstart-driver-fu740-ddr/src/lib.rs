//! SiFive FU740 DDR4 memory controller driver.
//!
//! Performs full DDR4 initialization on the HiFive Unmatched board:
//! register programming, PHY reset, training (write leveling, read
//! leveling, VREF), and physical filter (bus blocker) configuration.
//!
//! The DDR subsystem uses a Cadence Denali DDR4 controller with a
//! Denali DDR PHY. Register values are board-specific "magic numbers"
//! from SiFive, originally in U-Boot's device tree and coreboot's
//! ddrregs.c.
//!
//! ## DDR subsystem addresses
//!
//! | Block             | Address       | Size   |
//! |-------------------|---------------|--------|
//! | DDR Controller    | 0x100B_0000   | 0x0800 |
//! | DDR PHY           | 0x100B_2000   | 0x2000 |
//! | Physical Filter   | 0x100B_8000   | 0x1000 |
//!
//! ## Prerequisites
//!
//! The PRCI driver must have already:
//! 1. Configured DDRPLL to ~933 MHz
//! 2. Deasserted DDR controller, AXI, AHB, and PHY resets
//! 3. Waited 256 DDR controller clock cycles
//!
//! ## Reference
//!
//! - coreboot `src/soc/sifive/fu740/sdram.c`
//! - U-Boot `drivers/ram/sifive/sifive_ddr.c`
//! - FU740-C000 Manual Chapter 32: DDR Subsystem

#![no_std]

mod regs;

use fstart_mmio::{read32, read64, write32, write64};
use fstart_services::device::{Device, DeviceError};
use fstart_services::MemoryController;

use regs::{DENALI_CTL, DENALI_PHY};

// ---------------------------------------------------------------------------
// DDR controller register indices and bit definitions
// ---------------------------------------------------------------------------

/// DENALI_CTL_0: START bit (bit 0) and DRAM_CLASS (bits [11:8]).
const CTL_0_START: u32 = 1 << 0;
const DRAM_CLASS_OFFSET: u32 = 8;
const DRAM_CLASS_DDR4: u32 = 0xA;

/// DENALI_CTL_21: OPTIMAL_RMODW_EN (bit 0).
const CTL_21_OPTIMAL_RMODW_EN: u32 = 1 << 0;

/// DENALI_CTL_120: DISABLE_RD_INTERLEAVE (bit 16).
const CTL_120_DISABLE_RD_INTERLEAVE: u32 = 1 << 16;

/// DENALI_CTL_132: MC_INIT_COMPLETE (bit 8).
const CTL_132_MC_INIT_COMPLETE: u32 = 1 << 8;

/// DENALI_CTL_136: interrupt mask bits.
const CTL_136_OUT_OF_RANGE: u32 = 1 << 1;
const CTL_136_MULTI_OUT_OF_RANGE: u32 = 1 << 2;
const CTL_136_PORT_CMD_ERROR: u32 = 1 << 7;
const CTL_136_MC_INIT_COMPLETE: u32 = 1 << 8;
const CTL_136_LEVELING_DONE: u32 = 1 << 22;

/// DENALI_CTL_170: WRLVL_EN (bit 0), DFI_PHY_WRLELV_MODE (bit 24).
const CTL_170_WRLVL_EN: u32 = 1 << 0;
const CTL_170_DFI_PHY_WRLELV_MODE: u32 = 1 << 24;

/// DENALI_CTL_181: DFI_PHY_RDLVL_MODE (bit 24).
const CTL_181_DFI_PHY_RDLVL_MODE: u32 = 1 << 24;

/// DENALI_CTL_182: DFI_PHY_RDLVL_GATE_MODE (bit 0).
const CTL_182_DFI_PHY_RDLVL_GATE_MODE: u32 = 1 << 0;

/// DENALI_CTL_184: VREF_EN (bit 24, DDR4 only).
const CTL_184_VREF_EN: u32 = 1 << 24;

/// DENALI_CTL_208: PORT_ADDR_PROTECTION_EN (bit 0), AXI0_ADDRESS_RANGE_ENABLE (bit 8).
const CTL_208_PORT_ADDR_PROT_EN: u32 = 1 << 0;
const CTL_208_AXI0_ADDR_RANGE_EN: u32 = 1 << 8;

/// DENALI_CTL_224: AXI0_RANGE_PROT_BITS_0 (bits [25:24]).
const CTL_224_AXI0_RANGE_PROT: u32 = 0x3 << 24;

/// DENALI_CTL_260: RDLVL_EN (bit 16), RDLVL_GATE_EN (bit 24).
const CTL_260_RDLVL_EN: u32 = 1 << 16;
const CTL_260_RDLVL_GATE_EN: u32 = 1 << 24;

// ---------------------------------------------------------------------------
// Physical filter (bus blocker)
// ---------------------------------------------------------------------------

/// Physical filter TOR+RWX access control bits.
/// 0x0F = Read + Write + Execute + TOR (Top-Of-Range PMP-style).
const PHYS_FILTER_RWX_TOR: u64 = 0x0F00_0000_0000_0000;

// ---------------------------------------------------------------------------
// DRAM base address
// ---------------------------------------------------------------------------

const FU740_DRAM_BASE: u64 = 0x8000_0000;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Typed configuration for the FU740 DDR4 memory controller.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Fu740DdrConfig {
    /// DDR controller base address (0x100B_0000).
    pub ctl_base: u64,
    /// DDR PHY base address (0x100B_2000).
    pub phy_base: u64,
    /// Physical filter (bus blocker) base address (0x100B_8000).
    pub filter_base: u64,
    /// Total DRAM size in bytes (e.g., 0x4_0000_0000 for 16 GiB).
    pub dram_size: u64,
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// FU740 DDR4 memory controller driver.
///
/// Performs the full DDR4 initialization sequence including register
/// programming, PHY training, and physical filter configuration.
pub struct Fu740Ddr {
    ctl_base: usize,
    phy_base: usize,
    filter_base: usize,
    dram_size: u64,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
unsafe impl Send for Fu740Ddr {}
unsafe impl Sync for Fu740Ddr {}

impl Fu740Ddr {
    /// Read a 32-bit DDR controller register by index.
    #[inline(always)]
    fn ctl_read(&self, index: usize) -> u32 {
        let addr = self.ctl_base + index * 4;
        // SAFETY: ctl_base + index*4 is a valid DDR controller register.
        unsafe { read32(addr as *const u32) }
    }

    /// Write a 32-bit DDR controller register by index.
    #[inline(always)]
    fn ctl_write(&self, index: usize, val: u32) {
        let addr = self.ctl_base + index * 4;
        // SAFETY: ctl_base + index*4 is a valid DDR controller register.
        unsafe { write32(addr as *mut u32, val) }
    }

    /// Set bits in a DDR controller register.
    #[inline(always)]
    fn ctl_set_bits(&self, index: usize, bits: u32) {
        let val = self.ctl_read(index);
        self.ctl_write(index, val | bits);
    }

    /// Clear bits in a DDR controller register.
    #[inline(always)]
    fn ctl_clear_bits(&self, index: usize, bits: u32) {
        let val = self.ctl_read(index);
        self.ctl_write(index, val & !bits);
    }

    /// Write a 32-bit DDR PHY register by index.
    #[inline(always)]
    fn phy_write(&self, index: usize, val: u32) {
        let addr = self.phy_base + index * 4;
        // SAFETY: phy_base + index*4 is a valid DDR PHY register.
        unsafe { write32(addr as *mut u32, val) }
    }

    /// Read a 32-bit DDR PHY register by index.
    #[inline(always)]
    fn phy_read(&self, index: usize) -> u32 {
        let addr = self.phy_base + index * 4;
        // SAFETY: phy_base + index*4 is a valid DDR PHY register.
        unsafe { read32(addr as *const u32) }
    }

    /// Step 1: Write all DDR controller registers (265 x 32-bit).
    fn write_ctl_regs(&self) {
        for (i, &val) in DENALI_CTL.iter().enumerate() {
            self.ctl_write(i, val);
        }
    }

    /// Step 2: PHY reset — write registers 1152..1214 first, then 0..1151.
    ///
    /// The upper block (1152-1214) contains global PHY configuration that
    /// must be in place before the per-slice registers (0-1151) are written.
    fn phy_reset(&self) {
        // Phase 1: global PHY config (registers 1152-1214).
        for i in 1152..=1214 {
            self.phy_write(i, DENALI_PHY[i]);
        }

        // Phase 2: per-slice config (registers 0-1151).
        for i in 0..=1151 {
            self.phy_write(i, DENALI_PHY[i]);
        }
    }

    /// Step 3: Disable AXI read interleave.
    fn disable_rd_interleave(&self) {
        self.ctl_set_bits(120, CTL_120_DISABLE_RD_INTERLEAVE);
    }

    /// Step 4: Disable optimal read-modify-write.
    fn disable_optimal_rmodw(&self) {
        self.ctl_clear_bits(21, CTL_21_OPTIMAL_RMODW_EN);
    }

    /// Step 5: Enable write leveling.
    fn enable_write_leveling(&self) {
        self.ctl_set_bits(170, CTL_170_WRLVL_EN | CTL_170_DFI_PHY_WRLELV_MODE);
    }

    /// Step 6: Enable read leveling.
    fn enable_read_leveling(&self) {
        self.ctl_set_bits(181, CTL_181_DFI_PHY_RDLVL_MODE);
        self.ctl_set_bits(260, CTL_260_RDLVL_EN);
    }

    /// Step 7: Enable read leveling gate.
    fn enable_read_leveling_gate(&self) {
        self.ctl_set_bits(260, CTL_260_RDLVL_GATE_EN);
        self.ctl_set_bits(182, CTL_182_DFI_PHY_RDLVL_GATE_MODE);
    }

    /// Step 8: Enable VREF training (DDR4 only).
    fn enable_vref_training(&self) {
        let dram_class = (self.ctl_read(0) >> DRAM_CLASS_OFFSET) & 0xF;
        if dram_class == DRAM_CLASS_DDR4 {
            self.ctl_set_bits(184, CTL_184_VREF_EN);
        }
    }

    /// Step 9: Mask interrupts.
    fn mask_interrupts(&self) {
        self.ctl_set_bits(136, CTL_136_LEVELING_DONE);
        self.ctl_set_bits(136, CTL_136_MC_INIT_COMPLETE);
        self.ctl_set_bits(136, CTL_136_OUT_OF_RANGE | CTL_136_MULTI_OUT_OF_RANGE);
        self.ctl_set_bits(136, CTL_136_PORT_CMD_ERROR);
    }

    /// Step 10: Set up address range protection.
    fn setup_range_protection(&self) {
        self.ctl_write(209, 0x0);
        let size_16k_blocks = ((self.dram_size >> 14) & 0x7F_FFFF) as u32 - 1;
        self.ctl_write(210, size_16k_blocks);
        self.ctl_write(212, 0x0);
        self.ctl_write(214, 0x0);
        self.ctl_write(216, 0x0);
        self.ctl_set_bits(224, CTL_224_AXI0_RANGE_PROT);
        self.ctl_write(225, 0xFFFF_FFFF);
        self.ctl_set_bits(208, CTL_208_AXI0_ADDR_RANGE_EN);
        self.ctl_set_bits(208, CTL_208_PORT_ADDR_PROT_EN);
    }

    /// Step 11: Start DDR controller and wait for init complete.
    fn start_and_wait(&self) -> Result<(), DeviceError> {
        // Set START bit.
        self.ctl_set_bits(0, CTL_0_START);

        // Poll MC_INIT_COMPLETE (CTL_132 bit 8).
        // This waits for all training (write leveling, read leveling,
        // read gate leveling, VREF training) to complete.
        // Timeout: ~10 seconds at ~1 GHz (generous for DDR4 training).
        let mut timeout: u32 = 10_000_000;
        while self.ctl_read(132) & CTL_132_MC_INIT_COMPLETE == 0 {
            core::hint::spin_loop();
            timeout = timeout.wrapping_sub(1);
            if timeout == 0 {
                fstart_log::error!("DDR: MC_INIT_COMPLETE timeout");
                return Err(DeviceError::InitFailed);
            }
        }

        // Open the physical filter (bus blocker) to allow DRAM access.
        let ddr_end = FU740_DRAM_BASE + self.dram_size;
        let filter_val = PHYS_FILTER_RWX_TOR | (ddr_end >> 2);
        // SAFETY: filter_base is a valid physical filter register address.
        unsafe { write64(self.filter_base as *mut u64, filter_val) };

        Ok(())
    }

    /// Step 12: PHY fixup — errata workaround for RX calibration.
    ///
    /// Iterates over all 8 data slices and checks calibration quality.
    /// Logs errors but does not halt (non-fatal).
    fn phy_fixup(&self) {
        let mut fails: u64 = 0;
        let mut slice_base: usize = 0;
        let mut dq: u32 = 0;

        for _slice in 0..8u32 {
            let reg_base = slice_base + 34;
            for reg in 0..4u32 {
                let updownreg = self.phy_read(reg_base + reg as usize);
                for bit in 0..2u32 {
                    let offset = if bit == 0 { 0 } else { 16 };
                    let down = (updownreg >> offset) & 0x3F;
                    let up = (updownreg >> (offset + 6)) & 0x3F;

                    let fail_c0 = down == 0 && up == 0x3F;
                    let fail_c1 = up == 0 && down == 0x3F;

                    if fail_c0 || fail_c1 {
                        fails |= 1 << dq;
                    }
                    dq += 1;
                }
            }
            slice_base += 128;
        }

        if fails != 0 {
            fstart_log::error!("DDR PHY fixup: calibration failures 0x{:016x}", fails);
        }
    }
}

impl Device for Fu740Ddr {
    const NAME: &'static str = "fu740-ddr";
    const COMPATIBLE: &'static [&'static str] = &["sifive,fu740-c000-ddr"];
    type Config = Fu740DdrConfig;

    fn new(config: &Fu740DdrConfig) -> Result<Self, DeviceError> {
        // Minimum 16 KiB — setup_range_protection computes (size >> 14) - 1.
        if config.dram_size < 0x4000 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self {
            ctl_base: config.ctl_base as usize,
            phy_base: config.phy_base as usize,
            filter_base: config.filter_base as usize,
            dram_size: config.dram_size,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Step 1: Write all DDR controller registers.
        self.write_ctl_regs();

        // Step 2: PHY reset (1152-1214 first, then 0-1151).
        self.phy_reset();

        // Step 3-4: Disable AXI read interleave and optimal RMODW.
        self.disable_rd_interleave();
        self.disable_optimal_rmodw();

        // Step 5-7: Enable training modes.
        self.enable_write_leveling();
        self.enable_read_leveling();
        self.enable_read_leveling_gate();

        // Step 8: Enable VREF training (DDR4 only).
        self.enable_vref_training();

        // Step 9: Mask interrupts.
        self.mask_interrupts();

        // Step 10: Set up address range protection.
        self.setup_range_protection();

        // Step 11: Start controller, wait for MC_INIT_COMPLETE, open bus blocker.
        self.start_and_wait()?;

        // Step 12: PHY fixup errata check.
        self.phy_fixup();

        Ok(())
    }
}

impl MemoryController for Fu740Ddr {
    fn detected_size_bytes(&self) -> u64 {
        // Read the physical filter register to get the actual configured DRAM end.
        // SAFETY: filter_base is a valid physical filter register address.
        let pmp_val = unsafe { read64(self.filter_base as *const u64) };
        let ddr_end = (pmp_val & 0x00FF_FFFF_FFFF_FFFF) << 2;
        if ddr_end > FU740_DRAM_BASE {
            ddr_end - FU740_DRAM_BASE
        } else {
            self.dram_size
        }
    }
}
