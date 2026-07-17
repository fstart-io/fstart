//! QEMU `sifive_u` static policy and host build metadata.

#[cfg(feature = "host")]
use fstart_core::{hstr, BoardConfig};
use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_platform_qemu::{
    qemu_sifive_u_build_policy, qemu_sifive_u_linux_payload, qemu_sifive_u_memory,
    qemu_sifive_u_stages, qemu_virt_security_config,
};
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuSifiveUConfig;
#[cfg(feature = "stage")]
use fstart_core::mmio32;
#[cfg(feature = "stage")]
use fstart_driver_uart::sifive::SifiveUartConfig;

pub const BOARD_NAME: &str = "qemu-sifive-u";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-sifive-u";
pub const PLATFORM: Platform = Platform::Riscv64;

/// QEMU `sifive_u` platform defaults, retained in ROM.
#[cfg(feature = "stage")]
pub static QEMU_SIFIVE_U: QemuSifiveUConfig = QemuSifiveUConfig::new().build();

/// QEMU's FU740-compatible UART wiring and precomputed divisor.
#[cfg(feature = "stage")]
pub static QEMU_SIFIVE_U_UART: SifiveUartConfig =
    SifiveUartConfig::new(mmio32(0x1001_0000), 500_000_000, 115_200);

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: qemu_sifive_u_memory(),
        stages: qemu_sifive_u_stages(),
        security: qemu_virt_security_config("keys/dev-signing.pub"),
        payload: Some(qemu_sifive_u_linux_payload()),
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: false,
        build: qemu_sifive_u_build_policy(),
        acpi: None,
        smbios: None,
        smm: None,
        // QEMU models FU740's S7 monitor hart at 0; boot the first U74 at 1.
        boot_hart_id: 1,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
