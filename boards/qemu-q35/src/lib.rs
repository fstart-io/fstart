//! Host Rust board metadata for `qemu-q35`.

use fstart_board_qemu_q35_facts as facts;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardBuildPolicy, BoardConfig,
    BoardInfo, BuildInfo, BusAddress, Capability, Compression, CorebootSmmCompat, DeviceTopology,
    DigestAlgorithm, FdtSource, FirmwareImagePolicy, MemoryMap, MemoryRegion, MonolithicConfig,
    PayloadConfig, PayloadKind, Platform, RegionKind, SecurityConfig, SignatureAlgorithm,
    SmmConfig, SmmPlatform, SocImageFormat, StageLayout, TempRamBuffer,
};

pub const BOARD_NAME: &str = facts::BOARD_NAME;
pub const BOARD_PACKAGE: &str = facts::BOARD_PACKAGE;
pub const PLATFORM: Platform = Platform::X86_64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            (
                "flash",
                facts::FLASH_BASE,
                facts::FLASH_SIZE,
                RegionKind::Rom,
            ),
            (
                "workram",
                facts::WORKRAM_BASE,
                facts::WORKRAM_SIZE,
                RegionKind::Ram,
            ),
        ]),
        devices: DeviceTopology::new()
            .root(facts::UART0_NODE)
            .root(facts::FW_CFG0_NODE)
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
                Capability::MemoryDetect,
                Capability::MemoryInit,
                Capability::AcpiLoad,
                Capability::PciInit,
                Capability::MpInit {
                    max_cpus: facts::SMM_ENTRY_POINTS,
                    smm: true,
                },
                Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
                    temp_ram_buffer: Some(TempRamBuffer {
                        base: facts::FFS_TEMP_RAM_BASE,
                        size: facts::FFS_TEMP_RAM_SIZE,
                    }),
                }),
                Capability::SigVerify,
                Capability::DriverInit,
                Capability::PayloadLoad,
            ]),
            load_addr: facts::STAGE_LOAD_ADDR,
            stack_size: facts::STAGE_STACK_SIZE,
            heap_size: Some(facts::STAGE_HEAP_SIZE),
            data_addr: Some(facts::STAGE_DATA_ADDR),
            page_table_addr: Some((facts::PAGE_TABLE_ADDR, facts::PAGE_TABLE_SIZE)),
            page_size: fstart_types::stage::PageSize::Size1GiB,
        }),
        security: security_config([DigestAlgorithm::Sha256]),
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr(facts::KERNEL_FILE)),
            kernel_load_addr: Some(facts::KERNEL_LOAD_ADDR),
            fdt: FdtSource::Platform,
            dtb_addr: None,
            src_dtb_addr: None,
            bootargs: Some(hstr(facts::METADATA_BOOTARGS)),
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
        smm: Some(SmmConfig {
            platform: SmmPlatform::QemuQ35,
            entry_points: Some(facts::SMM_ENTRY_POINTS),
            stack_size: facts::SMM_STACK_SIZE,
            coreboot: CorebootSmmCompat {
                emit_header: true,
                module_args: true,
            },
        }),
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(
                facts::FFS_BASE,
                facts::FFS_SIZE_U64,
            ),
            pci_root_feature: Some(hstr("q35-hostbridge")),
            ..Default::default()
        },
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
    build_info_from_config(BOARD_NAME, BOARD_PACKAGE, &config, ["ns16550"])
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
