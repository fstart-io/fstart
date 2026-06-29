//! Rust board metadata for `qemu-riscv64-multi`.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, Compression, DeviceTopology, DigestAlgorithm, MemoryMap, MemoryRegion, Platform,
    RegionKind, RunsFrom, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageConfig,
    StageLayout,
};

pub const BOARD_NAME: &str = "qemu-riscv64-multi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-riscv64-multi";
pub const PLATFORM: Platform = Platform::Riscv64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("flash", 0x2000_0000, 0x0200_0000, RegionKind::Rom),
            ("ram", 0x8000_0000, 0x0800_0000, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new().root("uart0").build(),
        stages: StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                capabilities: hvec([
                    Capability::ConsoleInit,
                    firmware_boot_media(),
                    Capability::SigVerify,
                    Capability::StageLoad {
                        next_stage: hstr("main"),
                    },
                ]),
                load_addr: 0x2000_0000,
                stack_size: 0x4000,
                heap_size: None,
                runs_from: RunsFrom::Rom,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("main"),
                capabilities: hvec([
                    Capability::ConsoleInit,
                    Capability::MemoryInit,
                    Capability::DriverInit,
                ]),
                load_addr: 0x8010_0000,
                stack_size: 0x10000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: None,
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
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![DriverInstance::Ns16550(Ns16550Config {
        regs: AccessMode::Mmio {
            base: 0x1000_0000,
            reg_shift: 0,
            reg_width: 0,
        },
        clock_freq: 3_686_400,
        baud_rate: 115_200,
    })
    .bind("uart0")]
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
