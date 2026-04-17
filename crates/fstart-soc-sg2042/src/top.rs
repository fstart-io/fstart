//! SG2042 SYS_CTRL (TOP) register block and clock controller driver.
//!
//! The TOP register block is a sparse 3 KB address space with registers
//! scattered at non-contiguous offsets.  All access is via raw volatile
//! pointer reads/writes using the offset constants defined here.
//!
//! Hardware reference:
//! - `platform_def.h` — base addresses and offsets
//! - `mango_clock.c` — clock gate table
//! - `mango_common.c` — `bm_ip_reset()`
//! - `mango_bl2_setup.c` — init sequence

use serde::{Deserialize, Serialize};

use fstart_services::{
    clock::ClockController,
    device::{Device, DeviceError},
    ServiceError,
};

// ===================================================================
// Register offsets (relative to SYS_CTRL_BASE)
// ===================================================================

/// Chip version register.
pub const REG_CHIP_VERSION: usize = 0x000;
/// Boot configuration (BOOT_SEL, MODE_SEL, SOCKET_ID).
pub const REG_CONF_INFO: usize = 0x004;
/// System control (bit 2 = SW_ROOT_RESET_EN).
pub const REG_TOP_CTRL: usize = 0x008;
/// Watchdog reset status (write 1 to clear).
pub const REG_WDT_RST_STATUS: usize = 0x01C;
/// GP_REG28 — boot stage indicator.
pub const REG_BOOT_STAGE: usize = 0x230;
/// GP_REG31 — board type (written after MCU detection).
pub const REG_BOARD_TYPE: usize = 0x23C;
/// RISC-V CPU vendor ID lower 32 bits.
pub const REG_RP_CPU_VENDOR_ID_L: usize = 0x340;
/// RISC-V CPU vendor ID upper 32 bits.
pub const REG_RP_CPU_VENDOR_ID_H: usize = 0x344;
/// RISC-V reset vector lower 32 bits.
pub const REG_RP_CPU_RVBA_L: usize = 0x350;
/// RISC-V reset vector upper 32 bits.
pub const REG_RP_CPU_RVBA_H: usize = 0x354;
/// CMN-600 periphbase lower 32 bits.
pub const REG_CFGM_PERIPHBASE_L: usize = 0x358;
/// CMN-600 periphbase upper 32 bits.
pub const REG_CFGM_PERIPHBASE_H: usize = 0x35C;
/// Allow RISC-V secure transactions (write 1).
pub const REG_RP_CPU_SEC_ACC: usize = 0x360;
/// Cluster status registers base (+ cluster * 8).
pub const REG_MP0_STATUS_BASE: usize = 0x380;
/// Cluster control registers base (+ cluster * 8).
pub const REG_MP0_CONTROL_BASE: usize = 0x384;
/// Clock enable register 0.
pub const REG_CLOCK_ENABLE0: usize = 0x800;
/// Clock enable register 1.
pub const REG_CLOCK_ENABLE1: usize = 0x804;
/// Soft reset register 0 (bit 10=WDT, 13=I2C0, 28=SDIO).
pub const REG_SOFT_RST0: usize = 0xC00;
/// Soft reset register 1.
pub const REG_SOFT_RST1: usize = 0xC04;

/// CONF_INFO bit positions.
pub mod conf_info {
    pub const MODE_SEL_SHIFT: u32 = 8;
    pub const MODE_SEL_MASK: u32 = 0x7;
    pub const SOCKET_ID_SHIFT: u32 = 16;
    pub const SOCKET_ID_MASK: u32 = 0x3;

    pub const MODE_NORMAL: u32 = 0;
    pub const MODE_FAST: u32 = 1;
    pub const MODE_SAFE: u32 = 2;
    pub const MODE_BYPASS: u32 = 3;
}

/// SYS_CTRL base address (`SYS_CTRL_BASE` in platform_def.h).
pub const SYS_CTRL_BASE: u64 = 0x7030010000;

// ===================================================================
// Clock gate table
// ===================================================================

/// A single gate clock entry derived from `mango_clock.c`.
pub struct GateClkEntry {
    /// Logical gate ID used as `ClockController::enable_clock(gate_id)`.
    pub id: u32,
    /// 0 = CLOCK_ENABLE0 (TOP+0x800), 1 = CLOCK_ENABLE1 (TOP+0x804).
    pub reg_offset: u32,
    /// Bit position within the register.
    pub bit: u32,
    /// Enable this clock during `Sg2042Top::init()`.
    pub enable_at_boot: bool,
}

/// Logical gate clock IDs.
pub mod gate_id {
    pub const UART_500M: u32 = 0;
    pub const APB_I2C: u32 = 1;
    pub const APB_WDT: u32 = 2;
    pub const A53: u32 = 3;
    pub const RISCV: u32 = 4;
    pub const EMMC_200M: u32 = 5;
    pub const SD_200M: u32 = 6;
}

/// Static gate clock table from `mango_clock.c`.
pub static GATE_CLK_TABLE: &[GateClkEntry] = &[
    GateClkEntry {
        id: gate_id::UART_500M,
        reg_offset: 0,
        bit: 2,
        enable_at_boot: true,
    },
    GateClkEntry {
        id: gate_id::APB_I2C,
        reg_offset: 0,
        bit: 11,
        enable_at_boot: true,
    },
    GateClkEntry {
        id: gate_id::APB_WDT,
        reg_offset: 0,
        bit: 12,
        enable_at_boot: true,
    },
    GateClkEntry {
        id: gate_id::A53,
        reg_offset: 0,
        bit: 20,
        enable_at_boot: false,
    },
    GateClkEntry {
        id: gate_id::RISCV,
        reg_offset: 0,
        bit: 21,
        enable_at_boot: false,
    },
    GateClkEntry {
        id: gate_id::EMMC_200M,
        reg_offset: 1,
        bit: 0,
        enable_at_boot: false,
    },
    GateClkEntry {
        id: gate_id::SD_200M,
        reg_offset: 1,
        bit: 1,
        enable_at_boot: false,
    },
];

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 TOP (SYS_CTRL) clock controller.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042TopConfig {
    /// SYS_CTRL (TOP) register block base address.
    pub sys_ctrl_base: u64,
    /// DesignWare I2C1 base address — used for MCU board detection.
    pub i2c1_base: u64,
    /// I2C device address of the on-board MCU (always `0x17` on Pioneer).
    pub mcu_addr: u8,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 SYS_CTRL (TOP) clock controller driver.
///
/// Responsibilities in `init()`:
/// 1. Check socket ID (Pioneer must be socket 0)
/// 2. Assert/deassert IP soft-resets — equivalent to `bm_ip_reset()`
/// 3. Enable boot-required peripheral gate clocks (UART, I2C, WDT)
/// 4. Detect board type via I2C1 MCU probe; write to `BOARD_TYPE_REG`
pub struct Sg2042Top {
    base: usize,
    i2c1_base: u64,
    mcu_addr: u8,
}

// SAFETY: MMIO at fixed hardware addresses; single-threaded A53 boot context.
unsafe impl Send for Sg2042Top {}
unsafe impl Sync for Sg2042Top {}

impl Sg2042Top {
    /// Read a 32-bit TOP register.
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: address is within the SYS_CTRL MMIO window mapped as Device.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    /// Write a 32-bit TOP register.
    fn write(&self, offset: usize, val: u32) {
        // SAFETY: address is within the SYS_CTRL MMIO window mapped as Device.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, val) }
    }

    /// Set bits in a 32-bit TOP register.
    fn set_bits(&self, offset: usize, mask: u32) {
        self.write(offset, self.read(offset) | mask);
    }

    /// Clear bits in a 32-bit TOP register.
    fn clear_bits(&self, offset: usize, mask: u32) {
        self.write(offset, self.read(offset) & !mask);
    }
}

impl Device for Sg2042Top {
    const NAME: &'static str = "sg2042-top";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-sysctrl"];
    type Config = Sg2042TopConfig;

    fn new(config: &Sg2042TopConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            base: config.sys_ctrl_base as usize,
            i2c1_base: config.i2c1_base,
            mcu_addr: config.mcu_addr,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Refuse to boot if this is not socket 0 (Pioneer is single-socket).
        let conf = self.read(REG_CONF_INFO);
        let socket_id = (conf >> conf_info::SOCKET_ID_SHIFT) & conf_info::SOCKET_ID_MASK;
        if socket_id != 0 {
            fstart_log::error!(
                "[SG2042 TOP] socket_id={} — Pioneer must be socket 0",
                socket_id
            );
            return Err(DeviceError::ConfigError);
        }

        let mode = (conf >> conf_info::MODE_SEL_SHIFT) & conf_info::MODE_SEL_MASK;
        if mode != conf_info::MODE_NORMAL && mode != conf_info::MODE_FAST {
            fstart_log::warn!(
                "[SG2042 TOP] MODE_SEL={} — not Normal/Fast; UART clock may be wrong",
                mode
            );
        }

        self.ip_reset();
        self.enable_peripheral_clocks();
        self.detect_board();
        Ok(())
    }
}

impl ClockController for Sg2042Top {
    fn enable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        let entry = GATE_CLK_TABLE
            .iter()
            .find(|e| e.id == gate_id)
            .ok_or(ServiceError::InvalidParam)?;
        let offset = if entry.reg_offset == 0 {
            REG_CLOCK_ENABLE0
        } else {
            REG_CLOCK_ENABLE1
        };
        self.set_bits(offset, 1 << entry.bit);
        Ok(())
    }

    fn disable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        let entry = GATE_CLK_TABLE
            .iter()
            .find(|e| e.id == gate_id)
            .ok_or(ServiceError::InvalidParam)?;
        let offset = if entry.reg_offset == 0 {
            REG_CLOCK_ENABLE0
        } else {
            REG_CLOCK_ENABLE1
        };
        self.clear_bits(offset, 1 << entry.bit);
        Ok(())
    }

    fn get_frequency(&self, _clock_id: u32) -> Result<u32, ServiceError> {
        // Full PLL query not yet implemented.
        Err(ServiceError::NotSupported)
    }
}

impl Sg2042Top {
    /// Toggle soft-resets for I2C0 and SDIO.
    ///
    /// Reference: `mango_common.c:bm_ip_reset()`
    fn ip_reset(&self) {
        // I2C0 soft-reset toggle (SOFT_RST0 bit 13)
        // mango_common.c:42 mmio_clrbits_32(TOP+SOFT_RST0, BIT(13))
        self.clear_bits(REG_SOFT_RST0, 1 << 13);
        fstart_arch::udelay(10);
        // mango_common.c:44 mmio_setbits_32(TOP+SOFT_RST0, BIT(13))
        self.set_bits(REG_SOFT_RST0, 1 << 13);
        fstart_arch::udelay(10);

        // SDIO soft-reset toggle (SOFT_RST0 bit 28)
        // mango_common.c:46 mmio_clrbits_32(TOP+SOFT_RST0, BIT(28))
        self.clear_bits(REG_SOFT_RST0, 1 << 28);
        fstart_arch::udelay(10);
        // mango_common.c:48 mmio_setbits_32(TOP+SOFT_RST0, BIT(28))
        self.set_bits(REG_SOFT_RST0, 1 << 28);
        fstart_arch::udelay(10);
    }

    /// Enable gate clocks required before peripheral use.
    fn enable_peripheral_clocks(&self) {
        for entry in GATE_CLK_TABLE.iter().filter(|e| e.enable_at_boot) {
            let offset = if entry.reg_offset == 0 {
                REG_CLOCK_ENABLE0
            } else {
                REG_CLOCK_ENABLE1
            };
            self.set_bits(offset, 1 << entry.bit);
        }
    }

    /// Read MCU board type over I2C1 and store in BOARD_TYPE_REG.
    ///
    /// Reference: `mango_bl2_setup.c:bm_get_board_info()`
    fn detect_board(&self) {
        let board_type = self.i2c_read_byte(self.mcu_addr, 0x00); // HW_TYPE_REG
        let _bom_ver = self.i2c_read_byte(self.mcu_addr, 0x02); // HW_VERSION_REG
        self.write(REG_BOARD_TYPE, board_type.unwrap_or(0) as u32);
    }

    /// Single-byte I2C SMBus read via direct register access on I2C1.
    fn i2c_read_byte(&self, dev_addr: u8, reg_addr: u8) -> Option<u8> {
        let base = self.i2c1_base as usize;
        // DesignWare APB I2C register offsets
        const IC_CON: usize = 0x00;
        const IC_TAR: usize = 0x04;
        const IC_DATA_CMD: usize = 0x10;
        const IC_STATUS: usize = 0x70;
        const IC_ENABLE: usize = 0x6C;

        unsafe fn reg_r(base: usize, off: usize) -> u32 {
            core::ptr::read_volatile((base + off) as *const u32)
        }
        unsafe fn reg_w(base: usize, off: usize, val: u32) {
            core::ptr::write_volatile((base + off) as *mut u32, val);
        }

        // SAFETY: I2C1 MMIO mapped as Device; single-threaded boot context.
        unsafe {
            reg_w(base, IC_ENABLE, 0);
            reg_w(base, IC_CON, 0x65); // master, fast 400kHz
            reg_w(base, IC_TAR, dev_addr as u32);
            reg_w(base, IC_ENABLE, 1);
            reg_w(base, IC_DATA_CMD, reg_addr as u32); // write reg addr
            reg_w(base, IC_DATA_CMD, 0x100); // read command
        }

        let mut retries = 1000u32;
        loop {
            let status = unsafe { reg_r(base, IC_STATUS) };
            if (status & 0x08) != 0 {
                break;
            }
            retries = retries.saturating_sub(1);
            if retries == 0 {
                return None;
            }
            fstart_arch::udelay(1);
        }
        let data = unsafe { reg_r(base, IC_DATA_CMD) };
        Some((data & 0xFF) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uart_500m_gate_at_boot() {
        let entry = GATE_CLK_TABLE
            .iter()
            .find(|e| e.id == gate_id::UART_500M)
            .expect("UART_500M not in gate table");
        assert_eq!(entry.reg_offset, 0, "must be in CLOCK_ENABLE0");
        assert!(entry.enable_at_boot);
    }

    #[test]
    fn test_apb_i2c_gate_at_boot() {
        let entry = GATE_CLK_TABLE
            .iter()
            .find(|e| e.id == gate_id::APB_I2C)
            .expect("APB_I2C not in gate table");
        assert!(entry.enable_at_boot);
    }

    #[test]
    fn test_conf_info_mode_sel_bits() {
        // Normal mode = 0 at bits [10:8]
        let conf: u32 = conf_info::MODE_NORMAL << conf_info::MODE_SEL_SHIFT;
        let mode = (conf >> conf_info::MODE_SEL_SHIFT) & conf_info::MODE_SEL_MASK;
        assert_eq!(mode, conf_info::MODE_NORMAL);
    }

    #[test]
    fn test_conf_info_socket_id_bits() {
        // Socket 0 means bits [17:16] = 0
        let conf: u32 = 0u32;
        let socket = (conf >> conf_info::SOCKET_ID_SHIFT) & conf_info::SOCKET_ID_MASK;
        assert_eq!(socket, 0);
    }
}
