//! Generate stage entry point code from board configuration.
//!
//! Given a [`ParsedBoard`] (or a specific stage within it), this module
//! emits Rust source code that:
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
//! Driver-specific configuration comes from [`DriverInstance`] — each driver
//! defines its own typed `Config` struct.  The `config_ser` module converts
//! the validated config into a `TokenStream` for the generated source.
//!
//! Code generation uses the [`quote`] crate for quasi-quoting and
//! [`prettyplease`] for formatting. See [docs/driver-model.md](../../../docs/driver-model.md).

mod capabilities;
mod config_ser;
mod flexible;
pub(crate) mod registry;
mod tokens;
mod topology;
mod validation;

#[cfg(test)]
mod tests;

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_drivers::DriverInstance;
use fstart_types::{
    BoardConfig, BootMedium, BuildMode, Capability, DeviceConfig, DeviceNode, StageLayout,
};

use crate::ron_loader::ParsedBoard;

use capabilities::{
    generate_boot_media, generate_console_init, generate_driver_init, generate_fdt_prepare,
    generate_memory_init, generate_payload_load, generate_sig_verify, generate_stage_load,
};
use config_ser::{config_tokens, driver_type_tokens};
use flexible::{flexible_enum_for_device, generate_flexible_enums, SERVICE_TRAITS};
use tokens::{halt_expr, hex_addr};
use topology::validate_device_tree;
use validation::{get_boot_medium, needs_fdt, needs_ffs, validate_capability_ordering};

// =======================================================================
// Code generation — top-level
// =======================================================================

/// Generate the complete Rust source for a stage's main.rs.
///
/// This is the heart of fstart's "RON drives everything" philosophy.
/// The returned string is valid Rust source to be `include!()`d in the
/// `#![no_std] #![no_main]` crate root.
pub fn generate_stage_source(parsed: &ParsedBoard, stage_name: Option<&str>) -> String {
    let config = &parsed.config;
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

    // Extract heap_size for this stage (used for allocator backing store).
    let heap_size: Option<u32> = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => mono.heap_size,
        (StageLayout::MultiStage(stages), Some(name)) => stages
            .iter()
            .find(|s| s.name.as_str() == name)
            .and_then(|s| s.heap_size),
        _ => None,
    };

    // Validate that heap_size is set when FdtPrepare needs the allocator.
    if needs_fdt(capabilities) && heap_size.is_none() {
        return "compile_error!(\"FdtPrepare requires heap_size in stage config\");\n".to_string();
    }

    // Validate device tree (bus service requirements).
    // Ordering is already correct — ron_loader flattens in pre-order DFS.
    if let Err(err) = validate_device_tree(&config.devices, &parsed.device_tree) {
        return format!("compile_error!(\"{err}\");\n");
    }

    let mode = config.mode;

    // Assemble all code as a TokenStream
    let mut tokens = TokenStream::new();

    tokens.extend(generate_platform_externs(platform));
    tokens.extend(generate_imports(
        &config.devices,
        &parsed.driver_instances,
        mode,
        capabilities,
    ));

    if let Some(BootMedium::MemoryMapped { base, size }) = get_boot_medium(capabilities) {
        tokens.extend(generate_flash_constants(*base, *size));
    }

    if needs_ffs(capabilities) {
        tokens.extend(generate_anchor_static());
    }

    // When the allocator is needed, generate a sized heap backing store.
    // fstart-alloc references these symbols via `extern "C"`.
    if let Some(hs) = heap_size {
        tokens.extend(generate_heap_storage(hs));
    }

    if mode == BuildMode::Flexible {
        tokens.extend(generate_flexible_enums(
            &config.devices,
            &parsed.driver_instances,
        ));
    }

    tokens.extend(generate_devices_struct(
        &config.devices,
        &parsed.driver_instances,
        mode,
    ));
    tokens.extend(generate_stage_context(
        &config.devices,
        &parsed.driver_instances,
        mode,
    ));
    tokens.extend(generate_device_tree_table(&parsed.device_tree));
    tokens.extend(generate_fstart_main(
        config,
        &parsed.driver_instances,
        &parsed.device_tree,
        capabilities,
        platform,
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
    instances: &[DriverInstance],
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

    // Collect unique driver modules and import all public types via glob
    let mut seen_modules: Vec<&str> = Vec::new();
    for inst in instances {
        let meta = inst.meta();
        if !seen_modules.contains(&meta.module_path) {
            let module_path: TokenStream = meta.module_path.parse().unwrap();
            tokens.extend(quote! {
                use #module_path::*;
            });
            seen_modules.push(meta.module_path);
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

/// Generate heap backing store and size constant for the bump allocator.
///
/// Emits a 16-byte-aligned `#[no_mangle]` static that `fstart-alloc`
/// references via `extern "C"` to locate the heap at link time.
fn generate_heap_storage(heap_size: u32) -> TokenStream {
    let size_lit = Literal::usize_unsuffixed(heap_size as usize);
    quote! {
        /// Heap backing store — sized by the board RON `heap_size` field.
        #[repr(align(16))]
        #[allow(dead_code)]
        struct _FstartHeapStore(core::cell::UnsafeCell<[u8; #size_lit]>);

        // SAFETY: The bump allocator synchronises access via an atomic cursor.
        // Firmware is single-threaded at this point.
        unsafe impl Sync for _FstartHeapStore {}

        #[no_mangle]
        static _FSTART_HEAP: _FstartHeapStore =
            _FstartHeapStore(core::cell::UnsafeCell::new([0u8; #size_lit]));

        #[no_mangle]
        static _FSTART_HEAP_SIZE: usize = #size_lit;
    }
}

// =======================================================================
// Code generation — device tree table (approach B)
// =======================================================================

/// Emit the `static DEVICE_TREE` table — flat, index-based device tree
/// in `.rodata` for runtime introspection.
fn generate_device_tree_table(tree: &[DeviceNode]) -> TokenStream {
    let n = tree.len();
    let entries = tree.iter().map(|node| {
        let parent = match node.parent {
            Some(idx) => {
                let idx_lit = idx;
                quote! { Some(#idx_lit) }
            }
            None => quote! { None },
        };
        let depth = node.depth;
        quote! {
            fstart_types::DeviceNode { parent: #parent, depth: #depth }
        }
    });

    quote! {
        /// Flat device tree — parents before children, index-based references.
        ///
        /// Use `DEVICE_TREE[i].parent` to walk up to a device's bus controller.
        /// Guaranteed topological order: parent index < child index.
        #[allow(dead_code)]
        static DEVICE_TREE: [fstart_types::DeviceNode; #n] = [
            #(#entries,)*
        ];
    }
}

// =======================================================================
// Code generation — structs and context
// =======================================================================

/// Emit the `Devices` struct — one concrete typed field per device.
fn generate_devices_struct(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    mode: BuildMode,
) -> TokenStream {
    let fields = devices.iter().zip(instances.iter()).map(|(dev, inst)| {
        let field_name = format_ident!("{}", dev.name.as_str());
        let meta = inst.meta();

        let field_type = match mode {
            BuildMode::Rigid => format_ident!("{}", meta.type_name),
            BuildMode::Flexible => {
                if let Some((enum_name, _)) = flexible_enum_for_device(dev, inst) {
                    format_ident!("{}", enum_name)
                } else {
                    format_ident!("{}", meta.type_name)
                }
            }
        };
        quote! { #field_name: #field_type, }
    });

    quote! {
        /// All devices for this board.
        struct Devices {
            #(#fields)*
        }
    }
}

/// Emit the `StageContext` struct with typed service accessors.
fn generate_stage_context(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    mode: BuildMode,
) -> TokenStream {
    let accessors = SERVICE_TRAITS.iter().filter_map(|svc| {
        let (idx, dev) = devices
            .iter()
            .enumerate()
            .find(|(_, d)| d.services.iter().any(|s| s.as_str() == svc.name))?;
        let _inst = &instances[idx];
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
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    capabilities: &[Capability],
    platform: &str,
    mode: BuildMode,
) -> TokenStream {
    let halt = halt_expr(platform);
    let mut body = TokenStream::new();

    // --- Phase 1: Construct all devices in topological order ---
    // Devices are already in pre-order DFS order from RON flattening.
    for (idx, node) in device_tree.iter().enumerate() {
        let dev = &config.devices[idx];
        let inst = &instances[idx];
        let parent_name = node
            .parent
            .map(|pid| config.devices[pid as usize].name.as_str());
        body.extend(generate_device_construction(
            dev,
            inst,
            parent_name,
            &halt,
            mode,
        ));
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
                    instances,
                    &halt,
                    mode,
                ));
                inited_devices.push(dev_name.to_string());
            }
            Capability::BootMedia(medium) => {
                body.extend(generate_boot_media(medium));
            }
            Capability::MemoryInit => {
                body.extend(generate_memory_init());
            }
            Capability::DriverInit => {
                // Devices are in pre-order DFS — sequential indices are
                // already topological order.
                let sequential: Vec<usize> = (0..config.devices.len()).collect();
                body.extend(generate_driver_init(
                    &config.devices,
                    instances,
                    &sequential,
                    &inited_devices,
                    &halt,
                    mode,
                ));
                for idx in 0..config.devices.len() {
                    let name = config.devices[idx].name.as_str().to_string();
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

    let device_fields = config.devices.iter().map(|dev| {
        let name = format_ident!("{}", dev.name.as_str());
        quote! { #name: #name, }
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

/// Generate a device construction call.
///
/// Root devices use `Device::new(&config)`.
/// Bus children use `BusDevice::new_on_bus(&config, &parent)` — the parent
/// variable name is resolved at codegen time (approach A: compile-away).
fn generate_device_construction(
    dev: &DeviceConfig,
    instance: &DriverInstance,
    parent_name: Option<&str>,
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let name_str = dev.name.as_str();
    let type_name = driver_type_tokens(instance);
    let config = config_tokens(instance);

    let binding = match mode {
        BuildMode::Rigid => format_ident!("{}", name_str),
        BuildMode::Flexible => {
            if flexible_enum_for_device(dev, instance).is_some() {
                format_ident!("_{}_inner", name_str)
            } else {
                format_ident!("{}", name_str)
            }
        }
    };

    match parent_name {
        None => {
            // Root device — Device::new(&config)
            quote! {
                let #binding = #type_name::new(&#config).unwrap_or_else(|_| #halt);
            }
        }
        Some(parent) => {
            // Bus child — BusDevice::new_on_bus(&config, &parent)
            let parent_ident = format_ident!("{}", parent);
            quote! {
                let #binding = #type_name::new_on_bus(&#config, &#parent_ident).unwrap_or_else(|_| #halt);
            }
        }
    }
}
