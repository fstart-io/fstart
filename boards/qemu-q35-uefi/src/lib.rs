//! Rust board metadata for `qemu-q35-uefi`.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_q35_hostbridge::Q35HostBridgeConfig;
use fstart_driver_qemu_fw_cfg::QemuFwCfgConfig;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardBuildPolicy, BoardConfig,
    BoardInfo, BuildInfo, Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource,
    FirmwareImagePolicy, MemoryMap, MemoryRegion, PayloadConfig, PayloadKind, Platform, RegionKind,
    RunsFrom, SecurityConfig, SignatureAlgorithm, SocImageFormat, StageConfig, StageLayout,
    TempRamBuffer,
};

pub const BOARD_NAME: &str = "qemu-q35-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-q35-uefi";
pub const PLATFORM: Platform = Platform::X86_64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("flash", 0xff80_0000, 0x0080_0000, RegionKind::Rom),
            ("workram", 0x0010_0000, 0x03f0_0000, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new()
            .root("uart0")
            .root("fw_cfg0")
            .root("pci0")
            .build(),
        stages: StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                capabilities: hvec([
                    Capability::ConsoleInit,
                    firmware_boot_media(None),
                    Capability::StageLoad {
                        next_stage: hstr("main"),
                    },
                ]),
                load_addr: 0xff80_0000,
                stack_size: 0x40000,
                heap_size: Some(0x1000),
                runs_from: RunsFrom::Rom,
                compression: Compression::None,
                data_addr: Some(0x100000),
                page_table_addr: Some((0x1000, 0x4000)),
                page_size: fstart_types::stage::PageSize::Size1GiB,
            },
            StageConfig {
                name: hstr("main"),
                capabilities: hvec([
                    Capability::ConsoleInit,
                    Capability::MemoryDetect,
                    Capability::MemoryInit,
                    Capability::AcpiLoad,
                    Capability::PciInit,
                    firmware_boot_media(Some(TempRamBuffer {
                        base: 0x0200_0000,
                        size: 0x0100_0000,
                    })),
                    Capability::SigVerify,
                    Capability::PayloadLoad,
                ]),
                load_addr: 0x0100_0000,
                stack_size: 0x400000,
                heap_size: Some(0x100000),
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: Some(0x0100_0000),
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: security_config([DigestAlgorithm::Sha256]),
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
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(0xff90_0000, 0x006f_f000),
            pci_root_feature: Some(hstr("q35-hostbridge")),
        },
        boot_hart_id: 0,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![
        DriverInstance::Ns16550(Ns16550Config {
            regs: AccessMode::Pio { base: 0x3f8 },
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        })
        .bind("uart0"),
        DriverInstance::QemuFwCfg(QemuFwCfgConfig {
            ctl_port: 0x510,
            data_port: 0x511,
        })
        .bind("fw_cfg0"),
        DriverInstance::Q35HostBridge(Q35HostBridgeConfig {
            ecam_base: 0xb000_0000,
            ecam_size: 0x1000_0000,
            bus_start: 0,
            bus_end: 255,
        })
        .bind("pci0"),
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

fn firmware_boot_media(temp_ram_buffer: Option<TempRamBuffer>) -> Capability {
    Capability::BootMedia(fstart_types::BootMedium::FirmwareImage { temp_ram_buffer })
}
