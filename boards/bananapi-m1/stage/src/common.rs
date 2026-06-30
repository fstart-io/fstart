//! Shared Banana Pi M1 stage helpers and static driver configs.

use fstart_board_bananapi_m1_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_a20_dramc::SunxiA20DramcConfig;
use fstart_driver_sunxi_ccu::SunxiA20CcuConfig;
use fstart_driver_sunxi_mmc::SunxiMmcConfig;
use fstart_services::{DeviceError, ServiceError};

pub static CCU_CONFIG: SunxiA20CcuConfig = SunxiA20CcuConfig {
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

pub static DRAMC_CONFIG: SunxiA20DramcConfig = SunxiA20DramcConfig {
    dramc_base: facts::DRAMC_BASE,
    ccu_base: facts::CCU_BASE,
    clock: 432,
    mbus_clock: 0,
    zq: 123,
    odt_en: false,
    cas: 6,
    tpr0: 0x3092_6692,
    tpr1: 0x1090,
    tpr2: 0x0001_a0c8,
    tpr3: 0,
    tpr4: 0,
    emr1: 4,
    emr2: 0,
    emr3: 0,
    dqs_gating_delay: 0,
    active_windowing: false,
};

pub static MMC0_CONFIG: SunxiMmcConfig = SunxiMmcConfig::Sun7iA20 {
    base_addr: facts::MMC0_BASE,
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    mmc_index: facts::MMC0_INDEX,
};

pub const fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
