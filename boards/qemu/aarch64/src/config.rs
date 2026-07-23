//! QEMU AArch64 virt static platform policy and host build metadata.

#[cfg(feature = "host")]
use fstart_core::{hstr, BoardConfig};
use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_platform_qemu::{
    qemu_aarch64_virt_linux_payload, qemu_aarch64_virt_memory, qemu_aarch64_virt_stages,
    qemu_arm_virt_build_policy, qemu_virt_security_config,
};
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuAarch64VirtConfig;

pub const BOARD_NAME: &str = "qemu-aarch64";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64";
pub const PLATFORM: Platform = Platform::Aarch64;

/// AArch64 virt defaults are platform data, retained in ROM rather than built
/// on the early-stage stack.
#[cfg(feature = "stage")]
pub static QEMU_AARCH64_VIRT: QemuAarch64VirtConfig = QemuAarch64VirtConfig::new().build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: qemu_aarch64_virt_memory(),
        stages: qemu_aarch64_virt_stages(),
        security: qemu_virt_security_config("keys/dev-signing.pub"),
        payload: Some(qemu_aarch64_virt_linux_payload()),
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: true,
        build: qemu_arm_virt_build_policy(),
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
