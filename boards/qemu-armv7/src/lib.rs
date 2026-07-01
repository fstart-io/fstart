//! Host Rust board metadata for `qemu-armv7`.

#![cfg_attr(feature = "stage", no_std)]

pub mod facts;
#[cfg(feature = "stage")]
pub mod stage;

use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BootMedium,
    BuildInfo, Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, MemoryMap,
    MemoryRegion, MonolithicConfig, PayloadConfig, PayloadKind, Platform, RegionKind,
    SecurityConfig, SignatureAlgorithm, SocImageFormat, StageLayout,
};

pub const BOARD_NAME: &str = facts::BOARD_NAME;
pub const BOARD_PACKAGE: &str = facts::BOARD_PACKAGE;
pub const PLATFORM: Platform = Platform::Armv7;

fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            (
                "flash",
                facts::FLASH_BASE,
                facts::FLASH_SIZE as u64,
                RegionKind::Rom,
            ),
            ("ram", facts::RAM_BASE, facts::RAM_SIZE, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new().root(facts::UART0_NODE).build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::MemoryInit,
                Capability::BootMedia(BootMedium::FirmwareImage {
                    temp_ram_buffer: None,
                }),
                Capability::SigVerify,
                Capability::FdtPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: facts::FLASH_BASE,
            stack_size: 0x40000,
            heap_size: Some(0x40000),
            data_addr: Some(0x4020_0000),
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr(facts::KERNEL_FILE)),
            kernel_load_addr: Some(facts::KERNEL_LOAD_ADDR),
            fdt: FdtSource::Platform,
            dtb_addr: Some(facts::FDT_ADDR),
            src_dtb_addr: Some(facts::SRC_FDT_ADDR),
            bootargs: Some(hstr(facts::BOOTARGS)),
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: None,
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
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
    build_info_from_config(BOARD_NAME, BOARD_PACKAGE, &config, ["pl011"])
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
