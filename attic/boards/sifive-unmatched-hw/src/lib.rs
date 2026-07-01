//! Host Rust board metadata for SiFive HiFive Unmatched hardware.

#![cfg_attr(feature = "stage", no_std)]
pub mod facts;
#[cfg(feature = "stage")]
pub mod stage;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BootMedium,
    BuildInfo, Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, FirmwareConfig,
    FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig, PayloadKind, Platform,
    RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageLayout,
};

pub const BOARD_NAME: &str = facts::BOARD_NAME;
pub const BOARD_PACKAGE: &str = facts::BOARD_PACKAGE;
pub const PLATFORM: Platform = Platform::Riscv64;

fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([(
            "lim",
            facts::FFS_BASE,
            facts::FFS_SIZE as u64,
            RegionKind::Ram,
        )]),
        devices: DeviceTopology::new()
            .root(facts::PRCI_NODE)
            .root(facts::UART0_NODE)
            .root(facts::DDR_NODE)
            .build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ClockInit,
                Capability::ConsoleInit,
                Capability::DramInit,
                Capability::BootMedia(BootMedium::FirmwareImage {
                    temp_ram_buffer: None,
                }),
                Capability::SigVerify,
                Capability::FdtPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: facts::FFS_BASE,
            stack_size: 0x4000,
            heap_size: Some(0x4000),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256]),
        payload: Some(linux_payload()),
        microcode: None,
        soc_image_format: SocImageFormat::None,
        full_flash_image: false,
        acpi: None,
        smbios: None,
        smm: None,
        build: Default::default(),
        boot_hart_id: facts::BOOT_HART_ID,
    }
}

#[must_use]
pub fn board_config() -> BoardConfig {
    config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board_info_from_config(board_config())
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let config = board_config();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        ["fu740-prci", "sifive-uart", "fu740-ddr"],
    )
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

fn memory_map<const N: usize>(regions: [(&str, u64, u64, RegionKind); N]) -> MemoryMap {
    MemoryMap {
        regions: hvec(regions.map(|(name, base, size, kind)| MemoryRegion {
            name: hstr(name),
            base,
            size,
            kind,
        })),
        flash_layout: None,
        car: None,
    }
}

fn security_config<const N: usize>(digests: [DigestAlgorithm; N]) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr("keys/dev-signing.pub"),
        required_digests: hvec(digests),
    }
}

fn linux_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr(facts::KERNEL_FILE)),
        kernel_load_addr: Some(facts::KERNEL_LOAD_ADDR),
        fdt: FdtSource::Override(hstr(facts::DTB_FILE)),
        dtb_addr: Some(facts::FDT_ADDR),
        src_dtb_addr: None,
        bootargs: Some(hstr(facts::BOOTARGS)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: Some(FirmwareConfig {
            kind: FirmwareKind::OpenSbi,
            file: hstr(facts::FIRMWARE_FILE),
            load_addr: facts::FIRMWARE_LOAD_ADDR,
        }),
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}
