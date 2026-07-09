//! QEMU q35 board metadata and build policy.

#[cfg(feature = "host")]
use fstart_core::{dev_security_config, BoardConfig};
#[cfg(feature = "host")]
use fstart_core::hstr;
use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_platform_qemu::{qemu_q35_build_policy, qemu_q35_memory, qemu_q35_stages};
use fstart_platform_qemu::{
    QemuQ35Config, QEMU_Q35_UART_BAUD_RATE, QEMU_Q35_UART_CLOCK_FREQ, QEMU_Q35_UART_NODE,
    QEMU_Q35_UART_PIO_BASE,
};

pub const BOARD_NAME: &str = "qemu-q35";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-q35";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = QEMU_Q35_UART_NODE;
pub const UART0_PIO_BASE: u64 = QEMU_Q35_UART_PIO_BASE;
pub const UART0_CLOCK_FREQ: u32 = QEMU_Q35_UART_CLOCK_FREQ;
pub const UART0_BAUD_RATE: u32 = QEMU_Q35_UART_BAUD_RATE;

pub static QEMU_Q35_PLATFORM: QemuQ35Config = QemuQ35Config::new().build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: qemu_q35_memory(),
        stages: qemu_q35_stages(),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: None,
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: false,
        acpi: None,
        smbios: None,
        smm: None,
        build: qemu_q35_build_policy(),
        boot_hart_id: 0,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
