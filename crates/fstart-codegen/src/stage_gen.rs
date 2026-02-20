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
//! Code generation uses the [`quote`] crate for quasi-quoting and
//! [`prettyplease`] for formatting. See [docs/driver-model.md](../../../docs/driver-model.md).

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_types::{
    BoardConfig, BootMedium, BuildMode, Capability, DeviceConfig, FirmwareKind, PayloadKind,
    StageLayout,
};

// =======================================================================
// Driver registry
// =======================================================================

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

/// Distinguishes fstart-native traits from embedded-hal traits for codegen.
enum TraitKind {
    /// fstart-native trait with simple method delegation.
    Native { methods: &'static [ServiceMethod] },
    /// embedded-hal I2C trait (`I2c` + `ErrorType`).
    EmbeddedI2c,
    /// embedded-hal SPI trait (`SpiBus` + `ErrorType`).
    EmbeddedSpi,
}

/// Full description of a service trait for enum codegen.
struct ServiceTraitInfo {
    /// RON-level service name (e.g., "I2cBus"). Matches the value in
    /// `DeviceConfig::services`.
    name: &'static str,
    /// Generated enum name (e.g., "ConsoleDevice")
    enum_name: &'static str,
    /// What kind of trait this is and how to dispatch.
    kind: TraitKind,
    /// Accessor name on StageContext (e.g., "console")
    accessor: &'static str,
}

impl ServiceTraitInfo {
    /// The Rust trait name to use in generated code (e.g., `I2c` not `I2cBus`).
    fn rust_trait_name(&self) -> &'static str {
        match &self.kind {
            TraitKind::Native { .. } => self.name,
            TraitKind::EmbeddedI2c => "I2c",
            TraitKind::EmbeddedSpi => "SpiBus",
        }
    }

    /// Whether the StageContext accessor needs `&mut self` (embedded-hal
    /// traits take `&mut self`).
    fn is_mut_accessor(&self) -> bool {
        matches!(self.kind, TraitKind::EmbeddedI2c | TraitKind::EmbeddedSpi)
    }
}

/// Known service traits and their methods for enum dispatch generation.
const SERVICE_TRAITS: &[ServiceTraitInfo] = &[
    ServiceTraitInfo {
        name: "Console",
        enum_name: "ConsoleDevice",
        kind: TraitKind::Native {
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
        },
        accessor: "console",
    },
    ServiceTraitInfo {
        name: "BlockDevice",
        enum_name: "BlockDeviceEnum",
        kind: TraitKind::Native {
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
        },
        accessor: "block_device",
    },
    ServiceTraitInfo {
        name: "Timer",
        enum_name: "TimerDevice",
        kind: TraitKind::Native {
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
        },
        accessor: "timer",
    },
    ServiceTraitInfo {
        name: "I2cBus",
        enum_name: "I2cBusDevice",
        kind: TraitKind::EmbeddedI2c,
        accessor: "i2c_bus",
    },
    ServiceTraitInfo {
        name: "SpiBus",
        enum_name: "SpiBusDevice",
        kind: TraitKind::EmbeddedSpi,
        accessor: "spi_bus",
    },
    ServiceTraitInfo {
        name: "GpioController",
        enum_name: "GpioControllerDevice",
        kind: TraitKind::Native {
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
        },
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

// =======================================================================
// Validation
// =======================================================================

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
    let mut boot_media_declared = false;

    for cap in capabilities {
        match cap {
            Capability::ConsoleInit { .. } => {
                console_inited = true;
            }
            Capability::BootMedia(_) => {
                if !console_inited {
                    return Some(
                        "BootMedia capability requires ConsoleInit to appear earlier \
                         in the capability list (needed for logging)"
                            .to_string(),
                    );
                }
                boot_media_declared = true;
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
            Capability::SigVerify if !boot_media_declared => {
                return Some(
                    "SigVerify capability requires BootMedia to appear earlier \
                     in the capability list"
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
            Capability::PayloadLoad if !boot_media_declared => {
                return Some(
                    "PayloadLoad capability requires BootMedia to appear earlier \
                     in the capability list"
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
            Capability::StageLoad { .. } if !boot_media_declared => {
                return Some(
                    "StageLoad capability requires BootMedia to appear earlier \
                     in the capability list"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    None
}

/// Check whether a capability list uses FFS operations (SigVerify, StageLoad, PayloadLoad).
///
/// Used to decide whether the FFS anchor static needs to be emitted.
fn needs_ffs(capabilities: &[Capability]) -> bool {
    capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    })
}

/// Find the `BootMedia` capability's medium, if present.
fn get_boot_medium(capabilities: &[Capability]) -> Option<&BootMedium> {
    capabilities.iter().find_map(|c| match c {
        Capability::BootMedia(medium) => Some(medium),
        _ => None,
    })
}

/// Check whether a capability list uses FDT operations (FdtPrepare).
fn needs_fdt(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare))
}

/// Check whether this board has a LinuxBoot payload configured.
fn is_linux_boot(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == PayloadKind::LinuxBoot)
}

// =======================================================================
// Token helpers
// =======================================================================

/// Create a hex-formatted u64 literal token (e.g., `0x80000000`).
fn hex_addr(val: u64) -> TokenStream {
    let s = format!("{val:#x}");
    s.parse().expect("hex literal should parse as TokenStream")
}

/// Generate the platform halt expression (e.g., `fstart_platform_riscv64::halt()`).
fn halt_expr(platform: &str) -> TokenStream {
    match platform {
        "riscv64" => quote! { fstart_platform_riscv64::halt() },
        "aarch64" => quote! { fstart_platform_aarch64::halt() },
        _ => quote! { loop { core::hint::spin_loop() } },
    }
}

/// The `unsafe` expression that casts `&FSTART_ANCHOR` to `&[u8]` for
/// capability functions that read the anchor at runtime.
fn anchor_as_bytes_expr() -> TokenStream {
    quote! {
        unsafe {
            core::slice::from_raw_parts(
                &FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock as *const u8,
                core::mem::size_of::<fstart_types::ffs::AnchorBlock>(),
            )
        }
    }
}

// =======================================================================
// Code generation — top-level
// =======================================================================

/// Generate the complete Rust source for a stage's main.rs.
///
/// This is the heart of fstart's "RON drives everything" philosophy.
/// The returned string is valid Rust source to be `include!()`d in the
/// `#![no_std] #![no_main]` crate root.
pub fn generate_stage_source(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let platform = config.platform.as_str();

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

    // Assemble all code as a TokenStream
    let mut tokens = TokenStream::new();

    tokens.extend(generate_platform_externs(platform));
    tokens.extend(generate_imports(&config.devices, mode, capabilities));

    if let Some(BootMedium::MemoryMapped { base, size }) = get_boot_medium(capabilities) {
        tokens.extend(generate_flash_constants(*base, *size));
    }

    if needs_ffs(capabilities) {
        tokens.extend(generate_anchor_static());
    }

    if mode == BuildMode::Flexible {
        tokens.extend(generate_flexible_enums(&config.devices));
    }

    tokens.extend(generate_devices_struct(&config.devices, mode));
    tokens.extend(generate_stage_context(&config.devices, mode));
    tokens.extend(generate_fstart_main(
        config,
        capabilities,
        platform,
        &sorted_devices,
        mode,
    ));

    // Parse the token stream into a syn AST and format with prettyplease
    let file = syn::parse2::<syn::File>(tokens)
        .unwrap_or_else(|e| panic!("codegen produced unparseable Rust: {e}"));
    let formatted = prettyplease::unparse(&file);

    format!(
        "// AUTO-GENERATED by fstart-codegen from board.ron\n\
         // DO NOT EDIT \u{2014} changes will be overwritten.\n\n\
         {formatted}"
    )
}

// =======================================================================
// Code generation — individual sections
// =======================================================================

/// Generate `extern crate` items for platform and runtime.
fn generate_platform_externs(platform: &str) -> TokenStream {
    let platform_crate = match platform {
        "riscv64" => quote! { extern crate fstart_platform_riscv64; },
        "aarch64" => quote! { extern crate fstart_platform_aarch64; },
        p => {
            let msg = format!("unsupported platform: {p}");
            return quote! { compile_error!(#msg); };
        }
    };
    quote! {
        #platform_crate
        extern crate fstart_runtime;
    }
}

/// Emit `use` statements for all driver types needed by this board's devices.
fn generate_imports(
    devices: &[DeviceConfig],
    mode: BuildMode,
    capabilities: &[Capability],
) -> TokenStream {
    let mut tokens = TokenStream::new();

    tokens.extend(quote! {
        use fstart_services::Console;
        use fstart_services::device::Device;
    });

    // Flexible mode needs ServiceError for the enum trait impls
    if mode == BuildMode::Flexible {
        tokens.extend(quote! { use fstart_services::ServiceError; });
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
        tokens.extend(quote! {
            use fstart_services::i2c::{I2c, ErrorType as I2cErrorType, ErrorKind as I2cErrorKind, Operation as I2cOperation};
        });
    }
    if has_spi {
        tokens.extend(quote! {
            use fstart_services::spi::{SpiBus, ErrorType as SpiErrorType, ErrorKind as SpiErrorKind};
        });
    }
    if has_gpio {
        tokens.extend(quote! { use fstart_services::GpioController; });
    }

    // Collect unique driver modules
    let mut seen: Vec<&str> = Vec::new();
    for dev in devices {
        let drv_name = dev.driver.as_str();
        if !seen.contains(&drv_name) {
            if let Some(info) = find_driver(drv_name) {
                let module_path: TokenStream = info.module_path.parse().unwrap();
                let type_name = format_ident!("{}", info.type_name);
                let config_type = format_ident!("{}", info.config_type);
                tokens.extend(quote! {
                    use #module_path::{#type_name, #config_type};
                });
            }
            seen.push(drv_name);
        }
    }

    // Import boot media type based on the BootMedia capability variant.
    match get_boot_medium(capabilities) {
        Some(BootMedium::MemoryMapped { .. }) => {
            tokens.extend(quote! { use fstart_services::boot_media::MemoryMapped; });
        }
        Some(BootMedium::Device { .. }) => {
            tokens.extend(quote! { use fstart_services::boot_media::BlockDeviceMedia; });
        }
        None => {}
    }

    // When FDT feature is needed, pull in the alloc crate and force-link
    // the bump allocator.
    if needs_fdt(capabilities) {
        tokens.extend(quote! {
            extern crate alloc;
            extern crate fstart_alloc;
        });
    }

    tokens
}

/// Generate flash base address and size constants for FFS operations.
fn generate_flash_constants(base: u64, size: u64) -> TokenStream {
    let base_hex = hex_addr(base);
    let size_hex = hex_addr(size);
    quote! {
        /// CPU-visible base address of the firmware flash image.
        const FLASH_BASE: u64 = #base_hex;
        /// Size of the firmware flash image in bytes.
        const FLASH_SIZE: u64 = #size_hex;
    }
}

/// Emit the `FSTART_ANCHOR` static — a placeholder anchor block embedded
/// in the bootblock binary via `#[link_section = ".fstart.anchor"]`.
fn generate_anchor_static() -> TokenStream {
    quote! {
        /// FFS anchor block — patched by `xtask assemble` with real offsets.
        ///
        /// The bootblock reads this via volatile to find the FFS manifest.
        /// No scanning required at runtime.
        #[link_section = ".fstart.anchor"]
        #[used]
        static FSTART_ANCHOR: fstart_types::ffs::AnchorBlock =
            fstart_types::ffs::AnchorBlock::placeholder();
    }
}

/// Generate service enum types and their trait implementations for flexible mode.
fn generate_flexible_enums(devices: &[DeviceConfig]) -> TokenStream {
    let services = active_services(devices);
    let mut tokens = TokenStream::new();

    for svc_name in &services {
        let Some(svc_info) = find_service_trait(svc_name) else {
            continue;
        };

        let drivers = drivers_for_service(devices, svc_name);
        if drivers.is_empty() {
            continue;
        }

        let enum_name = format_ident!("{}", svc_info.enum_name);

        // Generate the enum (same for all trait kinds)
        let variants = drivers.iter().map(|drv| {
            let variant = format_ident!("{}", drv.type_name);
            quote! { #variant(#variant) }
        });

        let doc = format!(
            "Enum dispatch for {} service (Flexible mode).",
            svc_info.name
        );
        tokens.extend(quote! {
            #[doc = #doc]
            #[allow(dead_code)]
            enum #enum_name {
                #(#variants,)*
            }
        });

        // Generate trait impl based on kind
        match &svc_info.kind {
            TraitKind::Native { methods } => {
                tokens.extend(generate_native_enum_impl(svc_info, &drivers, methods));
            }
            TraitKind::EmbeddedI2c => {
                tokens.extend(generate_embedded_i2c_enum_impl(svc_info, &drivers));
            }
            TraitKind::EmbeddedSpi => {
                tokens.extend(generate_embedded_spi_enum_impl(svc_info, &drivers));
            }
        }
    }

    tokens
}

/// Generate native trait impl for a Flexible-mode enum (Console, Timer, etc.).
fn generate_native_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverInfo],
    methods: &[ServiceMethod],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);
    let trait_name = format_ident!("{}", svc_info.name);

    let method_impls = methods.iter().map(|method| {
        let sig: TokenStream = method.signature.parse().unwrap();
        let del: TokenStream = method.delegation.parse().unwrap();
        let arms = drivers.iter().map(|drv| {
            let variant = format_ident!("{}", drv.type_name);
            quote! { Self::#variant(d) => #del, }
        });
        quote! {
            #sig {
                match self {
                    #(#arms)*
                }
            }
        }
    });

    quote! {
        impl #trait_name for #enum_name {
            #(#method_impls)*
        }
    }
}

/// Generate `ErrorType` + `I2c` impls for a Flexible-mode enum.
///
/// NOTE: The enum's error type is hardcoded to `I2cErrorKind`
/// (`embedded_hal::i2c::ErrorKind`). All I2C driver variants must also use
/// `ErrorKind` as their `ErrorType::Error`. This is the standard type-erased
/// error in embedded-hal and should work for all MMIO-based controller
/// drivers. If a future driver needs a custom error type, this function
/// would need to be extended with error conversion logic.
fn generate_embedded_i2c_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverInfo],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);

    let transaction_arms = drivers.iter().map(|drv| {
        let variant = format_ident!("{}", drv.type_name);
        quote! { Self::#variant(d) => d.transaction(address, operations), }
    });

    quote! {
        impl I2cErrorType for #enum_name {
            type Error = I2cErrorKind;
        }

        impl I2c for #enum_name {
            fn transaction(
                &mut self,
                address: u8,
                operations: &mut [I2cOperation<'_>],
            ) -> Result<(), Self::Error> {
                match self {
                    #(#transaction_arms)*
                }
            }
        }
    }
}

/// Generate `ErrorType` + `SpiBus` impls for a Flexible-mode enum.
fn generate_embedded_spi_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverInfo],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);

    // Helper: generate match arms that delegate to variant `d`
    let make_arms = |delegation: &str| -> Vec<TokenStream> {
        let del: TokenStream = delegation.parse().unwrap();
        drivers
            .iter()
            .map(|drv| {
                let variant = format_ident!("{}", drv.type_name);
                quote! { Self::#variant(d) => #del, }
            })
            .collect()
    };

    let read_arms = make_arms("d.read(words)");
    let write_arms = make_arms("d.write(words)");
    let transfer_arms = make_arms("d.transfer(read, write)");
    let transfer_ip_arms = make_arms("d.transfer_in_place(words)");
    let flush_arms = make_arms("d.flush()");

    quote! {
        impl SpiErrorType for #enum_name {
            type Error = SpiErrorKind;
        }

        impl SpiBus for #enum_name {
            fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
                match self { #(#read_arms)* }
            }
            fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
                match self { #(#write_arms)* }
            }
            fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
                match self { #(#transfer_arms)* }
            }
            fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
                match self { #(#transfer_ip_arms)* }
            }
            fn flush(&mut self) -> Result<(), Self::Error> {
                match self { #(#flush_arms)* }
            }
        }
    }
}

/// Emit the `Devices` struct — one concrete typed field per device.
fn generate_devices_struct(devices: &[DeviceConfig], mode: BuildMode) -> TokenStream {
    let fields = devices.iter().filter_map(|dev| {
        let field_name = format_ident!("{}", dev.name.as_str());
        let info = find_driver(dev.driver.as_str())?;

        let field_type = match mode {
            BuildMode::Rigid => format_ident!("{}", info.type_name),
            BuildMode::Flexible => {
                if let Some((enum_name, _)) = flexible_enum_for_device(dev) {
                    format_ident!("{}", enum_name)
                } else {
                    format_ident!("{}", info.type_name)
                }
            }
        };
        Some(quote! { #field_name: #field_type, })
    });

    quote! {
        /// All devices for this board.
        struct Devices {
            #(#fields)*
        }
    }
}

/// Emit the `StageContext` struct with typed service accessors.
fn generate_stage_context(devices: &[DeviceConfig], mode: BuildMode) -> TokenStream {
    let accessors = SERVICE_TRAITS.iter().filter_map(|svc| {
        let dev = devices
            .iter()
            .find(|d| d.services.iter().any(|s| s.as_str() == svc.name))?;
        let accessor_name = format_ident!("{}", svc.accessor);
        let field = format_ident!("{}", dev.name.as_str());
        let is_mut = svc.is_mut_accessor();

        let (self_param, ref_token) = if is_mut {
            (quote! { &mut self }, quote! { &mut })
        } else {
            (quote! { &self }, quote! { & })
        };

        let return_type = match mode {
            BuildMode::Rigid => {
                let trait_name = format_ident!("{}", svc.rust_trait_name());
                quote! { #ref_token (impl #trait_name + '_) }
            }
            BuildMode::Flexible => {
                let enum_name = format_ident!("{}", svc.enum_name);
                quote! { #ref_token #enum_name }
            }
        };

        Some(quote! {
            #[inline]
            fn #accessor_name(#self_param) -> #return_type {
                #ref_token self.devices.#field
            }
        })
    });

    quote! {
        /// Stage context — provides typed access to services.
        #[allow(dead_code)]
        struct StageContext {
            devices: Devices,
        }

        #[allow(dead_code)]
        impl StageContext {
            #(#accessors)*
        }
    }
}

// =======================================================================
// Code generation — fstart_main()
// =======================================================================

/// Emit the `fstart_main()` function — device construction, capability
/// execution, and halt.
fn generate_fstart_main(
    config: &BoardConfig,
    capabilities: &[Capability],
    platform: &str,
    sorted_devices: &[&DeviceConfig],
    mode: BuildMode,
) -> TokenStream {
    let halt = halt_expr(platform);
    let mut body = TokenStream::new();

    // --- Phase 1: Construct all devices in topological order ---
    for dev in sorted_devices {
        body.extend(generate_device_construction(dev, &halt, mode));
    }

    // Track which devices have been initialised by capabilities so DriverInit
    // can skip them and avoid double-init.
    let mut inited_devices: Vec<String> = Vec::new();

    // --- Phase 2: Execute capabilities in declared order ---
    for cap in capabilities {
        match cap {
            Capability::ConsoleInit { device } => {
                let dev_name = device.as_str();
                body.extend(generate_console_init(
                    dev_name,
                    &config.devices,
                    &halt,
                    mode,
                ));
                inited_devices.push(dev_name.to_string());
            }
            Capability::BootMedia(medium) => {
                body.extend(generate_boot_media(medium, &config.devices));
            }
            Capability::MemoryInit => {
                body.extend(generate_memory_init());
            }
            Capability::DriverInit => {
                body.extend(generate_driver_init(
                    sorted_devices,
                    &inited_devices,
                    &halt,
                    mode,
                ));
                for dev in sorted_devices {
                    let name = dev.name.as_str().to_string();
                    if !inited_devices.contains(&name) {
                        inited_devices.push(name);
                    }
                }
            }
            Capability::SigVerify => {
                body.extend(generate_sig_verify());
            }
            Capability::FdtPrepare => {
                body.extend(generate_fdt_prepare(config, platform));
            }
            Capability::PayloadLoad => {
                body.extend(generate_payload_load(config, platform));
            }
            Capability::StageLoad { next_stage } => {
                body.extend(generate_stage_load(next_stage.as_str(), platform));
            }
        }
    }

    // --- Phase 3: Build context and finalize ---
    let ends_with_jump = capabilities
        .last()
        .is_some_and(|cap| matches!(cap, Capability::StageLoad { .. } | Capability::PayloadLoad));

    let device_fields = config.devices.iter().filter_map(|dev| {
        find_driver(dev.driver.as_str()).map(|_| {
            let name = format_ident!("{}", dev.name.as_str());
            quote! { #name: #name, }
        })
    });

    body.extend(quote! {
        let _ctx = StageContext {
            devices: Devices {
                #(#device_fields)*
            },
        };
    });

    if ends_with_jump {
        body.extend(quote! { #halt; });
    } else {
        body.extend(quote! {
            fstart_log::info!("all capabilities complete");
            #halt;
        });
    }

    quote! {
        #[no_mangle]
        #[allow(unreachable_code)]
        pub extern "Rust" fn fstart_main() -> ! {
            #body
        }
    }
}

// =======================================================================
// Code generation — device construction
// =======================================================================

/// Generate a device construction call using the `Device` trait.
fn generate_device_construction(
    dev: &DeviceConfig,
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let name_str = dev.name.as_str();
    let drv_name = dev.driver.as_str();

    let Some(info) = find_driver(drv_name) else {
        let msg = format!("unknown driver: {drv_name}");
        return quote! { compile_error!(#msg); };
    };

    let type_name = format_ident!("{}", info.type_name);
    let config_type = format_ident!("{}", info.config_type);
    let fields = generate_config_fields(dev, info);

    let binding = match mode {
        BuildMode::Rigid => format_ident!("{}", name_str),
        BuildMode::Flexible => {
            if flexible_enum_for_device(dev).is_some() {
                format_ident!("_{}_inner", name_str)
            } else {
                format_ident!("{}", name_str)
            }
        }
    };

    quote! {
        let #binding = #type_name::new(&#config_type {
            #fields
        }).unwrap_or_else(|_| #halt);
    }
}

/// Map RON Resources to driver-specific Config fields.
fn generate_config_fields(dev: &DeviceConfig, info: &DriverInfo) -> TokenStream {
    let res = &dev.resources;
    let name = dev.name.as_str();

    match info.name {
        "ns16550" | "pl011" => {
            let base_addr_field = if let Some(base) = res.mmio_base {
                let hex = hex_addr(base);
                quote! { base_addr: #hex, }
            } else {
                let msg = format!("device '{name}' requires mmio_base");
                quote! { base_addr: compile_error!(#msg), }
            };
            let clock = Literal::u32_unsuffixed(res.clock_freq.unwrap_or(0));
            let baud = Literal::u32_unsuffixed(res.baud_rate.unwrap_or(115200));
            quote! {
                #base_addr_field
                clock_freq: #clock,
                baud_rate: #baud,
            }
        }
        "designware-i2c" => {
            let base_addr_field = if let Some(base) = res.mmio_base {
                let hex = hex_addr(base);
                quote! { base_addr: #hex, }
            } else {
                let msg = format!("device '{name}' requires mmio_base");
                quote! { base_addr: compile_error!(#msg), }
            };
            let clock = Literal::u32_unsuffixed(res.clock_freq.unwrap_or(100_000_000));
            let speed: TokenStream = match res.bus_speed {
                Some(s) if s > 100_000 => "fstart_drivers::i2c::designware::I2cSpeed::Fast"
                    .parse()
                    .unwrap(),
                _ => "fstart_drivers::i2c::designware::I2cSpeed::Standard"
                    .parse()
                    .unwrap(),
            };
            quote! {
                #base_addr_field
                clock_freq: #clock,
                bus_speed: #speed,
            }
        }
        _ => TokenStream::new(),
    }
}

/// Generate the enum wrapping for a device after init (Flexible mode only).
fn generate_flexible_wrapping(dev: &DeviceConfig) -> TokenStream {
    let name_str = dev.name.as_str();
    if let Some((enum_name_str, variant_name_str)) = flexible_enum_for_device(dev) {
        let name = format_ident!("{}", name_str);
        let enum_name = format_ident!("{}", enum_name_str);
        let variant_name = format_ident!("{}", variant_name_str);
        let inner = format_ident!("_{}_inner", name_str);
        quote! { let #name = #enum_name::#variant_name(#inner); }
    } else {
        TokenStream::new()
    }
}

// =======================================================================
// Code generation — capabilities
// =======================================================================

/// Generate code for the ConsoleInit capability.
fn generate_console_init(
    device_name: &str,
    devices: &[DeviceConfig],
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let Some(dev) = devices.iter().find(|d| d.name.as_str() == device_name) else {
        let msg = format!("ConsoleInit references device '{device_name}' which is not declared");
        return quote! { compile_error!(#msg); };
    };

    let drv_name = dev.driver.as_str();
    let Some(_info) = find_driver(drv_name) else {
        let msg = format!("device '{device_name}' uses unknown driver '{drv_name}'");
        return quote! { compile_error!(#msg); };
    };

    if !dev.services.iter().any(|s| s.as_str() == "Console") {
        let msg = format!(
            "ConsoleInit requires Console service but device '{device_name}' does not provide it"
        );
        return quote! { compile_error!(#msg); };
    }

    let device = format_ident!("{}", device_name);

    match mode {
        BuildMode::Rigid => {
            quote! {
                #device.init().unwrap_or_else(|_| #halt);
                unsafe { fstart_log::init(&#device) };
                fstart_capabilities::console_ready(#device_name, #drv_name);
            }
        }
        BuildMode::Flexible => {
            let inner = if flexible_enum_for_device(dev).is_some() {
                format_ident!("_{}_inner", device_name)
            } else {
                format_ident!("{}", device_name)
            };
            let wrapping = generate_flexible_wrapping(dev);
            quote! {
                #inner.init().unwrap_or_else(|_| #halt);
                #wrapping
                unsafe { fstart_log::init(&#device) };
                fstart_capabilities::console_ready(#device_name, #drv_name);
            }
        }
    }
}

/// Generate code for the MemoryInit capability.
fn generate_memory_init() -> TokenStream {
    quote! { fstart_capabilities::memory_init(); }
}

/// Generate code for the DriverInit capability.
fn generate_driver_init(
    sorted_devices: &[&DeviceConfig],
    already_inited: &[String],
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let mut stmts = TokenStream::new();
    let mut count = 0usize;

    for dev in sorted_devices {
        let name_str = dev.name.as_str();
        if already_inited.iter().any(|s| s == name_str) {
            continue;
        }
        if find_driver(dev.driver.as_str()).is_none() {
            continue;
        }

        match mode {
            BuildMode::Rigid => {
                let name = format_ident!("{}", name_str);
                stmts.extend(quote! {
                    #name.init().unwrap_or_else(|_| #halt);
                });
            }
            BuildMode::Flexible => {
                let inner = if flexible_enum_for_device(dev).is_some() {
                    format_ident!("_{}_inner", name_str)
                } else {
                    format_ident!("{}", name_str)
                };
                stmts.extend(quote! {
                    #inner.init().unwrap_or_else(|_| #halt);
                });
                stmts.extend(generate_flexible_wrapping(dev));
            }
        }
        count += 1;
    }

    let count_lit = Literal::usize_unsuffixed(count);
    stmts.extend(quote! {
        fstart_capabilities::driver_init_complete(#count_lit);
    });

    stmts
}

/// Generate code for the BootMedia capability.
fn generate_boot_media(medium: &BootMedium, devices: &[DeviceConfig]) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { .. } => {
            quote! {
                let boot_media = unsafe {
                    MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)
                };
            }
        }
        BootMedium::Device { name } => {
            let dev_name = name.as_str();
            let dev_size = devices
                .iter()
                .find(|d| d.name.as_str() == dev_name)
                .and_then(|d| d.resources.size);
            if let Some(size) = dev_size {
                let dev_ident = format_ident!("{}", dev_name);
                let size_lit = Literal::u64_unsuffixed(size);
                quote! {
                    let boot_media = BlockDeviceMedia::new(&#dev_ident, 0, #size_lit as usize);
                }
            } else {
                let msg = format!(
                    "BootMedia device '{dev_name}' has no resources.size \u{2014} cannot determine media size"
                );
                quote! { compile_error!(#msg); }
            }
        }
    }
}

/// Generate code for the SigVerify capability.
fn generate_sig_verify() -> TokenStream {
    let anchor = anchor_as_bytes_expr();
    quote! {
        fstart_capabilities::sig_verify(#anchor, &boot_media);
    }
}

/// Generate code for the FdtPrepare capability.
fn generate_fdt_prepare(config: &BoardConfig, platform: &str) -> TokenStream {
    let Some(ref payload) = config.payload else {
        return quote! { fstart_capabilities::fdt_prepare_stub(); };
    };

    match &payload.fdt {
        fstart_types::FdtSource::Platform => {
            let dtb_src_expr = if let Some(addr) = payload.src_dtb_addr {
                hex_addr(addr)
            } else {
                match platform {
                    "riscv64" => quote! { fstart_platform_riscv64::boot_dtb_addr() },
                    "aarch64" => quote! { fstart_platform_aarch64::boot_dtb_addr() },
                    _ => quote! { 0 },
                }
            };
            let dtb_dst = hex_addr(payload.dtb_addr.unwrap_or(0));
            let bootargs = payload.bootargs.as_ref().map(|s| s.as_str()).unwrap_or("");
            quote! {
                fstart_capabilities::fdt_prepare_platform(#dtb_src_expr, #dtb_dst, #bootargs);
            }
        }
        _ => {
            quote! { fstart_capabilities::fdt_prepare_stub(); }
        }
    }
}

/// Generate code for the PayloadLoad capability.
fn generate_payload_load(config: &BoardConfig, platform: &str) -> TokenStream {
    if is_linux_boot(config) {
        return generate_payload_load_linux(config, platform);
    }

    let anchor = anchor_as_bytes_expr();
    let jump_fn: TokenStream = match platform {
        "riscv64" => quote! { fstart_platform_riscv64::jump_to },
        "aarch64" => quote! { fstart_platform_aarch64::jump_to },
        _ => quote! { fstart_platform_riscv64::jump_to },
    };
    quote! {
        fstart_capabilities::payload_load(#anchor, &boot_media, #jump_fn);
    }
}

/// Generate the Linux boot payload sequence for a specific platform.
fn generate_payload_load_linux(config: &BoardConfig, platform: &str) -> TokenStream {
    let payload = config.payload.as_ref().unwrap(); // caller verified is_linux_boot
    let halt = halt_expr(platform);
    let anchor = anchor_as_bytes_expr();

    let mut stmts = TokenStream::new();

    stmts.extend(quote! {
        fstart_log::info!("capability: PayloadLoad (LinuxBoot)");
        fstart_log::info!("loading kernel...");
    });

    stmts.extend(quote! {
        if !fstart_capabilities::load_ffs_file_by_type(
            #anchor,
            &boot_media,
            fstart_types::ffs::FileType::Payload,
        ) {
            fstart_log::error!("FATAL: failed to load kernel");
            #halt;
        }
    });

    // Load firmware blob from FFS
    if let Some(ref fw) = payload.firmware {
        let fw_kind_str = match fw.kind {
            FirmwareKind::OpenSbi => "SBI firmware",
            FirmwareKind::ArmTrustedFirmware => "ATF BL31",
        };
        let load_msg = format!("loading {fw_kind_str}...");
        let error_msg = format!("FATAL: failed to load {fw_kind_str}");
        let anchor2 = anchor_as_bytes_expr();
        stmts.extend(quote! {
            fstart_log::info!(#load_msg);
            if !fstart_capabilities::load_ffs_file_by_type(
                #anchor2,
                &boot_media,
                fstart_types::ffs::FileType::Firmware,
            ) {
                fstart_log::error!(#error_msg);
                #halt;
            }
        });
    }

    // Platform-specific boot protocol
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));
    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));

    match platform {
        "riscv64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let _fw_info = fstart_platform_riscv64::FwDynamicInfo::new(
                    #kernel_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                );
                fstart_log::info!("jumping to SBI firmware...");
                fstart_platform_riscv64::boot_linux_sbi(
                    #fw_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                    #dtb_addr,
                    &_fw_info,
                );
            });
        }
        "aarch64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let mut _bl33_ep: fstart_platform_aarch64::EntryPointInfo =
                    unsafe { core::mem::zeroed() };
                let mut _bl33_node: fstart_platform_aarch64::BlParamsNode =
                    unsafe { core::mem::zeroed() };
                let mut _bl_params: fstart_platform_aarch64::BlParams =
                    unsafe { core::mem::zeroed() };
                fstart_platform_aarch64::prepare_bl_params(
                    #kernel_addr,
                    #dtb_addr,
                    &mut _bl33_ep,
                    &mut _bl33_node,
                    &mut _bl_params,
                );
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform_aarch64::boot_linux_atf(#fw_addr, &_bl_params);
            });
        }
        _ => {
            let msg = format!("LinuxBoot not supported on platform '{platform}'");
            stmts.extend(quote! { compile_error!(#msg); });
        }
    }

    stmts
}

/// Generate code for the StageLoad capability.
fn generate_stage_load(next_stage: &str, platform: &str) -> TokenStream {
    let anchor = anchor_as_bytes_expr();
    let jump_fn: TokenStream = match platform {
        "riscv64" => quote! { fstart_platform_riscv64::jump_to },
        "aarch64" => quote! { fstart_platform_aarch64::jump_to },
        _ => quote! { fstart_platform_riscv64::jump_to },
    };
    quote! {
        fstart_capabilities::stage_load(#next_stage, #anchor, &boot_media, #jump_fn);
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
}
