//! QEMU RISC-V virt static platform policy and host build metadata.

#[cfg(feature = "host")]
use fstart_core::{hstr, BoardConfig};
use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_platform_qemu::{
    qemu_riscv64_virt_build_policy, qemu_riscv64_virt_linux_payload,
    qemu_riscv64_virt_memory, qemu_riscv64_virt_stages, qemu_virt_security_config,
};
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuRiscv64VirtConfig;

pub const BOARD_NAME: &str = "qemu-riscv64";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-riscv64";
pub const PLATFORM: Platform = Platform::Riscv64;

/// RISC-V virt defaults are platform data, retained in ROM rather than built
/// on the early-stage stack.
#[cfg(feature = "stage")]
pub static QEMU_RISCV64_VIRT: QemuRiscv64VirtConfig = QemuRiscv64VirtConfig::new().build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: qemu_riscv64_virt_memory(),
        stages: qemu_riscv64_virt_stages(),
        security: qemu_virt_security_config("keys/dev-signing.pub"),
        payload: Some(qemu_riscv64_virt_linux_payload()),
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: true,
        build: qemu_riscv64_virt_build_policy(),
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
