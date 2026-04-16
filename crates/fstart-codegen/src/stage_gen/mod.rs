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

use fstart_device_registry::DriverInstance;
use fstart_types::{
    BoardConfig, BootMedium, BuildMode, Capability, DeviceConfig, DeviceId, DeviceNode, Platform,
    StageLayout,
};

use crate::ron_loader::ParsedBoard;

use capabilities::{
    collect_boot_media_gated_devices, generate_acpi_load, generate_acpi_prepare,
    generate_boot_media, generate_chipset_init, generate_clock_init, generate_console_init,
    generate_dram_init, generate_driver_init, generate_fdt_prepare, generate_late_driver_init,
    generate_load_next_stage, generate_memory_detect, generate_memory_init, generate_payload_load,
    generate_pci_init, generate_return_to_fel, generate_sig_verify, generate_smbios_prepare,
    generate_stage_load,
};
use config_ser::{config_tokens, driver_type_tokens};
use flexible::{
    flexible_enum_for_device, generate_flexible_enums, generate_flexible_wrapping, SERVICE_TRAITS,
};
use tokens::{halt_expr, hex_addr};
use topology::validate_device_tree;
use validation::{get_boot_medium, needs_embedded_anchor, needs_ffs, validate_capability_ordering};

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
    let platform = config.platform;

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
    if let Some(err) = validate_capability_ordering(capabilities, config) {
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

    // Validate device tree (bus service requirements).
    // Ordering is already correct — ron_loader flattens in pre-order DFS.
    if let Err(err) = validate_device_tree(
        &config.devices,
        &parsed.driver_instances,
        &parsed.device_tree,
    ) {
        return format!("compile_error!(\"{err}\");\n");
    }

    let mode = config.mode;

    // Anchor strategy: every stage that uses FFS embeds FSTART_ANCHOR.
    // The FFS builder patches the placeholder in whichever binary contains
    // it.  This avoids fragile runtime scanning of boot media.
    let embed_anchor = needs_ffs(capabilities);

    // Assemble all code as a TokenStream
    let mut tokens = TokenStream::new();

    tokens.extend(generate_platform_externs(platform));
    tokens.extend(generate_imports(
        &config.devices,
        &parsed.driver_instances,
        mode,
        capabilities,
        embed_anchor,
    ));

    // Allwinner eGON: emit the eGON.BT0 header struct and branch
    // instruction in dedicated linker sections.  The platform _start is
    // in .text.entry; the linker script orders .head before .text.
    // Only for the first stage (BROM loads it) — later stages don't need the header.
    let is_first_stage = needs_embedded_anchor(&config.stages, stage_name);
    if is_first_stage {
        if let fstart_types::SocImageFormat::AllwinnerEgon = config.soc_image_format {
            tokens.extend(generate_allwinner_egon_header(platform));
        }
    }

    if let Some(BootMedium::MemoryMapped { base, size, .. }) = get_boot_medium(capabilities) {
        tokens.extend(generate_flash_constants(*base, *size));
    }

    if embed_anchor {
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

    // Bus children (e.g., PCI child devices) are only constructed in
    // stages with DriverInit. For bootblock stages without DriverInit,
    // exclude these from the Devices struct and StageContext.
    let has_driver_init = capabilities
        .iter()
        .any(|c| matches!(c, Capability::DriverInit));
    let excluded_indices: Vec<usize> = if !has_driver_init {
        parsed
            .device_tree
            .iter()
            .enumerate()
            .filter(|(_, node)| node.parent.is_some())
            .map(|(idx, _)| idx)
            .collect()
    } else {
        Vec::new()
    };

    tokens.extend(generate_devices_struct(
        &config.devices,
        &parsed.driver_instances,
        mode,
        &excluded_indices,
    ));
    tokens.extend(generate_stage_context(
        &config.devices,
        &parsed.driver_instances,
        mode,
        &excluded_indices,
    ));
    tokens.extend(generate_device_tree_table(&parsed.device_tree));
    tokens.extend(generate_fstart_main(
        config,
        &parsed.driver_instances,
        &parsed.device_tree,
        capabilities,
        platform,
        mode,
        embed_anchor,
        stage_name,
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
///
/// The platform crate is aliased to `fstart_platform` so that all
/// downstream codegen can reference `fstart_platform::halt()`,
/// `fstart_platform::jump_to()`, etc. without matching on the platform.
fn generate_platform_externs(platform: Platform) -> TokenStream {
    let platform_crate = match platform {
        Platform::Riscv64 => {
            quote! { extern crate fstart_platform_riscv64 as fstart_platform; }
        }
        Platform::Aarch64 => {
            quote! { extern crate fstart_platform_aarch64 as fstart_platform; }
        }
        Platform::Armv7 => {
            quote! { extern crate fstart_platform_armv7 as fstart_platform; }
        }
        Platform::X86_64 => {
            quote! { extern crate fstart_platform_x86_64 as fstart_platform; }
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
    _embed_anchor: bool,
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
    let has_block_device = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "BlockDevice"));
    if has_block_device {
        tokens.extend(quote! { use fstart_services::BlockDevice; });
    }

    // Import MemoryController trait only if this stage uses DramInit
    // (and thus calls detected_size_bytes() on the DRAM controller).
    let uses_dram_init = capabilities
        .iter()
        .any(|c| matches!(c, Capability::DramInit { .. }));
    if uses_dram_init {
        tokens.extend(quote! { use fstart_services::MemoryController; });
    }

    // BusDevice trait is needed when any device has a parent bus (e.g., PCI
    // child devices use BusDevice::new_on_bus).
    let has_bus_children = devices.iter().any(|d| d.parent.is_some());
    if has_bus_children {
        tokens.extend(quote! { use fstart_services::device::BusDevice; });
    }

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

    let has_pci = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "PciRootBus"));
    if has_pci {
        tokens.extend(quote! { use fstart_services::PciRootBus; });
    }

    let has_framebuffer = devices
        .iter()
        .any(|d| d.services.iter().any(|s| s.as_str() == "Framebuffer"));
    if has_framebuffer {
        tokens.extend(quote! { use fstart_services::Framebuffer; });
    }

    // Collect unique driver modules and import all public types via glob.
    // ACPI-only and structural devices are skipped — their types live in
    // fstart_types/fstart_acpi and fstart_device_registry respectively,
    // and are only used at codegen time, not in the generated stage code.
    let mut seen_modules: Vec<&str> = Vec::new();
    for inst in instances {
        if inst.is_acpi_only() || inst.is_structural() {
            continue;
        }
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
            // Import the BootMedia trait so as_slice() / read_at() are
            // callable in FFS loading code (PayloadLoad, SigVerify, etc.).
            if needs_ffs(capabilities) {
                tokens.extend(quote! { use fstart_services::BootMedia; });
            }
        }
        Some(BootMedium::Device { .. }) => {
            tokens.extend(quote! { use fstart_services::boot_media::BlockDeviceMedia; });
            // Import the BootMedia trait so read_at() is callable in the
            // anchor scan and FFS loading code.
            if needs_ffs(capabilities) {
                tokens.extend(quote! { use fstart_services::BootMedia; });
            }
        }
        Some(BootMedium::AutoDevice { .. }) => {
            tokens.extend(quote! { use fstart_services::boot_media::BlockDeviceMedia; });
            // AutoDevice generates a BlockDevice dispatch enum and
            // wraps it in BlockDeviceMedia. BootMedia trait needed for
            // anchor scan and FFS loading.
            if needs_ffs(capabilities) {
                tokens.extend(quote! { use fstart_services::BootMedia; });
            }
        }
        None => {}
    }

    // AcpiLoad needs the AcpiTableProvider trait
    let uses_acpi_load = capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiLoad { .. }));
    if uses_acpi_load {
        tokens.extend(quote! { use fstart_services::acpi_provider::AcpiTableProvider; });
    }

    // MemoryDetect: E820Entry type is used in the generated variable
    // declarations. The MemoryDetector trait itself is imported by the
    // fstart_capabilities::memory_detect() function, not the generated code.

    // FDT patching no longer requires alloc — the raw FDT patcher
    // operates directly on the blob without heap allocation.

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

/// Emit the Allwinner eGON.BT0 header for the binary image.
///
/// Generates:
/// 1. A `global_asm!` block placing a branch-to-`_start` instruction in
///    `.head.text` (the very first bytes of the binary).
/// 2. A `#[link_section = ".head.egon"]` static with the eGON header
///    struct — magic and sentinel checksum; length is a placeholder (0).
///
/// The linker script orders: `.head.text` → `.head.egon` → `.text.entry`.
/// Xtask computes the actual binary size (512-byte aligned), pads the
/// binary, and patches both the length and checksum fields post-build.
///
/// On ARMv7, the branch is `.arm` + `b _start`.
/// On AArch64 (sun50i H5/A64), the branch is a raw `.word 0xEA000016` —
/// the ARM32 encoding of `b .+0x60` that jumps over the 96-byte eGON
/// header.  The AArch64 assembler cannot emit ARM32 instructions, so
/// the branch must be encoded manually.  The `_start` entry point in
/// `entry_sunxi.rs` handles the AArch32→AArch64 RMR switch.
fn generate_allwinner_egon_header(platform: Platform) -> TokenStream {
    let branch_asm = if platform == Platform::Aarch64 {
        // AArch64 target: emit the ARM32 branch as a raw .word.
        // 0xEA000016 = ARM32 "b .+0x60" (branch forward 22 words from
        // PC+8 = 96 bytes = offset 0x60, past the eGON header).
        quote! {
            core::arch::global_asm!(
                ".section .head.text, \"ax\", %progbits",
                ".global _head_jump",
                "_head_jump:",
                ".word 0xEA000016",
            );
        }
    } else if platform == Platform::Riscv64 {
        // RISC-V target: emit an RV64 `j _start` instruction.
        // The RISC-V BROM on Allwinner D1 loads the eGON image into
        // SRAM at 0x20000 and jumps to offset 0x00. We emit `j _start`
        // which the assembler encodes as a JAL with rd=x0 (J-type).
        // The eGON header follows at offset 0x04, and `_start` is at
        // offset 0x60 (after the 92-byte header + 4-byte branch).
        quote! {
            core::arch::global_asm!(
                ".section .head.text, \"ax\"",
                ".global _head_jump",
                "_head_jump:",
                "j _start",
            );
        }
    } else {
        // ARMv7 target: assembler natively supports ARM mode.
        quote! {
            core::arch::global_asm!(
                ".section .head.text, \"ax\", %progbits",
                ".arm",
                ".global _head_jump",
                "_head_jump:",
                "b _start",
            );
        }
    };

    quote! {
        #branch_asm

        /// Allwinner eGON.BT0 header — length and checksum are placeholders,
        /// patched by xtask post-build from the actual binary size.
        #[link_section = ".head.egon"]
        #[used]
        static EGON_HEAD: fstart_soc_sunxi::EgonHead =
            fstart_soc_sunxi::EgonHead::new();
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
///
/// ACPI-only devices (no runtime driver) are excluded.
/// `excluded_indices` are device indices that this stage won't construct
/// (e.g., bus children in stages without DriverInit).
fn generate_devices_struct(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    mode: BuildMode,
    excluded_indices: &[usize],
) -> TokenStream {
    let fields = devices
        .iter()
        .zip(instances.iter())
        .enumerate()
        .filter(|(idx, (dev, inst))| {
            !inst.is_acpi_only()
                && !inst.is_structural()
                && dev.enabled
                && !excluded_indices.contains(idx)
        })
        .map(|(_, (dev, inst))| {
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
        #[allow(dead_code)]
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
    excluded_indices: &[usize],
) -> TokenStream {
    let accessors = SERVICE_TRAITS.iter().filter_map(|svc| {
        let (idx, dev) = devices.iter().enumerate().find(|(idx, d)| {
            !excluded_indices.contains(idx) && d.services.iter().any(|s| s.as_str() == svc.name)
        })?;
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
///
/// `embed_anchor` controls how the FFS anchor is accessed:
/// - `true`: the stage has an embedded `FSTART_ANCHOR` static (patched
///   by the builder). Capability functions read it via volatile.
/// - `false`: the stage scans the `boot_media` for the anchor at runtime
///   (used by non-first stages in a LoadNextStage multi-stage flow).
#[allow(clippy::too_many_arguments)]
fn generate_fstart_main(
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    capabilities: &[Capability],
    platform: Platform,
    mode: BuildMode,
    embed_anchor: bool,
    stage_name: Option<&str>,
) -> TokenStream {
    let halt = halt_expr(platform);
    let mut body = TokenStream::new();

    // --- Pre-phase: Declare mutable state for cross-capability communication ---
    // These variables are written by one capability and read by another (e.g.,
    // MemoryDetect writes e820 entries, PayloadLoad reads them for the zero page).
    let has_acpi_load = capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiLoad { .. }));
    let has_acpi_prepare = capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare));
    let has_memory_detect = capabilities
        .iter()
        .any(|c| matches!(c, Capability::MemoryDetect { .. }));

    // Both AcpiLoad and AcpiPrepare produce an RSDP address that
    // PayloadLoad later hands to the OS. Declare the variable up front
    // so the generated code compiles whichever capability is present.
    if has_acpi_load || has_acpi_prepare {
        body.extend(quote! {
            #[allow(unused_assignments)]
            let mut _acpi_rsdp_addr: u64 = 0;
        });
    }
    if has_memory_detect {
        body.extend(quote! {
            // Buffer for memory_detect() output. The detected entries are
            // also stored in the global E820State for consumers (PCI host
            // bridge, boot protocol, CrabEFI).
            let mut _e820_entries = [fstart_services::memory_detect::E820Entry::zeroed(); 128];
        });
    }

    // --- Phase 0: Handoff deserialization (non-first stages only) ---
    // For multi-stage boards, non-first stages receive a serialized
    // StageHandoff in r0 (ARMv7) from the previous stage.
    let is_first_stage = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    };

    let uses_handoff = !is_first_stage;
    if uses_handoff {
        body.extend(quote! {
            // Deserialize handoff from previous stage (if valid).
            let _handoff = fstart_capabilities::handoff::try_deserialize(handoff_ptr);
            if let Some(ref h) = _handoff {
                fstart_log::info!("handoff: dram_size={}MB", h.dram_size / (1024 * 1024));
            }
        });
    }

    // --- Phase 1: Construct root devices (no parent bus) ---
    //
    // Bus children (e.g., PCI devices) are deferred to Phase 2 because
    // their construction reads BARs from the parent bus, which must be
    // initialized first (via PciInit or similar capability).
    //
    // Devices are already in pre-order DFS order from RON flattening.
    // ACPI-only devices are skipped — they have no runtime driver.
    let mut deferred_children: Vec<usize> = Vec::new();
    // Track which devices have been constructed (root devices are
    // constructed here in Phase 1; bus children may be constructed
    // later by `ensure_device_ready` if a capability references them).
    let mut constructed_devices: Vec<String> = Vec::new();
    for (idx, node) in device_tree.iter().enumerate() {
        let inst = &instances[idx];
        let dev = &config.devices[idx];
        if inst.is_acpi_only() || inst.is_structural() {
            continue;
        }
        if !dev.enabled {
            continue;
        }
        if node.parent.is_some() {
            // Bus child — defer to after parent is initialized.
            deferred_children.push(idx);
            continue;
        }
        body.extend(generate_device_construction(dev, inst, None, &halt, mode));
        constructed_devices.push(dev.name.as_str().to_string());
    }

    // Track which devices have been initialised by capabilities so DriverInit
    // can skip them and avoid double-init.
    //
    // For non-first stages, pre-populate with devices that ALL previous stages
    // initialized. The codegen can determine this at compile time from the
    // board RON — no runtime check needed.
    let mut inited_devices: Vec<String> = previous_stages_inited_devices(config, stage_name);

    // Small closure to DRY out the ancestor-ready preamble that each
    // device-referencing capability needs. The helper walks the target
    // device's non-structural ancestor chain and emits `new` + `init`
    // calls in root-first order, updating `constructed_devices` and
    // `inited_devices` so no subsequent capability re-initializes the
    // same hardware.
    //
    // Use via `prelude(&mut body, device_name)`.
    let make_prelude = |body: &mut TokenStream,
                        dev_name: &str,
                        constructed: &mut Vec<String>,
                        inited: &mut Vec<String>| {
        body.extend(ensure_device_ready(
            dev_name,
            &config.devices,
            instances,
            device_tree,
            constructed,
            inited,
            &halt,
            mode,
        ));
    };

    // --- Phase 2: Execute capabilities in declared order ---
    for cap in capabilities {
        match cap {
            Capability::ClockInit { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_clock_init(
                    dev_name,
                    &config.devices,
                    instances,
                    &halt,
                ));
            }
            Capability::ConsoleInit { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_console_init(
                    dev_name,
                    &config.devices,
                    instances,
                    &halt,
                    mode,
                ));
            }
            Capability::BootMedia(medium) => {
                body.extend(generate_boot_media(
                    medium,
                    config,
                    &config.devices,
                    instances,
                    &halt,
                ));
                // With embed_anchor, all FFS-using stages reference the
                // FSTART_ANCHOR static directly — no boot-media scan needed.
            }
            Capability::MemoryInit => {
                body.extend(generate_memory_init());
            }
            Capability::DramInit { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_dram_init(
                    dev_name,
                    &config.devices,
                    instances,
                    &halt,
                ));
            }
            Capability::ChipsetInit {
                northbridge,
                southbridge,
            } => {
                make_prelude(
                    &mut body,
                    northbridge.as_str(),
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                make_prelude(
                    &mut body,
                    southbridge.as_str(),
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_chipset_init(
                    northbridge.as_str(),
                    southbridge.as_str(),
                    &config.devices,
                    &halt,
                ));
            }
            Capability::DriverInit => {
                // Construct any remaining deferred bus children before
                // DriverInit runs. By this point all parent devices
                // are constructed and initialized (via PciInit or
                // similar capability), so bus children can read BARs.
                //
                // Structural parents don't exist at runtime — walk up the
                // tree to find the nearest non-structural ancestor.
                for &idx in &deferred_children {
                    let dev = &config.devices[idx];
                    let inst = &instances[idx];
                    // Skip structural / acpi-only / disabled children
                    // (they have no `new_on_bus` implementation).
                    if inst.is_structural() || inst.is_acpi_only() || !dev.enabled {
                        continue;
                    }
                    let parent_name =
                        walk_to_real_parent(idx, device_tree, &config.devices, instances);
                    body.extend(generate_device_construction(
                        dev,
                        inst,
                        parent_name,
                        &halt,
                        mode,
                    ));
                }
                deferred_children.clear();
                // Devices are in pre-order DFS — sequential indices are
                // already topological order.
                let sequential: Vec<usize> = (0..config.devices.len()).collect();
                // Collect boot-media-gated devices from LoadNextStage /
                // BootMedia(AutoDevice) with multiple candidates.  Those
                // devices are only initialised when the eGON boot_media
                // field matches, preventing e.g. MMC init failure when
                // the BROM booted from SPI flash.
                let boot_media_gated = collect_boot_media_gated_devices(
                    capabilities,
                    &config.devices,
                    instances,
                    platform,
                );
                body.extend(generate_driver_init(
                    &config.devices,
                    instances,
                    &sequential,
                    &inited_devices,
                    &boot_media_gated,
                    platform,
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
            Capability::PciInit { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_pci_init(
                    dev_name,
                    &config.devices,
                    instances,
                    &halt,
                ));
            }
            Capability::SigVerify => {
                body.extend(generate_sig_verify(embed_anchor));
            }
            Capability::FdtPrepare => {
                body.extend(generate_fdt_prepare(
                    config,
                    platform,
                    uses_handoff,
                    embed_anchor,
                ));
            }
            Capability::PayloadLoad => {
                body.extend(generate_payload_load(config, platform, embed_anchor));
            }
            Capability::StageLoad { next_stage } => {
                body.extend(generate_stage_load(
                    next_stage.as_str(),
                    platform,
                    embed_anchor,
                ));
            }
            Capability::LateDriverInit => {
                // LateDriverInit: lockdown phase — currently a stub.
                // Future: call lockdown() on devices that implement it.
                body.extend(generate_late_driver_init());
            }
            Capability::AcpiPrepare => {
                body.extend(generate_acpi_prepare(config, &config.devices, instances));
            }
            Capability::SmBiosPrepare => {
                body.extend(generate_smbios_prepare(config));
            }
            Capability::AcpiLoad { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_acpi_load(dev_name, &halt));
            }
            Capability::MemoryDetect { device } => {
                let dev_name = device.as_str();
                make_prelude(
                    &mut body,
                    dev_name,
                    &mut constructed_devices,
                    &mut inited_devices,
                );
                body.extend(generate_memory_detect(dev_name, &halt));
            }
            Capability::ReturnToFel => {
                body.extend(generate_return_to_fel(platform));
            }
            Capability::LoadNextStage {
                devices: load_devs,
                next_stage,
            } => {
                body.extend(generate_load_next_stage(
                    load_devs.as_slice(),
                    next_stage.as_str(),
                    config,
                    &config.devices,
                    instances,
                    platform,
                    capabilities,
                    &halt,
                ));
                for ld in load_devs {
                    let name = ld.name.to_string();
                    if !inited_devices.contains(&name) {
                        inited_devices.push(name);
                    }
                }
            }
        }
    }

    // --- Phase 3: Build context and finalize ---
    let ends_with_jump = capabilities.last().is_some_and(|cap| {
        matches!(
            cap,
            Capability::StageLoad { .. }
                | Capability::PayloadLoad
                | Capability::LoadNextStage { .. }
                | Capability::ReturnToFel
        )
    });

    // Determine which devices are excluded (bus children in stages without
    // DriverInit — they're never constructed).
    let has_driver_init_cap = capabilities
        .iter()
        .any(|c| matches!(c, Capability::DriverInit));
    let device_fields = config
        .devices
        .iter()
        .zip(instances.iter())
        .enumerate()
        .filter(|(idx, (dev, inst))| {
            if inst.is_acpi_only() || inst.is_structural() {
                return false;
            }
            if !dev.enabled {
                return false;
            }
            // Exclude bus children when this stage doesn't have DriverInit
            if !has_driver_init_cap && device_tree[*idx].parent.is_some() {
                return false;
            }
            true
        })
        .map(|(_, (dev, _))| {
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

    // Emit fstart_main with handoff_ptr parameter.
    // The parameter is always present (all platforms pass it) but only
    // used by non-first stages that deserialize the handoff.
    let suppress_unused = if !uses_handoff {
        quote! { let _ = handoff_ptr; }
    } else {
        TokenStream::new()
    };

    quote! {
        #[no_mangle]
        #[allow(unreachable_code, unused_variables)]
        pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
            #suppress_unused
            #body
        }
    }
}

/// Collect device names that MUST NOT be re-initialized by later stages.
///
/// Only **targeted hardware-level** capabilities are counted:
///
/// - `ClockInit` — PLL/clock-gate configuration persists in hardware.
///   Re-init could glitch clocks used by already-running peripherals.
/// - `DramInit` — DRAM controller state persists.  Re-initialization
///   while executing from DRAM would be **catastrophic**.
///
/// Capabilities like `DriverInit`, `ConsoleInit`, and `LoadNextStage`
/// are NOT counted because they establish driver-side state (SDHC flag,
/// RCA, FIFO pointers, etc.) that is lost when a later stage constructs
/// a fresh driver instance via `Device::new()`.  Those devices must be
/// re-initialized to synchronize software state with hardware.
///
/// For the first stage or monolithic builds, returns an empty list.
fn previous_stages_inited_devices(config: &BoardConfig, stage_name: Option<&str>) -> Vec<String> {
    let stages = match &config.stages {
        StageLayout::MultiStage(stages) => stages,
        _ => return vec![],
    };
    let Some(name) = stage_name else {
        return vec![];
    };

    // Find our stage's index.
    let our_idx = match stages.iter().position(|s| s.name.as_str() == name) {
        Some(idx) => idx,
        None => return vec![],
    };
    if our_idx == 0 {
        return vec![]; // First stage — no predecessors.
    }

    // Walk all previous stages.  Only carry forward devices from
    // targeted init capabilities whose hardware state persists and
    // whose re-initialization would be harmful or redundant.
    let mut inited = Vec::new();
    for stage in &stages[..our_idx] {
        for cap in &stage.capabilities {
            match cap {
                Capability::ClockInit { device } | Capability::DramInit { device } => {
                    let name = device.to_string();
                    if !inited.contains(&name) {
                        inited.push(name);
                    }
                }
                // DriverInit, ConsoleInit, LoadNextStage — NOT counted.
                // Devices initialized by these need fresh init() in later
                // stages to rebuild driver-side state.
                _ => {}
            }
        }
    }

    inited
}

// =======================================================================
// Code generation — device construction
// =======================================================================

/// Walk up the device tree to find the first non-structural ancestor.
///
/// Structural nodes (bus bridges, LPC bus, SMBus) exist in the tree for
/// topology purposes but have no runtime representation. A bus child of
/// a structural node is actually attached to the structural node's
/// first real ancestor — e.g., a SuperIO at
/// `southbridge > lpc (structural) > superio` is constructed with the
/// southbridge as its `new_on_bus` parent.
fn walk_to_real_parent<'a>(
    child_idx: usize,
    device_tree: &[DeviceNode],
    devices: &'a [DeviceConfig],
    instances: &[DriverInstance],
) -> Option<&'a str> {
    let mut current = device_tree[child_idx].parent?;
    loop {
        let idx = current as usize;
        if !instances[idx].is_structural() {
            return Some(devices[idx].name.as_str());
        }
        current = device_tree[idx].parent?;
    }
}

/// Emit the "bring-up chain" for a device referenced by a capability.
///
/// The device tree carries the hardware dependency order: a bus child
/// cannot be reached until its parent bus is programmed. For example,
/// the IT8721F SuperIO on an ICH7 board lives as `southbridge → lpc
/// (structural) → superio`; the `southbridge.init()` call programs
/// the LPC I/O decode windows, and only after that can the SuperIO
/// at config port 0x2e be reached.
///
/// This helper walks the chain from the root ancestor down to (and
/// including) the target device. For each non-structural device in the
/// chain it:
/// 1. Emits a `Device::new` / `BusDevice::new_on_bus` call if the
///    device has not yet been constructed in this stage.
/// 2. Emits an `.init()` call if the device has not yet been initialized.
///
/// Structural nodes are skipped entirely (no runtime representation).
/// ACPI-only and disabled devices are skipped.
///
/// Updates `constructed` and `inited` in place so that subsequent
/// capabilities or a later `DriverInit` do not redo the work.
#[allow(clippy::too_many_arguments)]
fn ensure_device_ready(
    device_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    constructed: &mut Vec<String>,
    inited: &mut Vec<String>,
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let mut tokens = TokenStream::new();

    // Find the target device.
    let Some(target_idx) = devices.iter().position(|d| d.name.as_str() == device_name) else {
        return tokens;
    };

    // Collect the chain from root to target (inclusive), root-first.
    let mut chain: Vec<usize> = Vec::new();
    let mut cursor = Some(target_idx as DeviceId);
    while let Some(c) = cursor {
        chain.push(c as usize);
        cursor = device_tree[c as usize].parent;
    }
    chain.reverse();

    for idx in chain {
        let dev = &devices[idx];
        let inst = &instances[idx];
        let name_str = dev.name.as_str();

        // Skip nodes that have no runtime representation or have been
        // explicitly disabled by the board author.
        if !dev.enabled || inst.is_acpi_only() || inst.is_structural() {
            continue;
        }

        // In Flexible mode a device with a service-enum wrapper is
        // constructed into `_name_inner`, then wrapped into the
        // user-visible `name` variable via a match-style enum
        // constructor. The `.init()` call must run on the inner
        // variable (before wrapping), because the wrapper delegates
        // via the service trait, and `wrapping` moves the value.
        let has_flex_wrapper =
            mode == BuildMode::Flexible && flexible_enum_for_device(dev, inst).is_some();
        let init_target = if has_flex_wrapper {
            format_ident!("_{}_inner", name_str)
        } else {
            format_ident!("{}", name_str)
        };

        // Construct if not already done. Root devices are constructed
        // up front in Phase 1 (they appear in `constructed`); bus
        // children are constructed on demand here.
        if !constructed.iter().any(|s| s == name_str) {
            let parent_name = walk_to_real_parent(idx, device_tree, devices, instances);
            tokens.extend(generate_device_construction(
                dev,
                inst,
                parent_name,
                halt,
                mode,
            ));
            constructed.push(name_str.to_string());
        }

        // Init if not already done. A failure at this point often
        // happens before the logger is set up (we are setting up the
        // logger's underlying device), so the fallback is a silent halt.
        if !inited.iter().any(|s| s == name_str) {
            tokens.extend(quote! {
                #init_target.init().unwrap_or_else(|_| #halt);
            });
            // In Flexible mode, emit the wrapping immediately after
            // init so subsequent code (capability bodies, other
            // `ensure_device_ready` calls) can reference the outer
            // variable by its unadorned name.
            if has_flex_wrapper {
                tokens.extend(generate_flexible_wrapping(dev, inst));
            }
            inited.push(name_str.to_string());
        }
    }

    tokens
}

/// Generate a device construction call.
///
/// Dispatch rules:
///
/// - No parent → `Device::new(&cfg)`.
/// - Parent + `is_bus_device == true` → `BusDevice::new_on_bus(&cfg, &parent)`.
///   Used by drivers that implement
///   [`fstart_services::device::BusDevice`] (e.g., SuperIO on LPC, bochs
///   display on PCI, CK505 on SMBus).
/// - Parent + `is_bus_device == false` → `Device::new(&cfg)`.
///   The parent relationship is topological only — used for init
///   ordering by [`ensure_device_ready`] — not for construction.
///   Example: an NS16550 UART that sits behind a SuperIO on the LPC
///   bus. The NS16550 has its own absolute I/O base in its config;
///   its dependency on the SuperIO is satisfied at init-ordering
///   time (the SuperIO's LDN must be programmed before the UART
///   registers are touched), not via a bus handle.
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
    let cfg_binding = format_ident!("{}_cfg", name_str);
    let is_bus_device = instance.meta().is_bus_device;

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

    match (parent_name, is_bus_device) {
        (Some(parent), true) => {
            // Bus-device child: construction reads from the parent bus.
            let parent_ident = format_ident!("{}", parent);
            quote! {
                let #cfg_binding = #config;
                let mut #binding = #type_name::new_on_bus(&#cfg_binding, &#parent_ident).unwrap_or_else(|_| #halt);
            }
        }
        _ => {
            // Plain Device: root, or child in the tree purely for
            // init-ordering purposes. Config carries all the addresses
            // it needs; the parent is not passed as a constructor arg.
            quote! {
                let #cfg_binding = #config;
                let mut #binding = #type_name::new(&#cfg_binding).unwrap_or_else(|_| #halt);
            }
        }
    }
}
