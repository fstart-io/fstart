//! Rust board metadata for QEMU SBSA-ref.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_pci_ecam::PciEcamConfig;
use fstart_driver_pl011::Pl011Config;
use fstart_types::acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiExtraDevice, AcpiIort, AcpiPlatform, AcpiWatchdog,
    AcpiXhciDevice, ArmPlatformAcpi,
};
use fstart_types::smbios::{
    CacheAssociativity, CacheType, ChassisType, MemoryDeviceType, ProcessorFamily, SmbiosCache,
    SmbiosMemoryDevice, SmbiosProcessor,
};
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, DeviceTopology, DigestAlgorithm, MemoryMap, MemoryRegion, MonolithicConfig,
    Platform, RegionKind, SecurityConfig, SignatureAlgorithm, SmbiosConfig, SocImageFormat,
    StageLayout,
};

pub const BOARD_NAME: &str = "qemu-sbsa";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-sbsa";
pub const PLATFORM: Platform = Platform::Aarch64;

#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("flash", 0x1000_0000, 0x1000_0000, RegionKind::Rom),
            ("ram", 0x100_0000_0000, 0x4000_0000, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new().root("uart0").root("pci0").build(),
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::MemoryInit,
                Capability::PciInit,
                Capability::AcpiPrepare,
                Capability::SmBiosPrepare,
            ]),
            load_addr: 0x100_0010_0000,
            stack_size: 0x40000,
            heap_size: Some(0x40000),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: security_config([DigestAlgorithm::Sha256, DigestAlgorithm::Sha3_256]),
        payload: None,
        microcode: None,
        soc_image_format: SocImageFormat::None,
        full_flash_image: false,
        acpi: Some(acpi_config()),
        smbios: Some(smbios_config()),
        smm: None,
        boot_hart_id: 0,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![
        DriverInstance::Pl011(Pl011Config {
            base_addr: 0x6000_0000,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
            acpi_name: Some(hstr("COM0")),
            acpi_gsiv: Some(33),
            acpi_dbg2: true,
        })
        .bind("uart0"),
        DriverInstance::PciEcam(PciEcamConfig {
            ecam_base: 0xf000_0000,
            ecam_size: 0x1000_0000,
            mmio32_base: 0x8000_0000,
            mmio32_size: 0x7000_0000,
            mmio64_base: 0x1_0000_0000,
            mmio64_size: 0xff_0000_0000,
            pio_base: 0x7fff_0000,
            pio_size: 0x10000,
            bus_start: 0,
            bus_end: 255,
        })
        .bind("pci0"),
    ]
}

#[must_use]
pub fn acpi_only_devices() -> Vec<AcpiExtraDevice> {
    vec![
        AcpiExtraDevice::Ahci(AcpiAhciDevice {
            name: hstr("AHC0"),
            base: 0x6010_0000,
            size: 0x10000,
            gsiv: 42,
        }),
        AcpiExtraDevice::Xhci(AcpiXhciDevice {
            name: hstr("USB0"),
            base: 0x6011_0000,
            size: 0x10000,
            gsiv: 43,
        }),
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

fn acpi_config() -> AcpiConfig {
    AcpiConfig {
        platform: AcpiPlatform::Arm(ArmPlatformAcpi {
            num_cpus: 1,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            gic_redist_length: Some(0x400_0000),
            gic_its_base: Some(0x4408_1000),
            timer_gsivs: (29, 30, 27, 26),
            watchdog: Some(AcpiWatchdog {
                refresh_base: 0x5001_0000,
                control_base: 0x5001_1000,
                gsiv: 48,
            }),
            iort: Some(AcpiIort {
                its_ids: hvec([0]),
                pci_segment: 0,
                memory_address_limit: 0x30,
                id_count: 0x10000,
            }),
        }),
        print_hex: true,
    }
}

fn smbios_config() -> SmbiosConfig {
    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr("03/10/2026"),
        system_manufacturer: hstr("QEMU"),
        system_product: hstr("SBSA Reference"),
        system_version: hstr("1.0"),
        system_serial: hstr(""),
        baseboard_manufacturer: hstr("QEMU"),
        baseboard_product: hstr("sbsa-ref"),
        chassis_type: ChassisType::RackMount,
        chassis_manufacturer: hstr("QEMU"),
        processors: hvec([SmbiosProcessor {
            socket: hstr("CPU0"),
            manufacturer: hstr("ARM"),
            processor_family: ProcessorFamily::Aarch64,
            max_speed_mhz: Some(2000),
            core_count: Some(1),
            thread_count: Some(1),
            caches: hvec([
                SmbiosCache {
                    designation: hstr("L1 Instruction Cache"),
                    level: 1,
                    size_kb: 64,
                    associativity: CacheAssociativity::Way4,
                    cache_type: CacheType::Instruction,
                },
                SmbiosCache {
                    designation: hstr("L1 Data Cache"),
                    level: 1,
                    size_kb: 64,
                    associativity: CacheAssociativity::Way4,
                    cache_type: CacheType::Data,
                },
                SmbiosCache {
                    designation: hstr("L2 Unified Cache"),
                    level: 2,
                    size_kb: 1024,
                    associativity: CacheAssociativity::Way8,
                    cache_type: CacheType::Unified,
                },
            ]),
        }]),
        memory_devices: hvec([SmbiosMemoryDevice {
            locator: hstr("DIMM0"),
            size_mb: Some(1024),
            speed_mhz: Some(2400),
            memory_type: Some(MemoryDeviceType::Ddr4),
        }]),
    }
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
