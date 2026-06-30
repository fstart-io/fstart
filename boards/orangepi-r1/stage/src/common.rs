//! Shared Orange Pi R1 stage helpers and static driver configs.

use fstart_board_orangepi_r1_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_h3_ccu::SunxiH3CcuConfig;
use fstart_driver_sunxi_h3_dramc::{SunxiDramcVariant, SunxiH3DramcConfig};
use fstart_driver_sunxi_mmc::SunxiMmcConfig;
use fstart_services::{DeviceError, ServiceError};

pub static CCU_CONFIG: SunxiH3CcuConfig = SunxiH3CcuConfig {
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    uart_index: 0,
};

pub const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: facts::UART0_BASE,
        reg_shift: facts::UART0_REG_SHIFT,
        reg_width: facts::UART0_REG_WIDTH,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

pub static DRAMC_CONFIG: SunxiH3DramcConfig = SunxiH3DramcConfig {
    dramc_base: facts::DRAMC_BASE,
    ccu_base: facts::CCU_BASE,
    clock: 624,
    zq: 3_881_979,
    odt_en: true,
    variant: SunxiDramcVariant::H3,
};

pub static MMC0_CONFIG: SunxiMmcConfig = SunxiMmcConfig::Sun8iH3 {
    base_addr: facts::MMC0_BASE,
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    mmc_index: facts::MMC0_INDEX,
};

pub const fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
