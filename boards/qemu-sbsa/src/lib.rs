//! Host-side Rust board metadata for QEMU SBSA-ref.
//!
//! This crate is intentionally not firmware. It is loaded by `xtask` to produce
//! `BoardConfig`, `BuildInfo`, and driver metadata. The runnable firmware stage
//! lives in the separate `fstart-board-qemu-sbsa-stage` package.

use fstart_board_meta::{
    acpi_config_from_platform, ahci_extra_device, smbios_config_from_desc, xhci_extra_device,
};
use fstart_board_qemu_sbsa_facts as facts;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, DeviceTopology, DigestAlgorithm, FlowProfile, MemoryMap, MemoryRegion,
    MonolithicConfig, Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SocImageFormat,
    StageLayout,
};

pub use facts::{BOARD_NAME, BOARD_PACKAGE};

pub const PLATFORM: Platform = Platform::Aarch64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map(),
        devices: DeviceTopology::new()
            .root(facts::UART0_NODE)
            .root(facts::PCI0_NODE)
            .build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::MemoryInit,
                Capability::AcpiPrepare,
                Capability::SmBiosPrepare,
            ]),
            load_addr: facts::STAGE_LOAD_ADDR,
            stack_size: facts::STAGE_STACK_SIZE,
            heap_size: Some(facts::STAGE_HEAP_SIZE),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: None,
        microcode: None,
        soc_image_format: SocImageFormat::None,
        full_flash_image: false,
        acpi: Some(acpi_config_from_platform(&facts::ACPI_PLATFORM)),
        smbios: Some(smbios_config_from_desc(&facts::SMBIOS_DESC)),
        smm: None,
        build: Default::default(),
        boot_hart_id: 0,
    }
}

#[must_use]
pub fn acpi_only_devices() -> Vec<fstart_types::acpi::AcpiExtraDevice> {
    vec![
        ahci_extra_device(&facts::AHCI0_ACPI),
        xhci_extra_device(&facts::XHCI0_ACPI),
    ]
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board_info_from_config(board_config())
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let config = board_config();
    let mut info = build_info_from_config(BOARD_NAME, BOARD_PACKAGE, &config, []);
    // SBSA has no payload in this model, but it still needs the fixed-flow
    // handoff barrier to publish ACPI/SMBIOS tables before halting.
    info.flow_profile = FlowProfile::Uefi;
    info
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

fn memory_map() -> MemoryMap {
    MemoryMap {
        regions: hvec([
            MemoryRegion {
                name: hstr(facts::FLASH_NAME),
                base: facts::FLASH_BASE,
                size: facts::FLASH_SIZE,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr(facts::RAM_NAME),
                base: facts::RAM_BASE,
                size: facts::RAM_SIZE,
                kind: RegionKind::Ram,
            },
        ]),
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
