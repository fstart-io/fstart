//! Allwinner H3/H2+ (sun8i) DesignWare DRAM controller driver.
//!
//! Performs full DDR3 DRAM initialization: PLL5 setup, DesignWare PHY
//! training, ZQ calibration (with H3-specific quirk), MBUS master
//! priority configuration, and size auto-detection.
//!
//! Ported from u-boot `arch/arm/mach-sunxi/dram_sunxi_dw.c` (H3 paths).
//! Timing data from u-boot `arch/arm/mach-sunxi/dram_timings/ddr3_1333.c`.
//!
//! DRAM COM register block: `0x01C6_2000`
//! DRAM CTL register block: `0x01C6_3000`
//! CCU (for PLL5/MBUS/gates): `0x01C2_0000`
//! DRAM physical base: `0x4000_0000`

#![no_std]
#![allow(clippy::modulo_one)] // tock-registers alignment test
#![allow(clippy::identity_op)] // Bit-field shifts like (x << 0) document register layout
#![allow(clippy::too_many_arguments)] // mbus_configure_port mirrors U-Boot signature
#![allow(clippy::unnecessary_cast)] // Explicit casts clarify register-width intent
#![allow(clippy::needless_range_loop)] // Index-based loops match U-Boot's C style

use core::cell::Cell;

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_mmio::MmioReadWrite;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{MemoryController, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiH3CcuRegs, H3_DRAM_CLK, H3_PLL5_CFG};

use fstart_arch::udelay;

// ===================================================================
// COM register bitfields
// ===================================================================

register_bitfields! [u32,
    /// MCTL_CR — control register fields.
    ///
    /// U-Boot: MCTL_CR_BL8 = 0x4 << 20, MCTL_CR_DDR3 = 0x3 << 16,
    /// MCTL_CR_2T = 0x0 << 19, MCTL_CR_1T = 0x1 << 19.
    pub MCTL_CR [
        /// Rank: 0 = single, 1 = dual.
        RANK OFFSET(0) NUMBITS(1) [],
        /// Bank bits: 0 = 4 banks, 1 = 8 banks.
        BANK_BITS OFFSET(2) NUMBITS(1) [],
        /// Row bits encoding (raw = row_bits - 1).
        ROW_BITS OFFSET(4) NUMBITS(4) [],
        /// Page size encoding: fls(page_size) - 4.
        PAGE_SIZE OFFSET(8) NUMBITS(4) [],
        /// Bus width: 0 = half, 1 = full.
        BUS_FULL_WIDTH OFFSET(12) NUMBITS(1) [],
        /// Sequential vs interleaved: 0 = interleaved, 1 = sequential.
        SEQUENTIAL OFFSET(15) NUMBITS(1) [],
        /// DRAM type: 0x3 = DDR3.
        DRAM_TYPE OFFSET(16) NUMBITS(3) [],
        /// Command rate: 0 = 2T, 1 = 1T.
        COMMAND_RATE OFFSET(19) NUMBITS(1) [],
        /// Burst length encoding.
        BURST_LEN OFFSET(20) NUMBITS(3) []
    ]
];

// ===================================================================
// COM register block (0x01C6_2000)
// ===================================================================

register_structs! {
    /// DRAM COM register block.
    ///
    /// Layout from U-Boot `struct sunxi_mctl_com_reg`.
    pub SunxiH3DramComRegs {
        /// Control register.
        (0x000 => pub cr: MmioReadWrite<u32, MCTL_CR::Register>),
        /// Rank 1 control register (R40 only, unused on H3).
        (0x004 => pub cr_r1: MmioReadWrite<u32>),
        (0x008 => _res0: [u8; 0x04]),
        /// Timing mode register.
        (0x00C => pub tmr: MmioReadWrite<u32>),
        /// Master configuration registers: mcr[port][0] and mcr[port][1].
        /// 16 ports × 2 registers = 32 u32 values at 0x10..0x8F.
        (0x010 => _mcr: [u8; 0x80]),
        /// Bandwidth control register.
        (0x090 => pub bwcr: MmioReadWrite<u32>),
        /// Master access enable register.
        (0x094 => pub maer: MmioReadWrite<u32>),
        /// Master priority register.
        (0x098 => pub mapr: MmioReadWrite<u32>),
        (0x09C => _res1: [u8; 0x34]),
        /// Credit control register.
        (0x0D0 => pub cccr: MmioReadWrite<u32>),
        (0x0D4 => _res2: [u8; 0x72C]),
        /// Protection register.
        (0x800 => pub protect: MmioReadWrite<u32>),
        (0x804 => @END),
    }
}

impl SunxiH3DramComRegs {
    /// Write master configuration register 0 for a port.
    ///
    /// `mcr[port][0]` is at offset `0x10 + port * 8`.
    fn write_mcr0(&self, port: usize, val: u32) {
        let addr = (self as *const Self as usize) + 0x10 + port * 8;
        // SAFETY: within the COM register block, single-threaded boot.
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) };
    }

    /// Write master configuration register 1 for a port.
    ///
    /// `mcr[port][1]` is at offset `0x14 + port * 8`.
    fn write_mcr1(&self, port: usize, val: u32) {
        let addr = (self as *const Self as usize) + 0x14 + port * 8;
        // SAFETY: within the COM register block, single-threaded boot.
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) };
    }
}

// ===================================================================
// CTL register block (0x01C6_3000) — controller + PHY
// ===================================================================

register_structs! {
    /// DRAM CTL register block.
    ///
    /// Layout from U-Boot `struct sunxi_mctl_ctl_reg`.
    pub SunxiH3DramCtlRegs {
        /// PHY initialization register.
        (0x000 => pub pir: MmioReadWrite<u32>),
        /// Power control.
        (0x004 => pub pwrctl: MmioReadWrite<u32>),
        /// Mode register control.
        (0x008 => pub mrctrl: MmioReadWrite<u32>),
        /// Clock enable.
        (0x00C => pub clken: MmioReadWrite<u32>),
        /// PHY general status registers [0..1].
        (0x010 => pub pgsr0: MmioReadWrite<u32>),
        (0x014 => pub pgsr1: MmioReadWrite<u32>),
        /// Controller status register.
        (0x018 => pub statr: MmioReadWrite<u32>),
        (0x01C => _res1: [u8; 0x10]),
        /// LPDDR3 mode register 11.
        (0x02C => pub lp3mr11: MmioReadWrite<u32>),
        /// Mode registers [0..3].
        (0x030 => pub mr0: MmioReadWrite<u32>),
        (0x034 => pub mr1: MmioReadWrite<u32>),
        (0x038 => pub mr2: MmioReadWrite<u32>),
        (0x03C => pub mr3: MmioReadWrite<u32>),
        /// PLL general configuration.
        (0x040 => pub pllgcr: MmioReadWrite<u32>),
        /// PHY timing registers [0..4]. ptr[0] at 0x44.
        (0x044 => pub ptr0: MmioReadWrite<u32>),
        (0x048 => pub ptr1: MmioReadWrite<u32>),
        (0x04C => pub ptr2: MmioReadWrite<u32>),
        (0x050 => pub ptr3: MmioReadWrite<u32>),
        (0x054 => pub ptr4: MmioReadWrite<u32>),
        /// DRAM timing registers [0..8].
        (0x058 => pub dramtmg0: MmioReadWrite<u32>),
        (0x05C => pub dramtmg1: MmioReadWrite<u32>),
        (0x060 => pub dramtmg2: MmioReadWrite<u32>),
        (0x064 => pub dramtmg3: MmioReadWrite<u32>),
        (0x068 => pub dramtmg4: MmioReadWrite<u32>),
        (0x06C => pub dramtmg5: MmioReadWrite<u32>),
        (0x070 => pub dramtmg6: MmioReadWrite<u32>),
        (0x074 => pub dramtmg7: MmioReadWrite<u32>),
        (0x078 => pub dramtmg8: MmioReadWrite<u32>),
        /// ODT configuration.
        (0x07C => pub odtcfg: MmioReadWrite<u32>),
        /// PHY interface timing registers [0..1].
        (0x080 => pub pitmg0: MmioReadWrite<u32>),
        (0x084 => pub pitmg1: MmioReadWrite<u32>),
        (0x088 => _res2: [u8; 0x04]),
        /// Refresh control register 0.
        (0x08C => pub rfshctl0: MmioReadWrite<u32>),
        /// Refresh timing.
        (0x090 => pub rfshtmg: MmioReadWrite<u32>),
        (0x094 => _res3: [u8; 0x24]),
        /// VTF control register (unused on H3).
        (0x0B8 => pub vtfcr: MmioReadWrite<u32>),
        /// DQS gate mode register.
        (0x0BC => pub dqsgmr: MmioReadWrite<u32>),
        /// Data training configuration register.
        (0x0C0 => pub dtcr: MmioReadWrite<u32>),
        (0x0C4 => _res4: [u8; 0x3C]),
        /// PHY general configuration registers [0..3].
        (0x100 => pub pgcr0: MmioReadWrite<u32>),
        (0x104 => pub pgcr1: MmioReadWrite<u32>),
        (0x108 => pub pgcr2: MmioReadWrite<u32>),
        (0x10C => pub pgcr3: MmioReadWrite<u32>),
        (0x110 => _res5: [u8; 0x10]),
        /// ODT map register.
        (0x120 => pub odtmap: MmioReadWrite<u32>),
        (0x124 => _res6: [u8; 0x1C]),
        /// ZQ calibration control register.
        (0x140 => pub zqcr: MmioReadWrite<u32>),
        /// ZQ calibration status register.
        (0x144 => pub zqsr: MmioReadWrite<u32>),
        /// ZQ data registers [0..2].
        (0x148 => pub zqdr0: MmioReadWrite<u32>),
        (0x14C => pub zqdr1: MmioReadWrite<u32>),
        (0x150 => pub zqdr2: MmioReadWrite<u32>),
        (0x154 => _res7: [u8; 0xB4]),
        /// AC I/O configuration register.
        (0x208 => pub aciocr: MmioReadWrite<u32>),
        (0x20C => _res8: [u8; 0x04]),
        /// AC bit delay line registers [0..30] at 0x210..0x288.
        (0x210 => _acbdlr: [u8; 124]),
        (0x28C => _res9: [u8; 0x74]),
        /// DX byte lane 0 (0x300..0x37F).
        (0x300 => _dx0: [u8; 0x80]),
        /// DX byte lane 1 (0x380..0x3FF).
        (0x380 => _dx1: [u8; 0x80]),
        /// DX byte lane 2 (0x400..0x47F).
        (0x400 => _dx2: [u8; 0x80]),
        /// DX byte lane 3 (0x480..0x4FF).
        (0x480 => _dx3: [u8; 0x80]),
        (0x500 => _res10: [u8; 0x388]),
        /// Update register 2.
        (0x888 => pub upd2: MmioReadWrite<u32>),
        (0x88C => @END),
    }
}

/// DX byte-lane sub-register offsets within each DX block.
///
/// Each DX block is 0x80 bytes at CTL + 0x300 + lane * 0x80.
mod dx_off {
    pub const BDLR_BASE: usize = 0x10; // bdlr[0] at +0x10
    pub const GCR: usize = 0x44; // general configuration register
    pub const GSR0: usize = 0x48; // general status register 0
}

impl SunxiH3DramCtlRegs {
    /// Read a DX byte-lane register.
    fn dx_read(&self, lane: usize, sub_off: usize) -> u32 {
        let addr = (self as *const Self as usize) + 0x300 + lane * 0x80 + sub_off;
        // SAFETY: within the CTL register block, single-threaded boot.
        unsafe { core::ptr::read_volatile(addr as *const u32) }
    }

    /// Write a DX byte-lane register.
    fn dx_write(&self, lane: usize, sub_off: usize, val: u32) {
        let addr = (self as *const Self as usize) + 0x300 + lane * 0x80 + sub_off;
        // SAFETY: within the CTL register block, single-threaded boot.
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
    }

    /// Write an AC bit delay line register (index 0..30).
    ///
    /// ACBDLR registers at CTL + 0x210 + i * 4.
    fn write_acbdlr(&self, index: usize, val: u32) {
        let addr = (self as *const Self as usize) + 0x210 + index * 4;
        // SAFETY: within the CTL register block, single-threaded boot.
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
    }
}

// ===================================================================
// PIR (PHY Init Register) bit definitions — from U-Boot header
// ===================================================================

const PIR_INIT: u32 = 1 << 0;
const PIR_ZCAL: u32 = 1 << 1;
const PIR_PLLINIT: u32 = 1 << 4;
const PIR_DCAL: u32 = 1 << 5;
const PIR_PHYRST: u32 = 1 << 6;
const PIR_DRAMRST: u32 = 1 << 7;
const PIR_DRAMINIT: u32 = 1 << 8;
const PIR_QSGATE: u32 = 1 << 10;
const PIR_CLRSR: u32 = 1 << 27;

/// PGSR bits.
const PGSR_INIT_DONE: u32 = 1 << 0;

/// ZQ power down bit (bit 31 of ZQCR).
const ZQCR_PWRDOWN: u32 = 1 << 31;

/// PROTECT magic value to unlock register writes.
const PROTECT_MAGIC: u32 = 0x94be6fa3;

/// SRAM controller base.
const SUNXI_SRAMC_BASE: usize = 0x01C0_0000;

/// DRAM physical base address.
const DRAM_BASE: usize = 0x4000_0000;

/// Polling timeout for register completion.
const POLL_TIMEOUT: u32 = 1_000_000;

// ===================================================================
// DX_GCR ODT mode definitions — from U-Boot
// ===================================================================

/// ODT dynamically enabled during reads/writes.
const DX_GCR_ODT_DYNAMIC: u32 = 0x0 << 4;
/// ODT disabled.
const DX_GCR_ODT_OFF: u32 = 0x2 << 4;

// ===================================================================
// DDR3-1333 timing parameters — from u-boot ddr3_1333.c
// ===================================================================

/// Convert nanoseconds to clock ticks (rounding up).
#[inline]
fn ns_to_t(ns: u32, clk_mhz: u32) -> u32 {
    ((clk_mhz / 2) * ns).div_ceil(1000)
}

/// Maximum of two values (const-compatible).
#[inline]
fn max2(a: u32, b: u32) -> u32 {
    if a > b {
        a
    } else {
        b
    }
}

// ===================================================================
// Per-byte-lane delay constants for H3 (from u-boot)
// ===================================================================

/// H3 DX read delays — 4 byte lanes × 11 signals.
const H3_DX_READ_DELAYS: [[u8; 11]; 4] = [
    [18, 18, 18, 18, 18, 18, 18, 18, 18, 0, 0],
    [14, 14, 14, 14, 14, 14, 14, 14, 14, 0, 0],
    [18, 18, 18, 18, 18, 18, 18, 18, 18, 0, 0],
    [14, 14, 14, 14, 14, 14, 14, 14, 14, 0, 0],
];

/// H3 DX write delays — 4 byte lanes × 11 signals.
const H3_DX_WRITE_DELAYS: [[u8; 11]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 10],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 10],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 10],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 6],
];

/// H3 AC (address/command) delays — all zeros.
const H3_AC_DELAYS: [u8; 31] = [0; 31];

// ===================================================================
// Modified Gray code tables for ZQ calibration
// ===================================================================

const BIN_TO_MGRAY: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x06, 0x07, 0x04, 0x05, 0x0c, 0x0d, 0x0e, 0x0f, 0x0a, 0x0b, 0x08, 0x09,
    0x18, 0x19, 0x1a, 0x1b, 0x1e, 0x1f, 0x1c, 0x1d, 0x14, 0x15, 0x16, 0x17, 0x12, 0x13, 0x10, 0x11,
];

const MGRAY_TO_BIN: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x06, 0x07, 0x04, 0x05, 0x0e, 0x0f, 0x0c, 0x0d, 0x08, 0x09, 0x0a, 0x0b,
    0x1e, 0x1f, 0x1c, 0x1d, 0x18, 0x19, 0x1a, 0x1b, 0x10, 0x11, 0x12, 0x13, 0x16, 0x17, 0x14, 0x15,
];

/// Repeat a byte across all 4 positions of a u32.
#[inline]
fn repeat_byte(b: u8) -> u32 {
    let v = b as u32;
    v | (v << 8) | (v << 16) | (v << 24)
}

// ===================================================================
// Configuration
// ===================================================================

/// Typed configuration for the H3/H2+ DRAM controller driver.
///
/// The DesignWare controller auto-detects most parameters (rank count,
/// bus width, density). Only the clock frequency and ZQ value need to
/// be specified per-board.
///
/// For Orange Pi R1 (256 MB DDR3 at 624 MHz):
/// ```ron
/// SunxiH3Dramc((
///     dramc_base: 0x01C62000,
///     ccu_base:   0x01C20000,
///     clock:      624,
///     zq:         3881979,
///     odt_en:     true,
/// ))
/// ```
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiH3DramcConfig {
    /// DRAMC COM register base address (`0x01C6_2000`).
    pub dramc_base: u64,
    /// CCU register base address (`0x01C2_0000`).
    pub ccu_base: u64,
    /// DRAM clock frequency in MHz (e.g., 624).
    pub clock: u32,
    /// ZQ calibration value (e.g., 3881979 = 0x3B3BBB for H3).
    pub zq: u32,
    /// On-Die Termination enable.
    pub odt_en: bool,
}

// ===================================================================
// Driver struct
// ===================================================================

/// Allwinner H3/H2+ DesignWare DRAM controller driver.
pub struct SunxiH3Dramc {
    com: &'static SunxiH3DramComRegs,
    ctl: &'static SunxiH3DramCtlRegs,
    ccu: &'static SunxiH3CcuRegs,
    clock: u32,
    zq: u32,
    odt_en: bool,
    detected_size: Cell<u64>,
    // Run‑state: updated during init
    dual_rank: Cell<bool>,
    bus_full_width: Cell<bool>,
    // Auto‑detected DRAM geometry (page size in bytes)
    page_size: Cell<u16>,
    row_bits: Cell<u8>,
    bank_bits: Cell<u8>,
}

// SAFETY: MMIO registers at fixed hardware addresses. Single-threaded boot.
unsafe impl Send for SunxiH3Dramc {}
unsafe impl Sync for SunxiH3Dramc {}

// ===================================================================
// Device trait
// ===================================================================

impl Device for SunxiH3Dramc {
    const NAME: &'static str = "sunxi-h3-dramc";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun8i-h3-dramc"];
    type Config = SunxiH3DramcConfig;

    fn new(config: &SunxiH3DramcConfig) -> Result<Self, DeviceError> {
        let base = config.dramc_base as usize;
        Ok(Self {
            // SAFETY: addresses from board RON.
            com: unsafe { &*(base as *const SunxiH3DramComRegs) },
            ctl: unsafe { &*((base + 0x1000) as *const SunxiH3DramCtlRegs) },
            ccu: unsafe { &*(config.ccu_base as *const SunxiH3CcuRegs) },
            clock: config.clock,
            zq: config.zq,
            odt_en: config.odt_en,
            detected_size: Cell::new(0),
            dual_rank: Cell::new(true),
            bus_full_width: Cell::new(true),
            page_size: Cell::new(0),
            row_bits: Cell::new(0),
            bank_bits: Cell::new(0),
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        fstart_log::info!("DRAM: starting H3 DesignWare init");

        let size = self.sunxi_dram_init();
        if size == 0 {
            fstart_log::error!("DRAM: init failed");
            return Err(DeviceError::InitFailed);
        }

        fstart_log::info!("DRAM: {}MB", (size >> 20) as u32);
        self.detected_size.set(size);
        Ok(())
    }
}

// ===================================================================
// MemoryController trait
// ===================================================================

impl MemoryController for SunxiH3Dramc {
    fn detected_size_bytes(&self) -> u64 {
        self.detected_size.get()
    }

    fn memory_test(&self) -> Result<(), ServiceError> {
        let base = DRAM_BASE as *mut u32;
        let patterns: [u32; 4] = [0xAAAA_AAAA, 0x5555_5555, 0x0000_0000, 0xFFFF_FFFF];
        for (i, &pat) in patterns.iter().enumerate() {
            let addr = unsafe { base.add(i * 1024) };
            unsafe { core::ptr::write_volatile(addr, pat) };
            let read = unsafe { core::ptr::read_volatile(addr) };
            if read != pat {
                return Err(ServiceError::HardwareError);
            }
        }
        Ok(())
    }
}

// ===================================================================
// Top-level init — faithful port of U-Boot sunxi_dram_init()
// ===================================================================

impl SunxiH3Dramc {
    /// Top-level DRAM init. Returns detected size in bytes, or 0 on failure.
    ///
    /// Faithful port of U-Boot `sunxi_dram_init()` (H3 path).
    fn sunxi_dram_init(&self) -> u64 {
        // Step 1: System init — clocks, resets, PLL5
        self.mctl_sys_init();

        // Step 2: Channel init — PHY config, timing, training
        // This updates self.dual_rank and self.bus_full_width.
        if self.mctl_channel_init() != 0 {
            return 0;
        }

        // Step 3: ODT map
        if self.dual_rank.get() {
            self.ctl.odtmap.set(0x0000_0303);
        } else {
            self.ctl.odtmap.set(0x0000_0201);
        }
        udelay(1);

        // Step 4: H3 ODT delay configuration
        self.ctl.odtcfg.set(0x0c00_0400);

        // Step 5: Clear credit value
        let cccr = self.com.cccr.get();
        self.com.cccr.set(cccr | (1 << 31));
        udelay(10);

        // Step 6: Auto-detect DRAM size — stores page_size, row_bits, bank_bits in self
        self.mctl_auto_detect_dram_size();

        // Calculate total size using U-Boot formula:
        // rank0_size = (1 << (row + bank)) * page_size
        let row = self.row_bits.get() as u64;
        let bank = self.bank_bits.get() as u64;
        let page = self.page_size.get() as u64;
        let rank0_size = (1u64 << (row + bank)) * page;
        if self.dual_rank.get() {
            rank0_size * 2
        } else {
            rank0_size
        }
    }
}

// ===================================================================
// mctl_sys_init — exact U-Boot sequence
// ===================================================================

impl SunxiH3Dramc {
    /// System init: configure clocks and release DRAM controller from reset.
    ///
    /// Exact port of U-Boot `mctl_sys_init()` (H3 path).
    fn mctl_sys_init(&self) {
        // 1. Disable MBUS clock gate
        let mbus_clk = self.ccu.mbus_clk_cfg.get();
        self.ccu.mbus_clk_cfg.set(mbus_clk & !(1 << 31));

        // 2. Assert MBUS reset
        self.ccu
            .mbus_reset
            .set(self.ccu.mbus_reset.get() & !(1 << 31));

        // 3. Close AHB gate for MCTL (bit 14)
        self.ccu
            .bus_gate0
            .set(self.ccu.bus_gate0.get() & !(1 << 14));

        // 4. Assert AHB reset for MCTL (bit 14)
        self.ccu
            .bus_reset0
            .set(self.ccu.bus_reset0.get() & !(1 << 14));

        // 5. Disable PLL5 (DDR PLL)
        self.ccu.pll5_cfg.modify(H3_PLL5_CFG::EN::CLEAR);

        udelay(10);

        // 6. Assert DRAM clock reset
        self.ccu.dram_clk_cfg.modify(H3_DRAM_CLK::RST::CLEAR);
        udelay(1000);

        // 7. Program PLL5 (DDR PLL) = clock * 2
        // U-Boot: clock_set_pll5(CONFIG_DRAM_CLK * 2 * 1000000, false)
        // PLL5 formula: freq = 24MHz * (N+1) * (K+1) / (M+1)
        // We need: dram_clk_2x = clock * 2
        // Simplest: K=1(raw 0), M=1(raw 0), N = dram_clk_2x / 24 - 1
        let dram_clk_2x = self.clock * 2;
        let n = dram_clk_2x / 24;
        self.ccu.pll5_cfg.write(
            H3_PLL5_CFG::EN::SET
                + H3_PLL5_CFG::UPD::SET
                + H3_PLL5_CFG::N.val(n - 1)
                + H3_PLL5_CFG::K.val(0) // K=1
                + H3_PLL5_CFG::M.val(0), // M=1
        );

        // 8. Configure DRAM clock: source=PLL5, divider=1, set UPD
        // U-Boot: clrsetbits(dram_clk_cfg, DIV_MASK|SRC_MASK, DIV(1)|SRC_PLL5|UPD)
        self.ccu.dram_clk_cfg.write(
            H3_DRAM_CLK::M.val(0) // DIV(1) = M-1 = 0
                + H3_DRAM_CLK::CLK_SRC.val(0) // SRC_PLL5 = 0
                + H3_DRAM_CLK::UPD::SET,
        );

        // 9. Wait for UPD bit to clear
        for _ in 0..POLL_TIMEOUT {
            if self.ccu.dram_clk_cfg.read(H3_DRAM_CLK::UPD) == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // 10. Deassert AHB reset for MCTL
        self.ccu
            .bus_reset0
            .set(self.ccu.bus_reset0.get() | (1 << 14));

        // 11. Open AHB gate for MCTL
        self.ccu.bus_gate0.set(self.ccu.bus_gate0.get() | (1 << 14));

        // 12. Deassert MBUS reset
        self.ccu
            .mbus_reset
            .set(self.ccu.mbus_reset.get() | (1 << 31));

        // 13. Enable MBUS clock gate
        let mbus_clk = self.ccu.mbus_clk_cfg.get();
        self.ccu.mbus_clk_cfg.set(mbus_clk | (1 << 31));

        // 14. Deassert DRAM clock reset
        self.ccu.dram_clk_cfg.modify(H3_DRAM_CLK::RST::SET);
        udelay(10);

        // 15. Write clken magic value for H3
        self.ctl.clken.set(0xc00e);
        udelay(500);
    }
}

// ===================================================================
// mctl_channel_init — exact U-Boot sequence
// ===================================================================

impl SunxiH3Dramc {
    /// Channel init: PHY configuration, timing, training, ZQ calibration.
    ///
    /// Returns 0 on success, 1 on failure.
    /// Exact port of U-Boot `mctl_channel_init()` (H3 path).
    fn mctl_channel_init(&self) -> i32 {
        let mut dual_rank = self.dual_rank.get();
        let mut bus_full_width = self.bus_full_width.get();
        // 1. Set initial DRAM configuration in COM CR (max params for detection)
        self.mctl_set_cr(dual_rank, bus_full_width, 4096, 15, 3);

        // 2. Set timing parameters
        self.mctl_set_timing_params();

        // 3. Set MBUS master priority
        self.mctl_set_master_priority_h3();

        // 4. Disable VTC: clear bit 30 and bits [5:0] in PGCR0
        let pgcr0 = self.ctl.pgcr0.get();
        self.ctl.pgcr0.set(pgcr0 & !((1 << 30) | 0x3f));

        // 5. PGCR1: clear bit 24, set bit 26 (H3 path)
        let pgcr1 = self.ctl.pgcr1.get();
        self.ctl.pgcr1.set((pgcr1 & !(1 << 24)) | (1 << 26));

        // 6. Increase DFI_PHY_UPD clock (protect/upd2/unprotect)
        self.com.protect.set(PROTECT_MAGIC);
        udelay(100);
        let upd2 = self.ctl.upd2.get();
        self.ctl.upd2.set((upd2 & !(0xfff << 16)) | (0x50 << 16));
        self.com.protect.set(0);
        udelay(100);

        // 7. Set DRAMC ODT per byte lane
        for i in 0..4 {
            let clearmask: u32 = (0x3 << 4) | (0x1 << 1) | (0x3 << 2) | (0x3 << 12) | (0x3 << 14);
            let setmask: u32 = if self.odt_en {
                DX_GCR_ODT_DYNAMIC
            } else {
                DX_GCR_ODT_OFF
            };
            let gcr = self.ctl.dx_read(i, dx_off::GCR);
            self.ctl
                .dx_write(i, dx_off::GCR, (gcr & !clearmask) | setmask);
        }

        // 8. AC PDR: set bit 1 in ACIOCR (H3: no bits cleared)
        let aciocr = self.ctl.aciocr.get();
        self.ctl.aciocr.set(aciocr | (0x1 << 1));

        // 9. Set DQS auto gating PD mode: set bits [7:6] in PGCR2
        let pgcr2 = self.ctl.pgcr2.get();
        self.ctl.pgcr2.set(pgcr2 | (0x3 << 6));

        // 10. H3: dx ddr_clk & hdr_clk dynamic mode — clear bits [15:14] and [13:12] in PGCR0
        let pgcr0 = self.ctl.pgcr0.get();
        self.ctl.pgcr0.set(pgcr0 & !((0x3 << 14) | (0x3 << 12)));

        // 11. H3: dphy & aphy phase select 270 degree
        // clrsetbits(pgcr[2], (0x3 << 10) | (0x3 << 8), (0x1 << 10) | (0x2 << 8))
        let pgcr2 = self.ctl.pgcr2.get();
        self.ctl
            .pgcr2
            .set((pgcr2 & !((0x3 << 10) | (0x3 << 8))) | ((0x1 << 10) | (0x2 << 8)));

        // 12. Set half DQ: if not full width, disable upper byte lanes
        if !bus_full_width {
            self.ctl.dx_write(2, dx_off::GCR, 0);
            self.ctl.dx_write(3, dx_off::GCR, 0);
        }

        // 13. Data training configuration: set rank mask in DTCR bits [27:24]
        let dtcr = self.ctl.dtcr.get();
        let rank_mask: u32 = if dual_rank { 0x3 } else { 0x1 };
        self.ctl.dtcr.set((dtcr & !(0xf << 24)) | (rank_mask << 24));

        // 14. Set bit delays
        self.mctl_set_bit_delays();
        udelay(50);

        // 15. H3 ZQ calibration quirk
        self.mctl_h3_zq_calibration_quirk();

        // 16. PHY init: PLL init + digital cal + PHY reset + DRAM reset + DRAM init + QS gate
        // U-Boot: mctl_phy_init(PIR_PLLINIT | PIR_DCAL | PIR_PHYRST |
        //                       PIR_DRAMRST | PIR_DRAMINIT | PIR_QSGATE)
        // Note: mctl_phy_init adds PIR_INIT internally.
        self.mctl_phy_init(
            PIR_PLLINIT | PIR_DCAL | PIR_PHYRST | PIR_DRAMRST | PIR_DRAMINIT | PIR_QSGATE,
        );

        // 17. Detect ranks and bus width
        if self.ctl.pgsr0.get() & (0xfe << 20) != 0 {
            // Check if rank 1 failed (DQS gate error on dx[0] or dx[1])
            if ((self.ctl.dx_read(0, dx_off::GSR0) >> 24) & 0x2) != 0
                || ((self.ctl.dx_read(1, dx_off::GSR0) >> 24) & 0x2) != 0
            {
                let dtcr = self.ctl.dtcr.get();
                self.ctl.dtcr.set((dtcr & !(0xf << 24)) | (0x1 << 24));
                dual_rank = false;
            }

            // Check if upper byte lanes failed (half DQ width)
            if ((self.ctl.dx_read(2, dx_off::GSR0) >> 24) & 0x1) != 0
                || ((self.ctl.dx_read(3, dx_off::GSR0) >> 24) & 0x1) != 0
            {
                self.ctl.dx_write(2, dx_off::GCR, 0);
                self.ctl.dx_write(3, dx_off::GCR, 0);
                bus_full_width = false;
            }

            self.mctl_set_cr(dual_rank, bus_full_width, 4096, 15, 3);
            udelay(20);

            // Re-train with reduced configuration
            self.mctl_phy_init(PIR_QSGATE);
            if self.ctl.pgsr0.get() & (0xfe << 20) != 0 {
                return 1;
            }
        }

        // 18. Check the DRAMC status — wait for controller ready
        for _ in 0..POLL_TIMEOUT {
            if self.ctl.statr.get() & 0x1 == 0x1 {
                break;
            }
            core::hint::spin_loop();
        }

        // 19. Refresh trigger (liuke added for refresh debug)
        let rfsh = self.ctl.rfshctl0.get();
        self.ctl.rfshctl0.set(rfsh | (1 << 31));
        udelay(10);
        self.ctl.rfshctl0.set(rfsh & !(1 << 31));
        udelay(10);

        // 20. Set PGCR3, CKE polarity (H3 value)
        self.ctl.pgcr3.set(0x00aa_0060);

        // 21. Power down ZQ calibration module for power save
        let zqcr = self.ctl.zqcr.get();
        self.ctl.zqcr.set(zqcr | ZQCR_PWRDOWN);

        // 22. Enable master access
        self.com.maer.set(0xffff_ffff);

        // Store final detected rank/bus width values for size calculation
        self.dual_rank.set(dual_rank);
        self.bus_full_width.set(bus_full_width);

        0
    }
}

// ===================================================================
// Timing parameters — exact port of U-Boot ddr3_1333.c
// ===================================================================

impl SunxiH3Dramc {
    /// Set DDR3-1333 timing parameters.
    ///
    /// Exact port of U-Boot `mctl_set_timing_params()` for DDR3-1333.
    fn mctl_set_timing_params(&self) {
        let clk = self.clock;

        let tccd: u32 = 2;
        let tfaw = ns_to_t(50, clk);
        let trrd = max2(ns_to_t(10, clk), 4);
        let trcd = ns_to_t(15, clk);
        let trc = ns_to_t(53, clk);
        let txp = max2(ns_to_t(8, clk), 3);
        let twtr = max2(ns_to_t(8, clk), 4);
        let trtp = max2(ns_to_t(8, clk), 4);
        let twr = max2(ns_to_t(15, clk), 3);
        let trp = ns_to_t(15, clk);
        let tras = ns_to_t(38, clk);
        let trefi = ns_to_t(7800, clk) / 32;
        let trfc = ns_to_t(350, clk);

        let tmrw: u32 = 0;
        let tmrd: u32 = 4;
        let tmod: u32 = 12;
        let tcke: u32 = 3;
        let tcksrx: u32 = 5;
        let tcksre: u32 = 5;
        let tckesr: u32 = 4;
        let trasmax: u32 = 24;

        let tcl: u32 = 6; // CL 12
        let tcwl: u32 = 4; // CWL 8
        let t_rdata_en: u32 = 4;
        let wr_latency: u32 = 2;

        // U-Boot: tdinit0 = (500 * CONFIG_DRAM_CLK) + 1
        let tdinit0: u32 = 500 * clk + 1;
        let tdinit1: u32 = (360 * clk) / 1000 + 1;
        let tdinit2: u32 = 200 * clk + 1;
        let tdinit3: u32 = 1 * clk + 1;

        let twtp = tcwl + 2 + twr;
        let twr2rd = tcwl + 2 + twtr;
        let trd2wr = tcl + 2 + 1 - tcwl;

        // Mode registers
        self.ctl.mr0.set(0x1c70); // CL=11, WR=12
        self.ctl.mr1.set(0x40);
        self.ctl.mr2.set(0x18); // CWL=8
        self.ctl.mr3.set(0x0);

        // DRAMTMG0: wr2pre | tFAW | tRAS_max | tRAS_min
        self.ctl
            .dramtmg0
            .set((twtp << 24) | (tfaw << 16) | (trasmax << 8) | tras);

        // DRAMTMG1: tXP | tRTP | tRC
        self.ctl.dramtmg1.set((txp << 16) | (trtp << 8) | trc);

        // DRAMTMG2: tCWL | tCL | rd2wr | wr2rd
        self.ctl
            .dramtmg2
            .set((tcwl << 24) | (tcl << 16) | (trd2wr << 8) | twr2rd);

        // DRAMTMG3: tMRW | tMRD | tMOD
        self.ctl.dramtmg3.set((tmrw << 16) | (tmrd << 12) | tmod);

        // DRAMTMG4: tRCD | tCCD | tRRD | tRP
        self.ctl
            .dramtmg4
            .set((trcd << 24) | (tccd << 16) | (trrd << 8) | trp);

        // DRAMTMG5: tCKSRX | tCKSRE | tCKESR | tCKE
        self.ctl
            .dramtmg5
            .set((tcksrx << 24) | (tcksre << 16) | (tckesr << 8) | tcke);

        // DRAMTMG8: two-rank timing
        // U-Boot: clrsetbits(dramtmg[8], (0xff<<8)|(0xff<<0), (0x66<<8)|(0x10<<0))
        let dramtmg8 = self.ctl.dramtmg8.get();
        self.ctl
            .dramtmg8
            .set((dramtmg8 & !((0xff << 8) | 0xff)) | (0x66 << 8) | (0x10 << 0));

        // PITMG0: PHY interface timing
        self.ctl
            .pitmg0
            .set((0x2 << 24) | (t_rdata_en << 16) | (0x1 << 8) | wr_latency);

        // PTR3/PTR4: DRAM init timing
        self.ctl.ptr3.set((tdinit1 << 20) | tdinit0);
        self.ctl.ptr4.set((tdinit3 << 20) | tdinit2);

        // RFSHTMG: refresh timing
        self.ctl.rfshtmg.set((trefi << 16) | trfc);
    }
}

// ===================================================================
// PHY init
// ===================================================================

impl SunxiH3Dramc {
    /// Trigger PHY initialization and wait for completion.
    ///
    /// U-Boot: writel(val | PIR_INIT, &mctl_ctl->pir);
    ///         mctl_await_completion(&mctl_ctl->pgsr[0], PGSR_INIT_DONE, 0x1);
    fn mctl_phy_init(&self, val: u32) {
        self.ctl.pir.set(val | PIR_INIT);

        // Wait for PGSR INIT_DONE
        for _ in 0..POLL_TIMEOUT {
            if self.ctl.pgsr0.get() & PGSR_INIT_DONE != 0 {
                return;
            }
            core::hint::spin_loop();
        }
    }
}

// ===================================================================
// ZQ calibration quirk — exact U-Boot port
// ===================================================================

impl SunxiH3Dramc {
    /// H3-specific ZQ calibration quirk.
    ///
    /// Exact port of U-Boot `mctl_h3_zq_calibration_quirk()`.
    /// Two paths based on SRAMC version registers.
    fn mctl_h3_zq_calibration_quirk(&self) {
        // 32-bit bus = 6 ZQ iterations, 16-bit = 4
        let zq_count: usize = 6;

        // Read SRAMC version info
        let sramc_24 = unsafe { core::ptr::read_volatile((SUNXI_SRAMC_BASE + 0x24) as *const u32) };
        let sramc_f0 = unsafe { core::ptr::read_volatile((SUNXI_SRAMC_BASE + 0xf0) as *const u32) };

        if (sramc_24 & 0xff) == 0 && (sramc_f0 & 0x1) == 0 {
            // Path A: early silicon — run ZCAL, read/patch ZQDR registers

            // clrsetbits(zqcr, 0xffff, CONFIG_DRAM_ZQ & 0xffff)
            let zqcr = self.ctl.zqcr.get();
            self.ctl.zqcr.set((zqcr & !0xffff) | (self.zq & 0xffff));

            self.ctl.pir.set(PIR_CLRSR);
            self.mctl_phy_init(PIR_ZCAL);

            // Read and patch ZQDR0
            let mut reg_val = self.ctl.zqdr0.get();
            reg_val &= (0x1f << 16) | 0x1f;
            reg_val |= reg_val << 8;
            self.ctl.zqdr0.set(reg_val);

            // Read and patch ZQDR1, copy to ZQDR2
            let mut reg_val = self.ctl.zqdr1.get();
            reg_val &= (0x1f << 16) | 0x1f;
            reg_val |= reg_val << 8;
            self.ctl.zqdr1.set(reg_val);
            self.ctl.zqdr2.set(reg_val);
        } else {
            // Path B: later silicon — iterative per-nibble calibration with Gray code
            let mut zq_val: [u16; 6] = [0; 6];

            self.ctl.zqdr2.set(0x0a0a_0a0a);

            for i in 0..zq_count {
                let zq = ((self.zq >> (i as u32 * 4)) & 0xf) as u32;

                self.ctl
                    .zqcr
                    .set((zq << 20) | (zq << 16) | (zq << 12) | (zq << 8) | (zq << 4) | zq);

                self.ctl.pir.set(PIR_CLRSR);
                self.mctl_phy_init(PIR_ZCAL);

                zq_val[i] = (self.ctl.zqdr0.get() & 0xff) as u16;
                self.ctl.zqdr2.set(repeat_byte(zq_val[i] as u8));

                self.ctl.pir.set(PIR_CLRSR);
                self.mctl_phy_init(PIR_ZCAL);

                let val = (self.ctl.zqdr0.get() >> 24) as u8;
                let mgray_bin = MGRAY_TO_BIN[(val & 0x1f) as usize];
                let adjusted = if mgray_bin > 0 { mgray_bin - 1 } else { 0 };
                zq_val[i] |= (BIN_TO_MGRAY[adjusted as usize] as u16) << 8;
            }

            self.ctl
                .zqdr0
                .set(((zq_val[1] as u32) << 16) | (zq_val[0] as u32));
            self.ctl
                .zqdr1
                .set(((zq_val[3] as u32) << 16) | (zq_val[2] as u32));
            if zq_count > 4 {
                self.ctl
                    .zqdr2
                    .set(((zq_val[5] as u32) << 16) | (zq_val[4] as u32));
            }
        }
    }
}

// ===================================================================
// Bit delays — exact U-Boot port
// ===================================================================

impl SunxiH3Dramc {
    /// Set per-byte-lane DQ bit delays and AC delays.
    ///
    /// Exact port of U-Boot `mctl_set_bit_delays()`.
    /// Each BDLR register gets: `(write_delay << 8) | read_delay`.
    /// Each ACBDLR register gets: `write_delay << 8`.
    fn mctl_set_bit_delays(&self) {
        // Disable auto-calibration during delay programming
        let pgcr0 = self.ctl.pgcr0.get();
        self.ctl.pgcr0.set(pgcr0 & !(1 << 26));

        // DX byte-lane delays: 4 lanes × 11 signals per lane
        for i in 0..4 {
            for j in 0..11 {
                let wr = H3_DX_WRITE_DELAYS[i][j] as u32;
                let rd = H3_DX_READ_DELAYS[i][j] as u32;
                self.ctl
                    .dx_write(i, dx_off::BDLR_BASE + j * 4, (wr << 8) | rd);
            }
        }

        // AC bit delay line registers: 31 entries
        for i in 0..31 {
            self.ctl.write_acbdlr(i, (H3_AC_DELAYS[i] as u32) << 8);
        }

        // Re-enable auto-calibration
        let pgcr0 = self.ctl.pgcr0.get();
        self.ctl.pgcr0.set(pgcr0 | (1 << 26));
    }
}

// ===================================================================
// MBUS master priority — exact U-Boot port
// ===================================================================

impl SunxiH3Dramc {
    /// Configure a single MBUS port.
    ///
    /// Exact port of U-Boot `mbus_configure_port()`.
    fn mbus_configure_port(
        &self,
        port: usize,
        bwlimit: bool,
        priority: bool,
        qos: u8,
        waittime: u8,
        acs: u8,
        bwl0: u16,
        bwl1: u16,
        bwl2: u16,
    ) {
        let cfg0: u32 = (if bwlimit { 1u32 } else { 0 })
            | (if priority { 1u32 << 1 } else { 0 })
            | (((qos as u32) & 0x3) << 2)
            | (((waittime as u32) & 0xf) << 4)
            | (((acs as u32) & 0xff) << 8)
            | ((bwl0 as u32) << 16);
        let cfg1: u32 = ((bwl2 as u32) << 16) | (bwl1 as u32);

        self.com.write_mcr0(port, cfg0);
        self.com.write_mcr1(port, cfg1);
    }

    /// Set MBUS master priority for H3.
    ///
    /// Exact port of U-Boot `mctl_set_master_priority_h3()`.
    fn mctl_set_master_priority_h3(&self) {
        // Enable bandwidth limit windows and set window size 1us
        self.com.bwcr.set((1 << 16) | 400);

        // Set CPU high priority
        self.com.mapr.set(0x0000_0001);

        // QOS constants: LOWEST=0, LOW=1, HIGH=2, HIGHEST=3
        const HIGHEST: u8 = 3;
        const HIGH: u8 = 2;

        //            port  bwlimit  priority  qos      wait acs  bwl0   bwl1  bwl2
        self.mbus_configure_port(0, true, false, HIGHEST, 0, 0, 512, 256, 128); // CPU
        self.mbus_configure_port(1, true, false, HIGH, 0, 0, 1536, 1024, 256); // GPU
        self.mbus_configure_port(2, true, false, HIGHEST, 0, 0, 512, 256, 96); // UNUSED
        self.mbus_configure_port(3, true, false, HIGHEST, 0, 0, 256, 128, 32); // DMA
        self.mbus_configure_port(4, true, false, HIGH, 0, 0, 1792, 1600, 256); // VE
        self.mbus_configure_port(5, true, false, HIGHEST, 0, 0, 256, 128, 32); // CSI
        self.mbus_configure_port(6, true, false, HIGH, 0, 0, 256, 128, 64); // NAND
        self.mbus_configure_port(7, true, false, HIGHEST, 0, 0, 256, 128, 64); // SS
        self.mbus_configure_port(8, true, false, HIGHEST, 0, 0, 256, 128, 64); // TS
        self.mbus_configure_port(9, true, false, HIGH, 0, 0, 1024, 256, 64); // DI
        self.mbus_configure_port(10, true, false, HIGHEST, 0, 3, 8192, 6120, 1024); // DE
        self.mbus_configure_port(11, true, false, HIGH, 0, 0, 1024, 288, 64); // DE_CFD
    }
}

// ===================================================================
// CR (control register) helpers
// ===================================================================

impl SunxiH3Dramc {
    /// Set DRAM control register (CR) with given parameters.
    ///
    /// Exact port of U-Boot `mctl_set_cr()` (H3 DDR3 path).
    /// U-Boot CR encoding:
    ///   MCTL_CR_BL8 = 0x4 << 20
    ///   MCTL_CR_DDR3 = 0x3 << 16
    ///   MCTL_CR_2T = 0x0 << 19 (DDR3 uses 2T)
    ///   MCTL_CR_INTERLEAVED = 0x0 << 15
    ///   MCTL_CR_EIGHT_BANKS = 0x1 << 2
    ///   MCTL_CR_FOUR_BANKS = 0x0 << 2
    ///   MCTL_CR_DUAL_RANK = 0x1 << 0
    ///   MCTL_CR_SINGLE_RANK = 0x0 << 0
    ///   MCTL_CR_BUS_FULL_WIDTH(x) = (x) << 12
    ///   MCTL_CR_PAGE_SIZE(x) = (fls(x) - 4) << 8
    ///   MCTL_CR_ROW_BITS(x) = ((x) - 1) << 4
    fn mctl_set_cr(
        &self,
        dual_rank: bool,
        bus_full_width: bool,
        page_size: u16,
        row_bits: u8,
        bank_bits: u8,
    ) {
        // fls(page_size) - 4: 512→6, 1024→7, 2048→8, 4096→9, 8192→10
        // Then << 8 gives the page_size field.
        let page_enc = ((fls(page_size as u32) - 4) as u32) << 8;

        let cr = (0x4 << 20) // BL8
            | (0x3 << 16) // DDR3
            | (0x0 << 19) // 2T command rate
            | (0x0 << 15) // interleaved
            | (if bank_bits == 3 { 1u32 << 2 } else { 0 }) // eight banks
            | (if bus_full_width { 1u32 << 12 } else { 0 })
            | (if dual_rank { 1u32 } else { 0 })
            | page_enc
            | (((row_bits as u32) - 1) << 4);

        self.com.cr.set(cr);
    }

    /// Extract page_size from CR value.
    #[allow(dead_code)]
    fn cr_to_page_size(&self, cr: u32) -> u16 {
        let enc = (cr >> 8) & 0xf;
        1u16 << (enc + 4) // inverse of fls(page_size) - 4
    }

    /// Extract row_bits from CR value.
    #[allow(dead_code)]
    fn cr_to_row_bits(&self, _cr: u32) -> u8 {
        let enc = (_cr >> 4) & 0xf;
        (enc + 1) as u8
    }

    /// Extract bank_bits from CR value.
    #[allow(dead_code)]
    fn cr_to_bank_bits(&self, cr: u32) -> u8 {
        if (cr >> 2) & 0x1 != 0 {
            3
        } else {
            2
        }
    }
}

/// Find last set bit (1-indexed). Returns 0 for input 0.
fn fls(mut x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    let mut r = 32u32;
    if x & 0xffff_0000 == 0 {
        x <<= 16;
        r -= 16;
    }
    if x & 0xff00_0000 == 0 {
        x <<= 8;
        r -= 8;
    }
    if x & 0xf000_0000 == 0 {
        x <<= 4;
        r -= 4;
    }
    if x & 0xc000_0000 == 0 {
        x <<= 2;
        r -= 2;
    }
    if x & 0x8000_0000 == 0 {
        r -= 1;
    }
    r
}

// ===================================================================
// Auto-detect DRAM size — exact U-Boot port
// ===================================================================

impl SunxiH3Dramc {
    /// Check if memory at `offset` aliases to base address.
    ///
    /// Exact port of U-Boot `mctl_mem_matches_base()`.
    fn mctl_mem_matches_base(&self, offset: usize) -> bool {
        let base = DRAM_BASE as *mut u32;
        // SAFETY: probing DRAM addresses; write patterns to detect aliasing.
        unsafe {
            // Write two distinct patterns
            core::ptr::write_volatile(base, 0);
            core::ptr::write_volatile(base.add(offset / 4), 0xaa55aa55);
            // Compiler fence to prevent reordering
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
            // Check if base was clobbered (aliasing)
            core::ptr::read_volatile(base) == 0xaa55aa55
        }
    }

    /// Auto-detect DRAM size for a single rank.
    ///
    /// Exact port of U-Boot `mctl_auto_detect_dram_size_rank()`.
    fn mctl_auto_detect_dram_size_rank(
        &self,
        dual_rank: bool,
        bus_full_width: bool,
    ) -> (u16, u8, u8) {
        // Detect row address bits: start with page_size=512, row=16, bank=2
        let mut page_size: u16 = 512;
        let mut row_bits: u8 = 16;
        let mut bank_bits: u8 = 2;

        self.mctl_set_cr(dual_rank, bus_full_width, page_size, row_bits, bank_bits);

        // Sweep row_bits from 11 upward
        row_bits = 16; // start at max
        for rb in 11u8..16 {
            let offset = (1usize << (rb as usize + bank_bits as usize)) * page_size as usize;
            if self.mctl_mem_matches_base(offset) {
                row_bits = rb;
                break;
            }
        }

        // Detect bank address bits: start with bank=3
        bank_bits = 3;
        self.mctl_set_cr(dual_rank, bus_full_width, page_size, row_bits, bank_bits);

        for bb in 2u8..3 {
            let offset = (1usize << bb as usize) * page_size as usize;
            if self.mctl_mem_matches_base(offset) {
                bank_bits = bb;
                break;
            }
        }

        // Detect page size: start with page=8192
        page_size = 8192;
        self.mctl_set_cr(dual_rank, bus_full_width, page_size, row_bits, bank_bits);

        let mut ps: u16 = 512;
        while ps < 8192 {
            if self.mctl_mem_matches_base(ps as usize) {
                page_size = ps;
                break;
            }
            ps *= 2;
        }
        if ps >= 8192 {
            page_size = 8192;
        }

        (page_size, row_bits, bank_bits)
    }

    /// Auto-detect DRAM size.
    ///
    /// Exact port of U-Boot `mctl_auto_detect_dram_size()` (H3 path).
    fn mctl_auto_detect_dram_size(&self) {
        let dual_rank = self.dual_rank.get();
        let bus_full_width = self.bus_full_width.get();
        let (page_size, row_bits, bank_bits) =
            self.mctl_auto_detect_dram_size_rank(dual_rank, bus_full_width);

        // Store detected values into self for final size calculation
        self.page_size.set(page_size);
        self.row_bits.set(row_bits);
        self.bank_bits.set(bank_bits);

        // Re-program CR with detected values
        self.mctl_set_cr(dual_rank, bus_full_width, page_size, row_bits, bank_bits);
    }
}
