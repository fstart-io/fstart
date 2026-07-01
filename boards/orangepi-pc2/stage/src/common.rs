//! Shared Orange Pi PC2 stage policy and static driver configs.

use fstart_board_orangepi_pc2_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_h3_ccu::{SunxiH3Ccu, SunxiH3CcuConfig};
use fstart_driver_sunxi_h3_dramc::{SunxiDramcVariant, SunxiH3Dramc, SunxiH3DramcConfig};
use fstart_driver_sunxi_mmc::{SunxiMmc, SunxiMmcConfig};
#[cfg(feature = "ffs")]
use fstart_services::boot::BootLinuxParams;
use fstart_stage::fixed_helpers::SunxiFixedFlowBoard;
#[cfg(feature = "ffs")]
use fstart_stage::fixed_helpers::SunxiLinuxBoard;

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
    clock: 672,
    zq: 3_881_977,
    odt_en: true,
    variant: SunxiDramcVariant::H5,
};

pub static MMC0_CONFIG: SunxiMmcConfig = SunxiMmcConfig::Sun50iH5 {
    base_addr: facts::MMC0_BASE,
    ccu_base: facts::CCU_BASE,
    pio_base: facts::PIO_BASE,
    mmc_index: facts::MMC0_INDEX,
};

pub struct SunxiBoard;

impl SunxiFixedFlowBoard for SunxiBoard {
    type Ccu = SunxiH3Ccu;
    type Dramc = SunxiH3Dramc;
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
    const FIRMWARE_LOAD_ADDR: u64 = facts::FIRMWARE_LOAD_ADDR;
    const BOOTARGS: &'static str = facts::BOOTARGS;
    const LOAD_FIRMWARE: bool = true;
    const FIRMWARE_NAME: &'static str = "BL31";
    const FDT_RAM_FROM_HANDOFF: bool = true;

    fn boot_linux(params: &BootLinuxParams<'_>) -> ! {
        crate::fstart_platform::boot_linux(params)
    }
}
