//! Reusable Rust board defaults for QEMU `virt` machines.
//!
//! Board crates should describe board-specific choices such as the stable board
//! name, Cargo package, payload file names, and load addresses. Fixed emulator
//! facts such as flash/RAM windows and default UART configuration live here.

use fstart_types::{
    hstr, Board, BoardConfig, BoardInfo, Build, BuildInfo, BuildProfile, Capability, Compression,
    DeviceTopology, DigestAlgorithm, FdtSource, FirmwareConfig, FirmwareKind, FlowProfile,
    ImageBuildInfo, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig, PayloadInputInfo,
    PayloadKind, Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat,
    StageBuildInfo, StageLayout,
};
use heapless::Vec as HVec;

/// QEMU RISC-V `virt` board defaults.
#[derive(Debug, Clone)]
pub struct QemuRiscv64VirtConfig {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
}

impl QemuRiscv64VirtConfig {
    /// Start with QEMU RISC-V `virt` defaults.
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: PayloadConfig {
                kind: PayloadKind::LinuxBoot,
                kernel_file: Some(hstr("vmlinux")),
                kernel_load_addr: Some(0x8200_0000),
                fdt: FdtSource::Platform,
                dtb_addr: Some(0x87f0_0000),
                src_dtb_addr: None,
                bootargs: Some(hstr("console=ttyS0 earlycon=sbi")),
                print_x86_mtrrs: false,
                compression: Compression::Lz4,
                firmware: Some(FirmwareConfig {
                    kind: FirmwareKind::OpenSbi,
                    file: hstr("fw_dynamic.bin"),
                    load_addr: 0x8010_0000,
                }),
                fit_file: None,
                fit_config: None,
                fit_parse: None,
            },
        }
    }

    /// Override the Linux/OpenSBI payload policy.
    #[must_use]
    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.payload = payload;
        self
    }

    /// Complete board facts for runtime hardware, stage policy, and packaging.
    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::Riscv64,
            memory: memory_map(&[
                ("flash", 0x2000_0000, 0x0200_0000, RegionKind::Rom),
                ("ram", 0x8000_0000, 0x0800_0000, RegionKind::Ram),
            ]),
            devices: single_device("uart0"),
            stages: monolithic_stage(0x2000_0000, 0x100000, Some(0x40000), Some(0x8100_0000)),
            security: security_config(),
            payload: Some(self.payload.clone()),
            microcode: None,
            soc_image_format: SocImageFormat::None,
            full_flash_image: false,
            acpi: None,
            smbios: None,
            smm: None,
            build: Default::default(),
            boot_hart_id: 0,
        }
    }

    /// Runtime hardware facts for static typed board mode.
    #[must_use]
    pub fn board_info(&self) -> BoardInfo {
        board_info_from_config(self.board_config())
    }

    /// Host build/package facts for this board.
    #[must_use]
    pub fn build_info(&self) -> BuildInfo {
        build_info_from_parts(
            self.board_name,
            self.board_package,
            Platform::Riscv64,
            0x2000_0000,
            self.board_config(),
            ["ns16550"],
        )
    }
}

/// QEMU AArch64 `virt` board defaults.
#[derive(Debug, Clone)]
pub struct QemuAarch64VirtConfig {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
}

impl QemuAarch64VirtConfig {
    /// Start with QEMU AArch64 `virt` defaults.
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: PayloadConfig {
                kind: PayloadKind::LinuxBoot,
                kernel_file: Some(hstr("Image")),
                kernel_load_addr: Some(0x4100_0000),
                fdt: FdtSource::Platform,
                dtb_addr: Some(0x4010_0000),
                src_dtb_addr: Some(0x4000_0000),
                bootargs: Some(hstr("console=ttyAMA0 earlycon=pl011,0x09000000")),
                print_x86_mtrrs: false,
                compression: Compression::Lz4,
                firmware: Some(FirmwareConfig {
                    kind: FirmwareKind::ArmTrustedFirmware,
                    file: hstr("bl31.bin"),
                    load_addr: 0x0e09_0000,
                }),
                fit_file: None,
                fit_config: None,
                fit_parse: None,
            },
        }
    }

    /// Override the Linux/ATF payload policy.
    #[must_use]
    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.payload = payload;
        self
    }

    /// Complete board facts for runtime hardware, stage policy, and packaging.
    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::Aarch64,
            memory: memory_map(&[
                ("flash", 0x0000_0000, 0x0800_0000, RegionKind::Rom),
                ("ram", 0x4000_0000, 0x0800_0000, RegionKind::Ram),
            ]),
            devices: single_device("uart0"),
            stages: monolithic_stage(0x0000_0000, 0x40000, Some(0x40000), Some(0x4020_0000)),
            security: security_config(),
            payload: Some(self.payload.clone()),
            microcode: None,
            soc_image_format: SocImageFormat::None,
            full_flash_image: false,
            acpi: None,
            smbios: None,
            smm: None,
            build: Default::default(),
            boot_hart_id: 0,
        }
    }

    /// Runtime hardware facts for static typed board mode.
    #[must_use]
    pub fn board_info(&self) -> BoardInfo {
        board_info_from_config(self.board_config())
    }

    /// Host build/package facts for this board.
    #[must_use]
    pub fn build_info(&self) -> BuildInfo {
        build_info_from_parts(
            self.board_name,
            self.board_package,
            Platform::Aarch64,
            0x0000_0000,
            self.board_config(),
            ["pl011"],
        )
    }
}

fn board_info_from_config(config: BoardConfig) -> BoardInfo {
    let mut board = Board::new(config.name.as_str())
        .platform(config.platform)
        .memory(config.memory);
    for device in config.devices {
        board = board.device(device);
    }
    if let Some(payload) = config.payload {
        board = board.payload(payload);
    }
    board.build()
}

fn build_info_from_parts(
    board_name: &str,
    board_package: &str,
    platform: Platform,
    stage_load_addr: u64,
    config: BoardConfig,
    driver_features: impl IntoIterator<Item = &'static str>,
) -> BuildInfo {
    let mut build = Build::new(board_name)
        .board_package(board_package)
        .target(platform.target_triple())
        .profile(BuildProfile::Dev)
        .flow_profile(FlowProfile::LinuxBoot)
        .image(ImageBuildInfo {
            full_flash_image: config.full_flash_image,
            soc_image_format: config.soc_image_format,
        })
        .stage(StageBuildInfo::new("stage", stage_load_addr))
        .feature(platform.as_str());

    for feature in driver_features {
        build = build.feature(feature);
    }

    if let Some(payload) = &config.payload {
        if let Some(kernel) = &payload.kernel_file {
            build = build.payload_input(PayloadInputInfo::new("kernel", kernel.as_str()));
        }
        if let Some(firmware) = &payload.firmware {
            build = build.payload_input(PayloadInputInfo::new("firmware", firmware.file.as_str()));
        }
    }

    build.build()
}

fn monolithic_stage(
    load_addr: u64,
    stack_size: u32,
    heap_size: Option<u32>,
    data_addr: Option<u64>,
) -> StageLayout {
    let mut capabilities = HVec::new();
    for capability in [
        Capability::ConsoleInit,
        Capability::MemoryInit,
        Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
            temp_ram_buffer: None,
        }),
        Capability::SigVerify,
        Capability::FdtPrepare,
        Capability::PayloadLoad,
    ] {
        capabilities.push(capability).expect("capability capacity");
    }

    StageLayout::Monolithic(MonolithicConfig {
        capabilities,
        load_addr,
        stack_size,
        heap_size,
        data_addr,
        page_table_addr: None,
        page_size: Default::default(),
    })
}

fn memory_map(regions: &[(&str, u64, u64, RegionKind)]) -> MemoryMap {
    let mut out = HVec::new();
    for (name, base, size, kind) in regions {
        out.push(MemoryRegion {
            name: hstr(name),
            base: *base,
            size: *size,
            kind: *kind,
        })
        .expect("memory map capacity");
    }
    MemoryMap {
        regions: out,
        flash_layout: None,
        car: None,
    }
}

fn single_device(name: &str) -> HVec<fstart_types::DeviceConfig, 32> {
    DeviceTopology::new().root(name).build()
}

fn security_config() -> SecurityConfig {
    let mut required_digests = HVec::new();
    required_digests
        .push(DigestAlgorithm::Sha256)
        .expect("digest capacity");
    required_digests
        .push(DigestAlgorithm::Sha3_256)
        .expect("digest capacity");

    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr("keys/dev-signing.pub"),
        required_digests,
    }
}
