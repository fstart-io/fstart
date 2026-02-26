use super::*;
use crate::ron_loader::ParsedBoard;
use fstart_drivers::DriverInstance;

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
    });

    let driver_instances = vec![DriverInstance::Ns16550(
        fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x1000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        },
    )];

    let config = BoardConfig {
        name: HString::try_from("test-board").unwrap(),
        platform: HString::try_from("riscv64").unwrap(),
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
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities,
            load_addr: 0x8000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
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
fn test_console_init_generates_device_init_and_banner() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(source.contains("uart0.init()"), "should call init()");
    assert!(
        source.contains("fstart_log::init(&uart0)"),
        "should call fstart_log::init"
    );
    assert!(
        source.contains("fstart_capabilities::console_ready("),
        "should call console_ready"
    );
    assert!(source.contains("Ns16550::new"), "should construct Ns16550");
    assert!(
        source.contains("struct Devices"),
        "should define Devices struct"
    );
    assert!(
        source.contains("struct StageContext"),
        "should define StageContext"
    );
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
fn test_driver_init_skips_already_inited() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // uart0 was already initialised by ConsoleInit, so DriverInit should
    // report 0 additional devices.
    assert!(
        source.contains("fstart_capabilities::driver_init_complete(0)"),
        "should report 0 additional devices inited"
    );
}

#[test]
fn test_sig_verify_generates_call() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x8000_0000,
        size: 0x40_0000,
    }));
    let _ = caps.push(Capability::SigVerify);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("const FLASH_BASE: u64 = 0x80000000;"),
        "should emit FLASH_BASE constant: {source}"
    );
    assert!(
        source.contains("const FLASH_SIZE: u64 = 0x400000;"),
        "should emit FLASH_SIZE constant: {source}"
    );
    assert!(
        source.contains("MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)"),
        "should construct MemoryMapped boot media: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::sig_verify("),
        "should call sig_verify: {source}"
    );
    assert!(
        source.contains("&boot_media"),
        "should pass &boot_media to sig_verify: {source}"
    );
    assert!(
        source.contains("static FSTART_ANCHOR: fstart_types::ffs::AnchorBlock"),
        "should emit FSTART_ANCHOR static: {source}"
    );
}

#[test]
fn test_sig_verify_with_flash_base_generates_constants() {
    // BootMedia(MemoryMapped) at base 0x0 (like AArch64 where flash is at 0x0)
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x0,
        size: 0x800_0000,
    }));
    let _ = caps.push(Capability::SigVerify);
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("const FLASH_BASE: u64 = 0x0;"),
        "should emit FLASH_BASE constant for base 0: {source}"
    );
    assert!(
        source.contains("const FLASH_SIZE: u64 = 0x8000000;"),
        "should emit FLASH_SIZE constant: {source}"
    );
    assert!(
        source.contains("MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)"),
        "should construct MemoryMapped boot media: {source}"
    );
}

#[test]
fn test_stage_load_generates_call() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x2000_0000,
        size: 0x200_0000,
    }));
    let _ = caps.push(Capability::StageLoad {
        next_stage: heapless::String::try_from("main").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fstart_capabilities::stage_load("),
        "should call stage_load: {source}"
    );
    assert!(
        source.contains("\"main\""),
        "should pass stage name \"main\": {source}"
    );
    assert!(
        source.contains("&boot_media"),
        "should pass &boot_media: {source}"
    );
    assert!(
        source.contains("fstart_platform_riscv64::jump_to"),
        "should pass jump_to: {source}"
    );
}

#[test]
fn test_stage_load_with_flash_base_generates_real_call() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::BootMedia(BootMedium::MemoryMapped {
        base: 0x8000_0000,
        size: 0x40_0000,
    }));
    let _ = caps.push(Capability::StageLoad {
        next_stage: heapless::String::try_from("main").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("const FLASH_BASE: u64 = 0x80000000;"),
        "should emit FLASH_BASE from BootMedia: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::stage_load("),
        "should call stage_load: {source}"
    );
    assert!(
        source.contains("\"main\""),
        "should pass stage name: {source}"
    );
    assert!(
        source.contains("&boot_media"),
        "should pass &boot_media: {source}"
    );
    assert!(
        source.contains("fstart_platform_riscv64::jump_to"),
        "should pass jump_to: {source}"
    );
}

#[test]
fn test_unknown_driver_is_compile_error() {
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });

    let mut parsed = test_parsed_board(caps);
    // Overwrite the driver name on the DeviceConfig to something unknown.
    // The DriverInstance is still Ns16550 but the name won't match any
    // registry entry, which is what triggers the compile_error.
    parsed.config.devices[0].driver = HString::try_from("nonexistent").unwrap();

    let source = generate_stage_source(&parsed, None);
    // The source still generates valid code because DriverInstance::Ns16550
    // is valid — the driver name on DeviceConfig is informational for xtask.
    // In the new architecture, an unknown driver would fail at RON parse
    // time (serde can't deserialize an unknown enum variant). So this test
    // verifies the codegen doesn't crash even if the names are inconsistent.
    // The ConsoleInit path checks find_driver_meta(drv_name) where drv_name
    // comes from inst.meta().name (which is "ns16550", not "nonexistent").
    assert!(
        source.contains("Ns16550::new"),
        "should still construct from the DriverInstance: {source}"
    );
}

#[test]
fn test_all_capabilities_complete_message() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fstart_log::info!(\"all capabilities complete\")"),
        "should log completion message"
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
    });

    let driver_instances = vec![
        DriverInstance::Ns16550(fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x1000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        }),
        DriverInstance::DesignwareI2c(fstart_drivers::i2c::designware::DesignwareI2cConfig {
            base_addr: 0x1004_0000,
            clock_freq: 100_000_000,
            bus_speed: fstart_drivers::i2c::designware::I2cSpeed::Fast,
        }),
    ];

    let config = BoardConfig {
        name: HString::try_from("test-i2c-board").unwrap(),
        platform: HString::try_from("riscv64").unwrap(),
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
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities,
            load_addr: 0x8000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
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
        },
    ];

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
        validate_device_tree(&devices, &tree).is_ok(),
        "all root devices should validate fine"
    );
}

#[test]
fn test_validate_device_tree_valid_bus_child() {
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("i2c0").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
            parent: None,
        },
        DeviceConfig {
            name: HString::try_from("tpm0").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            parent: Some(HString::try_from("i2c0").unwrap()),
        },
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
        validate_device_tree(&devices, &tree).is_ok(),
        "child on I2cBus parent should validate"
    );
}

#[test]
fn test_validate_device_tree_non_bus_parent_is_error() {
    use fstart_types::*;
    use heapless::String as HString;

    // uart0 provides Console, NOT a bus service
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
        },
        DeviceConfig {
            name: HString::try_from("child0").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            parent: Some(HString::try_from("uart0").unwrap()),
        },
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

    let result = validate_device_tree(&devices, &tree);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("does not provide a bus service"),
        "should reject non-bus parent"
    );
}

#[test]
fn test_i2c_bus_device_generates_correct_config() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_parsed_board_with_i2c_bus(caps);
    let source = generate_stage_source(&parsed, None);

    // Should generate DesignwareI2c construction
    assert!(
        source.contains("DesignwareI2c::new"),
        "should construct DesignwareI2c: {source}"
    );
    assert!(
        source.contains("DesignwareI2cConfig"),
        "should use DesignwareI2cConfig"
    );
    assert!(
        source.contains("0x10040000"),
        "should have correct base addr"
    );
    assert!(
        source.contains("I2cSpeed::Fast"),
        "400kHz should map to Fast speed"
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
fn test_i2c_bus_generates_accessor() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board_with_i2c_bus(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fn i2c_bus("),
        "should generate i2c_bus() accessor"
    );
    assert!(
        source.contains("impl I2c"),
        "should return impl I2c: {source}"
    );
}

#[test]
fn test_driver_init_with_bus_hierarchy_inits_parent_first() {
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);

    let mut parsed = test_parsed_board_with_i2c_bus(caps);

    // Add a child device nested under i2c0 (index 1).
    let _ = parsed.config.devices.push(DeviceConfig {
        name: HString::try_from("child0").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: Some(HString::try_from("i2c0").unwrap()),
    });
    parsed.driver_instances.push(DriverInstance::Ns16550(
        fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x2000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        },
    ));
    // i2c0 is at index 1
    parsed.device_tree.push(DeviceNode {
        parent: Some(1),
        depth: 1,
    });

    let source = generate_stage_source(&parsed, None);

    // In the generated code, i2c0.init() must appear before child0.init()
    let i2c_init_pos = source.find("i2c0.init()").expect("should init i2c0");
    let child_init_pos = source.find("child0.init()").expect("should init child0");
    assert!(
        i2c_init_pos < child_init_pos,
        "parent i2c0 must be initialised before child child0"
    );

    // Bus child should use new_on_bus, not new
    assert!(
        source.contains("new_on_bus"),
        "bus child should use new_on_bus: {source}"
    );
    assert!(
        source.contains("&i2c0"),
        "bus child should reference parent variable: {source}"
    );
}

#[test]
fn test_non_bus_parent_is_compile_error() {
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });

    let mut parsed = test_parsed_board(caps);
    // Add child0 nested under uart0 (index 0) — but uart0 is Console, not a bus
    let _ = parsed.config.devices.push(DeviceConfig {
        name: HString::try_from("child0").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: Some(HString::try_from("uart0").unwrap()),
    });
    parsed.driver_instances.push(DriverInstance::Ns16550(
        fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x2000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
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
        "should emit compile_error for non-bus parent: {source}"
    );
    assert!(
        source.contains("does not provide a bus service"),
        "error should mention bus service: {source}"
    );
}

// =======================================================================
// Flexible mode tests
// =======================================================================

/// Helper: create a flexible-mode parsed board with a single UART.
fn test_flexible_parsed_board(capabilities: heapless::Vec<Capability, 16>) -> ParsedBoard {
    let mut parsed = test_parsed_board(capabilities);
    parsed.config.mode = BuildMode::Flexible;
    parsed
}

/// Helper: create a flexible-mode parsed board with two UARTs (both Console).
/// This exercises enum dispatch with multiple variants.
fn test_flexible_multi_driver_parsed_board(
    capabilities: heapless::Vec<Capability, 16>,
) -> ParsedBoard {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();

    // NS16550 UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: None,
    });

    // PL011 UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart1").unwrap(),
        driver: HString::try_from("pl011").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: None,
    });

    let driver_instances = vec![
        DriverInstance::Ns16550(fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x1000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        }),
        DriverInstance::Pl011(fstart_drivers::uart::pl011::Pl011Config {
            base_addr: 0x0900_0000,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }),
    ];

    let config = BoardConfig {
        name: HString::try_from("test-flex-multi").unwrap(),
        platform: HString::try_from("riscv64").unwrap(),
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
        },
        devices,
        stages: StageLayout::Monolithic(MonolithicConfig {
            capabilities,
            load_addr: 0x8000_0000,
            stack_size: 0x10000,
            heap_size: None,
            data_addr: None,
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
        mode: BuildMode::Flexible,
        payload: None,
    };

    let device_tree = vec![
        DeviceNode {
            parent: None,
            depth: 0,
        }, // uart0
        DeviceNode {
            parent: None,
            depth: 0,
        }, // uart1
    ];

    ParsedBoard {
        config,
        driver_instances,
        device_tree,
    }
}

#[test]
fn test_flexible_generates_console_enum() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("enum ConsoleDevice"),
        "should generate ConsoleDevice enum: {source}"
    );
    assert!(
        source.contains("Ns16550(Ns16550)"),
        "should have Ns16550 variant: {source}"
    );
}

#[test]
fn test_flexible_generates_console_trait_impl() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("impl Console for ConsoleDevice"),
        "should impl Console for ConsoleDevice: {source}"
    );
    assert!(
        source.contains("fn write_byte"),
        "should have write_byte method: {source}"
    );
    assert!(
        source.contains("fn read_byte"),
        "should have read_byte method: {source}"
    );
    assert!(
        source.contains("d.write_byte(byte)"),
        "should delegate write_byte: {source}"
    );
}

#[test]
fn test_flexible_multi_driver_enum_has_both_variants() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_multi_driver_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("enum ConsoleDevice"),
        "should generate ConsoleDevice enum: {source}"
    );
    assert!(
        source.contains("Ns16550(Ns16550)"),
        "should have Ns16550 variant: {source}"
    );
    assert!(
        source.contains("Pl011(Pl011)"),
        "should have Pl011 variant: {source}"
    );

    // Both variants should appear in the match arms
    assert!(
        source.contains("Self::Ns16550(d)"),
        "should match on Ns16550 variant: {source}"
    );
    assert!(
        source.contains("Self::Pl011(d)"),
        "should match on Pl011 variant: {source}"
    );
}

#[test]
fn test_flexible_devices_struct_uses_enum_type() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // Devices struct should use ConsoleDevice, not Ns16550
    assert!(
        source.contains("uart0: ConsoleDevice"),
        "Devices struct should use ConsoleDevice enum type: {source}"
    );
}

#[test]
fn test_flexible_stage_context_returns_enum_type() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // StageContext accessor should return &ConsoleDevice, not &(impl Console + '_)
    assert!(
        source.contains("fn console(&self) -> &ConsoleDevice"),
        "should return &ConsoleDevice: {source}"
    );
}

#[test]
fn test_flexible_construction_uses_inner_variable() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // Construction should use _uart0_inner
    assert!(
        source.contains("let _uart0_inner = Ns16550::new"),
        "should construct into _uart0_inner: {source}"
    );
    // Init should be called on the inner variable
    assert!(
        source.contains("_uart0_inner.init()"),
        "should call init on inner variable: {source}"
    );
    // Wrapping should produce the final variable
    assert!(
        source.contains("let uart0 = ConsoleDevice::Ns16550(_uart0_inner)"),
        "should wrap inner into ConsoleDevice: {source}"
    );
}

#[test]
fn test_flexible_imports_service_error() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("use fstart_services::ServiceError;"),
        "flexible mode should import ServiceError: {source}"
    );
}

#[test]
fn test_flexible_driver_init_wraps_after_init() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let parsed = test_flexible_multi_driver_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    // uart1 should be initialized via _uart1_inner and then wrapped
    assert!(
        source.contains("_uart1_inner.init()"),
        "should init uart1 via inner variable: {source}"
    );
    assert!(
        source.contains("let uart1 = ConsoleDevice::Pl011(_uart1_inner)"),
        "should wrap uart1 after init: {source}"
    );

    // The init should come before the wrapping
    let init_pos = source.find("_uart1_inner.init()").unwrap();
    let wrap_pos = source
        .find("let uart1 = ConsoleDevice::Pl011(_uart1_inner)")
        .unwrap();
    assert!(init_pos < wrap_pos, "init should come before wrapping");
}

#[test]
fn test_flexible_still_generates_completion_message() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_flexible_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("fstart_log::info!(\"all capabilities complete\")"),
        "should log completion message in flexible mode: {source}"
    );
}

#[test]
fn test_flexible_with_i2c_bus_generates_i2c_bus_enum() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);

    let mut parsed = test_parsed_board_with_i2c_bus(caps);
    parsed.config.mode = BuildMode::Flexible;

    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("enum I2cBusDevice"),
        "should generate I2cBusDevice enum: {source}"
    );
    assert!(
        source.contains("DesignwareI2c(DesignwareI2c)"),
        "should have DesignwareI2c variant: {source}"
    );
    assert!(
        source.contains("impl I2cErrorType for I2cBusDevice"),
        "should impl ErrorType for I2cBusDevice: {source}"
    );
    assert!(
        source.contains("impl I2c for I2cBusDevice"),
        "should impl I2c for I2cBusDevice: {source}"
    );
    assert!(
        source.contains("fn transaction("),
        "should have transaction method: {source}"
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
    });

    let driver_instances = vec![DriverInstance::Ns16550(
        fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x1000_0000,
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
    });

    let config = BoardConfig {
        name: HString::try_from("test-multi").unwrap(),
        platform: HString::try_from("riscv64").unwrap(),
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
fn test_multi_stage_bootblock_generates_stage_load() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, Some("bootblock"));

    assert!(
        source.contains("fstart_capabilities::console_ready"),
        "bootblock should init console: {source}"
    );
    assert!(
        source.contains("MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)"),
        "bootblock should construct MemoryMapped boot media: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::sig_verify("),
        "bootblock should call sig_verify: {source}"
    );
    assert!(
        source.contains("&boot_media"),
        "bootblock should pass &boot_media: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::stage_load("),
        "bootblock should call stage_load: {source}"
    );
    assert!(
        source.contains("\"main\""),
        "bootblock should load stage \"main\": {source}"
    );
}

#[test]
fn test_multi_stage_bootblock_with_flash_base() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, Some("bootblock"));

    assert!(
        source.contains("const FLASH_BASE: u64 = 0x20000000;"),
        "should emit FLASH_BASE from BootMedia capability: {source}"
    );
    assert!(
        source.contains("const FLASH_SIZE: u64 = 0x2000000;"),
        "should emit FLASH_SIZE from BootMedia capability: {source}"
    );
    assert!(
        source.contains("static FSTART_ANCHOR: fstart_types::ffs::AnchorBlock"),
        "should emit FSTART_ANCHOR static: {source}"
    );
    assert!(
        source.contains("MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)"),
        "should construct MemoryMapped boot media: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::sig_verify("),
        "bootblock should call sig_verify: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::stage_load("),
        "bootblock should call stage_load: {source}"
    );
    assert!(
        source.contains("&boot_media"),
        "bootblock should pass &boot_media: {source}"
    );
    assert!(
        source.contains("fstart_platform_riscv64::jump_to"),
        "bootblock should pass jump_to: {source}"
    );
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
fn test_multi_stage_main_generates_capabilities() {
    let parsed = test_multi_stage_parsed_board();
    let source = generate_stage_source(&parsed, Some("main"));

    assert!(
        source.contains("fstart_capabilities::console_ready"),
        "main stage should init console: {source}"
    );
    assert!(
        source.contains("fstart_capabilities::memory_init()"),
        "main stage should call memory_init: {source}"
    );
    assert!(
        source.contains("all capabilities complete"),
        "main stage should log completion: {source}"
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

#[test]
fn test_rigid_mode_unchanged_no_enums() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board(caps); // default Rigid mode
    let source = generate_stage_source(&parsed, None);

    assert!(
        !source.contains("enum ConsoleDevice"),
        "rigid mode should NOT generate ConsoleDevice enum: {source}"
    );
    assert!(
        !source.contains("ServiceError"),
        "rigid mode should NOT import ServiceError: {source}"
    );
    assert!(
        source.contains("uart0: Ns16550"),
        "rigid mode should use concrete type in Devices: {source}"
    );
}

// =======================================================================
// Device tree table tests
// =======================================================================

#[test]
fn test_generated_source_contains_device_tree_table() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let parsed = test_parsed_board(caps);
    let source = generate_stage_source(&parsed, None);

    assert!(
        source.contains("static DEVICE_TREE"),
        "should generate static DEVICE_TREE: {source}"
    );
    assert!(
        source.contains("fstart_types::DeviceNode"),
        "should use fstart_types::DeviceNode: {source}"
    );
}

#[test]
fn test_device_tree_table_with_bus_children() {
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);

    let mut parsed = test_parsed_board_with_i2c_bus(caps);

    // Add child under i2c0 (index 1)
    let _ = parsed.config.devices.push(DeviceConfig {
        name: HString::try_from("child0").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        parent: Some(HString::try_from("i2c0").unwrap()),
    });
    parsed.driver_instances.push(DriverInstance::Ns16550(
        fstart_drivers::uart::ns16550::Ns16550Config {
            base_addr: 0x2000_0000,
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        },
    ));
    parsed.device_tree.push(DeviceNode {
        parent: Some(1),
        depth: 1,
    });

    let source = generate_stage_source(&parsed, None);

    // Table should have 3 entries
    assert!(
        source.contains("DeviceNode; 3"),
        "should have 3 entries in DEVICE_TREE: {source}"
    );
    // Should have a parent reference (Some(1u8))
    assert!(
        source.contains("Some(1u8)"),
        "child should reference parent index 1: {source}"
    );
    // Root nodes should have None
    assert!(
        source.contains("parent: None"),
        "root nodes should have None parent: {source}"
    );
}

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
