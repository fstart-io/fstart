//! Rust board metadata for `qemu-aarch64-uefi`.

use fstart_device_registry::{bochs_display, pci_ecam, pl011, DriverBinding, DriverInstance};
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    BusAddress, Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource,
    FirmwareConfig, FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig,
    PayloadKind, Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat,
    StageLayout,
};

pub const BOARD_NAME: &str = "qemu-aarch64-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64-uefi";
pub const PLATFORM: Platform = Platform::Aarch64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("flash", 0x0000_0000, 0x0800_0000, RegionKind::Rom),
            ("ram", 0x4000_0000, 0x1_0000_0000, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new()
            .root("uart0")
            .root("pci0")
            .child("pci0", "bochs0", BusAddress::Pci(3, 0))
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
            load_addr: 0,
            stack_size: 0x300000,
            heap_size: Some(0x100000),
            data_addr: Some(0x4020_0000),
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
                file: hstr("bl31.bin"),
                load_addr: 0x4010_0000,
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
        boot_hart_id: 0,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![
        DriverInstance::Pl011(pl011::Pl011Config {
            base_addr: 0x0900_0000,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
            acpi_name: None,
            acpi_gsiv: None,
            acpi_dbg2: false,
        })
        .bind("uart0"),
        DriverInstance::PciEcam(pci_ecam::PciEcamConfig {
            ecam_base: 0x0040_1000_0000,
            ecam_size: 0x1000_0000,
            mmio32_base: 0x1000_0000,
            mmio32_size: 0x2eff_0000,
            mmio64_base: 0x0080_0000_0000,
            mmio64_size: 0x0080_0000_0000,
            pio_base: 0x3eff_0000,
            pio_size: 0x10000,
            bus_start: 0,
            bus_end: 255,
        })
        .bind("pci0"),
        DriverInstance::BochsDisplay(bochs_display::BochsDisplayConfig {
            device: 3,
            function: 0,
            width: 1024,
            height: 768,
        })
        .bind("bochs0"),
    ]
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
        bindings.iter().filter_map(DriverBinding::driver_feature),
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
