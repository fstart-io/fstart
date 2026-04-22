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

mod board_gen;
mod capabilities;
mod config_ser;
mod plan_gen;
pub(crate) mod registry;
mod tokens;
mod topology;
mod validation;

#[cfg(test)]
mod tests;

use proc_macro2::{Literal, TokenStream};
use quote::quote;

use fstart_device_registry::DriverInstance;
use fstart_types::{BootMedium, Capability, DeviceConfig, Platform, StageLayout};

use crate::ron_loader::ParsedBoard;

use tokens::hex_addr;
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

    // Emit the `StagePlan` literal consumed by `fstart_stage_runtime`'s
    // generic `run_stage` executor.  Carries the capability sequence
    // plus every per-stage fact the executor needs.
    //
    // See `.opencode/plans/stage-runtime-codegen-split.md`.
    tokens.extend(plan_gen::generate_stage_plan(
        config,
        &parsed.driver_instances,
        capabilities,
        stage_name,
    ));

    // Emit the `_BoardDevices` struct + `impl Board for _BoardDevices`
    // board adapter.  The other half of the stage-runtime input:
    // holds the concrete `Option<Driver>` fields and supplies
    // per-capability trampolines that `run_stage` dispatches through.
    tokens.extend(board_gen::generate_board_adapter(
        config,
        &parsed.driver_instances,
        &parsed.device_tree,
        capabilities,
        stage_name,
    ));

    // Emit `fstart_main()` — a thin stub that forwards to
    // `fstart_stage_runtime::run_stage(_BoardDevices::new(),
    // &STAGE_PLAN, handoff_ptr)`.  All device construction,
    // per-capability dispatch, and halt logic lives in the
    // handwritten executor + the board adapter emitted above.
    tokens.extend(generate_fstart_main());

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
    capabilities: &[Capability],
    _embed_anchor: bool,
) -> TokenStream {
    let mut tokens = TokenStream::new();

    tokens.extend(quote! {
        use fstart_services::Console;
        use fstart_services::device::Device;
    });

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

    // AcpiPrepare: the board adapter’s `acpi_prepare` trampoline
    // references `fstart_acpi::device::AcpiDevice` and
    // `fstart_capabilities::acpi::prepare`.  Pull in the crate and
    // the AcpiDevice trait so the generated code compiles.
    let uses_acpi_prepare = capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare));
    if uses_acpi_prepare {
        tokens.extend(quote! {
            use fstart_acpi::device::AcpiDevice;
        });
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
// Code generation — fstart_main()
// =======================================================================

/// Emit the `fstart_main()` function.
///
/// In the new runtime/codegen split, `fstart_main` is a thin stub that
/// forwards to [`fstart_stage_runtime::run_stage`], passing:
///
/// 1. A fresh `_BoardDevices::new()` (the board adapter emitted by
///    [`board_gen`](super::board_gen)) — holds `Option<Driver>` fields
///    for every enabled device, the boot-media state, FDT / DRAM /
///    handoff bookkeeping, and the RSDP / eGON scalars.
/// 2. A reference to the module-local `STAGE_PLAN` static (emitted by
///    [`plan_gen`](super::plan_gen)) — the capability sequence plus
///    every per-stage fact the executor needs.
/// 3. The handoff pointer the platform's `_start` stashed, forwarded
///    unchanged.  Non-first stages use it to deserialise the previous
///    stage's [`StageHandoff`]; first stages ignore it.
///
/// The platform-level fact set (`no_mangle`, `extern "Rust"`, `-> !`)
/// matches the old generator's signature exactly — the ELF entry
/// point is unchanged, only the body shrinks to a single call.
///
/// Signature kept stable across boards: per invariant #2 in the plan
/// doc, only this stub may call `_BoardDevices::new()`, which lets a
/// future multi-platform codegen emit `new_for(platform)` without
/// touching the trait or the executor.
///
/// [`StageHandoff`]: fstart_types::handoff::StageHandoff
fn generate_fstart_main() -> TokenStream {
    quote! {
        /// Stage entry point.  Called by the platform's `_start`
        /// after register setup + BSS zero + stack pointer load.
        ///
        /// `handoff_ptr` is the raw register value the platform
        /// decided to use for passing data from the previous stage
        /// (e.g. `r0` on ARMv7, `a0` on RISC-V).  `run_stage`
        /// forwards it to the board adapter, which interprets it
        /// per-platform (typically as a deserialisation target).
        #[no_mangle]
        #[allow(unreachable_code, unused_variables)]
        pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
            fstart_stage_runtime::run_stage(
                _BoardDevices::new(),
                &STAGE_PLAN,
                handoff_ptr,
            )
        }
    }
}
