//! Shared QEMU virt policy and fixed-flow helpers.

use fstart_core::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm, services::ServiceError};
use fstart_pci::{
    PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind,
};
use serde::Serialize;

pub const QEMU_RISCV64_ECAM_BASE: u64 = 0x3000_0000;
/// Generic ECAM host description used by QEMU's architecture machines.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuPciRootConfig {
    pub ecam_base: u64,
    pub bus_end: u8,
    pub mmio32_base: u64,
    pub mmio32_size: u64,
    pub mmio64_base: u64,
    pub mmio64_size: u64,
}

impl QemuPciRootConfig {
    #[must_use]
    pub const fn new(
        ecam_base: u64,
        bus_end: u8,
        mmio32_base: u64,
        mmio32_size: u64,
        mmio64_base: u64,
        mmio64_size: u64,
    ) -> Self {
        Self {
            ecam_base,
            bus_end,
            mmio32_base,
            mmio32_size,
            mmio64_base,
            mmio64_size,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.ecam_base & 0x000f_ffff != 0 {
            panic!("QEMU PCI ECAM base must be 1 MiB aligned");
        }
        if self.mmio32_base.checked_add(self.mmio32_size).is_none()
            || self.mmio64_base.checked_add(self.mmio64_size).is_none()
        {
            panic!("QEMU PCI resource window overflows");
        }
        self
    }
}

impl PciRootProvider for QemuPciRootConfig {
    fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.ecam_base,
            bus_start: 0,
            bus_end: self.bus_end,
        }
    }

    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        let mut windows = PciRootWindows::new();
        let mut push = |window| {
            windows
                .push(window)
                .map_err(|_| PciRootError::TooManyWindows)
        };

        if self.mmio32_size != 0 {
            push(PciWindow {
                kind: PciWindowKind::Mmio,
                base: self.mmio32_base,
                size: self.mmio32_size,
                prefetchable: false,
            })?;
        }
        if self.mmio64_size != 0 {
            push(PciWindow {
                kind: PciWindowKind::Mmio,
                base: self.mmio64_base,
                size: self.mmio64_size,
                prefetchable: true,
            })?;
        }
        push(PciWindow {
            kind: PciWindowKind::Io,
            base: 0x1000,
            size: 0xf000,
            prefetchable: false,
        })?;
        Ok(windows)
    }
}

/// Closed RISC-V virt platform facts consumed by its fixed flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuRiscv64VirtConfig {
    pub ram_base: u64,
    pub bootargs: &'static str,
    pub pci: QemuPciRootConfig,
}

impl QemuRiscv64VirtConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ram_base: 0x8000_0000,
            bootargs: "console=ttyS0 earlycon=sbi",
            pci: QemuPciRootConfig::new(
                QEMU_RISCV64_ECAM_BASE,
                0xff,
                0x4000_0000,
                0x4000_0000,
                0x0000_0004_0000_0000,
                0x0000_0004_0000_0000,
            ),
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        assert!(self.ram_base != 0);
        let _ = self.pci.build();
        self
    }
}

impl Default for QemuRiscv64VirtConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Closed AArch64 virt platform facts consumed by its fixed flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuAarch64VirtConfig {
    pub ram_base: u64,
    pub bootargs: &'static str,
    pub pci: QemuPciRootConfig,
    /// Where QEMU copies the DTB for pflash/`-bios` boots (base of RAM).
    pub source_dtb_addr: u64,
}

impl QemuAarch64VirtConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ram_base: 0x4000_0000,
            bootargs: "console=ttyAMA0 earlycon=pl011,0x09000000",
            pci: QemuPciRootConfig::new(
                0x0040_1000_0000,
                0xff,
                0x1000_0000,
                0x2eff_0000,
                0x0080_0000_0000,
                0x0080_0000_0000,
            ),
            source_dtb_addr: 0x4000_0000,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        let _ = self.pci.build();
        self
    }
}

impl Default for QemuAarch64VirtConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Closed ARMv7 virt platform facts consumed by its fixed flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuArmv7VirtConfig {
    pub ram_base: u64,
    pub bootargs: &'static str,
    pub pci: QemuPciRootConfig,
    pub source_dtb_addr: u64,
}

impl QemuArmv7VirtConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ram_base: 0x4000_0000,
            bootargs: "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000",
            pci: QemuPciRootConfig::new(0x3f00_0000, 0x0f, 0x1000_0000, 0x2eff_0000, 0, 0),
            source_dtb_addr: 0x4000_0000,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        let _ = self.pci.build();
        self
    }
}

impl Default for QemuArmv7VirtConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// QEMU virt's development policy intentionally verifies both supported digests.
#[must_use]
pub fn qemu_virt_security_config(pubkey_file: &str) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: fstart_core::hstr(pubkey_file),
        required_digests: fstart_core::hvec([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
    }
}

/// Enumerate one generic QEMU ECAM root and allocate its device resources.
#[cfg(all(
    feature = "stage",
    any(target_arch = "aarch64", target_arch = "arm", target_arch = "riscv64")
))]
pub(crate) fn enumerate_pci(
    config: &QemuPciRootConfig,
) -> Result<fstart_pci::PciEcam, ServiceError> {
    let mut pci =
        fstart_pci::PciEcam::from_provider(config).map_err(|_| ServiceError::InvalidParam)?;
    pci.enumerate_and_allocate()
        .map_err(|_| ServiceError::HardwareError)?;
    Ok(pci)
}

/// Log one fixed-flow phase. The caller owns failure handling so each ISA can
/// halt with its own architecture helper.
pub fn phase(machine: &str, name: &str, result: Result<(), ServiceError>) -> bool {
    fstart_log::info!("{} ramstage: {}", machine, name);
    if result.is_err() {
        fstart_log::error!("{} ramstage: {} failed", machine, name);
        false
    } else {
        true
    }
}
