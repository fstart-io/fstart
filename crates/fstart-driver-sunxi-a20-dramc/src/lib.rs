//! Allwinner A20 (sun7i) DRAM controller driver.
//!
//! Performs full DDR3 DRAM initialization: PLL5 setup, controller
//! configuration, DLL training, impedance calibration, DQS gate
//! training, size auto-detection, and host-port configuration.
//!
//! Ported from u-boot `arch/arm/mach-sunxi/dram_sun4i.c` (sun7i paths).
//! Only single-rank DDR3 is supported (covers all A20 boards).
//!
//! DRAM controller register block: `0x01C0_1000`
//! CCU (for PLL5/MBUS/gates): `0x01C2_0000`
//! DRAM physical base: `0x4000_0000`

#![no_std]
#![allow(clippy::modulo_one)] // tock-registers alignment test

use core::cell::Cell;

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_mmio::{MmioReadOnly, MmioReadWrite};

use fstart_services::device::{Device, DeviceError};
use fstart_services::{MemoryController, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiA20CcuRegs, MBUS_CLK, PLL5_CFG};

use fstart_arch::udelay;

// ===================================================================
// DRAMC register bitfields
// ===================================================================

register_bitfields! [u32,
    /// Controller Configuration Register.
    CCR [
        /// 1T command rate (vs default 2T).
        COMMAND_RATE_1T OFFSET(5) NUMBITS(1) [],
        /// DQS gating mode: 1 = passive window, 0 = active.
        DQS_GATE OFFSET(14) NUMBITS(1) [],
        /// DQS drift compensation enable.
        DQS_DRIFT_COMP OFFSET(17) NUMBITS(1) [],
        /// ITM (impedance trimming module) off.
        ITM_OFF OFFSET(28) NUMBITS(1) [],
        /// Hardware data training trigger.
        DATA_TRAINING OFFSET(30) NUMBITS(1) [],
        /// DRAM initialization trigger.
        INIT OFFSET(31) NUMBITS(1) []
    ],
    /// DRAM Configuration Register.
    DCR [
        /// Memory type: 0 = DDR2, 1 = DDR3.
        TYPE OFFSET(0) NUMBITS(1) [
            Ddr2 = 0,
            Ddr3 = 1
        ],
        /// Per-chip I/O width encoding (raw = io_width_bits / 8).
        IO_WIDTH OFFSET(1) NUMBITS(2) [],
        /// Chip density encoding (0 = 256Mb .. 5 = 8Gb).
        CHIP_DENSITY OFFSET(3) NUMBITS(3) [],
        /// Bus width encoding (raw = bus_width_bytes - 1).
        BUS_WIDTH OFFSET(6) NUMBITS(3) [],
        /// Rank select (raw = rank_num - 1).
        RANK_SEL OFFSET(10) NUMBITS(2) [],
        /// Send commands to all ranks.
        CMD_RANK_ALL OFFSET(12) NUMBITS(1) [],
        /// Address mapping mode: 0 = sequential, 1 = interleave.
        MODE OFFSET(13) NUMBITS(2) [
            Sequential = 0,
            Interleave = 1
        ]
    ],
    /// Controller Status Register.
    CSR [
        /// Data training error.
        DTERR OFFSET(20) NUMBITS(1) [],
        /// Data training iteration error.
        DTIERR OFFSET(21) NUMBITS(1) []
    ],
    /// Mode Configure Register.
    MCR [
        /// Normal mode configuration bits [1:0].
        MODE_NORM OFFSET(0) NUMBITS(2) [],
        /// DDR3 reset pin control.
        RESET OFFSET(12) NUMBITS(1) [],
        /// Mode enable bits [14:13].
        MODE_EN OFFSET(13) NUMBITS(2) [],
        /// DRAM clock output enable.
        DCLK_OUT OFFSET(16) NUMBITS(1) []
    ],
    /// Mode Register (MR0 for DDR3).
    MR [
        /// Burst length encoding.
        BURST_LENGTH OFFSET(0) NUMBITS(3) [],
        /// CAS latency (raw = CAS - 4).
        CAS_LAT OFFSET(4) NUMBITS(3) [],
        /// Write recovery time encoding.
        WRITE_RECOVERY OFFSET(9) NUMBITS(3) [],
        /// Active power-down mode.
        POWER_DOWN OFFSET(12) NUMBITS(1) []
    ],
    /// ZQ Calibration Control Register 0.
    ZQCR0 [
        /// Impedance divider program value.
        IMP_DIV OFFSET(20) NUMBITS(8) [],
        /// Use ZDATA directly instead of calibration.
        ZDEN OFFSET(28) NUMBITS(1) [],
        /// Start ZQ calibration.
        ZCAL OFFSET(31) NUMBITS(1) []
    ],
    /// ZQ Calibration Status Register.
    ZQSR [
        /// ZQ calibration done flag.
        ZDONE OFFSET(31) NUMBITS(1) []
    ],
    /// DLL Control Register (per-byte-lane).
    DLLCR [
        /// Active-low reset (0 = in reset, 1 = normal).
        NRESET OFFSET(30) NUMBITS(1) [],
        /// DLL disable (1 = disabled).
        DISABLE OFFSET(31) NUMBITS(1) []
    ]
];

// ===================================================================
// DRAMC register block
// ===================================================================

register_structs! {
    /// Allwinner A20 DRAM controller register block at `0x01C0_1000`.
    SunxiDramcRegs {
        (0x00 => pub ccr: MmioReadWrite<u32, CCR::Register>),
        (0x04 => pub dcr: MmioReadWrite<u32, DCR::Register>),
        (0x08 => pub iocr: MmioReadWrite<u32>),
        (0x0C => pub csr: MmioReadWrite<u32, CSR::Register>),
        (0x10 => pub drr: MmioReadWrite<u32>),
        (0x14 => pub tpr0: MmioReadWrite<u32>),
        (0x18 => pub tpr1: MmioReadWrite<u32>),
        (0x1C => pub tpr2: MmioReadWrite<u32>),
        (0x20 => pub gdllcr: MmioReadWrite<u32>),
        (0x24 => _res0: [u8; 0x28]),
        (0x4C => pub rslr0: MmioReadWrite<u32>),
        (0x50 => pub rslr1: MmioReadWrite<u32>),
        (0x54 => _res1: [u8; 0x08]),
        (0x5C => pub rdgr0: MmioReadWrite<u32>),
        (0x60 => pub rdgr1: MmioReadWrite<u32>),
        (0x64 => _res2: [u8; 0x34]),
        (0x98 => pub odtcr: MmioReadWrite<u32>),
        (0x9C => pub dtr0: MmioReadWrite<u32>),
        (0xA0 => pub dtr1: MmioReadWrite<u32>),
        (0xA4 => pub dtar: MmioReadWrite<u32>),
        (0xA8 => pub zqcr0: MmioReadWrite<u32, ZQCR0::Register>),
        (0xAC => pub zqcr1: MmioReadWrite<u32>),
        (0xB0 => pub zqsr: MmioReadOnly<u32, ZQSR::Register>),
        (0xB4 => pub idcr: MmioReadWrite<u32>),
        (0xB8 => _res3: [u8; 0x138]),
        (0x1F0 => pub mr: MmioReadWrite<u32, MR::Register>),
        (0x1F4 => pub emr: MmioReadWrite<u32>),
        (0x1F8 => pub emr2: MmioReadWrite<u32>),
        (0x1FC => pub emr3: MmioReadWrite<u32>),
        (0x200 => pub dllctr: MmioReadWrite<u32>),
        (0x204 => pub dllcr0: MmioReadWrite<u32, DLLCR::Register>),
        (0x208 => pub dllcr1: MmioReadWrite<u32, DLLCR::Register>),
        (0x20C => pub dllcr2: MmioReadWrite<u32, DLLCR::Register>),
        (0x210 => pub dllcr3: MmioReadWrite<u32, DLLCR::Register>),
        (0x214 => pub dllcr4: MmioReadWrite<u32, DLLCR::Register>),
        (0x218 => pub dqtr0: MmioReadWrite<u32>),
        (0x21C => pub dqtr1: MmioReadWrite<u32>),
        (0x220 => pub dqtr2: MmioReadWrite<u32>),
        (0x224 => pub dqtr3: MmioReadWrite<u32>),
        (0x228 => pub dqstr: MmioReadWrite<u32>),
        (0x22C => pub dqsbtr: MmioReadWrite<u32>),
        (0x230 => pub mcr: MmioReadWrite<u32, MCR::Register>),
        (0x234 => _res4: [u8; 0x08]),
        (0x23C => pub ppwrsctl: MmioReadWrite<u32>),
        (0x240 => pub apr: MmioReadWrite<u32>),
        (0x244 => pub pldtr: MmioReadWrite<u32>),
        (0x248 => _res5: [u8; 0x08]),
        /// HPCR[0..31] — host port configuration registers.
        /// Accessed via `write_hpcr()` helper (32 × u32, no bitfields).
        (0x250 => _hpcr: [u8; 0x80]),
        (0x2D0 => _res6: [u8; 0x10]),
        (0x2E0 => pub csel: MmioReadWrite<u32>),
        (0x2E4 => @END),
    }
}

// ===================================================================
// Constants
// ===================================================================

/// DRAM physical base address on A20.
const DRAM_BASE: usize = 0x4000_0000;

/// Maximum testable DRAM size (A20 has A0-A15 lines → up to 2 GB).
const DRAM_MAX_SIZE: usize = 0x8000_0000;

/// IOCR ODT enable mask (bits [31:30] and [1:0]).
#[allow(clippy::identity_op)]
const IOCR_ODT_EN: u32 = (3 << 30) | (3 << 0);

/// AHB gate bit: SDRAM controller.
const AHB_GATE_SDRAM: u32 = 1 << 14;
/// AHB gate bit: DLL controller.
const AHB_GATE_DLL: u32 = 1 << 15;
/// AHB gate bit: GPS module.
const AHB_GATE_GPS: u32 = 1 << 26;

/// GPS clock control: reset bit.
const GPS_CTRL_RESET: u32 = 1 << 0;
/// GPS clock control: gate bit.
const GPS_CTRL_GATE: u32 = 1 << 1;

/// MBUS clock source: PLL6.
const MBUS_CLK_SRC_PLL6: u32 = 1;
/// MBUS clock source: PLL5P.
const MBUS_CLK_SRC_PLL5: u32 = 2;

/// tRFC values in nanoseconds by density index (256Mb .. 8Gb).
const TRFC_DDR3: [u16; 6] = [90, 90, 110, 160, 300, 350];

/// Host port configuration values for sun7i (A20).
///
/// From u-boot `dram_sun4i.c`, sun7i variant.
const HPCR_SUN7I: [u32; 32] = [
    0x0301, 0x0301, 0x0301, 0x0301, 0x0301, 0x0301, 0x0301, 0x0301, 0, 0, 0, 0, 0, 0, 0, 0, 0x1031,
    0x1031, 0x0735, 0x1035, 0x1035, 0x0731, 0x1031, 0x0735, 0x1035, 0x1031, 0x0731, 0x1035, 0x0001,
    0x1031, 0, 0x1031,
];

/// Maximum iterations for register polling (generous timeout).
const POLL_TIMEOUT: u32 = 1_000_000;

// ===================================================================
// Configuration
// ===================================================================

/// Typed configuration for the A20 DRAM controller driver.
///
/// Carries all board-specific DDR3 timing parameters. Density, I/O width,
/// and bus width are auto-detected at init time.
///
/// For BananaPi M1 (vendor magic timings at 432 MHz):
/// ```ron
/// SunxiA20Dramc((
///     dramc_base: 0x01C01000,
///     ccu_base:   0x01C20000,
///     clock:      432,
///     mbus_clock: 0,
///     zq:         123,
///     odt_en:     false,
///     cas:        6,
///     tpr0:       0x30926692,
///     tpr1:       0x1090,
///     tpr2:       0x1a0c8,
///     tpr3:       0x0,
///     tpr4:       0x0,
///     emr1:       4,
///     emr2:       0,
///     emr3:       0,
///     dqs_gating_delay: 0x0,
///     active_windowing: false,
/// ))
/// ```
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiA20DramcConfig {
    /// DRAMC register base address (`0x01C0_1000`).
    pub dramc_base: u64,
    /// CCU register base address (`0x01C2_0000`).
    pub ccu_base: u64,
    /// DRAM clock frequency in MHz (e.g., 432).
    pub clock: u32,
    /// MBUS clock frequency in MHz (0 = default 300).
    pub mbus_clock: u32,
    /// ZQ calibration value.
    pub zq: u32,
    /// On-Die Termination enable.
    pub odt_en: bool,
    /// CAS latency (e.g., 6, 7, 8, 9).
    pub cas: u32,
    /// Timing parameter register 0 (raw value).
    pub tpr0: u32,
    /// Timing parameter register 1 (raw value).
    pub tpr1: u32,
    /// Timing parameter register 2 (raw value).
    pub tpr2: u32,
    /// DLL phase configuration (0 = default).
    pub tpr3: u32,
    /// Command rate control (bit 0: 1T mode on sun7i).
    pub tpr4: u32,
    /// Extended mode register 1.
    pub emr1: u32,
    /// Extended mode register 2.
    pub emr2: u32,
    /// Extended mode register 3.
    pub emr3: u32,
    /// DQS gating delay override per lane (0 = auto from training).
    pub dqs_gating_delay: u32,
    /// Use active DQS windowing instead of passive.
    pub active_windowing: bool,
}

// ===================================================================
// Driver struct
// ===================================================================

/// Allwinner A20 DRAM controller driver.
///
/// Implements `Device` (full DDR3 init in `init()`) and `MemoryController`
/// (detected size query after init). The auto-detection loop tries 32-bit
/// bus width first, falls back to 16-bit, and adjusts chip density based
/// on detected DRAM size.
pub struct SunxiA20Dramc {
    regs: &'static SunxiDramcRegs,
    ccu: &'static SunxiA20CcuRegs,
    clock: u32,
    mbus_clock: u32,
    zq: u32,
    odt_en: bool,
    cas: u32,
    tpr0: u32,
    tpr1: u32,
    tpr2: u32,
    tpr3: u32,
    tpr4: u32,
    emr1: u32,
    emr2: u32,
    emr3: u32,
    dqs_gating_delay: u32,
    active_windowing: bool,
    // Mutable state — modified during auto-detection in `init()`.
    density: Cell<u32>,
    io_width: Cell<u32>,
    bus_width: Cell<u32>,
    detected_size: Cell<u64>,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
// Early boot is single-threaded; no concurrent access to Cell fields.
unsafe impl Send for SunxiA20Dramc {}
unsafe impl Sync for SunxiA20Dramc {}

// ===================================================================
// Device trait
// ===================================================================

impl Device for SunxiA20Dramc {
    const NAME: &'static str = "sunxi-a20-dramc";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun7i-a20-dramc"];
    type Config = SunxiA20DramcConfig;

    fn new(config: &SunxiA20DramcConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: addresses come from the board RON, validated by codegen.
            regs: unsafe { &*(config.dramc_base as *const SunxiDramcRegs) },
            ccu: unsafe { &*(config.ccu_base as *const SunxiA20CcuRegs) },
            clock: config.clock,
            mbus_clock: config.mbus_clock,
            zq: config.zq,
            odt_en: config.odt_en,
            cas: config.cas,
            tpr0: config.tpr0,
            tpr1: config.tpr1,
            tpr2: config.tpr2,
            tpr3: config.tpr3,
            tpr4: config.tpr4,
            emr1: config.emr1,
            emr2: config.emr2,
            emr3: config.emr3,
            dqs_gating_delay: config.dqs_gating_delay,
            active_windowing: config.active_windowing,
            density: Cell::new(0),
            io_width: Cell::new(0),
            bus_width: Cell::new(0),
            detected_size: Cell::new(0),
        })
    }

    /// Run the full DRAM initialization with auto-detection.
    ///
    /// Tries 32-bit bus width first, falls back to 16-bit. Adjusts
    /// chip density if the detected size differs from the initial guess.
    fn init(&mut self) -> Result<(), DeviceError> {
        fstart_log::info!("DRAM: starting init");

        // A20 has all A0-A15 address lines, allowing max density 8192 Mb.
        self.io_width.set(16);
        self.bus_width.set(32);
        self.density.set(8192);

        let mut size = self.dramc_init_helper();
        if size == 0 {
            // 32-bit bus failed — try 16-bit
            self.bus_width.set(16);
            size = self.dramc_init_helper();
            if size == 0 {
                fstart_log::error!("DRAM: init failed");
                return Err(DeviceError::InitFailed);
            }
        }

        // Check if density needs adjustment
        let actual_density =
            (size >> 17) * self.io_width.get() as u64 / self.bus_width.get() as u64;
        if actual_density != self.density.get() as u64 {
            self.density.set(actual_density as u32);
            size = self.dramc_init_helper();
            if size == 0 {
                return Err(DeviceError::InitFailed);
            }
        }

        fstart_log::info!("DRAM: {}MB", (size >> 20) as u32);
        self.detected_size.set(size);
        Ok(())
    }
}

// ===================================================================
// MemoryController trait
// ===================================================================

impl MemoryController for SunxiA20Dramc {
    fn detected_size_bytes(&self) -> u64 {
        self.detected_size.get()
    }

    fn memory_test(&self) -> Result<(), ServiceError> {
        // Basic write/read test at a few addresses
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
// Main init helper — corresponds to u-boot's dramc_init_helper()
// ===================================================================

impl SunxiA20Dramc {
    /// Core DRAM init sequence. Returns detected RAM size in bytes, or 0 on failure.
    ///
    /// This is called potentially multiple times during auto-detection
    /// (with different density/bus_width settings).
    fn dramc_init_helper(&self) -> u64 {
        // Only single-rank DDR3 is supported.
        // (All known A20 boards use DDR3 rank-1.)

        // Step 1: Setup DRAM PLL (PLL5), MBUS clock, AHB gates
        self.mctl_setup_dram_clock();

        // Step 2: Disable pad power save (clear self-refresh state)
        self.mctl_disable_power_save();

        // Step 3: Set drive strength
        self.mctl_set_drive();

        // Step 4: Disable DRAM clock output
        self.dramc_clock_output_en(false);

        // Step 5: Disable ITM
        self.mctl_itm_disable();

        // Step 6: Enable master DLL (DLL0)
        self.mctl_enable_dll0(self.tpr3);

        // Step 7: Configure DCR — density, bus width, IO width, rank, mode
        let density_enc = self.density_to_encoding(self.density.get());
        self.regs.dcr.write(
            DCR::TYPE::Ddr3
                + DCR::IO_WIDTH.val(self.io_width.get() / 8)
                + DCR::CHIP_DENSITY.val(density_enc)
                + DCR::BUS_WIDTH.val(self.bus_width.get() / 8 - 1)
                + DCR::RANK_SEL.val(0) // rank_num - 1 = 0
                + DCR::CMD_RANK_ALL::SET
                + DCR::MODE::Interleave,
        );

        // Step 8: Enable DRAM clock output
        self.dramc_clock_output_en(true);

        // Step 9: Impedance calibration
        self.mctl_set_impedance();

        // Step 10: Set CKE delay to maximum
        self.mctl_set_cke_delay();

        // Step 11: DDR3 reset sequence
        self.mctl_ddr3_reset();
        udelay(1);

        // Wait for any pending init to complete
        self.await_ccr_clear(CCR::INIT);

        // Step 12: Enable per-lane DLLs
        self.mctl_enable_dllx(self.tpr3);

        // Step 13: Set auto-refresh timing
        self.dramc_set_autorefresh_cycle(density_enc);

        // Step 14: Write timing parameters
        self.regs.tpr0.set(self.tpr0);
        self.regs.tpr1.set(self.tpr1);
        self.regs.tpr2.set(self.tpr2);

        // Step 15: Configure mode register (MR0)
        self.regs.mr.write(
            MR::BURST_LENGTH.val(0)
                + MR::POWER_DOWN::SET // sun7i: active power-down
                + MR::CAS_LAT.val(self.cas - 4)
                + MR::WRITE_RECOVERY.val(self.ddr3_write_recovery()),
        );

        // Step 16: Write extended mode registers
        self.regs.emr.set(self.emr1);
        self.regs.emr2.set(self.emr2);
        self.regs.emr3.set(self.emr3);

        // Step 17: Disable drift compensation, set passive DQS window mode
        self.regs
            .ccr
            .modify(CCR::DQS_DRIFT_COMP::CLEAR + CCR::DQS_GATE::SET);

        // Step 18: sun7i — optional 1T command rate
        if self.tpr4 & 0x1 != 0 {
            self.regs.ccr.modify(CCR::COMMAND_RATE_1T::SET);
        }

        // Step 19: Trigger DDR3 initialization
        self.mctl_ddr3_initialize();

        // Step 20: Enable ITM, run data training
        self.mctl_itm_enable();

        if !self.dramc_scan_readpipe() {
            return 0;
        }

        // Step 21: Optional DQS gating delay override
        if self.dqs_gating_delay != 0 {
            self.mctl_set_dqs_gating_delay(0, self.dqs_gating_delay);
        }

        // Step 22: Set DQS gating window type
        if self.active_windowing {
            self.regs.ccr.modify(CCR::DQS_GATE::CLEAR);
        } else {
            self.regs.ccr.modify(CCR::DQS_GATE::SET);
        }

        // Step 23: ITM reset
        self.mctl_itm_reset();

        // Step 24: Configure all host ports
        self.mctl_configure_hostport();

        // Step 25: Detect actual RAM size
        self.get_ram_size()
    }
}

// ===================================================================
// Hardware sub-routines
// ===================================================================

impl SunxiA20Dramc {
    // ---------------------------------------------------------------
    // Polling helpers
    // ---------------------------------------------------------------

    /// Spin until the given CCR bits are clear (or timeout).
    fn await_ccr_clear(&self, field: tock_registers::fields::Field<u32, CCR::Register>) {
        for _ in 0..POLL_TIMEOUT {
            if self.regs.ccr.read(field) == 0 {
                return;
            }
            core::hint::spin_loop();
        }
    }

    /// Spin until ZQSR ZDONE is set (or timeout).
    fn await_zqsr_done(&self) {
        for _ in 0..POLL_TIMEOUT {
            if self.regs.zqsr.is_set(ZQSR::ZDONE) {
                return;
            }
            core::hint::spin_loop();
        }
    }

    // ---------------------------------------------------------------
    // DLLCR array access
    // ---------------------------------------------------------------

    /// Get a reference to DLLCR register by index (0-4).
    fn dllcr(&self, i: usize) -> &MmioReadWrite<u32, DLLCR::Register> {
        match i {
            0 => &self.regs.dllcr0,
            1 => &self.regs.dllcr1,
            2 => &self.regs.dllcr2,
            3 => &self.regs.dllcr3,
            4 => &self.regs.dllcr4,
            _ => &self.regs.dllcr0, // unreachable in practice
        }
    }

    // ---------------------------------------------------------------
    // HPCR array access
    // ---------------------------------------------------------------

    /// Write a Host Port Configuration Register by index (0-31).
    fn write_hpcr(&self, index: usize, value: u32) {
        // SAFETY: HPCR[0..31] at offset 0x250 within the DRAMC register block.
        let base = self.regs as *const SunxiDramcRegs as usize;
        unsafe {
            fstart_mmio::write32((base + 0x250 + index * 4) as *mut u32, value);
        }
    }

    // ---------------------------------------------------------------
    // Clock setup — PLL5, MBUS, AHB gates
    // ---------------------------------------------------------------

    /// Program PLL5 (DRAM PLL), MBUS clock, and open SDRAM/DLL AHB gates.
    ///
    /// Ported from u-boot `mctl_setup_dram_clock()` (sun7i path).
    fn mctl_setup_dram_clock(&self) {
        let clk = self.clock;

        // Determine PLL5 M, K, N for the requested DRAM clock frequency.
        let (m_x, k_x, n_x) = if (540..552).contains(&clk) {
            (2u32, 3u32, 15u32)
        } else if (512..528).contains(&clk) {
            (3, 4, 16)
        } else if (496..504).contains(&clk) {
            (3, 2, 31)
        } else if (468..480).contains(&clk) {
            (2, 3, 13)
        } else if (396..408).contains(&clk) {
            (2, 3, 11)
        } else {
            // Generic: any frequency that is a multiple of 24
            (2, 2, clk / 24)
        };

        // Program PLL5: read-modify-write to preserve LDO/BIAS/BW defaults.
        // U-Boot: readl → mask M/K/N/P → set new values → writel.
        self.ccu.pll5_cfg.modify(
            PLL5_CFG::M.val(m_x - 1)
                + PLL5_CFG::K.val(k_x - 1)
                + PLL5_CFG::N.val(n_x)
                + PLL5_CFG::P.val(0)
                + PLL5_CFG::VCO_GAIN::CLEAR
                + PLL5_CFG::EN::SET,
        );
        udelay(5500);

        // Enable DDR clock output from PLL5
        self.ccu.pll5_cfg.modify(PLL5_CFG::DDR_CLK::SET);

        // sun7i: reset GPS module (required for DRAM)
        let gps = self.ccu.gps_clk_cfg.get();
        self.ccu
            .gps_clk_cfg
            .set(gps & !(GPS_CTRL_RESET | GPS_CTRL_GATE));
        let ahb = self.ccu.ahb_gate0.get();
        self.ccu.ahb_gate0.set(ahb | AHB_GATE_GPS);
        udelay(1);
        self.ccu.ahb_gate0.set(ahb & !AHB_GATE_GPS);

        // Setup MBUS clock
        let mbus_clk = if self.mbus_clock == 0 {
            300
        } else {
            self.mbus_clock
        };

        // Compute PLL5P and PLL6×2 frequencies for MBUS source selection
        let pll5p_clk = self.get_pll5p_freq() / 1_000_000;
        let pll6_clk = self.get_pll6_freq() / 1_000_000;
        let pll6x_clk = pll6_clk * 2; // sun7i uses PLL6×2

        let pll6x_div = pll6x_clk.div_ceil(mbus_clk);
        let pll5p_div = pll5p_clk.div_ceil(mbus_clk);

        let pll6x_rate = pll6x_clk.checked_div(pll6x_div).unwrap_or(0);
        let pll5p_rate = pll5p_clk.checked_div(pll5p_div).unwrap_or(0);

        if pll6x_div <= 16 && pll6x_rate > pll5p_rate {
            // Use PLL6 as MBUS source
            self.ccu.mbus_clk_cfg.write(
                MBUS_CLK::GATE::SET
                    + MBUS_CLK::CLK_SRC.val(MBUS_CLK_SRC_PLL6)
                    + MBUS_CLK::N.val(0) // N=1 (2^0)
                    + MBUS_CLK::M.val(pll6x_div - 1),
            );
        } else {
            // Use PLL5P as MBUS source
            self.ccu.mbus_clk_cfg.write(
                MBUS_CLK::GATE::SET
                    + MBUS_CLK::CLK_SRC.val(MBUS_CLK_SRC_PLL5)
                    + MBUS_CLK::N.val(0) // N=1 (2^0)
                    + MBUS_CLK::M.val(pll5p_div - 1),
            );
        }

        // Open DRAMC and DLL AHB clock gates (close first, then reopen)
        let ahb = self.ccu.ahb_gate0.get();
        self.ccu
            .ahb_gate0
            .set(ahb & !(AHB_GATE_SDRAM | AHB_GATE_DLL));
        udelay(22);
        self.ccu.ahb_gate0.set(ahb | AHB_GATE_SDRAM | AHB_GATE_DLL);
        udelay(22);
    }

    /// Read PLL5P frequency in Hz.
    fn get_pll5p_freq(&self) -> u32 {
        self.ccu.pll5p_freq()
    }

    /// Read PLL6 frequency in Hz (PLL6 output / 2).
    fn get_pll6_freq(&self) -> u32 {
        self.ccu.pll6_freq()
    }

    // ---------------------------------------------------------------
    // Power save / drive strength
    // ---------------------------------------------------------------

    /// Clear self-refresh state. Sun7i requires magic value 0x1651_0000.
    fn mctl_disable_power_save(&self) {
        self.regs.ppwrsctl.set(0x1651_0000);
    }

    /// Configure DRAM I/O drive strength (sun7i variant).
    fn mctl_set_drive(&self) {
        let mcr = self.regs.mcr.get();
        // sun7i: clear MODE_NORM[1:0] and bits [29:28], set MODE_EN[14:13] and bits [11:2]
        let mcr = (mcr & !(0x3 | (0x3 << 28))) | ((0x3 << 13) | 0xffc);
        self.regs.mcr.set(mcr);
    }

    // ---------------------------------------------------------------
    // DRAM clock output control
    // ---------------------------------------------------------------

    /// Enable or disable DRAM clock output (sun7i: MCR DCLK_OUT bit).
    fn dramc_clock_output_en(&self, on: bool) {
        if on {
            self.regs.mcr.modify(MCR::DCLK_OUT::SET);
        } else {
            self.regs.mcr.modify(MCR::DCLK_OUT::CLEAR);
        }
    }

    // ---------------------------------------------------------------
    // ITM (Impedance Trimming Module) control
    // ---------------------------------------------------------------

    /// Disable ITM: clear INIT, set ITM_OFF.
    fn mctl_itm_disable(&self) {
        self.regs.ccr.modify(CCR::INIT::CLEAR + CCR::ITM_OFF::SET);
    }

    /// Enable ITM: clear ITM_OFF.
    fn mctl_itm_enable(&self) {
        self.regs.ccr.modify(CCR::ITM_OFF::CLEAR);
    }

    /// Reset ITM: disable, short delay, enable, short delay.
    fn mctl_itm_reset(&self) {
        self.mctl_itm_disable();
        udelay(1);
        self.mctl_itm_enable();
        udelay(1);
    }

    // ---------------------------------------------------------------
    // DLL enable sequences
    // ---------------------------------------------------------------

    /// Get the number of DDR byte lanes (2 for 16-bit, 4 for 32-bit bus).
    fn mctl_get_number_of_lanes(&self) -> u32 {
        // DCR BUS_WIDTH field: value 3 = 32-bit, value 1 = 16-bit
        if self.regs.dcr.read(DCR::BUS_WIDTH) == 3 {
            4
        } else {
            2
        }
    }

    /// Enable master DLL (DLLCR[0]).
    ///
    /// Phase bits in DLLCR[0] are at [11:6] (6 bits from tpr3[21:16]).
    fn mctl_enable_dll0(&self, phase: u32) {
        let dll = self.dllcr(0);
        // Set phase bits [11:6]
        let val = dll.get();
        dll.set((val & !(0x3f << 6)) | (((phase >> 16) & 0x3f) << 6));

        // Set DISABLE, clear NRESET
        let val = dll.get();
        dll.set((val & !(1 << 30)) | (1 << 31));
        udelay(2);

        // Clear both NRESET and DISABLE
        let val = dll.get();
        dll.set(val & !((1 << 30) | (1 << 31)));
        udelay(22);

        // Set NRESET, clear DISABLE
        let val = dll.get();
        dll.set((val & !(1 << 31)) | (1 << 30));
        udelay(22);
    }

    /// Enable per-lane DLLs (DLLCR[1..N]).
    ///
    /// Phase bits in DLLCR[1-4] are at [17:14] (4 bits per lane from tpr3).
    fn mctl_enable_dllx(&self, phase: u32) {
        let n_lanes = self.mctl_get_number_of_lanes();
        let mut ph = phase;

        // Step 1: Set phase and DISABLE, clear NRESET for each lane
        for i in 1..=n_lanes {
            let dll = self.dllcr(i as usize);
            let val = dll.get();
            let val = (val & !(0xf << 14)) | ((ph & 0xf) << 14);
            let val = (val & !(1 << 30)) | (1 << 31); // clear NRESET, set DISABLE
            dll.set(val);
            ph >>= 4;
        }
        udelay(2);

        // Step 2: Clear both NRESET and DISABLE
        for i in 1..=n_lanes {
            let dll = self.dllcr(i as usize);
            let val = dll.get();
            dll.set(val & !((1 << 30) | (1 << 31)));
        }
        udelay(22);

        // Step 3: Set NRESET, clear DISABLE
        for i in 1..=n_lanes {
            let dll = self.dllcr(i as usize);
            let val = dll.get();
            dll.set((val & !(1 << 31)) | (1 << 30));
        }
        udelay(22);
    }

    // ---------------------------------------------------------------
    // Impedance calibration
    // ---------------------------------------------------------------

    /// ZQ impedance calibration (sun7i path).
    fn mctl_set_impedance(&self) {
        let zprog = self.zq & 0xFF;
        let zdata = (self.zq >> 8) & 0xF_FFFF;

        // sun7i: skip initial ZDONE wait (not needed)

        // ZQ calibration is only useful with ODT enabled
        if !self.odt_en {
            return;
        }

        // sun7i magic: required to avoid deadlock when enabling ODT in IOCR
        self.regs.zqcr1.set((1 << 24) | (1 << 1));

        // Clear any pending calibration
        self.regs.zqcr0.modify(ZQCR0::ZCAL::CLEAR);

        if zdata != 0 {
            // Use user-supplied impedance data directly
            self.regs
                .zqcr0
                .write(ZQCR0::ZDEN::SET + ZQCR0::IMP_DIV.val(0));
            // Write raw zdata into the lower bits
            let val = self.regs.zqcr0.get() | zdata;
            self.regs.zqcr0.set(val);
        } else {
            // Perform calibration using external resistor
            self.regs
                .zqcr0
                .write(ZQCR0::ZCAL::SET + ZQCR0::IMP_DIV.val(zprog));
            self.await_zqsr_done();
        }

        // Clear calibration trigger
        self.regs.zqcr0.modify(ZQCR0::ZCAL::CLEAR);

        // Enable ODT in I/O configuration
        self.regs.iocr.set(IOCR_ODT_EN);
    }

    // ---------------------------------------------------------------
    // CKE delay / DDR3 reset
    // ---------------------------------------------------------------

    /// Set CKE delay to maximum (0x1ffff) as per Allwinner boot0.
    fn mctl_set_cke_delay(&self) {
        let val = self.regs.idcr.get();
        self.regs.idcr.set(val | 0x1_ffff);
    }

    /// DDR3 reset sequence (sun7i: assert low, wait 200µs, de-assert high, wait 500µs).
    fn mctl_ddr3_reset(&self) {
        // sun7i (non-Rev-A): clear RESET first, then set
        self.regs.mcr.modify(MCR::RESET::CLEAR);
        udelay(200);
        self.regs.mcr.modify(MCR::RESET::SET);
        // DDR3 spec: wait 500µs after RESET de-assert before CKE goes high.
        // The IDCR register provides an automatic delay, but we add an
        // explicit one for safety (matching u-boot's approach).
        udelay(500);
    }

    // ---------------------------------------------------------------
    // DRAM initialization trigger
    // ---------------------------------------------------------------

    /// Trigger DRAM controller initialization (sends mode registers to DRAM).
    fn mctl_ddr3_initialize(&self) {
        self.regs.ccr.modify(CCR::INIT::SET);
        self.await_ccr_clear(CCR::INIT);
    }

    // ---------------------------------------------------------------
    // Auto-refresh
    // ---------------------------------------------------------------

    /// Set auto-refresh cycle based on clock and density.
    fn dramc_set_autorefresh_cycle(&self, density: u32) {
        let clk = self.clock;
        let density_idx = density.min(5) as usize;
        let trfc = (TRFC_DDR3[density_idx] as u32 * clk).div_ceil(1000);
        let trefi = (7987 * clk) >> 10; // <= 7.8µs
                                        // DRR: tRFC in [7:0], tREFI in [23:8]
        self.regs.drr.set((trefi << 8) | (trfc & 0xFF));
    }

    // ---------------------------------------------------------------
    // Write recovery calculation
    // ---------------------------------------------------------------

    /// Calculate MR0 write recovery field from clock speed (DDR3 spec).
    fn ddr3_write_recovery(&self) -> u32 {
        let twr_ck = (15 * self.clock).div_ceil(1000); // tWR = 15ns for all DDR3
        if twr_ck < 5 {
            1
        } else if twr_ck <= 8 {
            twr_ck - 4
        } else if twr_ck <= 10 {
            5
        } else {
            6
        }
    }

    // ---------------------------------------------------------------
    // Data training
    // ---------------------------------------------------------------

    /// Hardware DQS gate training. Returns true on success.
    fn dramc_scan_readpipe(&self) -> bool {
        // Clear training error flags
        self.regs.csr.modify(CSR::DTERR::CLEAR + CSR::DTIERR::CLEAR);

        // Trigger data training
        self.regs.ccr.modify(CCR::DATA_TRAINING::SET);

        // Wait for training to complete
        self.await_ccr_clear(CCR::DATA_TRAINING);

        // Check for errors
        let failed = self.regs.csr.is_set(CSR::DTERR) || self.regs.csr.is_set(CSR::DTIERR);
        !failed
    }

    // ---------------------------------------------------------------
    // DQS gating delay override
    // ---------------------------------------------------------------

    /// Set DQS gating delay for a given rank.
    ///
    /// Each byte in `delay` encodes the delay for one lane:
    /// bits [7:2] = system latency, bits [1:0] = phase select.
    fn mctl_set_dqs_gating_delay(&self, rank: u32, delay: u32) {
        let n_lanes = self.mctl_get_number_of_lanes();
        let (slr_reg, dgr_reg) = if rank == 0 {
            (&self.regs.rslr0, &self.regs.rdgr0)
        } else {
            (&self.regs.rslr1, &self.regs.rdgr1)
        };
        let mut slr = slr_reg.get();
        let mut dgr = dgr_reg.get();
        for lane in 0..n_lanes {
            let tmp = delay >> (lane * 8);
            slr &= !(7 << (lane * 3));
            slr |= ((tmp >> 2) & 7) << (lane * 3);
            dgr &= !(3 << (lane * 2));
            dgr |= (tmp & 3) << (lane * 2);
        }
        slr_reg.set(slr);
        dgr_reg.set(dgr);
    }

    // ---------------------------------------------------------------
    // Host port configuration
    // ---------------------------------------------------------------

    /// Write all 32 host port configuration registers (sun7i values).
    fn mctl_configure_hostport(&self) {
        for (i, &val) in HPCR_SUN7I.iter().enumerate() {
            self.write_hpcr(i, val);
        }
    }

    // ---------------------------------------------------------------
    // Density encoding
    // ---------------------------------------------------------------

    /// Convert density in megabits to DCR encoding.
    fn density_to_encoding(&self, density_mb: u32) -> u32 {
        match density_mb {
            256 => 0,
            512 => 1,
            1024 => 2,
            2048 => 3,
            4096 => 4,
            8192 => 5,
            _ => 0,
        }
    }

    // ---------------------------------------------------------------
    // RAM size detection
    // ---------------------------------------------------------------

    /// Detect actual DRAM size by writing patterns at power-of-2 offsets.
    ///
    /// Faithful port of u-boot `common/memsize.c:get_ram_size()`.
    ///
    /// Algorithm: write `~cnt` at descending power-of-2 word offsets, then
    /// write 0 at base. Read back ascending: the first mismatch reveals
    /// address aliasing (wrapping) and thus the true size.
    ///
    /// The descending write order is critical: when DRAM is smaller than
    /// `maxsize`, higher addresses alias to lower ones. Writing descending
    /// ensures the aliased (high) address is written first, then the real
    /// (low) address is written later with a different value. On read-back,
    /// the aliased address returns the low address's value — a mismatch.
    fn get_ram_size(&self) -> u64 {
        let base = DRAM_BASE as *mut u32;
        let max_words = DRAM_MAX_SIZE / core::mem::size_of::<u32>();

        // Phase 1: write ~cnt at descending power-of-2 word offsets.
        // cnt goes: max_words/2, max_words/4, ..., 2, 1
        let mut cnt = max_words >> 1;
        while cnt > 0 {
            // SAFETY: testing DRAM addresses within the declared memory region.
            // Even if the physical address wraps/aliases, the write is safe.
            unsafe {
                core::ptr::write_volatile(base.add(cnt), !(cnt as u32));
            }
            cnt >>= 1;
        }

        // Phase 2: write 0 at base. This overwrites any aliased writes
        // that landed at physical address `base`.
        unsafe { core::ptr::write_volatile(base, 0u32) };

        // Phase 3: verify base reads back as 0 (basic DRAM sanity check).
        let base_val = unsafe { core::ptr::read_volatile(base) };
        if base_val != 0 {
            return 0;
        }

        // Phase 4: read back ascending. First address where the value
        // doesn't match ~cnt indicates address aliasing → true size.
        cnt = 1;
        while cnt < max_words {
            let read = unsafe { core::ptr::read_volatile(base.add(cnt)) };
            if read != !(cnt as u32) {
                return (cnt as u64) * core::mem::size_of::<u32>() as u64;
            }
            cnt <<= 1;
        }

        DRAM_MAX_SIZE as u64
    }
}
