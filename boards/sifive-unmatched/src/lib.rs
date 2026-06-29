//! Rust board metadata for SiFive HiFive Unmatched under QEMU `sifive_u`.

use fstart_device_registry::{DriverInstance, DriverInstanceBinding};
use fstart_driver_sifive_uart::SifiveUartConfig;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, FirmwareConfig,
    FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig, PayloadKind, Platform,
    RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageLayout,
};

pub const BOARD_NAME: &str = "sifive-unmatched";
pub const BOARD_PACKAGE: &str = "fstart-board-sifive-unmatched";
pub const PLATFORM: Platform = Platform::Riscv64;

fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([("ram", 0x8000_0000, 0x2000_0000, RegionKind::Ram)]),
        devices: DeviceTopology::new().root("uart0").build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::MemoryInit,
                firmware_boot_media(),
                Capability::SigVerify,
                Capability::FdtPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: 0x8000_0000,
            stack_size: 0x40000,
            heap_size: Some(0x40000),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: Some(linux_payload(
            "Image",
            0x8400_0000,
            FdtSource::Platform,
            0x8f00_0000,
            "console=ttySIF0 earlycon=sbi",
            0x8300_0000,
        )),
        microcode: None,
        soc_image_format: SocImageFormat::None,
        full_flash_image: false,
        acpi: None,
        smbios: None,
        smm: None,
        build: Default::default(),
        boot_hart_id: 1,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverInstanceBinding> {
    vec![DriverInstance::SifiveUart(SifiveUartConfig {
        base_addr: 0x1001_0000,
        clock_freq: 500_000_000,
        baud_rate: 115_200,
    })
    .bind("uart0")]
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
    let bindings = driver_bindings();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        bindings
            .iter()
            .filter_map(DriverInstanceBinding::driver_feature),
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

fn firmware_boot_media() -> Capability {
    Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
        temp_ram_buffer: None,
    })
}

fn linux_payload(
    kernel_file: &str,
    kernel_load_addr: u64,
    fdt: FdtSource,
    dtb_addr: u64,
    bootargs: &str,
    firmware_load_addr: u64,
) -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr(kernel_file)),
        kernel_load_addr: Some(kernel_load_addr),
        fdt,
        dtb_addr: Some(dtb_addr),
        src_dtb_addr: None,
        bootargs: Some(hstr(bootargs)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: Some(FirmwareConfig {
            kind: FirmwareKind::OpenSbi,
            file: hstr("fw_dynamic.bin"),
            load_addr: firmware_load_addr,
        }),
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}
