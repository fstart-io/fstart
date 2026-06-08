//! Generate stage entry point code from board configuration.
//!
//! Given a [`ParsedBoard`] (or a specific stage within it), this module
//! emits Rust source code that:
//!
//! 1. Defines a `_BoardDevices` struct with one concrete typed field per device.
//! 2. Emits data-only `StagePlan` static facts.
//! 3. Implements `fstart_stage_runtime::Board` for typed board access.
//! 4. Generates a small `fstart_main()` shim into the handwritten executor.
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
mod tokens;
mod topology;
mod validation;

#[cfg(test)]
mod tests;

use proc_macro2::{Literal, TokenStream};
use quote::quote;

use fstart_device_registry::{DriverInstance, Service, ServiceSet};
use fstart_types::{BootMedium, Capability, DeviceConfig, Platform, StageLayout};

use crate::ron_loader::ParsedBoard;

use topology::validate_device_tree;
use validation::{
    get_boot_medium, needs_embedded_anchor, needs_ffs, validate_capability_ordering,
    validate_capability_services, validate_stage_scope_requirements,
};

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

    let stage_runs_from_ram = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(_), _) => false,
        (StageLayout::MultiStage(stages), Some(name)) => stages
            .iter()
            .find(|s| s.name.as_str() == name)
            .is_some_and(|s| s.runs_from == fstart_types::stage::RunsFrom::Ram),
        _ => false,
    };

    // Validate capability ordering before generating code.
    if let Some(err) = validate_capability_ordering(
        capabilities,
        config,
        &parsed.device_services,
        stage_runs_from_ram,
    ) {
        return format!("compile_error!(\"{err}\");\n");
    }

    if let Some(err) = validate_capability_services(
        capabilities,
        config,
        &parsed.driver_instances,
        &parsed.device_services,
    ) {
        return format!("compile_error!(\"{err}\");\n");
    }

    if let Some(err) = validate_stage_scope_requirements(capabilities, config) {
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
        &parsed.device_services,
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
    let import_facts = ImportFacts::new(
        &config.devices,
        &parsed.driver_instances,
        &parsed.device_services,
        capabilities,
    );
    tokens.extend(generate_imports(&import_facts));

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

    if embed_anchor {
        let early_microcode = matches!(
            config.microcode.as_ref(),
            Some(fstart_types::board::MicrocodeConfig::Intel(microcode)) if microcode.early
        );
        tokens.extend(generate_anchor_static(early_microcode));
    }

    if stage_uses_smm(capabilities) {
        tokens.extend(generate_smm_image_static());
    }

    // When the allocator is needed, generate a sized heap backing store.
    // fstart-alloc references these symbols via `extern "C"`.
    if let Some(hs) = heap_size {
        tokens.extend(generate_heap_storage(hs));
    }

    tokens.extend(plan_gen::generate_stage_plan(
        config,
        &parsed.driver_instances,
        &parsed.device_services,
        capabilities,
        stage_name,
    ));

    // Emit the `_BoardDevices` struct + `impl Board for _BoardDevices`
    // board adapter.  It holds the concrete `Option<Driver>` fields and
    // supplies typed board operations used by the stage executor.
    tokens.extend(board_gen::generate_board_adapter(
        config,
        &parsed.driver_instances,
        &parsed.device_tree,
        &parsed.device_services,
        &parsed.acpi_only_devices,
        capabilities,
        stage_name,
    ));

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

/// Stage/import facts computed once before emitting `use` statements.
///
/// This is intentionally small and local to import generation. It mirrors the
/// StageScope/model direction without coupling top-level source generation to
/// the board-adapter internals.
struct ImportFacts<'a> {
    has_bus_children: bool,
    has_block_device: bool,
    has_i2c: bool,
    has_spi: bool,
    has_gpio: bool,
    has_smbus: bool,
    has_pci: bool,
    has_framebuffer: bool,
    has_flash_layout_verifier: bool,
    uses_dram_init: bool,
    uses_load_next_stage: bool,
    uses_ffs: bool,
    uses_acpi_load: bool,
    uses_acpi_prepare: bool,
    boot_medium: Option<&'a BootMedium>,
    driver_modules: Vec<&'static str>,
}

impl<'a> ImportFacts<'a> {
    fn new(
        devices: &[DeviceConfig],
        instances: &[DriverInstance],
        device_services: &[ServiceSet],
        capabilities: &'a [Capability],
    ) -> Self {
        let mut driver_modules = Vec::new();
        for (dev, inst) in devices.iter().zip(instances.iter()) {
            if !dev.enabled {
                continue;
            }
            let module_path = inst.meta().module_path;
            if !inst.has_runtime_driver() {
                continue;
            }
            if !driver_modules.contains(&module_path) {
                driver_modules.push(module_path);
            }
        }

        Self {
            has_bus_children: devices.iter().any(|device| device.parent.is_some()),
            has_block_device: device_services
                .iter()
                .any(|services| services.contains(Service::BlockDevice)),
            has_i2c: device_services
                .iter()
                .any(|services| services.contains(Service::I2cBus)),
            has_spi: device_services
                .iter()
                .any(|services| services.contains(Service::SpiBus)),
            has_gpio: device_services
                .iter()
                .any(|services| services.contains(Service::GpioController)),
            has_smbus: device_services
                .iter()
                .any(|services| services.contains(Service::SystemManagementBus)),
            has_pci: device_services
                .iter()
                .any(|services| services.contains(Service::PciRootBus)),
            has_framebuffer: device_services
                .iter()
                .any(|services| services.contains(Service::Framebuffer)),
            has_flash_layout_verifier: device_services
                .iter()
                .any(|services| services.contains(Service::FlashLayoutVerifier)),
            uses_dram_init: capabilities
                .iter()
                .any(|cap| matches!(cap, Capability::DramInit { .. })),
            uses_load_next_stage: capabilities
                .iter()
                .any(|cap| matches!(cap, Capability::LoadNextStage { .. })),
            uses_ffs: needs_ffs(capabilities),
            uses_acpi_load: capabilities
                .iter()
                .any(|cap| matches!(cap, Capability::AcpiLoad { .. })),
            uses_acpi_prepare: capabilities
                .iter()
                .any(|cap| matches!(cap, Capability::AcpiPrepare)),
            boot_medium: get_boot_medium(capabilities),
            driver_modules,
        }
    }
}

/// Emit `use` statements for all driver types needed by this board's devices.
fn generate_imports(facts: &ImportFacts<'_>) -> TokenStream {
    let mut tokens = TokenStream::new();

    tokens.extend(quote! {
        #[allow(unused_imports)]
        use fstart_services::Console;
        #[allow(unused_imports)]
        use fstart_services::device::Device;
    });

    // Check if any device provides bus services — import those traits too.
    if facts.has_block_device {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::BlockDevice; });
    }

    // Import MemoryController trait when DramInit + LoadNextStage are both
    // present — LoadNextStage calls detected_size_bytes() on the DRAM
    // controller to pass the runtime-detected size to the next stage.
    if facts.uses_dram_init && facts.uses_load_next_stage {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::MemoryController; });
    }

    // BusDevice trait is needed when any device has a parent bus (e.g., PCI
    // child devices use BusDevice::new_on_bus).
    if facts.has_bus_children {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::device::BusDevice; });
    }

    if facts.has_i2c {
        tokens.extend(quote! {
            #[allow(unused_imports)]
            use fstart_services::i2c::{I2c, ErrorType as I2cErrorType, ErrorKind as I2cErrorKind, Operation as I2cOperation};
        });
    }
    if facts.has_spi {
        tokens.extend(quote! {
            #[allow(unused_imports)]
            use fstart_services::spi::{SpiBus, ErrorType as SpiErrorType, ErrorKind as SpiErrorKind};
        });
    }
    if facts.has_gpio {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::GpioController; });
    }
    if facts.has_smbus {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::SmBus; });
    }

    if facts.has_pci {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::PciRootBus; });
    }

    if facts.has_framebuffer {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::Framebuffer; });
    }

    if facts.has_flash_layout_verifier {
        tokens.extend(quote! {
            #[allow(unused_imports)]
            use fstart_types::memory::{FlashLayout, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig};
        });
    }

    // Collect unique driver modules and import all public types via glob.
    // ACPI-only and structural devices are skipped — their types live in
    // fstart_types/fstart_acpi and fstart_device_registry respectively,
    // and are only used at codegen time, not in the generated stage code.
    for module_path in &facts.driver_modules {
        let module_path: TokenStream = module_path.parse().unwrap();
        tokens.extend(quote! {
            #[allow(unused_imports)]
            use #module_path::*;
        });
    }

    // Import boot media concrete types used by the generated adapter. The
    // BootMedia *trait* is imported only when FFS helpers call read_at()/as_slice().
    if facts.boot_medium.is_some() {
        tokens.extend(
            quote! { #[allow(unused_imports)] use fstart_services::boot_media::MemoryMapped; },
        );
        tokens.extend(
            quote! { #[allow(unused_imports)] use fstart_services::boot_media::BlockDeviceMedia; },
        );
        if facts.uses_ffs {
            tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::BootMedia; });
        }
    }

    // AcpiLoad needs the AcpiTableProvider trait.
    if facts.uses_acpi_load {
        tokens.extend(quote! { #[allow(unused_imports)] use fstart_services::acpi_provider::AcpiTableProvider; });
    }

    // AcpiPrepare: the board adapter’s `acpi_prepare` trampoline
    // references `fstart_acpi::device::AcpiDevice` and
    // `fstart_capabilities::acpi::prepare`.  Pull in the crate and
    // the AcpiDevice trait so the generated code compiles.
    if facts.uses_acpi_prepare {
        tokens.extend(quote! {
            #[allow(unused_imports)]
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
fn generate_anchor_static(early_microcode: bool) -> TokenStream {
    let early_microcode_value = u32::from(early_microcode);
    quote! {
        /// FFS anchor block — patched by `xtask assemble` with real offsets.
        ///
        /// The bootblock reads this via volatile to find the FFS manifest.
        /// No scanning required at runtime.
        #[no_mangle]
        #[link_section = ".fstart.anchor"]
        #[used]
        static FSTART_ANCHOR: fstart_types::ffs::AnchorBlock =
            fstart_types::ffs::AnchorBlock::placeholder();

        // Early x86 assembly runs before Rust and cannot refer to a Rust item
        // unless it has an unmangled linker-visible name.  Keep the platform
        // crate's reference on a separate weak alias so non-FFS x86 stages can
        // still link with the alias resolving to 0.
        #[cfg(feature = "x86_64")]
        core::arch::global_asm!(
            ".global _fstart_anchor_early",
            ".set _fstart_anchor_early, FSTART_ANCHOR",
        );

        #[cfg(feature = "x86_64")]
        #[no_mangle]
        #[used]
        static _fstart_early_microcode_enabled: u32 = #early_microcode_value;
    }
}

/// Emit the standalone SMM image bytes for stages that perform SMM setup.
fn generate_smm_image_static() -> TokenStream {
    quote! {
        #[used]
        static FSTART_SMM_IMAGE: &[u8] = include_bytes!(env!(
            "FSTART_SMM_IMAGE",
            "MpInit(smm: true) requires xtask to build and pass FSTART_SMM_IMAGE"
        ));
    }
}

fn stage_uses_smm(capabilities: &[Capability]) -> bool {
    capabilities
        .iter()
        .any(|cap| matches!(cap, Capability::MpInit { smm: true, .. }))
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

/// Emit the `fstart_main` shim into the handwritten stage executor.
fn generate_fstart_main() -> TokenStream {
    quote! {
        /// Stage entry point. Called by the platform's `_start` after register
        /// setup + BSS zero + stack pointer load.
        #[no_mangle]
        #[allow(unreachable_code, unused_variables, unused_mut)]
        pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
            #[cfg(feature = "handoff")]
            let handoff = fstart_capabilities::handoff::try_deserialize(handoff_ptr);
            #[cfg(not(feature = "handoff"))]
            let handoff = {
                let _ = handoff_ptr;
                None
            };
            let mut board = _BoardDevices::new(handoff);
            fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN)
        }
    }
}
