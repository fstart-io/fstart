#[cfg(feature = "host")]
use fstart_core::{hstr, BoardConfig, Platform};
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuSbsaConfig;
#[cfg(feature = "host")]
use fstart_platform_qemu::{
    qemu_sbsa_build_policy, qemu_sbsa_memory, qemu_sbsa_stages, qemu_virt_security_config,
};

pub const BOARD_NAME: &str = "qemu-sbsa";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-sbsa";
#[cfg(feature = "host")]
pub const PLATFORM: Platform = Platform::Aarch64;

#[cfg(feature = "stage")]
pub static QEMU_SBSA: QemuSbsaConfig = QemuSbsaConfig::new().build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: qemu_sbsa_memory(),
        stages: qemu_sbsa_stages(),
        security: qemu_virt_security_config("keys/dev-signing.pub"),
        payload: None,
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: true,
        build: qemu_sbsa_build_policy(),
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
