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

fn load_foxconn_d41s_board() -> ParsedBoard {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    load_parsed_board(&manifest_dir.join("../../boards/foxconn-d41s/board.ron"))
        .expect("foxconn-d41s board should parse")
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
// Stage-plan executor tests.
// =======================================================================

#[test]
fn stage_plan_entry_emits_data_and_run_stage() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::MemoryInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);
    let fstart_main = fstart_main_source(&source);

    assert!(
        source.contains("static STAGE_PLAN: fstart_stage_runtime::StagePlan"),
        "generated source should emit data-only STAGE_PLAN: {source}"
    );
    assert!(
        source.contains("fstart_stage_runtime::StageOp::ConsoleInit(0)"),
        "ConsoleInit should lower to StageOp data: {source}"
    );
    assert!(
        fstart_main.contains("fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN)"),
        "fstart_main should delegate to handwritten executor: {fstart_main}"
    );
    assert!(
        !fstart_main.contains("fstart_stage_runtime::Board::memory_init(&board)"),
        "fstart_main must not contain capability flow: {fstart_main}"
    );
}

#[test]
fn stage_plan_emits_flow_feature_guards() {
    let source = std::thread::Builder::new()
        .name("stage-plan-qemu-riscv64-codegen".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let parsed = load_parsed_board(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../boards/qemu-riscv64/board.ron"),
            )
            .expect("qemu-riscv64 board should parse");
            generate_stage_source(&parsed, None)
        })
        .expect("spawn qemu-riscv64 codegen thread")
        .join()
        .expect("qemu-riscv64 codegen should not panic");

    assert!(
        source.contains("stage-flow-console-init"),
        "ConsoleInit should require the console-init flow feature: {source}"
    );
    assert!(
        source.contains("stage-flow-memory-init"),
        "MemoryInit should require the memory-init flow feature: {source}"
    );
    assert!(
        source.contains("stage-flow-boot-media"),
        "BootMedia should require the boot-media flow feature: {source}"
    );
    assert!(
        source.contains("stage-flow-ffs"),
        "SigVerify/PayloadLoad should require the FFS flow feature: {source}"
    );
}

#[test]
fn generated_imports_include_smbus_for_typed_smbus_provider() {
    let source = std::thread::Builder::new()
        .name("foxconn-smbus-import-codegen".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let parsed = load_foxconn_d41s_board();
            generate_stage_source(&parsed, Some("bootblock"))
        })
        .expect("spawn foxconn codegen thread")
        .join()
        .expect("foxconn codegen should not panic");

    assert!(
        source.contains("use fstart_services::SmBus"),
        "ICH7's typed SystemManagementBus service must import the SmBus trait: {source}"
    );
}

#[test]
fn stage_plan_driver_init_emits_device_tables() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fstart_stage_runtime::StageOp::DriverInit"),
        "DriverInit should lower to StageOp data: {source}"
    );
    assert!(
        source.contains("static _FSTART_STAGE_PLAN_ALL_DEVICES"),
        "DriverInit should emit all-runtime-device data: {source}"
    );
    assert!(
        source.contains("static _FSTART_STAGE_PLAN_OPTIONAL_DEVICES"),
        "DriverInit should emit optional-device data: {source}"
    );
    assert!(
        source.contains("static _FSTART_STAGE_PLAN_BOOT_MEDIA_GATED"),
        "DriverInit should emit boot-media gated candidate data: {source}"
    );
    assert!(
        source.contains("0u8") || source.contains("[0]"),
        "DriverInit should preserve the runtime uart device ID: {source}"
    );
}

#[test]
fn stage_plan_driver_init_gated_devices_are_candidates() {
    let source = std::thread::Builder::new()
        .name("orangepi-r1-stage-plan-codegen".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let parsed = load_parsed_board(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../boards/orangepi-r1/board.ron"),
            )
            .expect("orangepi-r1 board should parse");
            generate_stage_source(&parsed, Some("main"))
        })
        .expect("spawn orangepi-r1 codegen thread")
        .join()
        .expect("orangepi-r1 codegen should not panic");

    assert!(
        source.contains(
            "static _FSTART_STAGE_PLAN_BOOT_MEDIA_GATED: [fstart_stage_runtime::BootMediaCandidate; 2usize]"
        ),
        "DriverInit boot-media gating should be candidate data: {source}"
    );
    assert!(
        source.contains("media_ids")
            && source.contains("device: 3")
            && source.contains("device: 4"),
        "gated candidate data should include both boot media devices and media ids: {source}"
    );
}

#[test]
fn stage_plan_lenovo_x61_bootblock_and_ramstage_data_are_explicit() {
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
        fstart_main_source(&bootblock)
            .contains("fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN)"),
        "X61 bootblock should enter through the stage executor"
    );
    assert!(
        fstart_main_source(&ramstage)
            .contains("fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN)"),
        "X61 ramstage should enter through the stage executor"
    );

    assert_ordered(
        &bootblock,
        &[
            "StageOp::PreConsoleInit",
            "StageOp::EarlyInit",
            "StageOp::DramInit(0)",
            "StageOp::StageLoad",
        ],
        "X61 bootblock stage plan data",
    );
    assert_ordered(
        &ramstage,
        &[
            "StageOp::StageLocalInit",
            "StageOp::MemoryDetect(0)",
            "StageOp::PciInit(0)",
            "StageOp::PostDramInit",
            "StageOp::FinalizeInit",
            "StageOp::MpInit",
            "StageOp::AcpiPrepare",
            "StageOp::SmBiosPrepare",
            "StageOp::PayloadLoad",
        ],
        "X61 ramstage stage plan data",
    );
    assert!(
        ramstage.contains("TempRamBuffer")
            && ramstage.contains("base: 0x2000000")
            && ramstage.contains("size: 0x1000000"),
        "X61 ramstage FirmwareImage BootMedia must keep temp_ram_buffer data"
    );
    assert!(
        ramstage.contains("TempRamArena::new") && !ramstage.contains("copy_firmware_image_to_ram"),
        "X61 ramstage adapter must treat temp_ram_buffer as scratch RAM, not a whole-image copy"
    );
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
