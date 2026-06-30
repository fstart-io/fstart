//! Shared Lichee RV Dock stage helpers and static driver configs.

use fstart_board_licheerv_dock_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_d1_ccu::SunxiD1CcuConfig;
use fstart_driver_sunxi_d1_dramc::SunxiD1DramcConfig;
use fstart_driver_sunxi_mmc::SunxiMmcConfig;
use fstart_services::{DeviceError, ServiceError};

pub static CCU_CONFIG: SunxiD1CcuConfig = SunxiD1CcuConfig {
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    uart_index: 0,
};

pub static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: facts::UART0_BASE,
        reg_shift: facts::UART0_REG_SHIFT,
        reg_width: facts::UART0_REG_WIDTH,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

pub static DRAMC_CONFIG: SunxiD1DramcConfig = SunxiD1DramcConfig {
    dram_clk: 792,
    dram_type: 3,
    dram_zq: 0x007b_7bfb,
    dram_odt_en: 0,
    dram_mr1: 0x42,
    dram_tpr11: 0x0034_0000,
    dram_tpr12: 0x46,
    dram_tpr13: 0x3400_0100,
};

pub static MMC0_CONFIG: SunxiMmcConfig = SunxiMmcConfig::Sun20iD1 {
    base_addr: facts::MMC0_BASE,
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    mmc_index: facts::MMC0_INDEX,
};

pub const fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
