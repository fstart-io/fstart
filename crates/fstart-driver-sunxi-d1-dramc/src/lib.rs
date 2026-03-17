//! Allwinner D1/T113 (sun20i) DRAM controller driver.
//!
//! Performs full DDR3 DRAM initialization: PLL_DDR0 setup, PHY training,
//! ZQ calibration, MBUS master configuration, eye delay compensation,
//! address/command remapping, and optional auto-detection of rank/width/size.
//!
//! Ported from U-Boot `drivers/ram/sunxi/dram_sun20i_d1.c` (DDR3 paths).
//!
//! Memory-mapped regions:
//! - CCU:  `0x0200_1000` (PLL_DDR0, DRAM clocks, MBUS)
//! - MCTL COM:  `0x0310_2000` (memory controller configuration)
//! - MCTL PHY:  `0x0310_3000` (PHY training, timing, ZQ)
//! - SYS_CFG:   `0x0300_0150` (DRAM LDO voltage)
//! - SID:       `0x0300_6000` (eFuse for AC remapping / LDO cal)
//! - DRAM physical base: `0x4000_0000`

#![no_std]
#![allow(clippy::identity_op)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

use fstart_services::device::{Device, DeviceError};
use fstart_services::MemoryController;

use fstart_arch::udelay;
use fstart_log::{info, warn};

// ---------------------------------------------------------------------------
// Register base addresses
// ---------------------------------------------------------------------------

const CCU_BASE: usize = 0x0200_1000;
const MCTL_COM_BASE: usize = 0x0310_2000;
const MCTL_PHY_BASE: usize = 0x0310_3000;
const SYS_LDO_REG: usize = 0x0300_0150;
const SYS_ZQ_REG: usize = 0x0300_0160;
const SID_BASE: usize = 0x0300_6000;
const RTC_STATUS: usize = 0x0700_05D4;
const SYS_CFG_250: usize = 0x0701_0254;
const SYS_CFG_250_2: usize = 0x0701_0250;
const DRAM_BASE: usize = 0x4000_0000;

// ---------------------------------------------------------------------------
// CCU registers (clock/PLL control)
// ---------------------------------------------------------------------------

const CCU_PLL_CPU_CTRL: usize = CCU_BASE + 0x0;
const CCU_PLL_DDR0_CTRL: usize = CCU_BASE + 0x10;
const CCU_MBUS_CLK: usize = CCU_BASE + 0x540;
const CCU_DRAM_CLK: usize = CCU_BASE + 0x800;
const CCU_DRAM_BGR: usize = CCU_BASE + 0x80C;

// ---------------------------------------------------------------------------
// MCTL_COM registers (memory controller common)
// ---------------------------------------------------------------------------

const MC_WORK_MODE: usize = MCTL_COM_BASE;
const MC_WORK_MODE2: usize = MCTL_COM_BASE + 0x4;
const MC_R1: usize = MCTL_COM_BASE + 0x8;
const MC_CLKDIV: usize = MCTL_COM_BASE + 0xC;
const MC_CCCR: usize = MCTL_COM_BASE + 0x14;
const MAER0: usize = MCTL_COM_BASE + 0x20;
const MAER1: usize = MCTL_COM_BASE + 0x24;
const MAER2: usize = MCTL_COM_BASE + 0x28;
const AC_REMAP0: usize = MCTL_COM_BASE + 0x500;
const AC_REMAP1: usize = MCTL_COM_BASE + 0x504;
const AC_REMAP2: usize = MCTL_COM_BASE + 0x508;
const AC_REMAP3: usize = MCTL_COM_BASE + 0x50C;

// ---------------------------------------------------------------------------
// MCTL_PHY registers (PHY control)
// ---------------------------------------------------------------------------

/// PHY Init Register
const PIR: usize = MCTL_PHY_BASE;
const PGCR1: usize = MCTL_PHY_BASE + 0x4;
const PHY_CLK_EN: usize = MCTL_PHY_BASE + 0xC;
/// PHY General Status
const PGSR0: usize = MCTL_PHY_BASE + 0x10;
/// Controller Status
const STATR: usize = MCTL_PHY_BASE + 0x18;
/// Data Training Config
const DTCR: usize = MCTL_PHY_BASE + 0x2C;
const MR0: usize = MCTL_PHY_BASE + 0x30;
const MR1: usize = MCTL_PHY_BASE + 0x34;
const MR2: usize = MCTL_PHY_BASE + 0x38;
const MR3: usize = MCTL_PHY_BASE + 0x3C;
const PTR3: usize = MCTL_PHY_BASE + 0x50;
const PTR4: usize = MCTL_PHY_BASE + 0x54;
const DRAMTMG0: usize = MCTL_PHY_BASE + 0x58;
const DRAMTMG1: usize = MCTL_PHY_BASE + 0x5C;
const DRAMTMG2: usize = MCTL_PHY_BASE + 0x60;
const DRAMTMG3: usize = MCTL_PHY_BASE + 0x64;
const DRAMTMG4: usize = MCTL_PHY_BASE + 0x68;
const DRAMTMG5: usize = MCTL_PHY_BASE + 0x6C;
const DRAMTMG8: usize = MCTL_PHY_BASE + 0x78;
const PITMG0: usize = MCTL_PHY_BASE + 0x80;
const RFSHCTL3: usize = MCTL_PHY_BASE + 0x8C;
const RFSHTMG: usize = MCTL_PHY_BASE + 0x90;
const RFSHTMG1: usize = MCTL_PHY_BASE + 0x94;
const PWRTMG: usize = MCTL_PHY_BASE + 0x9C;
const PWRCTL: usize = MCTL_PHY_BASE + 0xA0;
const VTFCR: usize = MCTL_PHY_BASE + 0xB8;
const DXCCR: usize = MCTL_PHY_BASE + 0xBC;
const DTCR0: usize = MCTL_PHY_BASE + 0xC0;
const PGCR0: usize = MCTL_PHY_BASE + 0x100;
const DSGCR: usize = MCTL_PHY_BASE + 0x108;
const DTCR1: usize = MCTL_PHY_BASE + 0x10C;
const IOCVR0: usize = MCTL_PHY_BASE + 0x110;
const IOCVR1: usize = MCTL_PHY_BASE + 0x114;
const DXGTR0: usize = MCTL_PHY_BASE + 0x11C;
const ODTMAP: usize = MCTL_PHY_BASE + 0x120;
const ZQ0CR: usize = MCTL_PHY_BASE + 0x140;
const ACIOCR0: usize = MCTL_PHY_BASE + 0x208;
const DX0IOCR_BASE: usize = MCTL_PHY_BASE + 0x310;
const DX0DQS0: usize = MCTL_PHY_BASE + 0x334;
const DX0DQS1: usize = MCTL_PHY_BASE + 0x338;
const DX0DM: usize = MCTL_PHY_BASE + 0x33C;
const DX0GCR0: usize = MCTL_PHY_BASE + 0x344;
const DX0GSR0: usize = MCTL_PHY_BASE + 0x348;
const DX1IOCR_BASE: usize = MCTL_PHY_BASE + 0x390;
const DX1DQS0: usize = MCTL_PHY_BASE + 0x3B4;
const DX1DQS1: usize = MCTL_PHY_BASE + 0x3B8;
const DX1DM: usize = MCTL_PHY_BASE + 0x3BC;
const DX1GCR0: usize = MCTL_PHY_BASE + 0x3C4;
const DX1GSR0: usize = MCTL_PHY_BASE + 0x3C8;

// ---------------------------------------------------------------------------
// Bit flags
// ---------------------------------------------------------------------------

const PLL_EN: u32 = 1 << 31;
const PLL_LDO_EN: u32 = 1 << 30;
const PLL_LOCK_EN: u32 = 1 << 29;
const PLL_LOCK_STATUS: u32 = 1 << 28;
const PLL_OUT_EN: u32 = 1 << 27;
const PGSR0_IDONE: u32 = 1 << 0;
const PGSR0_DQSGE: u32 = 1 << 22;
const PGSR0_ZQERR: u32 = 1 << 20;

// ---------------------------------------------------------------------------
// DRAM type constants
// ---------------------------------------------------------------------------

const DRAM_TYPE_DDR3: u32 = 3;

// ---------------------------------------------------------------------------
// Poll timeout
// ---------------------------------------------------------------------------

/// Maximum iterations for MMIO poll loops before giving up.
const POLL_TIMEOUT: u32 = 100_000;

// ---------------------------------------------------------------------------
// AC remapping tables (from U-Boot dram_sun20i_d1.c)
// ---------------------------------------------------------------------------

/// AC remapping table entries, indexed by eFuse value.
/// Table 0 = no remapping. Tables 1-7 are die-specific.
static AC_REMAP_TABLES: [[u8; 22]; 8] = [
    // [0] = no remapping
    [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    // [1]
    [
        1, 9, 3, 7, 8, 18, 4, 13, 5, 6, 10, 2, 14, 12, 0, 0, 21, 17, 20, 19, 11, 22,
    ],
    // [2]
    [
        4, 9, 3, 7, 8, 18, 1, 13, 2, 6, 10, 5, 14, 12, 0, 0, 21, 17, 20, 19, 11, 22,
    ],
    // [3]
    [
        1, 7, 8, 12, 10, 18, 4, 13, 5, 6, 3, 2, 9, 0, 0, 0, 21, 17, 20, 19, 11, 22,
    ],
    // [4]
    [
        4, 12, 10, 7, 8, 18, 1, 13, 2, 6, 3, 5, 9, 0, 0, 0, 21, 17, 20, 19, 11, 22,
    ],
    // [5]
    [
        13, 2, 7, 9, 12, 19, 5, 1, 6, 3, 4, 8, 10, 0, 0, 0, 21, 22, 18, 17, 11, 20,
    ],
    // [6]
    [
        3, 10, 7, 13, 9, 11, 1, 2, 4, 6, 8, 5, 12, 0, 0, 0, 20, 1, 0, 21, 22, 17,
    ],
    // [7]
    [
        3, 2, 4, 7, 9, 1, 17, 12, 18, 14, 13, 8, 15, 6, 10, 5, 19, 22, 16, 21, 20, 11,
    ],
];

// ---------------------------------------------------------------------------
// Helper: nanoseconds to DRAM clock ticks
// ---------------------------------------------------------------------------

/// Convert nanoseconds to controller clock ticks (DRAM_CLK / 2 MHz).
#[inline]
fn ns_to_t(ns: u32, dram_clk: u32) -> u32 {
    let ctrl_freq = dram_clk / 2;
    (ctrl_freq * ns).div_ceil(1000)
}

// ---------------------------------------------------------------------------
// MMIO helpers (barrier-aware via fstart_mmio)
// ---------------------------------------------------------------------------

#[inline(always)]
fn read32(addr: usize) -> u32 {
    // SAFETY: addr is a valid MMIO register at a fixed hardware address
    // within the D1's memory-mapped register space.
    unsafe { fstart_mmio::read32(addr as *const u32) }
}

#[inline(always)]
fn write32(addr: usize, val: u32) {
    // SAFETY: addr is a valid MMIO register at a fixed hardware address
    // within the D1's memory-mapped register space.
    unsafe { fstart_mmio::write32(addr as *mut u32, val) }
}

/// Set bits in an MMIO register (read-modify-write).
///
/// Safe wrapper around barrier-aware `read32`/`write32`; the caller is
/// responsible for passing a valid MMIO address.
#[inline(always)]
fn setbits(addr: usize, bits: u32) {
    write32(addr, read32(addr) | bits);
}

/// Clear bits in an MMIO register (read-modify-write).
///
/// Safe wrapper around barrier-aware `read32`/`write32`; the caller is
/// responsible for passing a valid MMIO address.
#[inline(always)]
fn clrbits(addr: usize, bits: u32) {
    write32(addr, read32(addr) & !bits);
}

/// Clear and set bits in an MMIO register (read-modify-write).
///
/// Safe wrapper around barrier-aware `read32`/`write32`; the caller is
/// responsible for passing a valid MMIO address.
#[inline(always)]
fn clrsetbits(addr: usize, clear: u32, set: u32) {
    write32(addr, (read32(addr) & !clear) | set);
}

/// Read-modify-write with poll: clears `clear` bits, sets `set` bits,
/// then spins until the register reads back the written value.
/// Used for MC_WORK_MODE changes where the controller needs time to reconfigure.
///
/// Returns `true` on success, `false` on timeout.
///
/// Safe wrapper around barrier-aware `read32`/`write32`; the caller is
/// responsible for passing a valid MMIO address.
#[inline(always)]
fn clrsetbits_poll(addr: usize, clear: u32, set: u32) -> bool {
    let val = (read32(addr) & !clear) | set;
    write32(addr, val);
    poll_reg(addr, !0, val)
}

/// Poll a register until `(read32(addr) & mask) == expected`, with timeout.
/// Returns `true` on success, `false` on timeout.
#[inline(always)]
fn poll_reg(addr: usize, mask: u32, expected: u32) -> bool {
    for _ in 0..POLL_TIMEOUT {
        if read32(addr) & mask == expected {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Typed configuration for the D1/T113 DRAM controller.
///
/// Parameters from U-Boot defconfig (MangoPi MQ-R / Lichee RV defaults).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiD1DramcConfig {
    /// DRAM clock frequency in MHz (e.g., 792).
    pub dram_clk: u32,
    /// DRAM type: 2=DDR2, 3=DDR3, 6=LPDDR2, 7=LPDDR3.
    pub dram_type: u32,
    /// ZQ impedance calibration value.
    pub dram_zq: u32,
    /// ODT enable (0 or 1).
    pub dram_odt_en: u32,
    /// Mode register 1 value.
    pub dram_mr1: u32,
    /// TPR11: eye delay compensation (byte lane 0/1, DQS/DM).
    pub dram_tpr11: u32,
    /// TPR12: eye delay compensation (byte lane 0/1, DQ).
    pub dram_tpr12: u32,
    /// TPR13: configuration bitfield (auto-scan, ZQ mode, gating mode, etc.).
    pub dram_tpr13: u32,
}

/// Runtime DRAM configuration — mutated during auto-scan.
#[derive(Clone, Copy)]
struct DramConfig {
    /// Geometry: page_size[3:0], row[7:4], bank[12], rank[15:12] per rank.
    dram_para1: u32,
    /// Rank info: bits [12]=dual_rank_different, [15:12]=second_rank, [31:16]=size_mb.
    dram_para2: u32,
    /// Control bitfield.
    dram_tpr13: u32,
}

/// Allwinner D1/T113 DRAM controller driver.
pub struct SunxiD1Dramc {
    config: SunxiD1DramcConfig,
}

// SAFETY: no mutable state after init; MMIO is at fixed hardware addresses.
unsafe impl Send for SunxiD1Dramc {}
unsafe impl Sync for SunxiD1Dramc {}

// ---------------------------------------------------------------------------
// DRAM initialization implementation
// ---------------------------------------------------------------------------

impl SunxiD1Dramc {
    /// Set DRAM LDO voltage.
    fn dram_voltage_set(&self) {
        let vol: u32 = match self.config.dram_type {
            2 => 47, // DDR2: 1.8V
            3 => 25, // DDR3: 1.5V
            _ => 0,
        };
        clrsetbits(SYS_LDO_REG, 0x0020_FF00, vol << 8);
        udelay(1);
        self.sid_read_ldob_cal();
    }

    /// Adjust LDO voltage from SID eFuse calibration.
    fn sid_read_ldob_cal(&self) {
        let reg = (read32(SID_BASE + 0x1C) & 0xFF00) >> 8;
        if reg == 0 {
            return;
        }
        let adjusted = match self.config.dram_type {
            3 => {
                if reg > 0x20 {
                    reg - 0x16
                } else {
                    reg
                }
            }
            _ => 0,
        };
        if adjusted > 0 {
            clrsetbits(SYS_LDO_REG, 0xFF00, adjusted << 8);
        }
    }

    /// ZQ pad setup (internal vs external ZQ).
    fn zq_init(&self, tpr13: u32) {
        if tpr13 & (1 << 16) != 0 {
            // Internal ZQ only
            setbits(SYS_ZQ_REG, 1 << 8);
            write32(SYS_ZQ_REG + 8, 0);
            udelay(10);
        } else {
            clrbits(SYS_ZQ_REG, 0x3);
            // Always writes 0 here (bit 16 is clear in this branch).
            // Matches U-Boot: writel(config.dram_tpr13 & BIT(16), 0x7010254).
            write32(SYS_CFG_250, tpr13 & (1 << 16));
            udelay(10);
            clrsetbits(SYS_ZQ_REG, 0x108, 1 << 1);
            udelay(10);
            setbits(SYS_ZQ_REG, 1 << 0);
            udelay(20);
        }
    }

    /// Set PLL_DDR0 clock.
    ///
    /// PLL_DDR = 24MHz * n, DRAM controller = PLL_DDR / 2.
    fn ccu_set_pll_ddr_clk(&self, config: &DramConfig) -> u32 {
        let clk = if config.dram_tpr13 & (1 << 6) != 0 {
            0 // tpr9 override — not used in typical configs
        } else {
            self.config.dram_clk
        };

        let n = (clk * 2) / 24;

        let mut val = read32(CCU_PLL_DDR0_CTRL);
        val &= !0x0007_FF03; // clear dividers
        val |= (n - 1) << 8; // set N factor
        val |= PLL_EN | PLL_LDO_EN; // enable PLL + LDO
        write32(CCU_PLL_DDR0_CTRL, val | PLL_LOCK_EN); // + lock enable

        // Wait for PLL lock
        if !poll_reg(CCU_PLL_DDR0_CTRL, PLL_LOCK_STATUS, PLL_LOCK_STATUS) {
            warn!("D1 DRAMC: PLL_DDR0 lock timeout");
        }
        udelay(20);

        // Enable PLL output
        setbits(CCU_PLL_CPU_CTRL, PLL_OUT_EN);

        // Configure DRAM clock: source=DDR PLL, N=1, M=1, gate=on
        let mut val = read32(CCU_DRAM_CLK);
        val &= !0x0300_0303;
        val |= PLL_EN;
        write32(CCU_DRAM_CLK, val);

        n * 24
    }

    /// Disable all DRAM masters (MBUS).
    fn dram_disable_all_master(&self) {
        write32(MAER0, 0);
        write32(MAER1, 0);
        write32(MAER2, 0);
    }

    /// Enable all DRAM masters (MBUS).
    fn dram_enable_all_master(&self) {
        write32(MAER0, 0xFFFF_FFFF);
        write32(MAER1, 0xFF);
        write32(MAER2, 0xFFFF);
    }

    /// System-level clock and reset init for DRAM.
    fn mctl_sys_init(&self, config: &DramConfig) {
        // Assert MBUS reset
        clrbits(CCU_MBUS_CLK, 1 << 30);

        // Turn off DRAM clock gate, assert DRAM reset
        clrbits(CCU_DRAM_BGR, 0x0001_0001);
        clrsetbits(CCU_DRAM_CLK, PLL_EN | PLL_LDO_EN, PLL_OUT_EN);
        udelay(10);

        // Set PLL_DDR0 clock
        self.ccu_set_pll_ddr_clk(config);
        udelay(100);
        self.dram_disable_all_master();

        // Release DRAM reset
        setbits(CCU_DRAM_BGR, 1 << 16);

        // Release MBUS reset, enable DRAM clock source
        setbits(CCU_MBUS_CLK, 1 << 30);
        setbits(CCU_DRAM_CLK, PLL_LDO_EN);
        udelay(5);

        // Turn on DRAM clock gate
        setbits(CCU_DRAM_BGR, 1 << 0);

        // Turn DRAM clock gate on, trigger update
        setbits(CCU_DRAM_CLK, PLL_EN | PLL_OUT_EN);
        udelay(5);

        // mCTL clock enable
        write32(PHY_CLK_EN, 0x8000);
        udelay(10);
    }

    /// Set Vref and ZQ PHY configuration.
    fn mctl_vrefzq_init(&self, config: &DramConfig) {
        if config.dram_tpr13 & (1 << 17) != 0 {
            return;
        }
        // IOCVR0: load from tpr5 (use default 0x48484848 for DDR3)
        clrsetbits(IOCVR0, 0x7F7F_7F7F, 0x4848_4848);

        // IOCVR1: load from tpr6 low 7 bits
        if config.dram_tpr13 & (1 << 16) == 0 {
            clrsetbits(IOCVR1, 0x7F, 0x48);
        }
    }

    /// Configure DRAM type, width, rank, bank, row, and column.
    fn mctl_com_init(&self, config: &DramConfig) {
        clrsetbits(MC_R1, 0x3F00, 0x2000);

        let mut val = read32(MC_WORK_MODE) & !0x00FFF000;
        val |= (self.config.dram_type & 0x7) << 16;
        val |= ((!config.dram_para2) & 0x1) << 12; // DQ width
        val |= 1 << 22;

        // 1T/2T command rate
        if self.config.dram_type == 6 || self.config.dram_type == 7 {
            val |= 1 << 19; // LPDDR must use 1T
        } else if config.dram_tpr13 & (1 << 5) != 0 {
            val |= 1 << 19;
        }
        write32(MC_WORK_MODE, val);

        // Init rank/bank/row/col for each rank
        let width = if (config.dram_para2 & (1 << 8)) != 0 && (config.dram_para2 & 0xF000) != 0x1000
        {
            32
        } else {
            16
        };

        let mut ptr = MC_WORK_MODE;
        let mut i = 0u32;
        while i < width {
            let mut v = read32(ptr) & 0xFFFF_F000;
            v |= (config.dram_para2 >> 12) & 0x3; // rank
            v |= ((config.dram_para1 >> (i + 12)) << 2) & 0x4; // bank - 2
            v |= (((config.dram_para1 >> (i + 4)) - 1) << 4) & 0xFF; // row - 1

            let page = (config.dram_para1 >> i) & 0xF;
            v |= match page {
                8 => 0xA00,
                4 => 0x900,
                2 => 0x800,
                1 => 0x700,
                _ => 0x600,
            };
            write32(ptr, v);
            ptr += 4;
            i += 16;
        }

        // ODTMAP: dual-rank → 0x303, single-rank → 0x201
        let odt = if read32(MC_WORK_MODE) & 0x1 != 0 {
            0x303
        } else {
            0x201
        };
        write32(ODTMAP, odt);

        // Half DQ: clear DX1GCR0
        if config.dram_para2 & 1 != 0 {
            write32(DX1GCR0, 0);
        }
    }

    /// Apply address/command remapping from SID eFuse.
    fn mctl_phy_ac_remapping(&self, config: &DramConfig) {
        if self.config.dram_type != 2 && self.config.dram_type != DRAM_TYPE_DDR3 {
            return;
        }

        let fuse = (read32(SID_BASE + 0x228) & 0xF00) >> 8;

        let cfg = if self.config.dram_type == 2 {
            // DDR2
            if fuse == 15 {
                return;
            }
            &AC_REMAP_TABLES[6]
        } else {
            // DDR3
            if config.dram_tpr13 & 0xC0000 != 0 {
                &AC_REMAP_TABLES[7]
            } else {
                match fuse {
                    8 => &AC_REMAP_TABLES[2],
                    9 => &AC_REMAP_TABLES[3],
                    10 => &AC_REMAP_TABLES[5],
                    11 => &AC_REMAP_TABLES[4],
                    13 | 14 => &AC_REMAP_TABLES[0],
                    _ => &AC_REMAP_TABLES[1], // default including fuse=12
                }
            }
        };

        let val = ((cfg[4] as u32) << 25)
            | ((cfg[3] as u32) << 20)
            | ((cfg[2] as u32) << 15)
            | ((cfg[1] as u32) << 10)
            | ((cfg[0] as u32) << 5);
        write32(AC_REMAP0, val);

        let val = ((cfg[10] as u32) << 25)
            | ((cfg[9] as u32) << 20)
            | ((cfg[8] as u32) << 15)
            | ((cfg[7] as u32) << 10)
            | ((cfg[6] as u32) << 5)
            | (cfg[5] as u32);
        write32(AC_REMAP1, val);

        let val = ((cfg[15] as u32) << 20)
            | ((cfg[14] as u32) << 15)
            | ((cfg[13] as u32) << 10)
            | ((cfg[12] as u32) << 5)
            | (cfg[11] as u32);
        write32(AC_REMAP2, val);

        let val = ((cfg[21] as u32) << 25)
            | ((cfg[20] as u32) << 20)
            | ((cfg[19] as u32) << 15)
            | ((cfg[18] as u32) << 10)
            | ((cfg[17] as u32) << 5)
            | (cfg[16] as u32);
        write32(AC_REMAP3, val);

        // Enable remapping (set bit 0)
        let val = ((cfg[4] as u32) << 25)
            | ((cfg[3] as u32) << 20)
            | ((cfg[2] as u32) << 15)
            | ((cfg[1] as u32) << 10)
            | ((cfg[0] as u32) << 5)
            | 1;
        write32(AC_REMAP0, val);
    }

    /// Program DDR3 timing parameters.
    fn mctl_set_timing_params(&self) {
        let clk = self.config.dram_clk;

        // DDR3 timing for clk <= 800 MHz
        let trfc = ns_to_t(350, clk);
        let trefi = ns_to_t(7800, clk) / 32 + 1;
        let twtr = ns_to_t(8, clk) + 2;
        let trrd = core::cmp::max(ns_to_t(10, clk), 2);
        let txp = core::cmp::max(ns_to_t(10, clk), 2);

        let (tfaw, trcd, trp, trc, tras, mr0, mr2, tcl, wr_latency, tcwl, t_rdata_en) =
            if clk <= 800 {
                (
                    ns_to_t(50, clk),
                    ns_to_t(15, clk),
                    ns_to_t(15, clk),
                    ns_to_t(53, clk),
                    ns_to_t(38, clk),
                    0x1C70u32,
                    0x18u32,
                    6u32,
                    2u32,
                    4u32,
                    4u32,
                )
            } else {
                (
                    ns_to_t(35, clk),
                    ns_to_t(14, clk),
                    ns_to_t(14, clk),
                    ns_to_t(48, clk),
                    ns_to_t(34, clk),
                    0x1E14u32,
                    0x20u32,
                    7u32,
                    3u32,
                    5u32,
                    5u32,
                )
            };

        let trasmax = clk / 30;
        let twtp = tcwl + 2 + twtr;
        let twr2rd = tcwl + twtr;
        let trtp = 4u32;
        let tccd = 2u32;
        let tcke = 3u32;
        let tcksrx = 5u32;
        let tckesr = 4u32;
        let tmod = 12u32;
        let tmrd = 4u32;
        let tmrw = 0u32;

        let trd2wr = if clk < 912 { 5u32 } else { 6u32 };

        let tdinit0 = 500 * clk + 1;
        let tdinit1 = 360 * clk / 1000 + 1;
        let tdinit2 = 200 * clk + 1;
        let tdinit3 = clk + 1;

        let mr1 = self.config.dram_mr1;

        // Write mode registers
        write32(MR0, mr0);
        write32(MR1, mr1);
        write32(MR2, mr2);
        write32(MR3, 0);
        write32(DTCR, (self.config.dram_odt_en >> 4) & 0x3);

        // DRAMTMG0-5
        write32(
            DRAMTMG0,
            (twtp << 24) | (tfaw << 16) | (trasmax << 8) | tras,
        );
        write32(DRAMTMG1, (txp << 16) | (trtp << 8) | trc);
        write32(
            DRAMTMG2,
            (tcwl << 24) | (tcl << 16) | (trd2wr << 8) | twr2rd,
        );
        write32(DRAMTMG3, (tmrw << 16) | (tmrd << 12) | tmod);
        write32(DRAMTMG4, (trcd << 24) | (tccd << 16) | (trrd << 8) | trp);
        write32(
            DRAMTMG5,
            (tcksrx << 24) | (tcksrx << 16) | (tckesr << 8) | tcke,
        );

        // Dual rank timing
        let drk = if clk < 800 {
            0xF000_6610u32
        } else {
            0xF000_7610u32
        };
        clrsetbits(DRAMTMG8, 0xF000_FFFF, drk);

        // PITMG0: phy interface timing
        write32(
            PITMG0,
            (0x2 << 24) | (t_rdata_en << 16) | (1 << 8) | wr_latency,
        );

        // PTR3 / PTR4: initialization timers
        write32(PTR3, tdinit0 | (tdinit1 << 20));
        write32(PTR4, tdinit2 | (tdinit3 << 20));

        // Refresh timing
        write32(RFSHTMG, (trefi << 16) | trfc);
        write32(RFSHTMG1, (trefi << 15) & 0x0FFF_0000);
    }

    /// Eye delay compensation from tpr10/tpr11/tpr12.
    fn eye_delay_compensation(&self) {
        let tpr11 = self.config.dram_tpr11;
        let tpr12 = self.config.dram_tpr12;

        // DATn0IOCR, n = 0..7
        let delay0 = ((tpr11 & 0xF) << 9) | ((tpr12 & 0xF) << 1);
        let mut ptr = DX0IOCR_BASE;
        while ptr < DX0DQS0 {
            setbits(ptr, delay0);
            ptr += 4;
        }

        // DATn1IOCR, n = 0..7
        let delay1 = ((tpr11 & 0xF0) << 5) | ((tpr12 & 0xF0) >> 3);
        ptr = DX1IOCR_BASE;
        while ptr < DX1DQS0 {
            setbits(ptr, delay1);
            ptr += 4;
        }

        // PGCR0: assert AC loopback FIFO reset
        clrbits(PGCR0, 0x0400_0000);

        let dqs0_delay = ((tpr11 & 0xF0000) >> 7) | ((tpr12 & 0xF0000) >> 15);
        setbits(DX0DQS0, dqs0_delay);
        setbits(DX0DQS1, dqs0_delay);

        let dqs1_delay = ((tpr11 & 0xF00000) >> 11) | ((tpr12 & 0xF00000) >> 19);
        setbits(DX1DQS0, dqs1_delay);
        setbits(DX1DQS1, dqs1_delay);

        setbits(DX0DM, (tpr11 & 0xF0000) << 9);
        setbits(DX1DM, (tpr11 & 0xF00000) << 5);

        // PGCR0: release AC loopback FIFO reset
        setbits(PGCR0, 1 << 26);
        udelay(1);
    }

    /// PHY initialization and training.
    fn mctl_channel_init(&self, config: &DramConfig) -> bool {
        let dqs_gating_mode = (config.dram_tpr13 & 0xC) >> 2;

        // Set DDR clock to half of CPU clock
        clrsetbits(MC_CLKDIV, 0xFFF, (self.config.dram_clk / 2) - 1);

        // MRCTRL0
        clrsetbits(DSGCR, 0xF00, 0x300);

        let dx_val = if self.config.dram_odt_en != 0 {
            0u32
        } else {
            1 << 5
        };

        // DX0GCR0
        if self.config.dram_clk > 672 {
            clrsetbits(DX0GCR0, 0xF63E, dx_val);
        } else {
            clrsetbits(DX0GCR0, 0xF03E, dx_val);
        }

        // DX1GCR0
        if self.config.dram_clk > 672 {
            setbits(DX0GCR0, 0x400);
            clrsetbits(DX1GCR0, 0xF63E, dx_val);
        } else {
            clrsetbits(DX1GCR0, 0xF03E, dx_val);
        }

        setbits(ACIOCR0, 1 << 1);

        self.eye_delay_compensation();

        // DQS gating mode setup
        if dqs_gating_mode == 1 {
            clrsetbits(DSGCR, 0xC0, 0);
            clrbits(DXCCR, 0x107);
        } else if dqs_gating_mode == 2 {
            clrsetbits(DSGCR, 0xC0, 0x80);
            let gating_val = ((config.dram_tpr13 >> 16) & 0x1F).wrapping_sub(2) | 0x100;
            clrsetbits(DXCCR, 0x107, gating_val);
            clrsetbits(DXGTR0, 1 << 31, 1 << 27);
        } else {
            clrbits(DSGCR, 0x40);
            udelay(10);
            setbits(DSGCR, 0xC0);
        }

        // Set controller configuration
        clrsetbits(
            DTCR0,
            0x0FFF_FFFF,
            if config.dram_para2 & (1 << 12) != 0 {
                0x0300_0001
            } else {
                0x0100_0007
            },
        );

        if read32(RTC_STATUS) & (1 << 16) != 0 {
            clrbits(SYS_CFG_250_2, 0x2);
            udelay(10);
        }

        // ZQ config
        clrsetbits(
            ZQ0CR,
            0x03FF_FFFF,
            (self.config.dram_zq & 0x00FF_FFFF) | (1 << 25),
        );

        // PHY initialization sequence
        if dqs_gating_mode == 1 {
            // Gating mode: two-phase init
            write32(PIR, 0x53); // PHY reset + PLL init + z-cal + GO
            if !poll_reg(PGSR0, PGSR0_IDONE, PGSR0_IDONE) {
                warn!("D1 DRAMC: PHY init phase-1 timeout");
            }
            udelay(10);

            if self.config.dram_type == DRAM_TYPE_DDR3 {
                write32(PIR, 0x5A0); // DQS gating + DRAM init + d-cal + DRAM reset
            } else {
                write32(PIR, 0x520);
            }
        } else if read32(RTC_STATUS) & (1 << 16) == 0 {
            // Normal mode
            if self.config.dram_type == DRAM_TYPE_DDR3 {
                write32(PIR, 0x1F2); // Full init + DRAM reset
            } else {
                write32(PIR, 0x172);
            }
        } else {
            write32(PIR, 0x62); // PHY reset + d-cal + z-cal
        }

        // GO
        setbits(PIR, 0x1);
        udelay(10);

        // Wait for IDONE
        if !poll_reg(PGSR0, PGSR0_IDONE, PGSR0_IDONE) {
            warn!("D1 DRAMC: PHY init IDONE timeout");
        }

        if read32(RTC_STATUS) & (1 << 16) != 0 {
            clrsetbits(DTCR1, 0x0600_0000, 0x0400_0000);
            udelay(10);
            setbits(PGCR1, 0x1);
            if !poll_reg(STATR, 0x7, 0x3) {
                warn!("D1 DRAMC: STATR wait-for-3 timeout");
            }
            clrbits(SYS_CFG_250_2, 0x1);
            udelay(10);
            clrbits(PGCR1, 0x1);
            if !poll_reg(STATR, 0x7, 0x1) {
                warn!("D1 DRAMC: STATR wait-for-1 timeout");
            }
            udelay(15);

            if dqs_gating_mode == 1 {
                clrbits(DSGCR, 0xC0);
                clrsetbits(DTCR1, 0x0600_0000, 0x0200_0000);
                udelay(1);
                write32(PIR, 0x401);
                if !poll_reg(PGSR0, PGSR0_IDONE, PGSR0_IDONE) {
                    warn!("D1 DRAMC: PHY gating-mode re-init timeout");
                }
            }
        }

        // Check for ZQ calibration error
        if read32(PGSR0) & PGSR0_ZQERR != 0 {
            return false;
        }

        // Wait for controller status 'normal'
        if !poll_reg(STATR, 0x1, 0x1) {
            warn!("D1 DRAMC: controller normal-status timeout");
        }

        // Refresh sequence
        setbits(RFSHCTL3, 1 << 31);
        udelay(10);
        clrbits(RFSHCTL3, 1 << 31);
        udelay(10);
        setbits(MC_CCCR, 1 << 31);
        udelay(10);

        clrbits(DTCR1, 0x0600_0000);

        if dqs_gating_mode == 1 {
            clrsetbits(DXGTR0, 0xC0, 0x40);
        }

        true
    }

    /// Full DRAM controller init: clocks -> vref -> config -> remap -> timing -> PHY.
    fn mctl_core_init(&self, config: &DramConfig) -> bool {
        self.mctl_sys_init(config);
        self.mctl_vrefzq_init(config);
        self.mctl_com_init(config);
        self.mctl_phy_ac_remapping(config);
        self.mctl_set_timing_params();
        self.mctl_channel_init(config)
    }

    /// Read DRAM size from controller registers.
    ///
    /// Matches oreboot's `dramc_get_dram_size`: reads MC_WORK_MODE and
    /// computes `1 << (page_field + row_field + bank_field - 14)` per rank.
    ///
    /// Register fields:
    /// - bits[11:8]: page (column byte-address bits - 3)
    /// - bits[7:4]:  row  (row address bits - 1)
    /// - bits[3:2]:  bank (bank address bits - 2)
    /// - bits[1:0]:  rank (0 = single, non-zero = dual)
    fn dramc_get_dram_size(&self) -> u32 {
        let val = read32(MC_WORK_MODE);
        let page = (val >> 8) & 0xF;
        let row = (val >> 4) & 0xF;
        let bank = (val >> 2) & 0x3;
        let temp = page + row + bank;
        // 14 = (3 + 1 + 2) field offsets + 8 remainder to reach 2^20 = 1 MB
        if temp < 14 {
            return 0;
        }
        let size0 = 1u32 << (temp - 14);

        let rank = val & 0x3;
        if rank == 0 {
            size0
        } else {
            // Dual rank — read MC_WORK_MODE2 for rank 1 geometry.
            let val2 = read32(MC_WORK_MODE2);
            let r1_rank = val2 & 0x3;
            if r1_rank == 0 {
                // Rank 1 has same geometry as rank 0.
                2 * size0
            } else {
                let p2 = (val2 >> 8) & 0xF;
                let r2 = (val2 >> 4) & 0xF;
                let b2 = (val2 >> 2) & 0x3;
                let t2 = p2 + r2 + b2;
                if t2 < 14 {
                    size0
                } else {
                    size0 + (1u32 << (t2 - 14))
                }
            }
        }
    }

    /// Check whether address `DRAM_BASE + j*4` and `DRAM_BASE + j*4 + (1 << bit_offset)`
    /// alias (i.e. read the same value), for all 64 test slots.
    ///
    /// Returns `true` if the addresses alias (all 64 slots match the base pattern),
    /// meaning the address bit at `bit_offset` is not decoded by the DRAM controller.
    fn scan_address_alias(&self, bit_offset: usize) -> bool {
        for j in 0..64usize {
            let ptr = DRAM_BASE + j * 4;
            let chk = ptr + (1 << bit_offset);
            let expected = if j & 1 != 0 {
                ptr as u32
            } else {
                !(ptr as u32)
            };
            if read32(chk) != expected {
                return false;
            }
        }
        true
    }

    /// Auto-detect DRAM rank count and bus width.
    ///
    /// Sets up a minimal probe configuration, runs `mctl_core_init`, and reads
    /// back DQS gate error / DX lane status to determine the DRAM topology.
    ///
    /// Returns `true` on success (config updated), `false` on init failure.
    fn auto_scan_dram_config(&self, config: &mut DramConfig) -> bool {
        // Set probe config for rank/width detection
        let saved_para1 = config.dram_para1;
        let saved_tpr13 = config.dram_tpr13;
        config.dram_para1 = 0x00B0_00B0;
        config.dram_para2 = (config.dram_para2 & !0xF) | (1 << 12);
        config.dram_tpr13 = (config.dram_tpr13 & !0x8) | (1 << 2) | (1 << 0);

        if !self.mctl_core_init(config) {
            config.dram_tpr13 = saved_tpr13;
            config.dram_para1 = saved_para1;
            return false;
        }

        // Check for DQS gate errors and detect rank/width
        if read32(PGSR0) & PGSR0_DQSGE == 0 {
            config.dram_para2 = (config.dram_para2 & !0xF) | (1 << 12);
        } else {
            let dx0 = (read32(DX0GSR0) & 0x300_0000) >> 24;
            if dx0 == 0 {
                config.dram_para2 = (config.dram_para2 & !0xF) | 0x1001;
            } else if dx0 == 2 {
                let dx1 = (read32(DX1GSR0) & 0x300_0000) >> 24;
                if dx1 == 2 {
                    config.dram_para2 &= !0xF00F;
                } else {
                    config.dram_para2 = (config.dram_para2 & !0xF00F) | 1;
                }
            } else {
                config.dram_tpr13 = saved_tpr13;
                config.dram_para1 = saved_para1;
                return false;
            }
        }

        config.dram_tpr13 = saved_tpr13;
        config.dram_para1 = saved_para1;
        true
    }

    /// Auto-detect DRAM row count, bank count, and page (column) size.
    ///
    /// Writes a test pattern at `DRAM_BASE`, then scans address lines to detect
    /// aliasing (wrap-around). Updates `config.dram_para1` with detected geometry.
    fn auto_scan_dram_size(&self, config: &mut DramConfig) {
        // Write test pattern at DRAM_BASE for address aliasing detection.
        // Pattern: even index = !ptr, odd index = ptr (matches oreboot).
        // The scan loops below read from higher addresses and compare against
        // this base pattern — if they match, the address wraps (alias).
        for i in 0..64usize {
            let ptr = DRAM_BASE + i * 4;
            let val = if i & 1 != 0 {
                ptr as u32
            } else {
                !(ptr as u32)
            };
            write32(ptr, val);
        }

        let maxrank = if config.dram_para2 & 0xF000 != 0 {
            2u32
        } else {
            1
        };

        for _rank in 0..maxrank {
            // Set row mode for detection (max 16 row address lines).
            // Poll until the controller acknowledges the mode change.
            if !clrsetbits_poll(MC_WORK_MODE, 0xF0C, 0x6F0) {
                warn!("D1 DRAMC: MC_WORK_MODE row-mode poll timeout");
            }

            // Scan row address lines: check if address bit (i+11) wraps
            let mut row_bits = 16u32;
            for i in 11u32..17 {
                if self.scan_address_alias((i + 11) as usize) {
                    row_bits = i;
                    break;
                }
            }

            // Store detected row geometry
            config.dram_para1 = (config.dram_para1 & !0xFF0) | (row_bits << 4);

            // Set bank mode for detection.
            if !clrsetbits_poll(MC_WORK_MODE, 0xFFC, 0x6A4) {
                warn!("D1 DRAMC: MC_WORK_MODE bank-mode poll timeout");
            }

            // Test if address bit A22 is BA2 or an alias (mirror)
            let banks_8 = !self.scan_address_alias(22);
            if banks_8 {
                config.dram_para1 |= 1 << 12;
            } else {
                config.dram_para1 &= !(1 << 12);
            }

            // Set page mode for detection.
            if !clrsetbits_poll(MC_WORK_MODE, 0xFFC, 0xAA0) {
                warn!("D1 DRAMC: MC_WORK_MODE page-mode poll timeout");
            }

            // Scan page (column) address lines
            let mut page = 0u32;
            for i in 9u32..14 {
                if self.scan_address_alias(i as usize) {
                    page = if i == 9 { 0 } else { 1 << (i - 10) };
                    break;
                }
            }
            config.dram_para1 = (config.dram_para1 & !0xF) | page;
        }
    }

    /// Post-initialization configuration: power management, PGCR0 tuning,
    /// ZQ trigger, VT/DSGCR settings, and master enable.
    fn post_init_config(&self, config: &DramConfig, mem_size: u32) {
        let _ = mem_size; // Currently unused, reserved for future size-dependent config.

        // Post-init: auto self-refresh control
        if config.dram_tpr13 & (1 << 30) != 0 {
            write32(PWRCTL, 0x1000_0200);
            write32(PWRTMG, 0x40A);
            setbits(PGCR1, 1);
        } else {
            clrbits(PWRCTL, 0xFFFF);
            clrbits(PGCR1, 0x1);
        }

        // PGCR0 tuning
        if config.dram_tpr13 & (1 << 9) != 0 {
            clrsetbits(PGCR0, 0xF000, 0x5000);
        } else if self.config.dram_type != 6 {
            clrbits(PGCR0, 0xF000);
        }

        setbits(ZQ0CR, 1 << 31);

        if config.dram_tpr13 & (1 << 8) != 0 {
            let v = read32(ZQ0CR) | 0x300;
            write32(VTFCR, v);
        }

        if config.dram_tpr13 & (1 << 16) != 0 {
            clrbits(DSGCR, 1 << 13);
        } else {
            setbits(DSGCR, 1 << 13);
        }

        self.dram_enable_all_master();
    }

    /// Top-level DRAM initialization entry point.
    fn init_dram(&self) -> u32 {
        let mut config = DramConfig {
            dram_para1: 0x0000_10D2,
            dram_para2: 0,
            dram_tpr13: self.config.dram_tpr13,
        };

        // ZQ setup
        self.zq_init(config.dram_tpr13);

        // Set DRAM voltage
        self.dram_voltage_set();

        // Auto-scan if tpr13 bit 0 is clear
        if config.dram_tpr13 & 1 == 0 {
            // Auto scan rank/width
            if config.dram_tpr13 & (1 << 14) == 0 && !self.auto_scan_dram_config(&mut config) {
                return 0;
            }

            // Auto scan DRAM size
            if !self.mctl_core_init(&config) {
                return 0;
            }

            self.auto_scan_dram_size(&mut config);
        }

        // Final DRAM init with detected/configured geometry
        if !self.mctl_core_init(&config) {
            return 0;
        }

        // Get DRAM size
        let mem_size = if config.dram_para2 & (1 << 31) != 0 {
            (config.dram_para2 >> 16) & !0x8000
        } else {
            let sz = self.dramc_get_dram_size();
            config.dram_para2 = (config.dram_para2 & 0xFFFF) | (sz << 16);
            sz
        };

        self.post_init_config(&config, mem_size);

        mem_size
    }
}

// ---------------------------------------------------------------------------
// Device trait implementation
// ---------------------------------------------------------------------------

impl Device for SunxiD1Dramc {
    const NAME: &'static str = "sunxi-d1-dramc";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun20i-d1-mbus"];
    type Config = SunxiD1DramcConfig;

    fn new(config: &SunxiD1DramcConfig) -> Result<Self, DeviceError> {
        Ok(Self { config: *config })
    }

    fn init(&self) -> Result<(), DeviceError> {
        let mem_mb = self.init_dram();
        if mem_mb == 0 {
            return Err(DeviceError::InitFailed);
        }
        info!("DRAM: {} MB (DDR3, {} MHz)", mem_mb, self.config.dram_clk);
        Ok(())
    }
}

impl MemoryController for SunxiD1Dramc {
    fn detected_size_bytes(&self) -> u64 {
        self.dramc_get_dram_size() as u64 * 1024 * 1024
    }
}
