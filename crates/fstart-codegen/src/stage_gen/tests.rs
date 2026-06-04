use std::path::PathBuf;

use super::*;
use crate::ron_loader::{load_parsed_board, ParsedBoard};
use fstart_device_registry::{DriverInstance, ServiceSet};

fn services_for(instances: &[DriverInstance]) -> Vec<ServiceSet> {
    instances
        .iter()
        .map(DriverInstance::provided_services)
        .collect()
}

fn load_lenovo_x61_board() -> ParsedBoard {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    load_parsed_board(&manifest_dir.join("../../boards/lenovo-x61/board.ron"))
        .expect("lenovo-x61 board should parse")
}

fn fstart_main_source(source: &str) -> &str {
    let start = source
        .find("pub extern \"Rust\" fn fstart_main")
        .expect("generated source should contain fstart_main");
    &source[start..]
}

fn assert_ordered(source: &str, needles: &[&str], context: &str) {
    let mut start = 0;
    for needle in needles {
        let relative = source[start..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?} while checking {context}"));
        start += relative + needle.len();
    }
}

/// Helper: create a minimal parsed board for testing.
fn test_parsed_board(capabilities: heapless::Vec<Capability, 16>) -> ParsedBoard {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });

    let driver_instances = vec![DriverInstance::Ns16550(
        fstart_driver_ns16550::Ns16550Config {
            regs: fstart_driver_ns16550::AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        },
    )];

    let config = BoardConfig {
        name: HString::try_from("qemu-riscv64").unwrap(),
        platform: Platform::Riscv64,
        memory: MemoryMap {
            regions: {
                let mut v = heapless::Vec::new();
                let _ = v.push(MemoryRegion {
                    name: HString::try_from("ram").unwrap(),
                    base: 0x8000_0000,
                    size: 0x0800_0000,
                    kind: RegionKind::Ram,
                });
                v
            },
            flash_layout: None,
            car: None,
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities,
            load_addr: 0x8000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
            page_table_addr: None,
            page_size: fstart_types::stage::PageSize::default(),
        }),
        security: SecurityConfig {
            signing_algorithm: SignatureAlgorithm::Ed25519,
            pubkey_file: HString::try_from("keys/dev.pub").unwrap(),
            required_digests: {
                let mut v = heapless::Vec::new();
                let _ = v.push(DigestAlgorithm::Sha256);
                v
            },
        },
        mode: BuildMode::Rigid,
        payload: None,
        full_flash_image: false,
        microcode: None,
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    };

    let device_tree = vec![DeviceNode {
        parent: None,
        depth: 0,
    }];

    ParsedBoard {
        config,
        device_services: services_for(&driver_instances),
        acpi_only_devices: Vec::new(),
        driver_instances,
        device_tree,
    }
}
#[test]
fn test_memory_init_after_console() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::MemoryInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fstart_capabilities::memory_init()"),
        "should call memory_init"
    );
}

#[test]
fn test_memory_init_without_console_is_error() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::MemoryInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!"),
        "should emit compile_error for MemoryInit without ConsoleInit"
    );
}

#[test]
fn test_console_init_requires_console_service() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("i2c0").unwrap(),
    });
    let parsed = test_parsed_board_with_i2c_bus(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!") && source.contains("does not provide Console"),
        "should reject ConsoleInit on non-console device: {source}"
    );
}

#[test]
fn acpi_prepare_requires_board_acpi_config() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::AcpiPrepare);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!")
            && source.contains("AcpiPrepare capability requires top-level board.acpi config"),
        "should reject AcpiPrepare without board.acpi config: {source}"
    );
}

#[test]
fn smbios_prepare_requires_board_smbios_config() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::SmBiosPrepare);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!")
            && source.contains("SmBiosPrepare capability requires top-level board.smbios config"),
        "should reject SmBiosPrepare without board.smbios config: {source}"
    );
}

#[test]
fn fdt_override_requires_ffs_using_stage() {
    use fstart_types::{FdtSource, PayloadConfig, PayloadKind};

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::FdtPrepare);
    let mut parsed = test_parsed_board(caps);
    parsed.config.payload = Some(PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: None,
        kernel_load_addr: None,
        fdt: FdtSource::Override(heapless::String::try_from("board.dtb").unwrap()),
        dtb_addr: Some(0x87f0_0000),
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: false,
        compression: fstart_types::ffs::Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    });
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!")
            && source.contains("FdtPrepare with an override DTB requires an FFS-using stage"),
        "should reject FDT override stages without FFS users: {source}"
    );
}

// =======================================================================
// Bus hierarchy tests
// =======================================================================

/// Helper: create a parsed board with UART + I2C bus + I2C child device.
fn test_parsed_board_with_i2c_bus(capabilities: heapless::Vec<Capability, 16>) -> ParsedBoard {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();

    // Root device: UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });

    // Root device: I2C bus controller
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("i2c0").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });

    let driver_instances = vec![
        DriverInstance::Ns16550(fstart_driver_ns16550::Ns16550Config {
            regs: fstart_driver_ns16550::AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        }),
        DriverInstance::DesignwareI2c(fstart_driver_designware_i2c::DesignwareI2cConfig {
            base_addr: 0x1004_0000,
            clock_freq: 100_000_000,
            bus_speed: fstart_driver_designware_i2c::I2cSpeed::Fast,
        }),
    ];

    let config = BoardConfig {
        name: HString::try_from("test-i2c-board").unwrap(),
        platform: Platform::Riscv64,
        memory: MemoryMap {
            regions: {
                let mut v = heapless::Vec::new();
                let _ = v.push(MemoryRegion {
                    name: HString::try_from("ram").unwrap(),
                    base: 0x8000_0000,
                    size: 0x0800_0000,
                    kind: RegionKind::Ram,
                });
                v
            },
            flash_layout: None,
            car: None,
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities,
            load_addr: 0x8000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
            page_table_addr: None,
            page_size: fstart_types::stage::PageSize::default(),
        }),
        security: SecurityConfig {
            signing_algorithm: SignatureAlgorithm::Ed25519,
            pubkey_file: HString::try_from("keys/dev.pub").unwrap(),
            required_digests: {
                let mut v = heapless::Vec::new();
                let _ = v.push(DigestAlgorithm::Sha256);
                v
            },
        },
        mode: BuildMode::Rigid,
        payload: None,
        full_flash_image: false,
        microcode: None,
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    };

    let device_tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        }, // uart0
        DeviceNode {
            parent: None,
            depth: 0,
        }, // i2c0
    ];

    ParsedBoard {
        config,
        device_services: services_for(&driver_instances),
        acpi_only_devices: Vec::new(),
        driver_instances,
        device_tree,
    }
}

/// Minimal test helper: a plain-device NS16550 instance so tests can
/// construct `DriverInstance` vectors without pulling in a full driver
/// config every time.
fn test_ns16550_instance() -> DriverInstance {
    DriverInstance::Ns16550(fstart_device_registry::ns16550::Ns16550Config {
        regs: fstart_driver_ns16550::AccessMode::Mmio {
            base: 0x1000_0000,
            reg_shift: 0,
            reg_width: 0,
        },
        clock_freq: 3_686_400,
        baud_rate: 115_200,
    })
}

/// Minimal test helper: a bus-device DesignWare I2C instance.
fn test_dw_i2c_instance() -> DriverInstance {
    DriverInstance::DesignwareI2c(
        fstart_device_registry::designware_i2c::DesignwareI2cConfig {
            base_addr: 0x1004_0000,
            clock_freq: 100_000_000,
            bus_speed: fstart_driver_designware_i2c::I2cSpeed::Fast,
        },
    )
}

/// Minimal test helper: a bus-device Bochs display instance — a bus child
/// of a PCI host bridge. Used to exercise bus-device-child validation.
fn test_bochs_instance() -> DriverInstance {
    DriverInstance::BochsDisplay(fstart_device_registry::bochs_display::BochsDisplayConfig {
        device: 2,
        function: 0,
        width: 1024,
        height: 768,
    })
}

#[test]
fn test_validate_device_tree_all_roots() {
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("uart0").unwrap(),
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("i2c0").unwrap(),
            parent: None,
            bus: None,
            enabled: true,
        },
    ];

    let instances = vec![test_ns16550_instance(), test_dw_i2c_instance()];

    let tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        },
        DeviceNode {
            parent: None,
            depth: 0,
        },
    ];

    assert!(
        validate_device_tree(&devices, &instances, &tree, &services_for(&instances)).is_ok(),
        "all root devices should validate fine"
    );
}

#[test]
fn test_validate_device_tree_valid_bus_child() {
    // A BusDevice child (bochs-display) nested under a PCI host bridge
    // that provides `PciRootBus` — this is the shape that validation
    // should accept.
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("pci0").unwrap(),
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("bochs0").unwrap(),
            parent: Some(HString::try_from("pci0").unwrap()),
            bus: None,
            enabled: true,
        },
    ];

    let instances = vec![
        DriverInstance::PciEcam(fstart_device_registry::pci_ecam::PciEcamConfig {
            ecam_base: 0x3000_0000,
            ecam_size: 0x1000_0000,
            mmio32_base: 0x4000_0000,
            mmio32_size: 0x4000_0000,
            mmio64_base: 0x4_0000_0000,
            mmio64_size: 0x4_0000_0000,
            pio_base: 0x3eff_0000,
            pio_size: 0x1_0000,
            bus_start: 0,
            bus_end: 255,
        }),
        test_bochs_instance(),
    ];

    let tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        },
        DeviceNode {
            parent: Some(0),
            depth: 1,
        },
    ];

    assert!(
        validate_device_tree(&devices, &instances, &tree, &services_for(&instances)).is_ok(),
        "bus-device child on PciRootBus parent should validate"
    );
}

#[test]
fn test_validate_device_tree_non_bus_parent_is_error() {
    use fstart_types::*;
    use heapless::String as HString;

    // uart0 provides Console, NOT a bus service. bochs0 is a BusDevice
    // child, so validation *must* run for it and must reject the parent.
    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("uart0").unwrap(),
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("bochs0").unwrap(),
            parent: Some(HString::try_from("uart0").unwrap()),
            bus: None,
            enabled: true,
        },
    ];

    let instances = vec![test_ns16550_instance(), test_bochs_instance()];

    let tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        },
        DeviceNode {
            parent: Some(0),
            depth: 1,
        },
    ];

    let result = validate_device_tree(&devices, &instances, &tree, &services_for(&instances));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("does not provide a bus service"),
        "should reject non-bus parent for bus-device child"
    );
}

#[test]
fn test_validate_device_tree_plain_device_child_ok() {
    // A plain-device child (NS16550) nested under a non-bus parent is
    // allowed under the new ordering-only semantics: the NS16550 is
    // constructed via `Device::new(&cfg)` — the parent is only used
    // by `ensure_device_ready` for init ordering.
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("uart0").unwrap(),
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("uart1").unwrap(),
            parent: Some(HString::try_from("uart0").unwrap()),
            bus: None,
            enabled: true,
        },
    ];

    let instances = vec![test_ns16550_instance(), test_ns16550_instance()];

    let tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        },
        DeviceNode {
            parent: Some(0),
            depth: 1,
        },
    ];

    assert!(
        validate_device_tree(&devices, &instances, &tree, &services_for(&instances)).is_ok(),
        "plain-device child should be accepted even when parent provides no bus service"
    );
}
#[test]
fn test_i2c_bus_generates_embedded_hal_import() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board_with_i2c_bus(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("use fstart_services::i2c::"),
        "should import embedded-hal I2C traits from fstart_services: {source}"
    );
    assert!(source.contains("I2c"), "should import I2c trait: {source}");
}
#[test]
fn test_non_bus_parent_is_compile_error() {
    // A *bus-device* child whose parent provides no bus service is a
    // topology error — you cannot construct it via `new_on_bus(...)`
    // because the parent exposes no bus handle. (A plain-Device child
    // is fine: it's just init-ordering — see
    // `test_validate_device_tree_plain_device_child_ok`.)
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });

    let mut parsed = test_parsed_board(caps);
    // Add a bus-device child (CK505) nested under uart0 (index 0).
    // uart0 is Console — not any of the accepted bus services.
    let _ = parsed.config.devices.push(DeviceConfig {
        name: HString::try_from("ck0").unwrap(),
        parent: Some(HString::try_from("uart0").unwrap()),
        bus: Some(fstart_types::BusAddress::I2c(0x69)),
        enabled: true,
    });
    parsed.driver_instances.push(DriverInstance::I2cCk505(
        fstart_device_registry::i2c_ck505::I2cCk505Config {
            mask: heapless::Vec::new(),
            regs: heapless::Vec::new(),
        },
    ));
    // uart0 is at index 0
    parsed.device_tree.push(DeviceNode {
        parent: Some(0),
        depth: 1,
    });

    let source = generate_stage_source(&parsed, None);
    assert!(
        source.contains("compile_error!"),
        "should emit compile_error for non-bus parent of bus-device child: {source}"
    );
    assert!(
        source.contains("does not provide a bus service"),
        "error should mention bus service: {source}"
    );
}

// =======================================================================
// Multi-stage tests
// =======================================================================

/// Helper: create a multi-stage parsed board (bootblock + main).
fn test_multi_stage_parsed_board() -> ParsedBoard {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });

    let driver_instances = vec![DriverInstance::Ns16550(
        fstart_driver_ns16550::Ns16550Config {
            regs: fstart_driver_ns16550::AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        },
    )];

    let mut stages = heapless::Vec::new();
    let _ = stages.push(StageConfig {
        name: HString::try_from("bootblock").unwrap(),
        capabilities: {
            let mut v = heapless::Vec::new();
            let _ = v.push(Capability::ConsoleInit {
                device: HString::try_from("uart0").unwrap(),
            });
            let _ = v.push(Capability::BootMedia(BootMedium::FirmwareImage {
                provider: None,
                temp_ram_buffer: None,
            }));
            let _ = v.push(Capability::SigVerify);
            let _ = v.push(Capability::StageLoad {
                next_stage: HString::try_from("main").unwrap(),
            });
            v
        },
        load_addr: 0x8000_0000,
        stack_size: 0x4000,
        heap_size: None,
        runs_from: RunsFrom::Ram,
        compression: fstart_types::ffs::Compression::None,
        data_addr: None,
        page_table_addr: None,
        page_size: fstart_types::stage::PageSize::default(),
    });
    let _ = stages.push(StageConfig {
        name: HString::try_from("main").unwrap(),
        capabilities: {
            let mut v = heapless::Vec::new();
            let _ = v.push(Capability::ConsoleInit {
                device: HString::try_from("uart0").unwrap(),
            });
            let _ = v.push(Capability::MemoryInit);
            let _ = v.push(Capability::DriverInit);
            v
        },
        load_addr: 0x8010_0000,
        stack_size: 0x10000,
        heap_size: None,
        runs_from: RunsFrom::Ram,
        compression: fstart_types::ffs::Compression::None,
        data_addr: None,
        page_table_addr: None,
        page_size: fstart_types::stage::PageSize::default(),
    });

    let config = BoardConfig {
        name: HString::try_from("qemu-riscv64").unwrap(),
        platform: Platform::Riscv64,
        memory: MemoryMap {
            regions: {
                let mut v = heapless::Vec::new();
                let _ = v.push(MemoryRegion {
                    name: HString::try_from("ram").unwrap(),
                    base: 0x8000_0000,
                    size: 0x0800_0000,
                    kind: RegionKind::Ram,
                });
                v
            },
            flash_layout: None,
            car: None,
        },
        devices,
        stages: StageLayout::MultiStage(stages),
        security: SecurityConfig {
            signing_algorithm: SignatureAlgorithm::Ed25519,
            pubkey_file: HString::try_from("keys/dev.pub").unwrap(),
            required_digests: {
                let mut v = heapless::Vec::new();
                let _ = v.push(DigestAlgorithm::Sha256);
                v
            },
        },
        mode: BuildMode::Rigid,
        payload: None,
        full_flash_image: false,
        microcode: None,
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    };

    let device_tree = vec![DeviceNode {
        parent: None,
        depth: 0,
    }];

    ParsedBoard {
        config,
        device_services: services_for(&driver_instances),
        acpi_only_devices: Vec::new(),
        driver_instances,
        device_tree,
    }
}
#[test]
fn test_multi_stage_bootblock_no_completion_message() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, Some("bootblock"));

    // Bootblock ends with StageLoad — should NOT log completion
    assert!(
        !source.contains("all capabilities complete"),
        "bootblock should NOT log completion (ends with StageLoad): {source}"
    );
}
#[test]
fn test_multi_stage_missing_stage_name_is_error() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!"),
        "multi-stage without FSTART_STAGE_NAME should be compile_error: {source}"
    );
}

#[test]
fn test_multi_stage_unknown_stage_name_is_error() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, Some("nonexistent"));

    assert!(
        source.contains("compile_error!"),
        "unknown stage name should be compile_error: {source}"
    );
}

#[test]
fn test_stage_ending_with_payload_load_no_completion() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::FirmwareImage {
        provider: None,
        temp_ram_buffer: None,
    }));
    let _ = caps.push(Capability::PayloadLoad);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // Ends with PayloadLoad — should not log completion
    assert!(
        !source.contains("all capabilities complete"),
        "stage ending with PayloadLoad should NOT log completion: {source}"
    );
}
// =======================================================================
// Device tree table tests
// =======================================================================
// =======================================================================
// ConfigTokenSerializer — compound type tests
// =======================================================================

#[test]
fn test_config_ser_option_none() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Cfg {
        x: Option<u32>,
    }

    let s = serialize_to_tokens(&Cfg { x: None }).to_string();
    assert!(
        s.contains("None"),
        "Option::None field should produce None: {s}"
    );
}

#[test]
fn test_config_ser_option_some() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Cfg {
        x: Option<u32>,
    }

    let s = serialize_to_tokens(&Cfg { x: Some(42) }).to_string();
    assert!(
        s.contains("Some"),
        "Option::Some should produce Some(...): {s}"
    );
    assert!(s.contains("42u32"), "should contain inner value: {s}");
}

#[test]
fn test_config_ser_array_field() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Cfg {
        pins: [u8; 3],
    }

    let s = serialize_to_tokens(&Cfg { pins: [1, 2, 3] }).to_string();
    assert!(s.contains("pins"), "should have field name: {s}");
    assert!(s.contains("1u8"), "should have first element: {s}");
    assert!(s.contains("2u8"), "should have second element: {s}");
    assert!(s.contains("3u8"), "should have third element: {s}");
}

#[test]
fn test_config_ser_newtype_variant() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    enum Source {
        External(u32),
        #[allow(dead_code)]
        Internal(u32),
    }

    #[derive(serde::Serialize)]
    struct Cfg {
        src: Source,
    }

    let s = serialize_to_tokens(&Cfg {
        src: Source::External(100),
    })
    .to_string();
    assert!(s.contains("Source"), "newtype variant enum name: {s}");
    assert!(s.contains("External"), "newtype variant name: {s}");
    assert!(s.contains("100u32"), "inner value: {s}");
}

#[test]
fn test_config_ser_struct_variant() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    enum Mode {
        Custom { speed: u32, duplex: bool },
    }

    #[derive(serde::Serialize)]
    struct Cfg {
        mode: Mode,
    }

    let s = serialize_to_tokens(&Cfg {
        mode: Mode::Custom {
            speed: 9600,
            duplex: true,
        },
    })
    .to_string();
    assert!(s.contains("Mode"), "struct variant enum name: {s}");
    assert!(s.contains("Custom"), "struct variant name: {s}");
    assert!(s.contains("speed"), "struct variant field name: {s}");
    assert!(s.contains("9600u32"), "struct variant field value: {s}");
    assert!(s.contains("duplex"), "struct variant field name: {s}");
    assert!(s.contains("true"), "struct variant bool value: {s}");
}

#[test]
fn test_config_ser_tuple_variant() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    enum Pair {
        Coords(u32, u32),
    }

    #[derive(serde::Serialize)]
    struct Cfg {
        pos: Pair,
    }

    let s = serialize_to_tokens(&Cfg {
        pos: Pair::Coords(10, 20),
    })
    .to_string();
    assert!(s.contains("Pair"), "tuple variant enum name: {s}");
    assert!(s.contains("Coords"), "tuple variant name: {s}");
    assert!(s.contains("10u32"), "first element: {s}");
    assert!(s.contains("20u32"), "second element: {s}");
}

#[test]
fn test_config_ser_newtype_struct() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Addr(u64);

    #[derive(serde::Serialize)]
    struct Cfg {
        base: Addr,
    }

    let s = serialize_to_tokens(&Cfg { base: Addr(0x1000) }).to_string();
    assert!(s.contains("Addr"), "newtype struct name: {s}");
    // u64 emits hex
    assert!(s.contains("0x1000"), "inner hex value: {s}");
}

#[test]
fn test_config_ser_unit_and_char() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Cfg {
        parity: char,
    }

    let s = serialize_to_tokens(&Cfg { parity: 'N' }).to_string();
    assert!(s.contains("'N'"), "char literal: {s}");
}

#[test]
fn test_config_ser_nested_option_in_struct() {
    use config_ser::serialize_to_tokens;

    #[derive(serde::Serialize)]
    struct Inner {
        val: u32,
    }

    #[derive(serde::Serialize)]
    struct Cfg {
        extra: Option<Inner>,
    }

    let none_s = serialize_to_tokens(&Cfg { extra: None }).to_string();
    assert!(
        none_s.contains("None"),
        "nested Option::None should be None: {none_s}"
    );

    let some_s = serialize_to_tokens(&Cfg {
        extra: Some(Inner { val: 7 }),
    })
    .to_string();
    assert!(some_s.contains("Some"), "nested Option::Some: {some_s}");
    assert!(
        some_s.contains("Inner"),
        "inner struct name in Some: {some_s}"
    );
    assert!(some_s.contains("7u32"), "inner struct value: {some_s}");
}

// =======================================================================
// Direct stage-flow tests.
// =======================================================================

#[test]
fn direct_flow_replaces_runtime_interpreter_entry() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::MemoryInit);
    let _ = caps.push(Capability::LateDriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        !source.contains("STAGE_PLAN") && !source.contains("fstart_stage_runtime::CapOp"),
        "generated stage should not emit legacy plan/interpreter metadata: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::handoff::try_deserialize(handoff_ptr)"),
        "direct fstart_main should consume the incoming handoff pointer when handoff is enabled: {source}"
    );
    assert!(
        source.contains("let mut board = _BoardDevices::new(handoff);"),
        "direct fstart_main should construct the board adapter with handoff state: {source}"
    );
    assert!(
        !source.contains("fstart_stage_runtime::run_stage("),
        "fstart_main should not pull in the generic CapOp interpreter: {source}"
    );
    assert!(
        source.contains("fstart_stage_runtime::Board::memory_init(&board);"),
        "MemoryInit should lower to a direct Board call: {source}"
    );
}

#[test]
fn direct_flow_driver_init_uses_board_batch_init_without_runtime_interpreter() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("let _no_skip = fstart_stage_runtime::DeviceMask::new();"),
        "DriverInit should preserve current empty-skip semantics: {source}"
    );
    assert!(
        source.contains("fstart_stage_runtime::Board::init_all_devices"),
        "DriverInit should lower to Board::init_all_devices: {source}"
    );
    assert!(
        source.contains("_inited.set(0);"),
        "DriverInit should mark runtime devices inited: {source}"
    );
    assert!(
        !source.contains("fstart_stage_runtime::run_stage("),
        "direct flow should not call the generic runtime interpreter: {source}"
    );
}

#[test]
fn direct_flow_lenovo_x61_bootblock_and_ramstage_calls_are_explicit() {
    // X61's generated source is large enough that prettyplease can exhaust the
    // default test-thread stack.  Generate it on a larger stack, then keep the
    // assertions to stable substrings.
    let (bootblock, ramstage) = std::thread::Builder::new()
        .name("x61-codegen-direct-flow".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let parsed = load_lenovo_x61_board();
            (
                generate_stage_source(&parsed, Some("bootblock")),
                generate_stage_source(&parsed, Some("ramstage")),
            )
        })
        .expect("spawn x61 codegen thread")
        .join()
        .expect("x61 codegen should not panic");

    assert!(
        !bootblock.contains("fstart_stage_runtime::run_stage(")
            && !ramstage.contains("fstart_stage_runtime::run_stage("),
        "X61 stages should use direct fstart_main codeflow"
    );

    let bootblock_main = fstart_main_source(&bootblock);
    let ramstage_main = fstart_main_source(&ramstage);

    assert_ordered(
        bootblock_main,
        &[
            "fstart_stage_runtime::Board::boot_media_firmware_image",
            "fstart_stage_runtime::Board::stage_load",
        ],
        "X61 bootblock boot media before stage load",
    );
    assert_ordered(
        ramstage_main,
        &[
            "fstart_stage_runtime::Board::boot_media_firmware_image",
            "fstart_stage_runtime::Board::sig_verify",
            "fstart_stage_runtime::Board::init_all_devices",
        ],
        "X61 ramstage boot media before signature before DriverInit",
    );
    assert!(
        ramstage_main.contains("TempRamBuffer")
            && ramstage_main.contains("base: 0x2000000")
            && ramstage_main.contains("size: 0x1000000"),
        "X61 ramstage FirmwareImage BootMedia must pass temp_ram_buffer arena through direct flow"
    );
    assert!(
        ramstage.contains("TempRamArena::new") && !ramstage.contains("copy_firmware_image_to_ram"),
        "X61 ramstage adapter must treat temp_ram_buffer as scratch RAM, not a whole-image copy"
    );

    for (source, needle, context) in [
        (
            bootblock.as_str(),
            "fstart_stage_runtime::Board::pre_console_init(&mut board, &[0, 1, 8])",
            "bootblock PreConsoleInit",
        ),
        (
            bootblock.as_str(),
            "fstart_stage_runtime::Board::early_init(&mut board, &[0, 1])",
            "bootblock EarlyInit",
        ),
        (
            bootblock.as_str(),
            "fstart_stage_runtime::Board::dram_init(&mut board, 0)",
            "bootblock DramInit",
        ),
        (
            bootblock.as_str(),
            "fstart_stage_runtime::Board::stage_load(&board, \"ramstage\")",
            "bootblock StageLoad",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::DeviceMask::from_slice(&[0])",
            "ramstage persistent DeviceMask",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::stage_local_init(&mut board, &[0])",
            "ramstage StageLocalInit",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::memory_detect(&mut board, 0)",
            "ramstage MemoryDetect",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::pci_init(&mut board, 0)",
            "ramstage PciInit",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::post_dram_init(&mut board, &[0, 1, 8])",
            "ramstage PostDramInit",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::finalize_init(&mut board, &[1, 8])",
            "ramstage FinalizeInit",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::mp_init(&mut board, \"core2\", 2u16, false)",
            "ramstage MpInit",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::acpi_prepare(&mut board);",
            "ramstage AcpiPrepare",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::smbios_prepare(&board);",
            "ramstage SmBiosPrepare",
        ),
        (
            ramstage.as_str(),
            "fstart_stage_runtime::Board::payload_load(&board);",
            "ramstage PayloadLoad",
        ),
    ] {
        assert!(source.contains(needle), "missing direct call for {context}");
    }
}

#[test]
fn load_next_stage_rejects_non_block_device() {
    // ns16550 has no boot_media_values_for_device mapping; LoadNextStage
    // should reject it before generating an unusable candidate table.
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let mut load_devs = heapless::Vec::new();
    let _ = load_devs.push(fstart_types::LoadDevice {
        name: heapless::String::try_from("uart0").unwrap(),
        base_offset: 0,
    });
    let _ = caps.push(Capability::LoadNextStage {
        devices: load_devs,
        next_stage: heapless::String::try_from("main").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!") && source.contains("does not provide BlockDevice"),
        "LoadNextStage should reject non-block devices before candidate emission: {source}"
    );
}

#[test]
fn load_next_stage_rejects_block_device_without_boot_media_mapping() {
    use fstart_device_registry::sunxi_mmc::SunxiMmcConfig;
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });
    let mut load_devs = heapless::Vec::new();
    let _ = load_devs.push(LoadDevice {
        name: HString::try_from("mmc1").unwrap(),
        base_offset: 0,
    });
    let _ = caps.push(Capability::LoadNextStage {
        devices: load_devs,
        next_stage: HString::try_from("main").unwrap(),
    });

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("mmc1").unwrap(),
        parent: None,
        bus: None,
        enabled: true,
    });

    let driver_instances = vec![
        DriverInstance::Ns16550(fstart_driver_ns16550::Ns16550Config {
            regs: fstart_driver_ns16550::AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        }),
        DriverInstance::SunxiMmc(SunxiMmcConfig::Sun7iA20 {
            base_addr: 0x01c1_0000,
            ccu_base: 0x01c2_0000,
            pio_base: 0x01c2_0800,
            mmc_index: 1,
        }),
    ];
    let config = BoardConfig {
        name: HString::try_from("test-sunxi-mmc1").unwrap(),
        platform: Platform::Armv7,
        memory: MemoryMap {
            regions: {
                let mut v = heapless::Vec::new();
                let _ = v.push(MemoryRegion {
                    name: HString::try_from("ram").unwrap(),
                    base: 0x4000_0000,
                    size: 0x0800_0000,
                    kind: RegionKind::Ram,
                });
                v
            },
            flash_layout: None,
            car: None,
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities: caps,
            load_addr: 0x4000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
            page_table_addr: None,
            page_size: fstart_types::stage::PageSize::default(),
        }),
        security: SecurityConfig {
            signing_algorithm: SignatureAlgorithm::Ed25519,
            pubkey_file: HString::try_from("keys/dev.pub").unwrap(),
            required_digests: {
                let mut v = heapless::Vec::new();
                let _ = v.push(DigestAlgorithm::Sha256);
                v
            },
        },
        mode: BuildMode::Rigid,
        payload: None,
        full_flash_image: false,
        microcode: None,
        soc_image_format: SocImageFormat::AllwinnerEgon,
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    };
    let parsed = ParsedBoard {
        config,
        device_services: services_for(&driver_instances),
        acpi_only_devices: Vec::new(),
        driver_instances,
        device_tree: vec![
            DeviceNode {
                parent: None,
                depth: 0,
            },
            DeviceNode {
                parent: None,
                depth: 0,
            },
        ],
    };
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("compile_error!") && source.contains("has no boot-source mapping"),
        "LoadNextStage should reject block devices without boot-source mapping: {source}"
    );
}
