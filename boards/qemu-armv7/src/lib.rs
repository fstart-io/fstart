//! Rust board metadata for `qemu-armv7`.

#![cfg_attr(not(feature = "host"), no_std)]

#[cfg(feature = "host")]
use fstart_board_meta::{BindDriver, DriverBinding};
#[cfg(feature = "host")]
use fstart_driver_pl011::Pl011Config;
#[cfg(feature = "host")]
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, MemoryMap, MemoryRegion,
    MonolithicConfig, PayloadConfig, PayloadKind, Platform, RegionKind, SecurityConfig,
    SignatureAlgorithm, SocImageFormat, StageLayout,
};

pub const BOARD_NAME: &str = "qemu-armv7";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-armv7";
#[cfg(feature = "host")]
pub const PLATFORM: Platform = Platform::Armv7;

#[cfg(feature = "host")]
fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("flash", 0x0000_0000, 0x0800_0000, RegionKind::Rom),
            ("ram", 0x4000_0000, 0x0800_0000, RegionKind::Ram),
        ]),
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
            load_addr: 0x0000_0000,
            stack_size: 0x40000,
            heap_size: Some(0x40000),
            data_addr: Some(0x4020_0000),
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr("zImage")),
            kernel_load_addr: Some(0x4100_0000),
            fdt: FdtSource::Platform,
            dtb_addr: Some(0x40f0_0000),
            src_dtb_addr: Some(0x4000_0000),
            bootargs: Some(hstr("console=ttyAMA0 earlycon=pl011,mmio32,0x09000000")),
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
#[cfg(feature = "host")]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![Pl011Config {
        base_addr: 0x0900_0000,
        clock_freq: 1_843_200,
        baud_rate: 115_200,
        acpi_name: None,
        acpi_gsiv: None,
        acpi_dbg2: false,
    }
    .bind("uart0")]
}

#[must_use]
#[cfg(feature = "host")]
pub fn board_config() -> BoardConfig {
    config()
}

#[must_use]
#[cfg(feature = "host")]
pub fn board_info() -> BoardInfo {
    board_info_from_board_config()
}

#[must_use]
#[cfg(feature = "host")]
pub fn build_info() -> BuildInfo {
    build_info_from_board_config()
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
#[cfg(feature = "host")]
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

#[cfg(feature = "host")]
fn security_config<const N: usize>(digests: [DigestAlgorithm; N]) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr("keys/dev-signing.pub"),
        required_digests: hvec(digests),
    }
}

#[cfg(feature = "host")]
fn firmware_boot_media() -> Capability {
    Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
        temp_ram_buffer: None,
    })
}

#[cfg(feature = "host")]
fn board_info_from_board_config() -> BoardInfo {
    board_info_from_config(board_config())
}

#[cfg(feature = "host")]
fn build_info_from_board_config() -> BuildInfo {
    let config = board_config();
    let bindings = driver_bindings();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        bindings.iter().map(DriverBinding::driver_feature),
    )
}
