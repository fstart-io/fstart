//! Shared QEMU virt policy and fixed-flow helpers.

#[cfg(feature = "host")]
use fstart_core::{
    BoardBuildPolicy, Compression, FdtSource, FirmwareConfig, FirmwareImageConfig,
    FirmwareImagePolicy, FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig,
    PayloadKind, RegionKind, StageBuildConfig, StageLayout, hstr, hvec,
};
use fstart_core::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm, services::ServiceError};
use fstart_pci::{
    PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind,
};
use serde::Serialize;

pub const QEMU_RISCV64_ECAM_BASE: u64 = 0x3000_0000;
pub const QEMU_AARCH64_FLASH_BANK_SIZE: u64 = 0x0400_0000;
pub const QEMU_AARCH64_STAGE_STACK_SIZE: u32 = 0x30_0000;
pub const QEMU_AARCH64_STAGE_HEAP_SIZE: u32 = 0x10_0000;
pub const QEMU_AARCH64_STAGE_DATA_ADDR: u64 = 0x4020_0000;
pub const QEMU_ARMV7_STAGE_STACK_SIZE: u32 = 0x40_000;
pub const QEMU_ARMV7_STAGE_HEAP_SIZE: u32 = 0x40_000;
pub const QEMU_ARMV7_STAGE_DATA_ADDR: u64 = 0x4020_0000;

/// Legacy ARM virt policy; RISC-V build geometry is metadata-owned.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuVirtConfig {
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub bootargs: &'static str,
}

impl QemuVirtConfig {
    #[must_use]
    pub const fn new(
        firmware_base: u64,
        firmware_size: u64,
        ram_base: u64,
        ram_size: u64,
        dtb_addr: u64,
        kernel_addr: u64,
        firmware_addr: u64,
        bootargs: &'static str,
    ) -> Self {
        Self {
            firmware_base,
            firmware_size,
            ram_base,
            ram_size,
            dtb_addr,
            kernel_addr,
            firmware_addr,
            bootargs,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0 || self.ram_size == 0 {
            panic!("QEMU virt firmware and RAM windows must not be empty");
        }
        if self.firmware_base.checked_add(self.firmware_size).is_none()
            || self.ram_base.checked_add(self.ram_size).is_none()
        {
            panic!("QEMU virt address window overflows");
        }
        self
    }
}

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
    pub common: QemuVirtConfig,
    pub flash_base: u64,
    pub flash_size: u64,
    pub pci: QemuPciRootConfig,
    /// Where QEMU copies the DTB for pflash/`-bios` boots (base of RAM).
    pub source_dtb_addr: u64,
}

impl QemuAarch64VirtConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            common: QemuVirtConfig::new(
                0x0400_0000,
                0x0400_0000,
                0x4000_0000,
                0x0800_0000,
                0x4010_0000,
                0x4100_0000,
                0x0e09_0000,
                "console=ttyAMA0 earlycon=pl011,0x09000000",
            ),
            flash_base: 0,
            flash_size: QEMU_AARCH64_FLASH_BANK_SIZE * 2,
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
        let _ = self.common.build();
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
    pub common: QemuVirtConfig,
    pub pci: QemuPciRootConfig,
    pub source_dtb_addr: u64,
}

impl QemuArmv7VirtConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            common: QemuVirtConfig::new(
                0x0400_0000,
                0x0400_0000,
                0x4000_0000,
                0x0800_0000,
                0x40f0_0000,
                0x4100_0000,
                0,
                "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000",
            ),
            pci: QemuPciRootConfig::new(0x3f00_0000, 0x0f, 0x1000_0000, 0x2eff_0000, 0, 0),
            source_dtb_addr: 0x4000_0000,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        let _ = self.common.build();
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

/// Shared host metadata for the AArch64 QEMU virt platform.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_aarch64_virt_memory() -> MemoryMap {
    qemu_arm_virt_memory(&QemuAarch64VirtConfig::new().common)
}

/// Shared host metadata for the ARMv7 QEMU virt platform.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_armv7_virt_memory() -> MemoryMap {
    qemu_arm_virt_memory(&QemuArmv7VirtConfig::new().common)
}

#[cfg(feature = "host")]
fn qemu_arm_virt_memory(config: &QemuVirtConfig) -> MemoryMap {
    MemoryMap {
        regions: hvec([
            MemoryRegion {
                name: hstr("flash0"),
                base: 0,
                size: QEMU_AARCH64_FLASH_BANK_SIZE,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("flash1"),
                base: config.firmware_base,
                size: config.firmware_size,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("ram"),
                base: config.ram_base,
                size: config.ram_size,
                kind: RegionKind::Ram,
            },
        ]),
        flash_layout: None,
        car: None,
    }
}

/// Shared Linux payload defaults for AArch64 QEMU virt.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_aarch64_virt_linux_payload() -> PayloadConfig {
    let config = QemuAarch64VirtConfig::new().common;
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("Image")),
        kernel_load_addr: Some(config.kernel_addr),
        fdt: FdtSource::Platform,
        dtb_addr: Some(config.dtb_addr),
        src_dtb_addr: Some(config.ram_base),
        bootargs: Some(hstr(config.bootargs)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: Some(FirmwareConfig {
            kind: FirmwareKind::ArmTrustedFirmware,
            file: hstr("bl31.bin"),
            load_addr: config.firmware_addr,
        }),
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Shared Linux payload defaults for ARMv7 QEMU virt.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_armv7_virt_linux_payload() -> PayloadConfig {
    let config = QemuArmv7VirtConfig::new();
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("zImage")),
        kernel_load_addr: Some(config.common.kernel_addr),
        fdt: FdtSource::Platform,
        dtb_addr: Some(config.common.dtb_addr),
        src_dtb_addr: Some(config.source_dtb_addr),
        bootargs: Some(hstr(config.common.bootargs)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Shared monolithic stage policy for AArch64 QEMU virt.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_aarch64_virt_stages() -> StageLayout {
    // Leave 0x4000_0000..0x4040_0000 for QEMU's source/final DTBs and
    // CrabEFI's 0x4020_0000 data area; keep the stage below the 0x4100_0000
    // kernel load address. The reset stub remains at flash offset zero and
    // relocates this RAM-linked image before the shared AArch64 entry runs.
    qemu_virt_stages(
        0x4040_0000,
        QEMU_AARCH64_STAGE_STACK_SIZE,
        QEMU_AARCH64_STAGE_HEAP_SIZE,
        QEMU_AARCH64_STAGE_DATA_ADDR,
    )
}

/// Shared monolithic stage policy for ARMv7 QEMU virt.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_armv7_virt_stages() -> StageLayout {
    qemu_virt_stages(
        0,
        QEMU_ARMV7_STAGE_STACK_SIZE,
        QEMU_ARMV7_STAGE_HEAP_SIZE,
        QEMU_ARMV7_STAGE_DATA_ADDR,
    )
}

#[cfg(feature = "host")]
fn qemu_virt_stages(
    load_addr: u64,
    stack_size: u32,
    heap_size: u32,
    data_addr: u64,
) -> StageLayout {
    StageLayout::Monolithic(MonolithicConfig {
        build: StageBuildConfig {
            firmware_image: Some(FirmwareImageConfig {
                temp_ram_buffer: None,
            }),
            verify_firmware: true,
            payload: true,
            fdt: true,
            pci: true,
            ..StageBuildConfig::default()
        },
        load_addr,
        stack_size,
        heap_size: Some(heap_size),
        data_addr: Some(data_addr),
        page_table_addr: None,
        page_size: Default::default(),
    })
}

/// Shared image policy for ARM QEMU virt platforms.
#[cfg(feature = "host")]
#[must_use]
pub fn qemu_arm_virt_build_policy() -> BoardBuildPolicy {
    BoardBuildPolicy {
        qemu_machine: None,
        firmware_image: FirmwareImagePolicy::memory_mapped(
            QEMU_AARCH64_FLASH_BANK_SIZE,
            QEMU_AARCH64_FLASH_BANK_SIZE,
        ),
        flash_image: Some(FirmwareImagePolicy::memory_mapped(
            0,
            QEMU_AARCH64_FLASH_BANK_SIZE * 2,
        )),
        pci_root_feature: None,
        cpu_feature: None,
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
