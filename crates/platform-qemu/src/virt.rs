//! Shared QEMU virt policy and fixed-flow helpers.

use fstart_core::description::Range;
use fstart_core::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
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

/// One QEMU `virt` machine: where its RAM and PCI root are, and what its
/// payload is told.
#[derive(Debug, Clone, Copy)]
pub struct QemuVirtConfig {
    /// Log prefix, such as `qemu-riscv64`.
    pub name: &'static str,
    pub ram_base: u64,
    pub bootargs: &'static str,
    pub pci: QemuPciRootConfig,
    /// Where QEMU leaves the DTB when the boot register carries none: the
    /// base of RAM for Arm `-bios` boots. RISC-V always passes it in `a1`.
    pub dtb_fallback: u64,
    /// Device MMIO a UEFI payload maps.
    pub mmio: &'static [Range],
}

impl QemuVirtConfig {
    #[must_use]
    pub const fn riscv64() -> Self {
        Self {
            name: "qemu-riscv64",
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
            dtb_fallback: 0,
            mmio: &QEMU_RISCV64_MMIO,
        }
    }

    #[must_use]
    pub const fn aarch64() -> Self {
        Self {
            name: "qemu-aarch64",
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
            dtb_fallback: 0x4000_0000,
            mmio: &[],
        }
    }

    #[must_use]
    pub const fn armv7() -> Self {
        Self {
            name: "qemu-armv7",
            ram_base: 0x4000_0000,
            bootargs: "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000",
            pci: QemuPciRootConfig::new(0x3f00_0000, 0x0f, 0x1000_0000, 0x2eff_0000, 0, 0),
            dtb_fallback: 0x4000_0000,
            mmio: &[],
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        assert!(self.ram_base != 0);
        let _ = self.pci.build();
        self
    }
}

/// QEMU virt verifies both supported digests to exercise digest agility.
#[must_use]
pub fn qemu_virt_security_config() -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        required_digests: fstart_core::hvec([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
    }
}

/// Enumerate one generic QEMU ECAM root and allocate its device resources.
#[cfg(all(
    feature = "stage",
    any(
        target_arch = "aarch64",
        target_arch = "arm",
        all(feature = "virt-riscv64", target_arch = "riscv64")
    )
))]
pub(crate) fn enumerate_pci(
    config: &QemuPciRootConfig,
) -> Result<fstart_pci::PciEcam, fstart_core::services::ServiceError> {
    use fstart_core::services::ServiceError;
    let mut pci =
        fstart_pci::PciEcam::from_provider(config).map_err(|_| ServiceError::InvalidParam)?;
    pci.enumerate_and_allocate()
        .map_err(|_| ServiceError::HardwareError)?;
    Ok(pci)
}

/// The RISC-V virt machine's NS16550.
pub const QEMU_RISCV64_UART_BASE: u64 = 0x1000_0000;

/// RISC-V virt device MMIO a UEFI payload maps: the UART, CLINT, PLIC and
/// PCI memory window.
pub const QEMU_RISCV64_MMIO: [Range; 4] = [
    Range::new(0x1000_0000, 0x100),
    Range::new(0x0200_0000, 0x1_0000),
    Range::new(0x0c00_0000, 0x0400_0000),
    Range::new(0x4000_0000, 0x4000_0000),
];
/// The AArch64 virt machine's PL011.
pub const QEMU_AARCH64_UART_BASE: u64 = 0x0900_0000;
/// The ARMv7 virt machine's PL011.
pub const QEMU_ARMV7_UART_BASE: u64 = 0x0900_0000;
