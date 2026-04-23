use super::*;
use crate::ron_loader::ParsedBoard;
use fstart_device_registry::DriverInstance;

/// Helper: create a minimal parsed board for testing.
fn test_parsed_board(capabilities: heapless::Vec<Capability, 16>) -> ParsedBoard {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
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
        name: HString::try_from("test-board").unwrap(),
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
            flash_base: None,
            flash_size: None,
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
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
        boot_hart_id: 0,
    };

    let device_tree = vec![DeviceNode {
        parent: None,
        depth: 0,
    }];

    ParsedBoard {
        config,
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
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: None,
        bus: None,
        enabled: true,
    });

    // Root device: I2C bus controller
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("i2c0").unwrap(),
        driver: HString::try_from("designware-i2c").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("I2cBus").unwrap());
            v
        },
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
            flash_base: None,
            flash_size: None,
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
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
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
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("i2c0").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
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
        validate_device_tree(&devices, &instances, &tree).is_ok(),
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
            driver: HString::try_from("pci-ecam").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("PciRootBus").unwrap());
                v
            },
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("bochs0").unwrap(),
            driver: HString::try_from("bochs-display").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Framebuffer").unwrap());
                v
            },
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
        validate_device_tree(&devices, &instances, &tree).is_ok(),
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
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("bochs0").unwrap(),
            driver: HString::try_from("bochs-display").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Framebuffer").unwrap());
                v
            },
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

    let result = validate_device_tree(&devices, &instances, &tree);
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
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            parent: None,
            bus: None,
            enabled: true,
        },
        DeviceConfig {
            name: HString::try_from("uart1").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
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
        validate_device_tree(&devices, &instances, &tree).is_ok(),
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
        driver: HString::try_from("i2c-ck505").unwrap(),
        services: heapless::Vec::new(),
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
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
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
            let _ = v.push(Capability::BootMedia(BootMedium::MemoryMapped {
                base: 0x2000_0000,
                size: 0x200_0000,
                ram_copy_addr: None,
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
        data_addr: None,
        page_table_addr: None,
        page_size: fstart_types::stage::PageSize::default(),
    });

    let config = BoardConfig {
        name: HString::try_from("test-multi").unwrap(),
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
            flash_base: None,
            flash_size: None,
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
        soc_image_format: SocImageFormat::default(),
        acpi: None,
        smbios: None,
        boot_hart_id: 0,
    };

    let device_tree = vec![DeviceNode {
        parent: None,
        depth: 0,
    }];

    ParsedBoard {
        config,
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
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x2000_0000,
        size: 0x200_0000,
        ram_copy_addr: None,
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
// Phase 1: StagePlan emission tests.
//
// The codegen emits a `static STAGE_PLAN: StagePlan = ...;` literal
// alongside the existing `fstart_main`.  These tests check the
// structural content of that literal — device-name → DeviceId
// resolution, CapOp ordering, is_first_stage / ends_with_jump flags,
// boot-media descriptors.
//
// See `.opencode/plans/stage-runtime-codegen-split.md`.
// =======================================================================

#[test]
fn plan_emits_static_with_correct_flags_for_monolithic_stage() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x2000_0000,
        size: 0x0200_0000,
        ram_copy_addr: None,
    }));
    let _ = caps.push(Capability::PayloadLoad);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("static STAGE_PLAN: fstart_stage_runtime::StagePlan"),
        "should emit STAGE_PLAN static: {source}"
    );
    assert!(
        source.contains("is_first_stage: true"),
        "monolithic => is_first_stage=true: {source}"
    );
    assert!(
        source.contains("ends_with_jump: true"),
        "PayloadLoad last => ends_with_jump=true: {source}"
    );
}

#[test]
fn plan_resolves_device_names_to_device_ids() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // uart0 is the only device, index 0.
    assert!(
        source.contains("fstart_stage_runtime::CapOp::ConsoleInit(0)"),
        "ConsoleInit should carry DeviceId 0: {source}"
    );
}

#[test]
fn plan_lists_all_runtime_devices() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // One non-structural device => all_devices == [0]
    assert!(
        source.contains("_FSTART_PLAN_ALL_DEVICES: [fstart_types::DeviceId; 1usize] = [0]"),
        "all_devices should list uart0 as id 0: {source}"
    );
    assert!(
        source.contains("fstart_stage_runtime::CapOp::DriverInit"),
        "DriverInit CapOp should be present: {source}"
    );
}

#[test]
fn plan_memory_mapped_boot_media_emits_bootmediastatic_with_none_device() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x2000_0000,
        size: 0x0200_0000,
        ram_copy_addr: None,
    }));
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("CapOp::BootMediaStatic"),
        "MemoryMapped => BootMediaStatic: {source}"
    );
    // device: None is the memory-mapped marker.  prettyplease may split
    // fields over lines; just look for "device: None" anywhere.
    assert!(
        source.contains("device: None"),
        "MemoryMapped device should be None: {source}"
    );
    assert!(
        source.contains("offset: 0x20000000") || source.contains("offset : 0x20000000"),
        "offset should be 0x20000000: {source}"
    );
}

#[test]
fn plan_ends_with_jump_false_when_last_cap_does_not_hand_off() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::MemoryInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("ends_with_jump: false"),
        "MemoryInit last => ends_with_jump=false: {source}"
    );
}

#[test]
fn plan_capop_count_matches_capability_count() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::MemoryInit);
    let _ = caps.push(Capability::LateDriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("_FSTART_PLAN_CAPS: [fstart_stage_runtime::CapOp; 3usize]"),
        "CAPS array should be length 3: {source}"
    );
}

#[test]
fn plan_media_ids_empty_for_device_without_boot_media_mapping() {
    // ns16550 has no boot_media_values_for_device mapping; the plan's
    // LoadNextStage candidate should fall back to an empty slice.
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

    // We don't care if the overall stage rejects this (it might — ns16550
    // isn't a block device).  We care that plan_gen didn't panic when
    // asked to compute media_ids for a device without a mapping.
    // If the stage rejected the semantics, the output is a compile_error
    // and plan_gen still produced a usable candidate entry.
    assert!(
        source.contains("media_ids: &[]") || source.contains("compile_error!"),
        "non-sunxi device should have empty media_ids (or compile_error): {source}"
    );
}
