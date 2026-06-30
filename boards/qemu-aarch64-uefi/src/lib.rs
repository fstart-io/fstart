//! Host Rust board metadata for `qemu-aarch64-uefi`.

use fstart_board_qemu_aarch64_uefi_facts as facts;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    BusAddress, Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource,
    FirmwareConfig, FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig,
    PayloadKind, Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat,
    StageLayout,
};

pub const BOARD_NAME: &str = facts::BOARD_NAME;
pub const BOARD_PACKAGE: &str = facts::BOARD_PACKAGE;
pub const PLATFORM: Platform = Platform::Aarch64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            (
                "flash",
                facts::FLASH_BASE,
                facts::FLASH_SIZE_U64,
                RegionKind::Rom,
            ),
            ("ram", facts::RAM_BASE, facts::RAM_SIZE, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new()
            .root(facts::UART0_NODE)
            .root(facts::PCI0_NODE)
            .child(
                facts::PCI0_NODE,
                facts::BOCHS0_NODE,
                BusAddress::Pci(facts::BOCHS0_DEVICE, facts::BOCHS0_FUNCTION),
            )
            .build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ConsoleInit,
                firmware_boot_media(),
                Capability::MemoryInit,
                Capability::PciInit,
                Capability::DriverInit,
                Capability::PayloadLoad,
            ]),
            load_addr: facts::STAGE_LOAD_ADDR,
            stack_size: facts::STAGE_STACK_SIZE,
            heap_size: Some(facts::STAGE_HEAP_SIZE),
            data_addr: Some(facts::STAGE_DATA_ADDR),
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: Some(PayloadConfig {
            kind: PayloadKind::UefiPayload,
            kernel_file: None,
            kernel_load_addr: None,
            fdt: FdtSource::Platform,
            dtb_addr: None,
            src_dtb_addr: None,
            bootargs: None,
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::ArmTrustedFirmware,
                file: hstr(facts::FIRMWARE_FILE),
                load_addr: facts::FIRMWARE_LOAD_ADDR,
            }),
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

fn firmware_boot_media() -> Capability {
    Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
        temp_ram_buffer: None,
    })
}
