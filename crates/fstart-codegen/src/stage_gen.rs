//! Generate stage entry point code from board configuration.
//!
//! Given a BoardConfig (or a specific stage within it), this module emits
//! Rust source code that:
//!
//! 1. Defines a `Devices` struct with one concrete typed field per device
//! 2. Defines a `StageContext` with service accessor methods
//! 3. Generates `fstart_main()` that constructs devices, runs capabilities
//!    in declared order, and halts
//!
//! In **Rigid** mode, all types are concrete — zero overhead.
//! In **Flexible** mode, service enum wrappers are generated for runtime
//! driver selection via match dispatch (no trait objects, no alloc).
//!
//! See [docs/driver-model.md](../../../docs/driver-model.md).

use fstart_types::{BoardConfig, BuildMode, Capability, DeviceConfig, StageLayout};

/// Information about a known driver — maps RON driver name to Rust type path
/// and its config construction logic.
struct DriverInfo {
    /// RON driver name (e.g., "ns16550")
    name: &'static str,
    /// Rust module path (e.g., "fstart_drivers::uart::ns16550")
    module_path: &'static str,
    /// Rust type name (e.g., "Ns16550")
    type_name: &'static str,
    /// Rust config type name (e.g., "Ns16550Config")
    config_type: &'static str,
    /// Which service traits this driver implements.
    /// Used for flexible-mode enum dispatch codegen and validation.
    services: &'static [&'static str],
}

/// Registry of known drivers.
const KNOWN_DRIVERS: &[DriverInfo] = &[
    DriverInfo {
        name: "ns16550",
        module_path: "fstart_drivers::uart::ns16550",
        type_name: "Ns16550",
        config_type: "Ns16550Config",
        services: &["Console"],
    },
    DriverInfo {
        name: "pl011",
        module_path: "fstart_drivers::uart::pl011",
        type_name: "Pl011",
        config_type: "Pl011Config",
        services: &["Console"],
    },
    DriverInfo {
        name: "designware-i2c",
        module_path: "fstart_drivers::i2c::designware",
        type_name: "DesignwareI2c",
        config_type: "DesignwareI2cConfig",
        services: &["I2cBus"],
    },
];

/// Look up driver info by RON driver name.
fn find_driver(name: &str) -> Option<&'static DriverInfo> {
    KNOWN_DRIVERS.iter().find(|d| d.name == name)
}

/// Bus service names that indicate a device is a bus controller.
const BUS_SERVICES: &[&str] = &["I2cBus", "SpiBus", "GpioController"];

/// Returns true if a device provides a bus service.
fn is_bus_provider(dev: &DeviceConfig) -> bool {
    dev.services
        .iter()
        .any(|s| BUS_SERVICES.contains(&s.as_str()))
}

/// Topological sort of devices: parents before children.
///
/// Returns the devices sorted so that any device with a `parent` field comes
/// after its parent. Also validates:
/// - Every `parent` reference names an existing device
/// - Every parent device provides a bus service
/// - No cycles in the parent chain
///
/// Returns either the sorted indices or an error message.
fn topological_sort_devices(devices: &[DeviceConfig]) -> Result<Vec<usize>, String> {
    let n = devices.len();

    // Build name -> index map
    let name_to_idx: std::collections::HashMap<&str, usize> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    // Build adjacency: parent_idx -> vec of child indices
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, dev) in devices.iter().enumerate() {
        if let Some(ref parent_name) = dev.parent {
            let parent_str = parent_name.as_str();

            // Validate parent exists
            let Some(&parent_idx) = name_to_idx.get(parent_str) else {
                return Err(format!(
                    "device '{}' has parent '{}' which is not declared",
                    dev.name.as_str(),
                    parent_str,
                ));
            };

            // Validate parent provides a bus service
            if !is_bus_provider(&devices[parent_idx]) {
                return Err(format!(
                    "device '{}' has parent '{}' which does not provide a bus service \
                     (expected one of: I2cBus, SpiBus, GpioController)",
                    dev.name.as_str(),
                    parent_str,
                ));
            };

            children[parent_idx].push(i);
            in_degree[i] += 1;
        }
    }

    // Kahn's algorithm for topological sort
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted: Vec<usize> = Vec::with_capacity(n);

    while let Some(node) = queue.pop() {
        sorted.push(node);
        for &child in &children[node] {
            in_degree[child] -= 1;
            if in_degree[child] == 0 {
                queue.push(child);
            }
        }
    }

    if sorted.len() != n {
        // Cycle detected — find the devices involved
        let cycle_devices: Vec<&str> = (0..n)
            .filter(|&i| in_degree[i] > 0)
            .map(|i| devices[i].name.as_str())
            .collect();
        return Err(format!(
            "cycle detected in device parent chain involving: {}",
            cycle_devices.join(", "),
        ));
    }

    Ok(sorted)
}

// =======================================================================
// Flexible mode: service enum dispatch
// =======================================================================

/// Description of a service trait's methods for enum dispatch codegen.
struct ServiceMethod {
    /// Method signature (without the `&self`)
    /// e.g., "fn write_byte(&self, byte: u8) -> Result<(), ServiceError>"
    signature: &'static str,
    /// How to call the inner variant
    /// e.g., "d.write_byte(byte)"
    delegation: &'static str,
}

/// Full description of a service trait for enum codegen.
struct ServiceTraitInfo {
    /// Trait name (e.g., "Console")
    name: &'static str,
    /// Generated enum name (e.g., "ConsoleDevice")
    enum_name: &'static str,
    /// Methods that need delegation (only required methods — defaults are inherited)
    methods: &'static [ServiceMethod],
    /// Accessor name on StageContext (e.g., "console")
    accessor: &'static str,
}

/// Known service traits and their methods for enum dispatch generation.
const SERVICE_TRAITS: &[ServiceTraitInfo] = &[
    ServiceTraitInfo {
        name: "Console",
        enum_name: "ConsoleDevice",
        methods: &[
            ServiceMethod {
                signature: "fn write_byte(&self, byte: u8) -> Result<(), ServiceError>",
                delegation: "d.write_byte(byte)",
            },
            ServiceMethod {
                signature: "fn read_byte(&self) -> Result<Option<u8>, ServiceError>",
                delegation: "d.read_byte()",
            },
        ],
        accessor: "console",
    },
    ServiceTraitInfo {
        name: "BlockDevice",
        enum_name: "BlockDeviceEnum",
        methods: &[
            ServiceMethod {
                signature:
                    "fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError>",
                delegation: "d.read(offset, buf)",
            },
            ServiceMethod {
                signature:
                    "fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError>",
                delegation: "d.write(offset, buf)",
            },
            ServiceMethod {
                signature: "fn size(&self) -> u64",
                delegation: "d.size()",
            },
        ],
        accessor: "block_device",
    },
    ServiceTraitInfo {
        name: "Timer",
        enum_name: "TimerDevice",
        methods: &[
            ServiceMethod {
                signature: "fn delay_us(&self, us: u64)",
                delegation: "d.delay_us(us)",
            },
            ServiceMethod {
                signature: "fn timestamp_us(&self) -> u64",
                delegation: "d.timestamp_us()",
            },
        ],
        accessor: "timer",
    },
    ServiceTraitInfo {
        name: "I2cBus",
        enum_name: "I2cBusDevice",
        methods: &[
            ServiceMethod {
                signature: "fn read(&self, addr: u8, reg: u8, buf: &mut [u8]) -> Result<usize, ServiceError>",
                delegation: "d.read(addr, reg, buf)",
            },
            ServiceMethod {
                signature: "fn write(&self, addr: u8, reg: u8, data: &[u8]) -> Result<usize, ServiceError>",
                delegation: "d.write(addr, reg, data)",
            },
        ],
        accessor: "i2c_bus",
    },
    ServiceTraitInfo {
        name: "SpiBus",
        enum_name: "SpiBusDevice",
        methods: &[
            ServiceMethod {
                signature: "fn transfer(&self, cs: u8, tx: &[u8], rx: &mut [u8]) -> Result<usize, ServiceError>",
                delegation: "d.transfer(cs, tx, rx)",
            },
        ],
        accessor: "spi_bus",
    },
    ServiceTraitInfo {
        name: "GpioController",
        enum_name: "GpioControllerDevice",
        methods: &[
            ServiceMethod {
                signature: "fn get(&self, pin: u32) -> Result<bool, ServiceError>",
                delegation: "d.get(pin)",
            },
            ServiceMethod {
                signature: "fn set(&self, pin: u32, value: bool) -> Result<(), ServiceError>",
                delegation: "d.set(pin, value)",
            },
            ServiceMethod {
                signature:
                    "fn set_direction(&self, pin: u32, output: bool) -> Result<(), ServiceError>",
                delegation: "d.set_direction(pin, output)",
            },
        ],
        accessor: "gpio",
    },
];

/// Find a ServiceTraitInfo by trait name.
fn find_service_trait(name: &str) -> Option<&'static ServiceTraitInfo> {
    SERVICE_TRAITS.iter().find(|s| s.name == name)
}

/// For a given service trait, collect all unique (driver_info, type_name) pairs
/// from the board's devices that provide that service.
fn drivers_for_service<'a>(devices: &[DeviceConfig], service_name: &str) -> Vec<&'a DriverInfo> {
    let mut result: Vec<&DriverInfo> = Vec::new();
    let mut seen_types: Vec<&str> = Vec::new();

    for dev in devices {
        // Check if this device declares the service
        if !dev.services.iter().any(|s| s.as_str() == service_name) {
            continue;
        }
        if let Some(info) = find_driver(dev.driver.as_str()) {
            // Also verify the driver actually implements this service
            if info.services.contains(&service_name) && !seen_types.contains(&info.type_name) {
                seen_types.push(info.type_name);
                result.push(info);
            }
        }
    }
    result
}

/// Collect all service trait names that are used by at least one device.
fn active_services(devices: &[DeviceConfig]) -> Vec<&'static str> {
    let mut result: Vec<&'static str> = Vec::new();
    for svc in SERVICE_TRAITS {
        if devices
            .iter()
            .any(|d| d.services.iter().any(|s| s.as_str() == svc.name))
        {
            result.push(svc.name);
        }
    }
    result
}

/// Generate service enum types and their trait implementations for flexible mode.
///
/// For each service trait that has drivers in this board, generates:
/// ```ignore
/// enum ConsoleDevice {
///     Ns16550(Ns16550),
///     Pl011(Pl011),
/// }
///
/// impl Console for ConsoleDevice {
///     fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
///         match self {
///             Self::Ns16550(d) => d.write_byte(byte),
///             Self::Pl011(d) => d.write_byte(byte),
///         }
///     }
///     // ...
/// }
/// ```
fn generate_flexible_enums(out: &mut String, devices: &[DeviceConfig]) {
    let services = active_services(devices);

    for svc_name in &services {
        let Some(svc_info) = find_service_trait(svc_name) else {
            continue;
        };

        let drivers = drivers_for_service(devices, svc_name);
        if drivers.is_empty() {
            continue;
        }

        // --- Generate enum ---
        out.push_str(&format!(
            "/// Enum dispatch for {} service (Flexible mode).\n",
            svc_info.name
        ));
        out.push_str("#[allow(dead_code)]\n");
        out.push_str(&format!("enum {} {{\n", svc_info.enum_name));
        for drv in &drivers {
            out.push_str(&format!("    {}({}),\n", drv.type_name, drv.type_name));
        }
        out.push_str("}\n\n");

        // --- Generate trait impl ---
        out.push_str(&format!(
            "impl {} for {} {{\n",
            svc_info.name, svc_info.enum_name
        ));
        for method in svc_info.methods {
            out.push_str(&format!("    {} {{\n", method.signature));
            out.push_str("        match self {\n");
            for drv in &drivers {
                out.push_str(&format!(
                    "            Self::{}(d) => {},\n",
                    drv.type_name, method.delegation
                ));
            }
            out.push_str("        }\n");
            out.push_str("    }\n\n");
        }
        out.push_str("}\n\n");
    }
}

/// In flexible mode, find the enum type name for a device based on its first
/// service. Returns the enum variant name (driver type name) and the enum
/// type name.
fn flexible_enum_for_device(dev: &DeviceConfig) -> Option<(&'static str, &'static str)> {
    // Find the first service this device provides that has a service enum
    for svc_str in &dev.services {
        if let Some(svc_info) = find_service_trait(svc_str.as_str()) {
            if let Some(drv_info) = find_driver(dev.driver.as_str()) {
                return Some((svc_info.enum_name, drv_info.type_name));
            }
        }
    }
    None
}

/// Generate the complete Rust source for a stage's main.rs.
///
/// This is the heart of fstart's "RON drives everything" philosophy.
/// The returned string is valid Rust source to be `include!()`d in the
/// `#![no_std] #![no_main]` crate root.
pub fn generate_stage_source(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let mut out = String::new();

    // File header
    out.push_str("// AUTO-GENERATED by fstart-codegen from board.ron\n");
    out.push_str("// DO NOT EDIT — changes will be overwritten.\n\n");

    // Pull in platform entry point (_start -> fstart_main) and runtime (panic handler).
    let platform = config.platform.as_str();
    match platform {
        "riscv64" => {
            out.push_str("extern crate fstart_platform_riscv64;\n");
        }
        "aarch64" => {
            out.push_str("extern crate fstart_platform_aarch64;\n");
        }
        p => {
            out.push_str(&format!("compile_error!(\"unsupported platform: {p}\");\n"));
        }
    }
    out.push_str("extern crate fstart_runtime;\n\n");

    // Get capabilities for this stage
    let capabilities = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => &mono.capabilities,
        (StageLayout::MultiStage(stages), Some(name)) => {
            if let Some(stage) = stages.iter().find(|s| s.name.as_str() == name) {
                &stage.capabilities
            } else {
                return format!("compile_error!(\"stage '{name}' not found in board config\");\n");
            }
        }
        (StageLayout::MultiStage(_), None) => {
            return "compile_error!(\"multi-stage board requires FSTART_STAGE_NAME\");\n"
                .to_string();
        }
    };

    // Validate capability ordering before generating code.
    if let Some(err) = validate_capability_ordering(capabilities) {
        return format!("compile_error!(\"{err}\");\n");
    }

    // Topological sort: validate parent references and determine init order.
    let sorted_indices = match topological_sort_devices(&config.devices) {
        Ok(indices) => indices,
        Err(err) => {
            return format!("compile_error!(\"{err}\");\n");
        }
    };

    // Build sorted device list for code generation.
    let sorted_devices: Vec<&DeviceConfig> =
        sorted_indices.iter().map(|&i| &config.devices[i]).collect();

    let mode = config.mode;

    generate_imports(&mut out, &config.devices, mode);

    if mode == BuildMode::Flexible {
        generate_flexible_enums(&mut out, &config.devices);
    }

    generate_devices_struct(&mut out, &config.devices, mode);
    generate_stage_context(&mut out, &config.devices, mode);
    generate_fstart_main(
        &mut out,
        config,
        capabilities,
        platform,
        &sorted_devices,
        mode,
    );

    out
}

/// Validate that capabilities are in a legal order.
///
/// Rules:
/// - Any capability that logs (all of them except ConsoleInit itself) must
///   come after at least one ConsoleInit.
/// - DriverInit must come after ConsoleInit (it logs device init results).
/// - StageLoad / PayloadLoad should be the last capability (nothing runs after
///   a jump). We warn but don't hard-error since the board author may know
///   what they're doing.
fn validate_capability_ordering(capabilities: &[Capability]) -> Option<String> {
    let mut console_inited = false;

    for cap in capabilities {
        match cap {
            Capability::ConsoleInit { .. } => {
                console_inited = true;
            }
            Capability::MemoryInit if !console_inited => {
                return Some(
                    "MemoryInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::DriverInit if !console_inited => {
                return Some(
                    "DriverInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::SigVerify if !console_inited => {
                return Some(
                    "SigVerify capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::FdtPrepare if !console_inited => {
                return Some(
                    "FdtPrepare capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::PayloadLoad if !console_inited => {
                return Some(
                    "PayloadLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::StageLoad { .. } if !console_inited => {
                return Some(
                    "StageLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    None
}

/// Emit `use` statements for all driver types needed by this board's devices.
fn generate_imports(out: &mut String, devices: &[DeviceConfig], mode: BuildMode) {
    out.push_str("use fstart_services::Console;\n");
    out.push_str("use fstart_services::device::Device;\n");

    // Flexible mode needs ServiceError for the enum trait impls
    if mode == BuildMode::Flexible {
        out.push_str("use fstart_services::ServiceError;\n");
    }

    // Check if any device provides bus services — import those traits too
    let has_i2c = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "I2cBus"));
    let has_spi = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "SpiBus"));
    let has_gpio = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "GpioController"));

    if has_i2c {
        out.push_str("use fstart_services::I2cBus;\n");
    }
    if has_spi {
        out.push_str("use fstart_services::SpiBus;\n");
    }
    if has_gpio {
        out.push_str("use fstart_services::GpioController;\n");
    }
    out.push('\n');

    // Collect unique driver modules
    let mut seen: Vec<&str> = Vec::new();
    for dev in devices {
        let drv_name = dev.driver.as_str();
        if !seen.contains(&drv_name) {
            if let Some(info) = find_driver(drv_name) {
                out.push_str(&format!(
                    "use {}::{{{}, {}}};\n",
                    info.module_path, info.type_name, info.config_type,
                ));
            }
            seen.push(drv_name);
        }
    }
    out.push('\n');
}

/// Emit the `Devices` struct — one concrete typed field per device.
///
/// In Rigid mode, fields use the concrete driver type (e.g., `Ns16550`).
/// In Flexible mode, fields use the generated service enum (e.g., `ConsoleDevice`).
fn generate_devices_struct(out: &mut String, devices: &[DeviceConfig], mode: BuildMode) {
    out.push_str("/// All devices for this board.\n");
    out.push_str("struct Devices {\n");
    for dev in devices {
        let field_name = dev.name.as_str();
        if let Some(info) = find_driver(dev.driver.as_str()) {
            match mode {
                BuildMode::Rigid => {
                    out.push_str(&format!("    {field_name}: {},\n", info.type_name));
                }
                BuildMode::Flexible => {
                    // Use the service enum type instead of the concrete type
                    if let Some((enum_name, _)) = flexible_enum_for_device(dev) {
                        out.push_str(&format!("    {field_name}: {enum_name},\n"));
                    } else {
                        // Fallback: use concrete type (device has no service enum)
                        out.push_str(&format!("    {field_name}: {},\n", info.type_name));
                    }
                }
            }
        } else {
            out.push_str(&format!("    // unknown driver: {}\n", dev.driver.as_str()));
        }
    }
    out.push_str("}\n\n");
}

/// Emit the `StageContext` struct with typed service accessors.
///
/// In Rigid mode, accessors return `&(impl Trait + '_)` — monomorphized.
/// In Flexible mode, accessors return `&EnumType` — the generated enum
/// implements the trait, so callers use it identically.
fn generate_stage_context(out: &mut String, devices: &[DeviceConfig], mode: BuildMode) {
    out.push_str("/// Stage context — provides typed access to services.\n");
    out.push_str("#[allow(dead_code)]\n");
    out.push_str("struct StageContext {\n");
    out.push_str("    devices: Devices,\n");
    out.push_str("}\n\n");

    out.push_str("#[allow(dead_code)]\n");
    out.push_str("impl StageContext {\n");

    for svc in SERVICE_TRAITS {
        if let Some(dev) = devices
            .iter()
            .find(|d| d.services.iter().any(|s| s.as_str() == svc.name))
        {
            let field = dev.name.as_str();
            out.push_str("    #[inline]\n");
            match mode {
                BuildMode::Rigid => {
                    out.push_str(&format!(
                        "    fn {}(&self) -> &(impl {} + '_) {{\n",
                        svc.accessor, svc.name,
                    ));
                }
                BuildMode::Flexible => {
                    out.push_str(&format!(
                        "    fn {}(&self) -> &{} {{\n",
                        svc.accessor, svc.enum_name,
                    ));
                }
            }
            out.push_str(&format!("        &self.devices.{field}\n"));
            out.push_str("    }\n\n");
        }
    }

    out.push_str("}\n\n");
}

/// Emit the `fstart_main()` function — device construction, capability
/// execution, and halt.
///
/// `sorted_devices` is the topologically sorted list of devices (parents
/// before children) for correct init ordering during DriverInit.
fn generate_fstart_main(
    out: &mut String,
    config: &BoardConfig,
    capabilities: &[Capability],
    platform: &str,
    sorted_devices: &[&DeviceConfig],
    mode: BuildMode,
) {
    let halt_fn = match platform {
        "riscv64" => "fstart_platform_riscv64::halt()",
        "aarch64" => "fstart_platform_aarch64::halt()",
        _ => "loop { core::hint::spin_loop(); }",
    };

    out.push_str("#[no_mangle]\n");
    out.push_str("pub extern \"Rust\" fn fstart_main() -> ! {\n");

    // --- Phase 1: Construct all devices in topological order (bind phase) ---
    out.push_str("    // === Device construction (bind phase, topologically sorted) ===\n");
    for dev in sorted_devices {
        generate_device_construction(out, dev, halt_fn, mode);
    }
    out.push('\n');

    // Track which devices have been initialised by capabilities so DriverInit
    // can skip them and avoid double-init.
    let mut inited_devices: Vec<String> = Vec::new();

    // Track the first console device name for passing to later capabilities.
    let mut console_device: Option<String> = None;

    // --- Phase 2: Execute capabilities in declared order ---
    out.push_str("    // === Capability execution ===\n");
    for cap in capabilities {
        match cap {
            Capability::ConsoleInit { device } => {
                let dev_name = device.as_str();
                generate_console_init(out, dev_name, &config.devices, halt_fn, mode);
                inited_devices.push(dev_name.to_string());
                if console_device.is_none() {
                    console_device = Some(dev_name.to_string());
                }
            }
            Capability::MemoryInit => {
                generate_memory_init(out, &console_device);
            }
            Capability::DriverInit => {
                generate_driver_init(
                    out,
                    sorted_devices,
                    &inited_devices,
                    halt_fn,
                    &console_device,
                    mode,
                );
                // After DriverInit, all devices are initialised.
                for dev in sorted_devices {
                    let name = dev.name.as_str().to_string();
                    if !inited_devices.contains(&name) {
                        inited_devices.push(name);
                    }
                }
            }
            Capability::SigVerify => {
                generate_sig_verify(out, &console_device);
            }
            Capability::FdtPrepare => {
                generate_fdt_prepare(out, &console_device);
            }
            Capability::PayloadLoad => {
                generate_payload_load(out, &console_device);
            }
            Capability::StageLoad { next_stage } => {
                generate_stage_load(out, next_stage.as_str(), &console_device);
            }
        }
    }

    // --- Phase 3: Build context and finalize ---
    out.push_str("\n    // === Build stage context ===\n");
    let ctx_binding = if console_device.is_some() {
        "ctx"
    } else {
        "_ctx"
    };
    out.push_str(&format!("    let {ctx_binding} = StageContext {{\n"));
    out.push_str("        devices: Devices {\n");
    for dev in &config.devices {
        if find_driver(dev.driver.as_str()).is_some() {
            out.push_str(&format!("            {0}: {0},\n", dev.name.as_str()));
        }
    }
    out.push_str("        },\n");
    out.push_str("    };\n\n");

    // Log completion via the context's console accessor (devices have been
    // moved into the Devices struct, so we use ctx instead of bare variables).
    if console_device.is_some() {
        out.push_str(
            "    let _ = ctx.console().write_line(\"[fstart] all capabilities complete\");\n",
        );
    }

    // Halt
    out.push_str("    // Stage complete — halt\n");
    out.push_str(&format!("    {halt_fn};\n"));

    out.push_str("}\n");
}

/// Generate a device construction call using the `Device` trait.
///
/// In Rigid mode, the binding is the concrete driver type directly.
/// In Flexible mode, we construct the concrete device into a temporary,
/// which is later wrapped in the service enum after init.
///
/// The split between construction and wrapping is intentional: `Device::init()`
/// is a trait method on the concrete type, not on the enum. So the init order is:
///   1. `let _uart0_inner = Ns16550::new(...)` (construction)
///   2. `_uart0_inner.init()` (ConsoleInit or DriverInit)
///   3. `let uart0 = ConsoleDevice::Ns16550(_uart0_inner)` (wrapping)
///
/// In Rigid mode, steps 1 and 3 are collapsed (no wrapping needed).
fn generate_device_construction(
    out: &mut String,
    dev: &DeviceConfig,
    halt_fn: &str,
    mode: BuildMode,
) {
    let name = dev.name.as_str();
    let drv_name = dev.driver.as_str();

    let Some(info) = find_driver(drv_name) else {
        out.push_str(&format!(
            "    compile_error!(\"unknown driver: {drv_name}\");\n"
        ));
        return;
    };

    match mode {
        BuildMode::Rigid => {
            out.push_str(&format!(
                "    let {name} = {}::new(&{} {{\n",
                info.type_name, info.config_type
            ));
            generate_config_fields(out, dev, info);
            out.push_str(&format!("    }}).unwrap_or_else(|_| {halt_fn});\n"));
        }
        BuildMode::Flexible => {
            // In flexible mode, construct into a _inner variable.
            // Wrapping into the enum happens after init (see generate_flexible_wrapping).
            let binding = if flexible_enum_for_device(dev).is_some() {
                format!("_{name}_inner")
            } else {
                name.to_string()
            };
            out.push_str(&format!(
                "    let {binding} = {}::new(&{} {{\n",
                info.type_name, info.config_type
            ));
            generate_config_fields(out, dev, info);
            out.push_str(&format!("    }}).unwrap_or_else(|_| {halt_fn});\n"));
        }
    }
}

/// Generate the enum wrapping for a device after init (Flexible mode only).
///
/// Produces: `let uart0 = ConsoleDevice::Ns16550(_uart0_inner);`
fn generate_flexible_wrapping(out: &mut String, dev: &DeviceConfig) {
    let name = dev.name.as_str();
    if let Some((enum_name, variant_name)) = flexible_enum_for_device(dev) {
        out.push_str(&format!(
            "    let {name} = {enum_name}::{variant_name}(_{name}_inner);\n"
        ));
    }
}

/// Map RON Resources to driver-specific Config fields.
fn generate_config_fields(out: &mut String, dev: &DeviceConfig, info: &DriverInfo) {
    let res = &dev.resources;
    let name = dev.name.as_str();

    match info.name {
        "ns16550" | "pl011" => {
            // UART drivers need: base_addr, clock_freq, baud_rate
            if let Some(base) = res.mmio_base {
                out.push_str(&format!("        base_addr: {base:#x},\n"));
            } else {
                out.push_str(&format!(
                    "        base_addr: compile_error!(\"device '{name}' requires mmio_base\"),\n"
                ));
            }
            out.push_str(&format!(
                "        clock_freq: {},\n",
                res.clock_freq.unwrap_or(0)
            ));
            out.push_str(&format!(
                "        baud_rate: {},\n",
                res.baud_rate.unwrap_or(115200)
            ));
        }
        "designware-i2c" => {
            // DesignWare I2C needs: base_addr, clock_freq, bus_speed
            if let Some(base) = res.mmio_base {
                out.push_str(&format!("        base_addr: {base:#x},\n"));
            } else {
                out.push_str(&format!(
                    "        base_addr: compile_error!(\"device '{name}' requires mmio_base\"),\n"
                ));
            }
            out.push_str(&format!(
                "        clock_freq: {},\n",
                res.clock_freq.unwrap_or(100_000_000)
            ));
            // Map bus_speed Hz to the I2cSpeed enum
            let speed_enum = match res.bus_speed {
                Some(s) if s > 100_000 => "fstart_drivers::i2c::designware::I2cSpeed::Fast",
                _ => "fstart_drivers::i2c::designware::I2cSpeed::Standard",
            };
            out.push_str(&format!("        bus_speed: {speed_enum},\n"));
        }
        _ => {
            out.push_str(&format!(
                "        // TODO: config mapping for driver '{}'\n",
                info.name
            ));
        }
    }
}

/// Generate code for the ConsoleInit capability.
///
/// In Flexible mode, the device variable at this point is `_uart0_inner`
/// (the concrete type). After init, we wrap it into the enum and call
/// `console_ready` on the wrapped version.
fn generate_console_init(
    out: &mut String,
    device_name: &str,
    devices: &[DeviceConfig],
    halt_fn: &str,
    mode: BuildMode,
) {
    let Some(dev) = devices.iter().find(|d| d.name.as_str() == device_name) else {
        out.push_str(&format!(
            "    compile_error!(\"ConsoleInit references device '{device_name}' which is not declared\");\n"
        ));
        return;
    };

    let drv_name = dev.driver.as_str();
    let Some(_info) = find_driver(drv_name) else {
        out.push_str(&format!(
            "    compile_error!(\"device '{device_name}' uses unknown driver '{drv_name}'\");\n"
        ));
        return;
    };

    // Check that this device actually provides Console service
    if !dev.services.iter().any(|s| s.as_str() == "Console") {
        out.push_str(&format!(
            "    compile_error!(\"ConsoleInit requires Console service but device '{device_name}' does not provide it\");\n"
        ));
        return;
    }

    out.push_str(&format!("    // ConsoleInit: {drv_name}\n"));

    match mode {
        BuildMode::Rigid => {
            out.push_str(&format!(
                "    {device_name}.init().unwrap_or_else(|_| {halt_fn});\n"
            ));
            out.push_str(&format!(
                "    fstart_capabilities::console_ready(&{device_name}, \"{device_name}\", \"{drv_name}\");\n"
            ));
        }
        BuildMode::Flexible => {
            // Init the inner concrete device, then wrap in enum
            let inner = if flexible_enum_for_device(dev).is_some() {
                format!("_{device_name}_inner")
            } else {
                device_name.to_string()
            };
            out.push_str(&format!(
                "    {inner}.init().unwrap_or_else(|_| {halt_fn});\n"
            ));
            // Wrap into enum before calling console_ready (which uses Console trait)
            generate_flexible_wrapping(out, dev);
            out.push_str(&format!(
                "    fstart_capabilities::console_ready(&{device_name}, \"{device_name}\", \"{drv_name}\");\n"
            ));
        }
    }
}

/// Generate code for the MemoryInit capability.
fn generate_memory_init(out: &mut String, console_device: &Option<String>) {
    out.push_str("    // MemoryInit\n");
    if let Some(ref con) = console_device {
        out.push_str(&format!("    fstart_capabilities::memory_init(&{con});\n"));
    }
}

/// Generate code for the DriverInit capability.
///
/// Initialises all devices that were not already initialised by earlier
/// capabilities (e.g., ConsoleInit already called init() on the UART).
/// Devices are processed in topological order (parents before children).
///
/// In Flexible mode, calls `init()` on the `_dev_inner` variable then
/// wraps into the service enum.
fn generate_driver_init(
    out: &mut String,
    sorted_devices: &[&DeviceConfig],
    already_inited: &[String],
    halt_fn: &str,
    console_device: &Option<String>,
    mode: BuildMode,
) {
    out.push_str("    // DriverInit: init remaining devices (topological order)\n");

    let mut count = 0usize;
    for dev in sorted_devices {
        let name = dev.name.as_str();
        if already_inited.iter().any(|s| s == name) {
            out.push_str(&format!("    // {name}: already initialised\n"));
            continue;
        }
        if find_driver(dev.driver.as_str()).is_none() {
            continue; // unknown driver, already compile_error'd in construction
        }

        match mode {
            BuildMode::Rigid => {
                out.push_str(&format!(
                    "    {name}.init().unwrap_or_else(|_| {halt_fn});\n"
                ));
            }
            BuildMode::Flexible => {
                let inner = if flexible_enum_for_device(dev).is_some() {
                    format!("_{name}_inner")
                } else {
                    name.to_string()
                };
                out.push_str(&format!(
                    "    {inner}.init().unwrap_or_else(|_| {halt_fn});\n"
                ));
                // Wrap into enum after init
                generate_flexible_wrapping(out, dev);
            }
        }
        count += 1;
    }

    if let Some(ref con) = console_device {
        out.push_str(&format!(
            "    fstart_capabilities::driver_init_complete(&{con}, {count});\n"
        ));
    }
}

/// Generate code for the SigVerify capability.
fn generate_sig_verify(out: &mut String, console_device: &Option<String>) {
    out.push_str("    // SigVerify\n");
    if let Some(ref con) = console_device {
        out.push_str(&format!("    fstart_capabilities::sig_verify(&{con});\n"));
    }
}

/// Generate code for the FdtPrepare capability.
fn generate_fdt_prepare(out: &mut String, console_device: &Option<String>) {
    out.push_str("    // FdtPrepare\n");
    if let Some(ref con) = console_device {
        out.push_str(&format!("    fstart_capabilities::fdt_prepare(&{con});\n"));
    }
}

/// Generate code for the PayloadLoad capability.
fn generate_payload_load(out: &mut String, console_device: &Option<String>) {
    out.push_str("    // PayloadLoad\n");
    if let Some(ref con) = console_device {
        out.push_str(&format!("    fstart_capabilities::payload_load(&{con});\n"));
    }
}

/// Generate code for the StageLoad capability.
fn generate_stage_load(out: &mut String, next_stage: &str, console_device: &Option<String>) {
    out.push_str(&format!("    // StageLoad -> {next_stage}\n"));
    if let Some(ref con) = console_device {
        out.push_str(&format!(
            "    fstart_capabilities::stage_load(&{con}, \"{next_stage}\");\n"
        ));
    }
}

#[cfg(test)]
mod tests {
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
            },
            devices,
            stages: StageLayout::Monolithic(MonolithicConfig {
                capabilities,
                load_addr: 0x8000_0000,
                stack_size: 0x10000,
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
            source.contains("fstart_capabilities::console_ready"),
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
            source.contains("fstart_capabilities::memory_init(&uart0)"),
            "should call memory_init with console ref"
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

        assert!(
            source.contains("uart0: already initialised"),
            "should skip uart0 since ConsoleInit already inited it"
        );
        assert!(
            source.contains("fstart_capabilities::driver_init_complete(&uart0, 0)"),
            "should report 0 additional devices inited"
        );
    }

    #[test]
    fn test_sig_verify_generates_call() {
        let mut caps = heapless::Vec::new();
        let _ = caps.push(Capability::ConsoleInit {
            device: heapless::String::try_from("uart0").unwrap(),
        });
        let _ = caps.push(Capability::SigVerify);
        let config = test_board_config(caps);
        let source = generate_stage_source(&config, None);

        assert!(
            source.contains("fstart_capabilities::sig_verify(&uart0)"),
            "should call sig_verify"
        );
    }

    #[test]
    fn test_stage_load_generates_call() {
        let mut caps = heapless::Vec::new();
        let _ = caps.push(Capability::ConsoleInit {
            device: heapless::String::try_from("uart0").unwrap(),
        });
        let _ = caps.push(Capability::StageLoad {
            next_stage: heapless::String::try_from("main").unwrap(),
        });
        let config = test_board_config(caps);
        let source = generate_stage_source(&config, None);

        assert!(
            source.contains("fstart_capabilities::stage_load(&uart0, \"main\")"),
            "should call stage_load with next_stage name"
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
            source.contains("[fstart] all capabilities complete"),
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
            },
            devices,
            stages: StageLayout::Monolithic(MonolithicConfig {
                capabilities,
                load_addr: 0x8000_0000,
                stack_size: 0x10000,
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
    fn test_i2c_bus_generates_i2c_bus_import() {
        let mut caps = heapless::Vec::new();
        let _ = caps.push(Capability::ConsoleInit {
            device: heapless::String::try_from("uart0").unwrap(),
        });
        let config = test_board_with_i2c_bus(caps);
        let source = generate_stage_source(&config, None);

        assert!(
            source.contains("use fstart_services::I2cBus;"),
            "should import I2cBus trait"
        );
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
        assert!(source.contains("impl I2cBus"), "should return impl I2cBus");
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
        // Use a known driver (ns16550) to keep the test simple — we just
        // need to verify init order. In a real board this would be a TPM
        // or sensor driver.
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
    fn test_flexible_multi_driver_board(
        capabilities: heapless::Vec<Capability, 16>,
    ) -> BoardConfig {
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
            },
            devices,
            stages: StageLayout::Monolithic(MonolithicConfig {
                capabilities,
                load_addr: 0x8000_0000,
                stack_size: 0x10000,
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
            source.contains("[fstart] all capabilities complete"),
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
            source.contains("impl I2cBus for I2cBusDevice"),
            "should impl I2cBus for I2cBusDevice: {source}"
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
}
