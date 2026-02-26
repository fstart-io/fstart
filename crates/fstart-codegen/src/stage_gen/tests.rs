use super::*;

/// Helper: create a minimal board config for testing.
fn test_board_config(capabilities: heapless::Vec<Capability, 16>) -> BoardConfig {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        compatible: HString::try_from("ns16550a").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x1000_0000),
            clock_freq: Some(3_686_400),
            baud_rate: Some(115_200),
            irq: Some(10),
            ..Default::default()
        },
        parent: None,
    });

    BoardConfig {
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
    }
}

#[test]
fn test_console_init_generates_device_init_and_banner() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

    assert!(
        source.contains("fstart_capabilities::memory_init()"),
        "should call memory_init"
    );
}

#[test]
fn test_memory_init_without_console_is_error() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::MemoryInit);
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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

    let mut config = test_board_config(caps);
    config.devices[0].driver = HString::try_from("nonexistent").unwrap();

    let source = generate_stage_source(&config, None);
    assert!(
        source.contains("compile_error!"),
        "should emit compile_error for unknown driver"
    );
}

#[test]
fn test_all_capabilities_complete_message() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

    assert!(
        source.contains("fstart_log::info!(\"all capabilities complete\")"),
        "should log completion message"
    );
}

// =======================================================================
// Bus hierarchy tests
// =======================================================================

/// Helper: create a board config with UART + I2C bus + I2C child device.
fn test_board_with_i2c_bus(capabilities: heapless::Vec<Capability, 16>) -> BoardConfig {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();

    // Root device: UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        compatible: HString::try_from("ns16550a").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x1000_0000),
            clock_freq: Some(3_686_400),
            baud_rate: Some(115_200),
            irq: Some(10),
            ..Default::default()
        },
        parent: None,
    });

    // Root device: I2C bus controller
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("i2c0").unwrap(),
        compatible: HString::try_from("dw-apb-i2c").unwrap(),
        driver: HString::try_from("designware-i2c").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("I2cBus").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x1004_0000),
            clock_freq: Some(100_000_000),
            bus_speed: Some(400_000),
            ..Default::default()
        },
        parent: None,
    });

    BoardConfig {
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
    }
}

#[test]
fn test_topological_sort_no_parents() {
    // All root devices should sort fine (preserving relative order)
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("uart0").unwrap(),
            compatible: HString::try_from("ns16550a").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            resources: Resources {
                mmio_base: Some(0x1000_0000),
                ..Default::default()
            },
            parent: None,
        },
        DeviceConfig {
            name: HString::try_from("i2c0").unwrap(),
            compatible: HString::try_from("dw-apb-i2c").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
            resources: Resources {
                mmio_base: Some(0x1004_0000),
                ..Default::default()
            },
            parent: None,
        },
    ];

    let sorted = topological_sort_devices(&devices).expect("should succeed");
    assert_eq!(sorted.len(), 2);
    // Both root devices — both should be present (order among roots is unspecified)
    assert!(sorted.contains(&0), "should contain device 0");
    assert!(sorted.contains(&1), "should contain device 1");
}

#[test]
fn test_topological_sort_parent_before_child() {
    use fstart_types::*;
    use heapless::String as HString;

    // Child listed BEFORE parent in RON — sort must reorder
    let devices: Vec<DeviceConfig> = vec![
        // Index 0: child (listed first but has parent)
        DeviceConfig {
            name: HString::try_from("tpm0").unwrap(),
            compatible: HString::try_from("infineon,slb9670").unwrap(),
            driver: HString::try_from("ns16550").unwrap(), // fake driver, doesn't matter for sort
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            resources: Resources {
                bus_addr: Some(0x50),
                ..Default::default()
            },
            parent: Some(HString::try_from("i2c0").unwrap()),
        },
        // Index 1: parent
        DeviceConfig {
            name: HString::try_from("i2c0").unwrap(),
            compatible: HString::try_from("dw-apb-i2c").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
            resources: Resources {
                mmio_base: Some(0x1004_0000),
                ..Default::default()
            },
            parent: None,
        },
    ];

    let sorted = topological_sort_devices(&devices).expect("should succeed");
    assert_eq!(sorted.len(), 2);
    // Parent (index 1) must come before child (index 0)
    assert_eq!(sorted[0], 1, "parent i2c0 should come first");
    assert_eq!(sorted[1], 0, "child tpm0 should come second");
}

#[test]
fn test_topological_sort_unknown_parent_is_error() {
    use fstart_types::*;
    use heapless::String as HString;

    let devices: Vec<DeviceConfig> = vec![DeviceConfig {
        name: HString::try_from("tpm0").unwrap(),
        compatible: HString::try_from("infineon,slb9670").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources::default(),
        parent: Some(HString::try_from("nonexistent").unwrap()),
    }];

    let result = topological_sort_devices(&devices);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("parent 'nonexistent' which is not declared"));
}

#[test]
fn test_topological_sort_parent_not_bus_is_error() {
    use fstart_types::*;
    use heapless::String as HString;

    // uart0 provides Console, NOT a bus service
    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("uart0").unwrap(),
            compatible: HString::try_from("ns16550a").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            resources: Resources {
                mmio_base: Some(0x1000_0000),
                ..Default::default()
            },
            parent: None,
        },
        DeviceConfig {
            name: HString::try_from("child0").unwrap(),
            compatible: HString::try_from("some-device").unwrap(),
            driver: HString::try_from("ns16550").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("Console").unwrap());
                v
            },
            resources: Resources::default(),
            parent: Some(HString::try_from("uart0").unwrap()),
        },
    ];

    let result = topological_sort_devices(&devices);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("does not provide a bus service"),
        "should reject non-bus parent"
    );
}

#[test]
fn test_topological_sort_cycle_detection() {
    use fstart_types::*;
    use heapless::String as HString;

    // Create a cycle: a -> b -> a
    let devices: Vec<DeviceConfig> = vec![
        DeviceConfig {
            name: HString::try_from("a").unwrap(),
            compatible: HString::try_from("x").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
            resources: Resources::default(),
            parent: Some(HString::try_from("b").unwrap()),
        },
        DeviceConfig {
            name: HString::try_from("b").unwrap(),
            compatible: HString::try_from("x").unwrap(),
            driver: HString::try_from("designware-i2c").unwrap(),
            services: {
                let mut v = heapless::Vec::new();
                let _ = v.push(HString::try_from("I2cBus").unwrap());
                v
            },
            resources: Resources::default(),
            parent: Some(HString::try_from("a").unwrap()),
        },
    ];

    let result = topological_sort_devices(&devices);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().contains("cycle detected"),
        "should detect cycle"
    );
}

#[test]
fn test_i2c_bus_device_generates_correct_config() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let _ = caps.push(Capability::DriverInit);
    let config = test_board_with_i2c_bus(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_with_i2c_bus(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_with_i2c_bus(caps);
    let source = generate_stage_source(&config, None);

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

    let mut config = test_board_with_i2c_bus(caps);

    // Add a child device that references i2c0 as parent.
    let _ = config.devices.push(DeviceConfig {
        name: HString::try_from("child0").unwrap(),
        compatible: HString::try_from("test-child").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x2000_0000),
            bus_addr: Some(0x50),
            ..Default::default()
        },
        parent: Some(HString::try_from("i2c0").unwrap()),
    });

    let source = generate_stage_source(&config, None);

    // In the generated code, i2c0.init() must appear before child0.init()
    let i2c_init_pos = source.find("i2c0.init()").expect("should init i2c0");
    let child_init_pos = source.find("child0.init()").expect("should init child0");
    assert!(
        i2c_init_pos < child_init_pos,
        "parent i2c0 must be initialised before child child0"
    );
}

#[test]
fn test_parent_reference_unknown_device_is_compile_error() {
    use fstart_types::*;
    use heapless::String as HString;

    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: HString::try_from("uart0").unwrap(),
    });

    let mut config = test_board_config(caps);
    let _ = config.devices.push(DeviceConfig {
        name: HString::try_from("child0").unwrap(),
        compatible: HString::try_from("test-child").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources::default(),
        parent: Some(HString::try_from("ghost").unwrap()),
    });

    let source = generate_stage_source(&config, None);
    assert!(
        source.contains("compile_error!"),
        "should emit compile_error for unknown parent"
    );
    assert!(
        source.contains("ghost"),
        "error should mention the unknown parent name"
    );
}

// =======================================================================
// Flexible mode tests
// =======================================================================

/// Helper: create a flexible-mode board config with a single UART.
fn test_flexible_board_config(capabilities: heapless::Vec<Capability, 16>) -> BoardConfig {
    let mut config = test_board_config(capabilities);
    config.mode = BuildMode::Flexible;
    config
}

/// Helper: create a flexible-mode board config with two UARTs (both Console).
/// This exercises enum dispatch with multiple variants.
fn test_flexible_multi_driver_board(capabilities: heapless::Vec<Capability, 16>) -> BoardConfig {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();

    // NS16550 UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        compatible: HString::try_from("ns16550a").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x1000_0000),
            clock_freq: Some(3_686_400),
            baud_rate: Some(115_200),
            irq: Some(10),
            ..Default::default()
        },
        parent: None,
    });

    // PL011 UART
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart1").unwrap(),
        compatible: HString::try_from("arm,pl011").unwrap(),
        driver: HString::try_from("pl011").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x0900_0000),
            clock_freq: Some(1_843_200),
            baud_rate: Some(115_200),
            ..Default::default()
        },
        parent: None,
    });

    BoardConfig {
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
    }
}

#[test]
fn test_flexible_generates_console_enum() {
    let mut caps = heapless::Vec::new();
    let _ = caps.push(Capability::ConsoleInit {
        device: heapless::String::try_from("uart0").unwrap(),
    });
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_multi_driver_board(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_multi_driver_board(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_flexible_board_config(caps);
    let source = generate_stage_source(&config, None);

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

    let mut config = test_board_with_i2c_bus(caps);
    config.mode = BuildMode::Flexible;

    let source = generate_stage_source(&config, None);

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

/// Helper: create a multi-stage board config (bootblock + main).
fn test_multi_stage_board() -> BoardConfig {
    use fstart_types::*;
    use heapless::String as HString;

    let mut devices = heapless::Vec::new();
    let _ = devices.push(DeviceConfig {
        name: HString::try_from("uart0").unwrap(),
        compatible: HString::try_from("ns16550a").unwrap(),
        driver: HString::try_from("ns16550").unwrap(),
        services: {
            let mut v = heapless::Vec::new();
            let _ = v.push(HString::try_from("Console").unwrap());
            v
        },
        resources: Resources {
            mmio_base: Some(0x1000_0000),
            clock_freq: Some(3_686_400),
            baud_rate: Some(115_200),
            irq: Some(10),
            ..Default::default()
        },
        parent: None,
    });

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
        runs_from: RunsFrom::Ram,
        data_addr: None,
    });

    BoardConfig {
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
    }
}

#[test]
fn test_multi_stage_bootblock_generates_stage_load() {
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, Some("bootblock"));

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
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, Some("bootblock"));

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
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, Some("bootblock"));

    // Bootblock ends with StageLoad — should NOT log completion
    assert!(
        !source.contains("all capabilities complete"),
        "bootblock should NOT log completion (ends with StageLoad): {source}"
    );
}

#[test]
fn test_multi_stage_main_generates_capabilities() {
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, Some("main"));

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
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, None);

    assert!(
        source.contains("compile_error!"),
        "multi-stage without FSTART_STAGE_NAME should be compile_error: {source}"
    );
}

#[test]
fn test_multi_stage_unknown_stage_name_is_error() {
    let config = test_multi_stage_board();
    let source = generate_stage_source(&config, Some("nonexistent"));

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
    let config = test_board_config(caps);
    let source = generate_stage_source(&config, None);

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
    let config = test_board_config(caps); // default Rigid mode
    let source = generate_stage_source(&config, None);

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
