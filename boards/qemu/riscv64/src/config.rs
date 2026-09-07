//! Static hardware policy only. Build geometry is platform Cargo metadata.

use fstart_platform_qemu::QemuRiscv64VirtConfig;

pub static QEMU_RISCV64_VIRT: QemuRiscv64VirtConfig = QemuRiscv64VirtConfig::new().build();
