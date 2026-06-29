//! Rust board metadata for SiFive HiFive Unmatched hardware.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_fu740_ddr::Fu740DdrConfig;
use fstart_driver_fu740_prci::Fu740PrciConfig;
use fstart_driver_sifive_uart::SifiveUartConfig;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, FirmwareConfig,
    FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig, PayloadKind, Platform,
    RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageLayout,
};

pub const BOARD_NAME: &str = "sifive-unmatched-hw";
pub const BOARD_PACKAGE: &str = "fstart-board-sifive-unmatched-hw";
pub const PLATFORM: Platform = Platform::Riscv64;

fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([("lim", 0x0800_0000, 0x20_0000, RegionKind::Ram)]),
        devices: DeviceTopology::new()
            .root("prci0")
            .root("uart0")
            .root("ddr0")
            .build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ClockInit,
                Capability::ConsoleInit,
                Capability::DramInit,
                firmware_boot_media(),
                Capability::SigVerify,
                Capability::FdtPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: 0x0800_0000,
            stack_size: 0x4000,
            heap_size: Some(0x4000),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256]),
        payload: Some(linux_payload(
            "Image",
            0x8400_0000,
            FdtSource::Override(hstr("unmatched.dtb")),
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
        boot_hart_id: 1,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![
        DriverInstance::Fu740Prci(Fu740PrciConfig {
            base_addr: 0x1000_0000,
            gpio_base: 0x1006_0000,
        })
        .bind("prci0"),
        DriverInstance::SifiveUart(SifiveUartConfig {
            base_addr: 0x1001_0000,
            clock_freq: 130_000_000,
            baud_rate: 115_200,
        })
        .bind("uart0"),
        DriverInstance::Fu740Ddr(Fu740DdrConfig {
            ctl_base: 0x100b_0000,
            phy_base: 0x100b_2000,
            filter_base: 0x100b_8000,
            dram_size: 0x4_0000_0000,
        })
        .bind("ddr0"),
    ]
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
