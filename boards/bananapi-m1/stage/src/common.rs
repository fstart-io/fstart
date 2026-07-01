//! Shared Banana Pi M1 stage policy and static driver configs.

use fstart_board_bananapi_m1_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_a20_dramc::{SunxiA20Dramc, SunxiA20DramcConfig};
use fstart_driver_sunxi_ccu::{SunxiA20Ccu, SunxiA20CcuConfig};
use fstart_driver_sunxi_mmc::{SunxiMmc, SunxiMmcConfig};
#[cfg(feature = "ffs")]
use fstart_services::boot::BootLinuxParams;
use fstart_stage::fixed_helpers::SunxiFixedFlowBoard;
#[cfg(feature = "ffs")]
use fstart_stage::fixed_helpers::SunxiLinuxBoard;

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

pub struct SunxiBoard;

impl SunxiFixedFlowBoard for SunxiBoard {
    type Ccu = SunxiA20Ccu;
    type Dramc = SunxiA20Dramc;
    type Mmc = SunxiMmc;

    const UART0_NODE: &'static str = facts::UART0_NODE;
    const MMC0_NODE: &'static str = facts::MMC0_NODE;
    const RAM_BASE: u64 = facts::RAM_BASE;
    const RAM_SIZE: u64 = facts::RAM_SIZE;
    const MMC_FIRMWARE_IMAGE_OFFSET: u64 = facts::MMC_FIRMWARE_IMAGE_OFFSET;
    const MAIN_LOAD_ADDR: u64 = facts::MAIN_LOAD_ADDR;
    const HANDOFF_ADDR: u64 = facts::HANDOFF_ADDR;

    fn ccu_config() -> <Self::Ccu as fstart_services::Device>::Config {
        CCU_CONFIG.clone()
    }

    fn dramc_config() -> <Self::Dramc as fstart_services::Device>::Config {
        DRAMC_CONFIG.clone()
    }

    fn mmc_config() -> <Self::Mmc as fstart_services::Device>::Config {
        MMC0_CONFIG.clone()
    }

    fn uart0_config() -> Ns16550Config {
        UART0_CONFIG
    }

    fn halt() -> ! {
        crate::fstart_platform::halt()
    }

    fn jump_to_main(main_addr: u64, handoff_addr: usize) -> ! {
        crate::fstart_platform::jump_to_with_handoff(main_addr, handoff_addr)
    }
}

#[cfg(feature = "ffs")]
impl SunxiLinuxBoard for SunxiBoard {
    const FDT_ADDR: u64 = facts::FDT_ADDR;
    const KERNEL_LOAD_ADDR: u64 = facts::KERNEL_LOAD_ADDR;
    const FIRMWARE_LOAD_ADDR: u64 = 0;
    const BOOTARGS: &'static str = facts::BOOTARGS;
    const LOAD_FIRMWARE: bool = false;
    const FIRMWARE_NAME: &'static str = "";
    const FDT_RAM_FROM_HANDOFF: bool = false;

    fn boot_linux(params: &BootLinuxParams<'_>) -> ! {
        crate::fstart_platform::boot_linux(params)
    }
}
