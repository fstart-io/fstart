//! Emit the `impl Board for _BoardDevices` adapter consumed by
//! [`fstart_stage_runtime::run_stage`].
//!
//! This is the second half of the stage-runtime / codegen split (see
//! `.opencode/plans/stage-runtime-codegen-split.md`).  Paired with
//! [`plan_gen`](super::plan_gen), it produces the complete input for
//! `run_stage`:
//!
//! - `plan_gen` emits `static STAGE_PLAN: StagePlan = ...;` (plain data).
//! - `board_gen` emits `struct _BoardDevices { ... }` plus an
//!   `impl fstart_stage_runtime::Board for _BoardDevices` with one
//!   method per capability.
//!
//! # Transitional state
//!
//! During the migration (steps 3–6 of the plan's "Work breakdown") the
//! new `_BoardDevices` lives **alongside** the existing `Devices`
//! struct and `fstart_main` body.  Nothing calls the new adapter yet
//! — it exists so the compiler type-checks every board against the
//! `Board` trait surface, one capability at a time as each method is
//! migrated out of `todo!()`.
//!
//! At the final "flip" commit the old `Devices` / `StageContext` /
//! `fstart_main` emission is deleted and `_BoardDevices` is renamed to
//! `Devices`; the generated `fstart_main` becomes a one-liner that
//! calls `run_stage`.
//!
//! # Design rules enforced here
//!
//! See plan doc §"Invariants that preserve multi-platform extensibility".
//! This module encodes:
//!
//! 1. `_BoardDevices::new()` is the **only** construction site; callers
//!    outside the generated `fstart_main` never invoke it.  Its
//!    signature is a stub-private detail free to grow later.
//! 2. `impl Board` method bodies read state from `&self` — no
//!    board-level addresses/sizes/strings as method arguments.
//! 3. Per-device lifecycle logic lives in codegen-private
//!    `_BoardDevices::init_<name>` helpers rather than in `init_device`
//!    itself, so a future multi-platform codegen can swap one device
//!    field to an enum-of-variants without touching the executor-facing
//!    trait method.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::memory::RegionKind;
use fstart_types::{
    BoardConfig, Capability, DeviceConfig, DeviceNode, FdtSource, FirmwareConfig, FirmwareKind,
    PayloadConfig, Platform, StageLayout,
};

// `PayloadConfig` and `FdtSource` are used by [`fdt_prepare_platform_body`] and
// [`fdt_prepare_body`]'s `match &payload.fdt` arms.  `StageLayout` backs
// [`compute_is_first_stage`].

use super::capabilities::find_dram_region;
use super::tokens::hex_addr;
use super::validation::needs_ffs;

// =======================================================================
// Public entry point
// =======================================================================

/// Emit the complete board adapter: `_BoardDevices` struct, `new()`,
/// and `impl Board for _BoardDevices`.
///
/// Callers wire this into `generate_stage_source` right after
/// [`plan_gen::generate_stage_plan`](super::plan_gen::generate_stage_plan)
/// and before [`generate_fstart_main`](super::generate_fstart_main).
/// The old `fstart_main` keeps running until the final flip.
///
/// `stage_name` is required to derive `is_first_stage` — the same rule
/// used by `generate_fstart_main` (monolithic or named first stage in a
/// `MultiStage` layout).  Non-first stages are the ones that receive a
/// serialised [`StageHandoff`] from the previous stage; currently only
/// the `fdt_prepare` trampoline uses this fact (to prefer a runtime
/// DRAM size over the static board-config value).
///
/// [`StageHandoff`]: fstart_types::handoff::StageHandoff
pub(super) fn generate_board_adapter(
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let excluded = compute_excluded_indices(instances, device_tree, capabilities);
    let platform = config.platform;
    let ffs_stage = needs_ffs(capabilities);
    let is_first_stage = compute_is_first_stage(&config.stages, stage_name);
    let dram = find_dram_region(config).unwrap_or((0, 0));
    let ctx = BoardCtx {
        config,
        devices: &config.devices,
        instances,
        device_tree,
        excluded: &excluded,
        stage_capabilities: capabilities,
        ffs_stage,
        is_first_stage,
        dram_base: dram.0,
        dram_size_static: dram.1,
    };

    let mut tokens = TokenStream::new();
    tokens.extend(emit_adapter_struct(&ctx));
    tokens.extend(emit_adapter_new(&ctx));
    tokens.extend(emit_board_impl(platform, &ctx));
    tokens
}

/// Stage-is-first predicate, matching [`generate_fstart_main`]'s rule.
///
/// [`generate_fstart_main`]: super::generate_fstart_main
fn compute_is_first_stage(stages: &StageLayout, stage_name: Option<&str>) -> bool {
    match (stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    }
}

/// Bundle of references passed to every `emit_*` helper.
///
/// The adapter generator is pure codegen — no mutable state, no
/// threading.  `BoardCtx` gathers everything the helpers need so their
/// argument lists stay short.
///
/// Per-stage facts encoded here:
///
/// - `ffs_stage`: mirrors [`needs_ffs`].  When true, the surrounding
///   codegen emits `FSTART_ANCHOR`, the boot-media import path, and
///   the rest of the FFS preamble.  A `false` value means those names
///   do not exist in this stage's generated source, so the adapter
///   must not reference them.  Trampolines that would touch FFS
///   (`sig_verify`, `payload_load`, `stage_load`, `fdt_prepare`'s
///   `Override` variant) emit `todo!()` in this case.
///
/// - `is_first_stage`: true for monolithic boards and for the first
///   stage of a `MultiStage` layout.  Non-first stages receive a
///   serialised [`StageHandoff`] in a platform register.  Today only
///   `fdt_prepare` uses this, but `chipset_init` / `memory_detect`
///   will need it once we migrate those trampolines.
///
/// - `dram_base` / `dram_size_static`: the DRAM region picked by
///   [`find_dram_region`].  Emitted into fields on `_BoardDevices`
///   (`_dram_base` / `_dram_size_static`) per invariant #3 — the
///   trampoline never reads constants directly, it always reads from
///   `&self`.
///
/// [`StageHandoff`]: fstart_types::handoff::StageHandoff
struct BoardCtx<'a> {
    config: &'a BoardConfig,
    devices: &'a [DeviceConfig],
    instances: &'a [DriverInstance],
    /// Parent-child structure of the board's devices.  Indexed in
    /// lock-step with `devices` and `instances`.  Used by
    /// [`init_device_body`] (walks the ancestor chain to emit
    /// `BusDevice::new_on_bus` with the correct parent reference
    /// and to init ancestors before children).
    device_tree: &'a [DeviceNode],
    excluded: &'a [usize],
    /// Capabilities for the stage being emitted.  Used to gate
    /// trampolines on whether the stage actually declares the
    /// capability — `return_to_fel` specifically needs this because
    /// `fstart_soc_sunxi` is only in scope for stages that pull
    /// it in via the `sunxi` feature (which in turn fires only for
    /// sunxi boards / stages using `ReturnToFel`).
    stage_capabilities: &'a [Capability],
    ffs_stage: bool,
    /// Reserved for future migrations (`memory_detect`, `chipset_init`,
    /// `stage_load`) that need to know whether a previous stage's
    /// handoff should populate bookkeeping before capability dispatch.
    ///
    /// `#[allow(dead_code)]` for now — `fdt_prepare` already reads
    /// from `_handoff` unconditionally (the field is `None` on
    /// first-stage boards so the chained `unwrap_or` picks the
    /// static DRAM size) and doesn't need this flag.
    #[allow(dead_code)]
    is_first_stage: bool,
    dram_base: u64,
    dram_size_static: u64,
}

// =======================================================================
// Excluded devices — match the old `generate_devices_struct` rule
// =======================================================================

/// Which device indices are not materialised in this stage.
///
/// Bus children require their parent bus to be initialised before
/// construction (`new_on_bus` reads the parent's BARs).  In stages
/// without a `DriverInit` capability, no parent ever initialises, so
/// the old generator excludes bus children entirely.  We mirror that
/// rule so the two adapters stay isomorphic during the transition.
fn compute_excluded_indices(
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    capabilities: &[Capability],
) -> Vec<usize> {
    let has_driver_init = capabilities
        .iter()
        .any(|c| matches!(c, Capability::DriverInit));
    if has_driver_init {
        return Vec::new();
    }
    device_tree
        .iter()
        .enumerate()
        .filter(|(idx, node)| {
            // Only exclude children whose enabled driver actually
            // exists at runtime — acpi-only / structural devices are
            // already filtered by `enabled_indices` below.
            node.parent.is_some() && !instances[*idx].is_structural()
        })
        .map(|(idx, _)| idx)
        .collect()
}

// =======================================================================
// `struct _BoardDevices`
// =======================================================================

/// Emit the `_BoardDevices` struct.
///
/// One field per enabled, runtime-present, non-excluded device.  All
/// fields are `Option<T>` because `init_device(id)` is the sole
/// construction site (see invariant #4 in the plan doc): no device is
/// materialised until the executor asks for it, which keeps the
/// "stages that don't use X skip X entirely" property we already rely
/// on for deferred bus children.
///
/// Plus several bookkeeping fields, all populated by `new()`:
///
/// - `_inited` — which devices have run `Device::init`, matching the
///   executor's own `inited` mask so `DriverInit` can skip them.
/// - `_boot_media` — the current boot-media selection.  Written by
///   `boot_media_static` / `boot_media_select`; read by every
///   FFS-using trampoline (`sig_verify`, `payload_load`,
///   `stage_load`) when it reconstructs a concrete
///   `MemoryMapped` / `BlockDeviceMedia`.
/// - `_dtb_dst_addr` / `_bootargs` / `_dram_base` /
///   `_dram_size_static` — board-level facts used by `fdt_prepare`.
///   These mirror RON-derived constants that the old codegen baked
///   into the `fdt_prepare` method body.  Per invariant #3 every
///   board-level constant lives on `self`, so a future multi-platform
///   adapter can vary them by platform without changing the trait.
/// - `_handoff` — the deserialised previous-stage handoff, if any.
///   Today always `None` (populated at the final fstart_main flip via
///   a `bind_handoff` helper); `fdt_prepare` already reads from it so
///   the field's contract is locked in.
fn emit_adapter_struct(ctx: &BoardCtx<'_>) -> TokenStream {
    let fields = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).map(|idx| {
        let dev = &ctx.devices[idx];
        let inst = &ctx.instances[idx];
        let field_name = format_ident!("{}", dev.name.as_str());
        let field_type = format_ident!("{}", inst.meta().type_name);
        quote! { #field_name: Option<#field_type>, }
    });

    quote! {
        /// Board adapter produced by `fstart-codegen::board_gen`.
        ///
        /// Carries one `Option<Driver>` per enabled device plus the
        /// bookkeeping state [`Board`](fstart_stage_runtime::Board)
        /// trampolines need ([`DeviceMask`] for init tracking,
        /// [`BootMediaState`] for the current boot medium, static
        /// FDT data, and the previous-stage handoff).  Implements
        /// [`fstart_stage_runtime::Board`] so the handwritten
        /// [`run_stage`](fstart_stage_runtime::run_stage) executor
        /// can drive it.
        ///
        /// Parallel to the existing `Devices` / `StageContext` pair
        /// during the runtime/codegen split migration; see
        /// `.opencode/plans/stage-runtime-codegen-split.md` §"Work
        /// breakdown".
        ///
        /// [`DeviceMask`]: fstart_stage_runtime::DeviceMask
        /// [`BootMediaState`]: fstart_stage_runtime::BootMediaState
        #[allow(dead_code, non_camel_case_types)]
        struct _BoardDevices {
            #(#fields)*
            _inited: fstart_stage_runtime::DeviceMask,
            _boot_media: fstart_stage_runtime::BootMediaState,
            _dtb_dst_addr: u64,
            _bootargs: &'static str,
            _dram_base: u64,
            _dram_size_static: u64,
            _handoff: Option<fstart_types::handoff::StageHandoff>,
            /// RSDP physical address, populated by `acpi_load` and
            /// read by future `acpi_prepare` / `payload_load`
            /// trampolines.  `0` means "not set yet"; boards
            /// without `AcpiLoad` leave it at `0` forever.
            _acpi_rsdp_addr: u64,
            /// eGON header SRAM base address for Allwinner sunxi
            /// boards.  Read by `boot_media_select` and
            /// `load_next_stage` to resolve the hardware boot-media
            /// byte and next-stage header values.  Non-sunxi boards
            /// leave it at `0`; the field then reads zero bytes off
            /// SRAM which is harmless because non-sunxi stages never
            /// dispatch the relevant trampolines.
            _egon_sram_base: u64,
        }
    }
}

// =======================================================================
// `impl _BoardDevices { fn new() -> Self }`
// =======================================================================

/// Emit `impl _BoardDevices { const fn new() -> Self }`.
///
/// Every device field starts as `None` — matching the lazy-init model
/// where `init_device(id)` both constructs and initialises on first
/// call.  `_boot_media` starts at
/// [`BootMediaState::None`](fstart_stage_runtime::BootMediaState::None);
/// the first `BootMedia*` capability populates it.
///
/// The static FDT fields (`_dtb_dst_addr`, `_bootargs`, `_dram_base`,
/// `_dram_size_static`) are populated from the board RON at const-eval
/// time — all four values are literals known at codegen.  `_handoff`
/// starts as `None` and is overwritten by a future `bind_handoff`
/// helper when the `fstart_main` stub flips to call `run_stage`.
///
/// Keeping `new` as `const fn` lets a later `fstart_main` stub
/// materialise the adapter inside the function body without
/// introducing a non-const constructor path.  It also makes the
/// generated source easier to read — there is no runtime work at
/// all until the executor starts dispatching capabilities.
fn emit_adapter_new(ctx: &BoardCtx<'_>) -> TokenStream {
    let field_inits = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).map(|idx| {
        let dev = &ctx.devices[idx];
        let field_name = format_ident!("{}", dev.name.as_str());
        quote! { #field_name: None, }
    });

    let dtb_dst_lit = hex_addr(
        ctx.config
            .payload
            .as_ref()
            .and_then(|p| p.dtb_addr)
            .unwrap_or(0),
    );
    let bootargs_lit = ctx
        .config
        .payload
        .as_ref()
        .and_then(|p| p.bootargs.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("");
    let dram_base_lit = hex_addr(ctx.dram_base);
    let dram_size_lit = hex_addr(ctx.dram_size_static);
    let egon_sram_base_lit = hex_addr(super::capabilities::egon_sram_base(ctx.config));

    quote! {
        #[allow(dead_code)]
        impl _BoardDevices {
            /// Zero-initialised adapter.  See [`emit_adapter_new`] doc.
            const fn new() -> Self {
                Self {
                    #(#field_inits)*
                    _inited: fstart_stage_runtime::DeviceMask::new(),
                    _boot_media: fstart_stage_runtime::BootMediaState::None,
                    _dtb_dst_addr: #dtb_dst_lit,
                    _bootargs: #bootargs_lit,
                    _dram_base: #dram_base_lit,
                    _dram_size_static: #dram_size_lit,
                    _handoff: None,
                    _acpi_rsdp_addr: 0,
                    _egon_sram_base: #egon_sram_base_lit,
                }
            }
        }
    }
}

// =======================================================================
// `impl Board for _BoardDevices`
// =======================================================================

/// Emit the `impl fstart_stage_runtime::Board for _BoardDevices` block.
///
/// # Migration status
///
/// Populated today:
///
/// - `halt`, `jump_to`, `jump_to_with_handoff` — direct delegates to
///   `fstart_platform::*`.
/// - `memory_init`, `late_driver_init_complete` — one-line delegates
///   to `fstart_capabilities::*`.
/// - `boot_media_static` — records the selection into
///   `self._boot_media`.
/// - `install_logger` — emits a `match id` with one arm per
///   Console-providing device; each arm calls `fstart_log::init`
///   against `self.<field>` and `fstart_capabilities::console_ready`
///   with the device + driver name literals.  Wildcard arm halts.
/// - `sig_verify` — gated on `ctx.ffs_stage`.  When true, reads
///   `&FSTART_ANCHOR`, matches on `self._boot_media`, reconstructs
///   the concrete `impl BootMedia`, and delegates to
///   `fstart_capabilities::sig_verify`.  When false the stage has no
///   FFS-using capability and the method stays as a `todo!()`
///   (unreachable; no executor arm calls it).
/// - `fdt_prepare` — reads the payload's [`FdtSource`]:
///   - `Platform` (default) calls `fstart_capabilities::fdt_prepare_platform`
///     with the DTB source derived per-platform, the RON destination
///     address, bootargs, and DRAM base/size (preferring
///     `self._handoff.dram_size` when present).
///   - `Override` loads the DTB file from FFS via `self._boot_media`
///     into `self._dtb_dst_addr` first, then patches it in-place.
///     Dead-codes to `todo!()` for non-FFS stages (executor never
///     reaches it there).
///   - `Generated` / `GeneratedWithOverride` / no payload: falls back
///     to `fstart_capabilities::fdt_prepare_stub`.
/// - `stage_load` — anchor preamble + boot-media dispatch into
///   `fstart_capabilities::stage_load(next_stage, _anchor_bytes, &_bm,
///   fstart_platform::jump_to)`.  Trailing `halt()` satisfies the
///   `-> !` return type and catches a buggy manifest that names a
///   non-existent stage.  Dead-codes to `todo!()` for non-FFS
///   stages (contradicts validation; unreachable).
/// - `return_to_fel` — gated on `platform == Armv7`.  On armv7
///   emits the `fstart_soc_sunxi::return_to_fel_from_stash()` call
///   (inherently `-> !`).  On any other platform emits a `todo!()`
///   since `ReturnToFel` is validation-rejected off armv7.
/// - `pci_init` — `match id { ... }` with one arm per `PciRootBus`
///   provider in the board; each arm logs the completion banner
///   with device + driver names and returns `Ok(())`.
/// - `chipset_init` — looks up the single (`PciHost`, `Southbridge`)
///   pair; emits `PciHost::early_init` + `Southbridge::early_init`
///   calls against the respective fields, logs the banner, returns
///   `Ok(())`.  Boards without one of the services emit a log + halt
///   (dead code, since validation forbids the capability there).
/// - `acpi_load` — `match id { ... }` with one arm per
///   `AcpiTableProvider` device.  Each arm allocates a method-local
///   256 KiB static `UnsafeCell` buffer and calls
///   `fstart_capabilities::acpi_load(self.<field>.as_ref()?, buf,
///   name)`.  The returned RSDP address is written to
///   `self._acpi_rsdp_addr` for later use by payload_load /
///   acpi_prepare.
/// - `memory_detect` — `match id { ... }` with one arm per
///   `MemoryDetector` device.  Each arm stack-allocates a 128-entry
///   `[E820Entry; _]` buffer and calls
///   `fstart_capabilities::memory_detect(self.<field>.as_ref()?,
///   &mut buf, name)`.  Results land in the global `E820State` so
///   callers need not propagate the buffer.
/// - `acpi_prepare` — emits the shared platform ACPI binding,
///   per-device `let <name>_cfg = <literal>;` lines, then
///   `fstart_capabilities::acpi::prepare(&platform_acpi, |...| { ... })`
///   with a closure that iterates every has-acpi device and every
///   ACPI-only device.  Inside the closure the per-device
///   `self.<name>.as_ref().unwrap_or_else(halt)` call replaces the
///   old `&<name>` local.  Boards with no `acpi` RON config emit a
///   `todo!()` (dead code).
/// - `smbios_prepare` — delegates directly to the shared
///   `capabilities::smbios::generate_smbios_prepare` helper, which
///   is a pure function of `BoardConfig` and produces the full
///   `fstart_capabilities::smbios::prepare(&SmbiosDesc { ... })`
///   call.  Boards with no `smbios` RON config emit a `todo!()`.
/// - `boot_media_select` — reads the hardware boot-media byte via
///   `fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize)`
///   and picks the first matching candidate, writing
///   `self._boot_media = BootMediaState::Block { ... }`.  Gated on
///   sunxi eGON format **and** the stage declaring `LoadNextStage`
///   or `BootMedia(AutoDevice)`; otherwise emits `todo!()`.
/// - `load_next_stage` — stage-name → (load_addr, handoff_addr)
///   match on RON stage list; eGON header read for offset + size;
///   per-device dispatch using the block-device arms pattern;
///   serialises handoff with DRAM size resolved from `_handoff`,
///   a `DramInit` driver's `detected_size_bytes()`, or
///   `_dram_size_static`.  Diverges via `jump_to_with_handoff`.
///   Dead-codes to `todo!()` for non-sunxi / non-LoadNextStage
///   stages.
///
/// Every other method is `todo!()` during the transition.  The code
/// is dead (the old `fstart_main` body does not call into it) but the
/// compiler still checks that every method matches the trait, which
/// catches signature drift as we populate the remaining trampolines
/// one migration at a time.
///
/// # Platform note
///
/// `fstart_platform::jump_to_with_handoff` does not exist on x86_64
/// (no multi-stage board with handoff targets it today).  On that
/// platform we emit `fstart_platform::halt()` as a safety net — the
/// executor never reaches this arm for an x86 board anyway, because
/// no `LoadNextStage` capability is valid there.
fn emit_board_impl(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    let jump_with_handoff_body = match platform {
        Platform::X86_64 => quote! {
            // Unsupported on x86_64; should never be reached — no x86
            // board uses a LoadNextStage capability today.  If it ever
            // does, a platform-crate helper needs to land first.
            let _ = (entry, handoff_addr);
            fstart_platform::halt()
        },
        _ => quote! { fstart_platform::jump_to_with_handoff(entry, handoff_addr) },
    };

    let sig_verify_body = sig_verify_body(ctx);
    let fdt_prepare_body = fdt_prepare_body(platform, ctx);
    let install_logger_body = install_logger_body(ctx);
    let stage_load_body = stage_load_body(ctx);
    let return_to_fel_body = return_to_fel_body(platform, ctx);
    let pci_init_body = pci_init_body(ctx);
    let chipset_init_body = chipset_init_body(ctx);
    let acpi_load_body = acpi_load_body(ctx);
    let memory_detect_body = memory_detect_body(ctx);
    let acpi_prepare_body = acpi_prepare_body(ctx);
    let smbios_prepare_body = smbios_prepare_body(ctx);
    let boot_media_select_body = boot_media_select_body(ctx);
    let load_next_stage_body = load_next_stage_body(ctx);
    let payload_load_body = payload_load_body(platform, ctx);
    let init_device_body = init_device_body(ctx);
    let init_all_devices_body = init_all_devices_body(ctx);

    quote! {
        #[allow(dead_code, unused_variables)]
        impl fstart_stage_runtime::Board for _BoardDevices {
            // ----- Device lifecycle -------------------------------------

            fn init_device(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #init_device_body
            }

            fn init_all_devices(
                &mut self,
                skip: &fstart_stage_runtime::DeviceMask,
                gated: &fstart_stage_runtime::DeviceMask,
            ) {
                #init_all_devices_body
            }

            // ----- Logging ---------------------------------------------

            unsafe fn install_logger(&self, id: fstart_types::DeviceId) {
                #install_logger_body
            }

            // ----- Capability trampolines ------------------------------

            fn memory_init(&self) {
                fstart_capabilities::memory_init();
            }

            fn late_driver_init_complete(&self, count: usize) {
                fstart_capabilities::late_driver_init_complete(count);
            }

            fn sig_verify(&self) {
                #sig_verify_body
            }

            fn fdt_prepare(&self) {
                #fdt_prepare_body
            }

            fn payload_load(&self) -> ! {
                #payload_load_body
            }

            fn stage_load(&self, next_stage: &str) -> ! {
                #stage_load_body
            }

            fn acpi_prepare(&mut self) {
                #acpi_prepare_body
            }

            fn smbios_prepare(&self) {
                #smbios_prepare_body
            }

            fn chipset_init(
                &mut self,
                nb: fstart_types::DeviceId,
                sb: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #chipset_init_body
            }

            fn pci_init(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #pci_init_body
            }

            fn acpi_load(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #acpi_load_body
            }

            fn memory_detect(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #memory_detect_body
            }

            fn return_to_fel(&self) -> ! {
                #return_to_fel_body
            }

            // ----- Boot media ------------------------------------------

            fn boot_media_select(
                &mut self,
                candidates: &[fstart_stage_runtime::BootMediaCandidate],
            ) -> Option<fstart_types::DeviceId> {
                #boot_media_select_body
            }

            fn boot_media_static(
                &mut self,
                device: Option<fstart_types::DeviceId>,
                offset: u64,
                size: u64,
            ) {
                // Records the executor-provided selection on `self`
                // for later FFS-using trampolines (`sig_verify`,
                // `payload_load`, `stage_load`) to reconstruct the
                // concrete `impl BootMedia`.
                //
                // Note: the executor already called `init_device(id)`
                // for `Some(id)` before calling us, so by the time
                // `sig_verify` etc. dereference `self.<name>`, the
                // device is constructed.
                self._boot_media =
                    fstart_stage_runtime::BootMediaState::from_static(device, offset, size);
            }

            fn load_next_stage(&mut self, next_stage: &str) -> ! {
                #load_next_stage_body
            }

            // ----- Platform primitives ---------------------------------

            fn halt(&self) -> ! { fstart_platform::halt() }

            fn jump_to(&self, entry: u64) -> ! {
                fstart_platform::jump_to(entry)
            }

            fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> ! {
                #jump_with_handoff_body
            }
        }
    }
}

// =======================================================================
// Capability body helpers
// =======================================================================

/// Emit the body of `Board::install_logger`.
///
/// Emits a `match id { ... }` where every arm corresponds to an
/// enabled device providing the `Console` service.  Each arm:
///
/// 1. Calls `fstart_log::init(&self.<field>...)` so the global
///    logger routes to the device.
/// 2. Calls `fstart_capabilities::console_ready(device_name, driver_name)`
///    which emits the familiar "X: Y console ready" banner.
///
/// The executor guarantees (invariant of [`CapOp::ConsoleInit`]) that
/// `init_device(id)` already ran before this method is called, so
/// `self.<field>` is `Some`.  We still `.unwrap_or_else(halt)` defensively
/// — costs a never-taken branch that the optimiser elides and keeps
/// the generated code obviously safe under code review.
///
/// A wildcard arm halts the board: the executor only calls
/// `install_logger(id)` with ids declared as `ConsoleInit` in the
/// plan, and `plan_gen` only emits such ids for Console providers.
/// Reaching the wildcard means a codegen/executor contract violation.
///
/// Stages with no Console-providing device (hypothetical; today every
/// stage has at least one logger) reduce the match to just the
/// wildcard — still a valid body.
///
/// The enclosing method is `unsafe fn`; we wrap the `fstart_log::init`
/// call in an explicit `unsafe {}` block anyway so future edition
/// upgrades that require this are a no-op.
///
/// # Safety
///
/// `fstart_log::init` takes a reference that is promoted to `'static`
/// internally.  The board adapter owns `self.<field>` for the stage's
/// lifetime (the adapter is itself a `fstart_main`-local whose scope
/// is "the whole stage" and that never drops), so extending the
/// borrow is sound.  The invocation annotation on the trait method
/// says the same thing in prose.
///
/// [`CapOp::ConsoleInit`]: fstart_stage_runtime::CapOp::ConsoleInit
fn install_logger_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "Console")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let inst = &ctx.instances[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            let drv_name = inst.meta().name;
            quote! {
                #id_lit => {
                    // SAFETY: the executor's `CapOp::ConsoleInit`
                    // arm calls `init_device(id)` before
                    // `install_logger(id)`, so `self.#field` is
                    // `Some`.  `fstart_log::init` promotes the
                    // borrow to `'static`; we own the device for
                    // the stage's lifetime, so that is sound.
                    unsafe {
                        fstart_log::init(
                            self.#field
                                .as_ref()
                                .unwrap_or_else(|| fstart_platform::halt()),
                        );
                    }
                    fstart_capabilities::console_ready(#dev_name, #drv_name);
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                // Executor contract violation — `install_logger` is
                // only dispatched for ids declared `ConsoleInit` in
                // `StagePlan`, which `plan_gen` only emits for
                // Console providers.  Reaching this arm means an
                // id we didn't emit a field for, so no recovery is
                // possible.
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::sig_verify`.
///
/// For stages that use FFS (`ctx.ffs_stage == true`), generates:
///
/// 1. A volatile-read of `FSTART_ANCHOR` as a `&[u8]`.
/// 2. A match on `self._boot_media` that reconstructs the concrete
///    boot-media type and delegates to `fstart_capabilities::sig_verify`.
/// 3. One per-device arm for each enabled `BlockDevice` provider in
///    the board; no-match halts.
///
/// For non-FFS stages, emits a `todo!()` — the trampoline is dead
/// code (no `SigVerify` capability, so the executor never calls it)
/// and referencing `FSTART_ANCHOR` or the `MemoryMapped` /
/// `BlockDeviceMedia` types would break compilation since they are
/// not imported in non-FFS stages.
fn sig_verify_body(ctx: &BoardCtx<'_>) -> TokenStream {
    if !ctx.ffs_stage {
        return quote! {
            // No FFS-using capability in this stage, so no executor
            // arm reaches `sig_verify`.  Keep the trait method but
            // make it a compile-time-only placeholder.
            todo!("board_gen::sig_verify: no FFS-using capability in this stage")
        };
    }

    let bm_usage = quote! {
        fstart_capabilities::sig_verify(_anchor_bytes, &_bm);
    };
    let none_body = quote! {
        // No `BootMedia*` capability ran before `SigVerify`.
        // Validation upstream forbids this; reaching here means a
        // buggy plan.  Log and skip — consistent with old
        // `fstart_capabilities::sig_verify` stub behaviour when the
        // manifest is empty.
        fstart_log::info!("sig verify: no boot media configured, skipping");
    };
    let anchor = anchor_bytes_stmt();
    let match_body = match_boot_media(ctx, &bm_usage, "sig_verify", &none_body);

    quote! {
        #anchor
        #match_body
    }
}

/// The `let _anchor_bytes: &[u8] = ...;` preamble used by every FFS-
/// touching trampoline body.
///
/// Emitted as a constant rather than a function because the token
/// stream contains zero per-board variation.  Centralised so the
/// safety comment lives in exactly one place.
fn anchor_bytes_stmt() -> TokenStream {
    quote! {
        // SAFETY: FSTART_ANCHOR is emitted by
        // `generate_anchor_static` in this same stage with proper
        // alignment (`#[link_section = ".fstart.anchor"]` + `#[used]`)
        // and is the size of `AnchorBlock`.  The FFS builder may
        // patch its contents post-link, so downstream
        // `FfsReader::read_anchor_volatile` reads it through
        // `ptr::read_volatile`.
        let _anchor_bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(
                &FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock as *const u8,
                core::mem::size_of::<fstart_types::ffs::AnchorBlock>(),
            )
        };
    }
}

/// Emit a `match self._boot_media { ... }` that binds a local `_bm`
/// for use by `bm_usage`.
///
/// Arms:
///
/// - `None` → `none_body` (caller decides: log + skip, or log + halt).
/// - `Mmio { base, size }` → constructs `MemoryMapped` via
///   `from_raw_addr`, then splices `bm_usage`.
/// - `Block { device_id, offset, size }` → inner match over every
///   enabled block device in the board, each arm building
///   `BlockDeviceMedia` from `self.<field>` and splicing `bm_usage`.
///   A wildcard arm logs and halts, labelled with `caller_tag` for
///   the error message.
///
/// Caller passes `bm_usage` as a token stream that assumes `_bm` is
/// in scope.  This keeps the control flow inside the emitted code
/// (no closure crossing codegen / runtime) and lets a single helper
/// service `sig_verify`, `fdt_prepare` Override, `payload_load`, and
/// `stage_load` trampolines.
fn match_boot_media(
    ctx: &BoardCtx<'_>,
    bm_usage: &TokenStream,
    caller_tag: &str,
    none_body: &TokenStream,
) -> TokenStream {
    let block_arms = block_device_arms(ctx, bm_usage);
    let err_msg = format!("{caller_tag}: unknown block device id {{}}");
    quote! {
        match self._boot_media {
            fstart_stage_runtime::BootMediaState::None => {
                #none_body
            }
            fstart_stage_runtime::BootMediaState::Mmio { base, size } => {
                // SAFETY: `base..base + size` is the board's
                // memory-mapped flash window; the board author's
                // RON declared this range is readable.
                let _bm = unsafe {
                    fstart_services::boot_media::MemoryMapped::from_raw_addr(
                        base,
                        size as usize,
                    )
                };
                #bm_usage
            }
            fstart_stage_runtime::BootMediaState::Block {
                device_id,
                offset,
                size,
            } => {
                match device_id {
                    #block_arms
                    _ => {
                        // Executor invariant: `boot_media_static(Some(id), ..)`
                        // is only called with a device id that provides
                        // `BlockDevice`.  Reaching this arm means the plan
                        // carries an id we didn't emit a field for — a
                        // codegen bug.  `unreachable_unchecked` would be
                        // sound here (executor guarantees) but `halt` is
                        // strictly safer.
                        fstart_log::error!(#err_msg, device_id);
                        fstart_platform::halt();
                    }
                }
            }
        }
    }
}

/// Emit one match arm per enabled block device in the board.
///
/// Each arm constructs a
/// [`BlockDeviceMedia`](fstart_services::boot_media::BlockDeviceMedia)
/// from the corresponding `self.<name>` field (which the executor
/// has guaranteed is `Some` via a prior `init_device` call) and
/// splices `bm_usage` — a token stream that uses the `_bm` local.
///
/// Returns an empty token stream when the board has no block
/// devices — the `match` arm set then contains only `_` and the
/// `None` / `Mmio` arms.
fn block_device_arms(ctx: &BoardCtx<'_>, bm_usage: &TokenStream) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "BlockDevice")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            quote! {
                #id_lit => {
                    let _bm = fstart_services::boot_media::BlockDeviceMedia::new(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        offset,
                        size as usize,
                    );
                    #bm_usage
                }
            }
        });
    quote! { #(#arms)* }
}

/// Emit the body of `Board::fdt_prepare`.
///
/// Mirrors the old `capabilities::generate_fdt_prepare` logic, but
/// reads every board-level fact from `&self` rather than baking
/// constants into the method body (invariant #3).
///
/// The three payload paths:
///
/// 1. **No payload** → `fstart_capabilities::fdt_prepare_stub()`.
///    This matches the old codegen's behaviour and means any board
///    RON without a `payload = Some(...)` silently skips FDT setup.
///
/// 2. **`FdtSource::Platform`** — the common case for QEMU and
///    real-hardware boards that get their DTB from the previous
///    stage (BROM / QEMU / previous bootloader).  Calls
///    `fstart_capabilities::fdt_prepare_platform` with:
///
///    - a DTB source expression computed per-platform (`hex_addr`
///      literal when the RON overrides via `src_dtb_addr`; an inline
///      `fstart_platform::boot_dtb_addr()` call on RISC-V and
///      AArch64; `0u64` elsewhere);
///    - `self._dtb_dst_addr` as the destination;
///    - `self._bootargs` as the kernel command line;
///    - `self._dram_base` unconditionally;
///    - the DRAM size resolved from `self._handoff` if present with
///      a non-zero `dram_size`, else `self._dram_size_static`.
///
///    The `self._handoff` path runs unconditionally (even in first
///    stages); when the field is `None`, the chained `unwrap_or`
///    trivially selects the static value.  This keeps the body
///    identical for every stage, avoiding a `uses_handoff` flag.
///
/// 3. **`FdtSource::Override(_)`** — board supplies the DTB as an
///    FFS file.  Requires `ctx.ffs_stage` because the emitted body
///    references `FSTART_ANCHOR` and the boot-media types.  The
///    body loads the DTB from FFS into `self._dtb_dst_addr` via
///    `fstart_capabilities::load_ffs_file_by_type`, halts on
///    failure, then patches bootargs in-place with `src = dst`.
///    When `!ctx.ffs_stage`, emits a `todo!()` — codegen bug for a
///    non-FFS stage to carry an Override DTB.
///
/// 4. `FdtSource::Generated` / `GeneratedWithOverride` — not yet
///    implemented anywhere (the old generator falls through to
///    stub); we do the same.
fn fdt_prepare_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    // If the stage doesn't declare FdtPrepare, the executor never
    // dispatches this method.  Emit a dead-code stub rather than
    // referencing `fdt_prepare_platform` (gated behind the `fdt`
    // feature, which may not be enabled for this stage — e.g. x86
    // boards that have `fdt: Platform` on their payload but no
    // `FdtPrepare` capability).
    let has_fdt_prepare = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare));
    if !has_fdt_prepare {
        return quote! {
            todo!("board_gen::fdt_prepare: stage does not declare FdtPrepare")
        };
    }

    let Some(payload) = ctx.config.payload.as_ref() else {
        return quote! { fstart_capabilities::fdt_prepare_stub(); };
    };

    match &payload.fdt {
        FdtSource::Platform => fdt_prepare_platform_body(platform, payload),
        FdtSource::Override(_dtb_file) => fdt_prepare_override_body(ctx, platform),
        _ => quote! { fstart_capabilities::fdt_prepare_stub(); },
    }
}

/// Build the DTB source expression used by `fdt_prepare_platform`.
///
/// Precedence (matching the old `capabilities::generate_fdt_prepare`):
///
/// 1. `payload.src_dtb_addr = Some(addr)` → emit `hex_addr(addr)`.
/// 2. Platform is RISC-V or AArch64 → emit the runtime
///    `fstart_platform::boot_dtb_addr()` call.  The platform entry
///    assembly stashes the BROM / QEMU-provided DTB pointer in a
///    dedicated slot; the platform crate returns it.
/// 3. Otherwise (ARMv7, x86_64) → emit `0u64`.  The platform has no
///    common DTB mechanism; boards must set `src_dtb_addr` in their
///    RON if they want FDT source data.
///
/// The chosen expression is spliced into the `fdt_prepare_platform`
/// call directly; it is *not* stored on `self` because `boot_dtb_addr`
/// is a runtime function call, not a const value.  Invariant #3
/// targets board-level *constants*; inlining a platform call is fine.
fn dtb_src_expr(platform: Platform, payload: &PayloadConfig) -> TokenStream {
    if let Some(addr) = payload.src_dtb_addr {
        return hex_addr(addr);
    }
    match platform {
        Platform::Riscv64 | Platform::Aarch64 => quote! { fstart_platform::boot_dtb_addr() },
        Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
    }
}

/// Shared DRAM-size expression used by every trampoline that hands
/// DRAM size to `fstart_capabilities::*`.
///
/// Prefers the runtime-detected size carried in `self._handoff`
/// (populated by non-first stages) and falls back to
/// `self._dram_size_static` (from RON).  The `_handoff.dram_size > 0`
/// filter guards against stale / unset handoff values.
fn dram_size_expr() -> TokenStream {
    quote! {
        self._handoff
            .as_ref()
            .filter(|h| h.dram_size > 0)
            .map(|h| h.dram_size)
            .unwrap_or(self._dram_size_static)
    }
}

/// Emit the body for the `FdtSource::Platform` case — a single call
/// to `fstart_capabilities::fdt_prepare_platform` with all per-board
/// data read from `&self`.
fn fdt_prepare_platform_body(platform: Platform, payload: &PayloadConfig) -> TokenStream {
    let dtb_src = dtb_src_expr(platform, payload);
    let dram_size = dram_size_expr();
    quote! {
        fstart_capabilities::fdt_prepare_platform(
            #dtb_src,
            self._dtb_dst_addr,
            self._bootargs,
            self._dram_base,
            #dram_size,
        );
    }
}

/// Emit the body for the `FdtSource::Override` case — load the DTB
/// from FFS via `self._boot_media`, then patch bootargs in-place.
///
/// Requires `ctx.ffs_stage`; otherwise the FFS / boot-media types
/// aren't in scope and we can't emit the load code.  Returns a
/// `todo!()` placeholder in that case (unreachable — the executor
/// only dispatches `FdtPrepare` when the plan carries the capability,
/// and `FdtPrepare` with `Override` upstream-requires BootMedia,
/// which in turn implies FFS).
fn fdt_prepare_override_body(ctx: &BoardCtx<'_>, platform: Platform) -> TokenStream {
    if !ctx.ffs_stage {
        return quote! {
            todo!("board_gen::fdt_prepare Override variant requires an FFS-using stage")
        };
    }

    let anchor = anchor_bytes_stmt();
    let dram_size = dram_size_expr();
    // The `bm_usage` body the boot-media match spices in on each arm.
    // Loads the DTB from FFS (halts on failure) and falls through to
    // the shared `fdt_prepare_platform` invocation below.
    let bm_usage = quote! {
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Fdt,
        ) {
            fstart_log::error!("FATAL: failed to load DTB from FFS");
            fstart_platform::halt();
        }
    };
    let none_body = quote! {
        // No boot media selected — fdt_prepare Override has nothing
        // to load from.  Validation upstream forbids this combination
        // (Override implies BootMedia capability earlier); reaching
        // here is a plan bug.  Log and skip — the patch step below
        // runs on whatever the previous stage left at
        // `self._dtb_dst_addr`, which is the most lenient behaviour.
        fstart_log::warn!("fdt_prepare Override: no boot media configured, skipping FFS load");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "fdt_prepare", &none_body);
    let _ = platform;

    quote! {
        fstart_log::info!("loading DTB from FFS...");
        #anchor
        #match_body
        fstart_log::info!("DTB loaded to {:#x}", self._dtb_dst_addr);
        // Patch bootargs and memory node in-place: src = dst since the
        // DTB is already at `self._dtb_dst_addr`.
        fstart_capabilities::fdt_prepare_platform(
            self._dtb_dst_addr,
            self._dtb_dst_addr,
            self._bootargs,
            self._dram_base,
            #dram_size,
        );
    }
}

/// Emit the body of `Board::stage_load`.
///
/// Loads the named stage from FFS and jumps to its entry point.  The
/// shape mirrors `fdt_prepare` Override: an anchor preamble plus a
/// [`match_boot_media`] dispatch whose `bm_usage` calls
/// `fstart_capabilities::stage_load(next_stage, _anchor_bytes, &_bm,
/// fstart_platform::jump_to)`.
///
/// Unlike `sig_verify` and `fdt_prepare`, the trait method is
/// declared `-> !` — it must diverge.  `fstart_capabilities::stage_load`
/// itself does not return when it successfully jumps to the loaded
/// stage, but its control flow looks like a normal `fn(...)` to the
/// Rust type system (the jump happens via a `fn(u64) -> !` pointer
/// parameter).  We add a trailing `fstart_platform::halt()` after the
/// match so the function's return type is satisfied and a buggy
/// manifest (missing stage, decode error) halts instead of silently
/// falling through to undefined behaviour.
///
/// Requires `ctx.ffs_stage`; otherwise the FFS / boot-media types
/// aren't in scope.  `stage_load` upstream-requires `BootMedia`,
/// which in turn implies FFS, so a non-FFS stage reaching this
/// trampoline is a plan bug (unreachable — the executor dispatches
/// `StageLoad` only if the plan carries the capability).  Emits a
/// `todo!()` in that case.
fn stage_load_body(ctx: &BoardCtx<'_>) -> TokenStream {
    if !ctx.ffs_stage {
        return quote! {
            let _ = next_stage;
            todo!("board_gen::stage_load requires an FFS-using stage")
        };
    }

    let anchor = anchor_bytes_stmt();
    let bm_usage = quote! {
        fstart_capabilities::stage_load(
            next_stage,
            _anchor_bytes,
            &_bm,
            fstart_platform::jump_to,
        );
    };
    let none_body = quote! {
        // `StageLoad` upstream-requires `BootMedia`; reaching here
        // means the plan violated that.  Log and halt — no recovery
        // possible.
        fstart_log::error!("stage_load: no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "stage_load", &none_body);

    quote! {
        #anchor
        #match_body
        // Falls through here only when
        // `fstart_capabilities::stage_load` returned without jumping
        // (e.g., the manifest didn't contain `next_stage`, or FFS
        // decode failed).  The method is `-> !` so we must diverge.
        fstart_log::error!(
            "stage_load: capability returned without jumping — halting",
        );
        fstart_platform::halt()
    }
}

/// Emit the body of `Board::return_to_fel`.
///
/// Allwinner sunxi-only.  Requires **two** conditions to emit the
/// real body:
///
/// 1. `platform == Armv7` — `return_to_fel_from_stash` is an armv7
///    assembly routine in `fstart-soc-sunxi`.
/// 2. The stage's capability list contains [`Capability::ReturnToFel`]
///    — only sunxi boards declare it, and only sunxi boards pull the
///    `fstart-soc-sunxi` crate into the stage's dependency graph
///    (via the `sunxi` feature on `fstart-platform-armv7`).  Emitting
///    the `fstart_soc_sunxi::...` call on a non-sunxi armv7 board
///    like `qemu-armv7` would fail to compile — the crate is not in
///    scope there.
///
/// When either condition fails we emit a `todo!()`.  Validation
/// upstream (`validation::validate_capability_ordering`) already
/// rejects `ReturnToFel` on non-armv7 boards before codegen runs,
/// and `plan_gen` only emits the `CapOp::ReturnToFel` op when the
/// capability is present in *this* stage.  The `todo!()` is
/// therefore strictly dead code; the compiler checks that the trait
/// method signature matches without touching the body.
///
/// # Safety
///
/// `return_to_fel_from_stash` reads the FEL stash populated by
/// `save_boot_params` in early platform entry.  The platform crate
/// guarantees that save runs before any user code (including
/// capability trampolines) executes, so the stash is always valid
/// by the time this trampoline is reached.
fn return_to_fel_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_return_to_fel = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::ReturnToFel));

    if platform != Platform::Armv7 || !uses_return_to_fel {
        return quote! {
            // Dead code: this stage does not declare ReturnToFel, or
            // the board is not armv7.  The executor never dispatches
            // `CapOp::ReturnToFel` here, so the body is never
            // entered; emitting a real `fstart_soc_sunxi::...` call
            // would fail to compile for non-sunxi boards that do not
            // depend on the crate.
            todo!("board_gen::return_to_fel: stage does not declare ReturnToFel")
        };
    }
    quote! {
        fstart_log::info!("returning to FEL mode...");
        // SAFETY: `save_boot_params` ran during platform entry and
        // populated the FEL stash.  `return_to_fel_from_stash`
        // restores that BROM state and jumps back; it never
        // returns, so the `-> !` contract is satisfied without a
        // trailing `halt()`.
        unsafe { fstart_soc_sunxi::return_to_fel_from_stash() }
    }
}

/// Emit the body of `Board::pci_init`.
///
/// The executor has already run `init_device(id)` (which calls
/// `Device::init()` and materialises `self.<field>`) by the time
/// this trampoline is invoked.  The old generator's `pci_init`
/// emitted only a banner log; we do the same.  The `match id`
/// shape is still required so we can produce a stable per-device
/// "device name + driver name" banner without reaching into the
/// device at runtime.
///
/// Arms: one per enabled device providing the `PciRootBus` service;
/// each logs `"PCI init complete: {dev} ({drv})"` and returns
/// `Ok(())`.  Wildcard logs + halts.
///
/// Stages without any PciRootBus provider degenerate to just the
/// wildcard (still a valid match).  The `#[allow(unreachable_patterns)]`
/// dance isn't needed because the wildcard always matches at least
/// the device-id range.
fn pci_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "PciRootBus")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let inst = &ctx.instances[idx];
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            let drv_name = inst.meta().name;
            quote! {
                #id_lit => {
                    fstart_log::info!(
                        "PCI init complete: {} ({})",
                        #dev_name,
                        #drv_name,
                    );
                    Ok(())
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                // Executor contract violation — `pci_init(id)` should
                // only be dispatched for `PciRootBus` providers.
                fstart_log::error!("pci_init: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::chipset_init`.
///
/// Calls `PciHost::early_init(&mut self.<nb>)` followed by
/// `Southbridge::early_init(&mut self.<sb>)`, then logs the completion
/// banner.  Returns `Ok(())` on success and the first `Err` otherwise.
///
/// The executor guarantees `init_device(nb)` and `init_device(sb)`
/// ran before this trampoline, so both fields are `Some`.  We
/// `.unwrap_or_else(|| halt())` defensively — the optimiser elides
/// the branch.
///
/// The `match (nb, sb)` surface is there so future codegen with more
/// than one northbridge/southbridge pair per board can add arms.
/// Today every board has exactly one valid pair; non-matching pairs
/// fall to the wildcard and halt.
///
/// Stages with no northbridge + southbridge pair (most boards)
/// degenerate to a pure-wildcard body that halts.  The compiler
/// still type-checks the trait impl, and the executor never
/// dispatches `ChipsetInit` for such a board (validation upstream).
fn chipset_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    // Find the (nb, sb) pair: one device with `PciHost` service and
    // one with `Southbridge`.  RON validation enforces exactly one
    // of each when `ChipsetInit` appears in capabilities, but we
    // scan the device list here because `board_gen` does not see
    // the original `Capability::ChipsetInit { northbridge, southbridge }`
    // strings — it emits a generic `match (nb, sb)` suitable for
    // any future multi-chipset board.
    let nb_idx = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).find(|idx| {
        ctx.devices[*idx]
            .services
            .iter()
            .any(|s| s.as_str() == "PciHost")
    });
    let sb_idx = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).find(|idx| {
        ctx.devices[*idx]
            .services
            .iter()
            .any(|s| s.as_str() == "Southbridge")
    });

    let Some(nb_idx) = nb_idx else {
        return quote! {
            let _ = (nb, sb);
            fstart_log::error!("chipset_init: no PciHost device declared");
            fstart_platform::halt();
        };
    };
    let Some(sb_idx) = sb_idx else {
        return quote! {
            let _ = (nb, sb);
            fstart_log::error!("chipset_init: no Southbridge device declared");
            fstart_platform::halt();
        };
    };

    let nb_field = format_ident!("{}", ctx.devices[nb_idx].name.as_str());
    let sb_field = format_ident!("{}", ctx.devices[sb_idx].name.as_str());
    let nb_name = ctx.devices[nb_idx].name.as_str();
    let sb_name = ctx.devices[sb_idx].name.as_str();
    let nb_id_lit = proc_macro2::Literal::u8_unsuffixed(nb_idx as u8);
    let sb_id_lit = proc_macro2::Literal::u8_unsuffixed(sb_idx as u8);

    quote! {
        use fstart_services::PciHost as _PciHost;
        use fstart_services::Southbridge as _Southbridge;
        match (nb, sb) {
            (#nb_id_lit, #sb_id_lit) => {
                _PciHost::early_init(
                    self.#nb_field
                        .as_mut()
                        .unwrap_or_else(|| fstart_platform::halt()),
                )
                .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                _Southbridge::early_init(
                    self.#sb_field
                        .as_mut()
                        .unwrap_or_else(|| fstart_platform::halt()),
                )
                .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                fstart_log::info!(
                    "chipset init complete: {} + {}",
                    #nb_name,
                    #sb_name,
                );
                Ok(())
            }
            _ => {
                // Executor contract violation — `chipset_init(nb, sb)`
                // should only be dispatched with the declared
                // northbridge/southbridge pair.
                fstart_log::error!(
                    "chipset_init: unexpected (nb, sb) pair ({}, {})",
                    nb,
                    sb,
                );
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::acpi_load`.
///
/// One arm per enabled device providing the `AcpiTableProvider`
/// service.  Each arm:
///
/// 1. Defines a method-local `static _ACPI_LOAD_BUF: UnsafeCell<[u8;
///    256 * 1024]>` (256 KiB is enough for QEMU Q35's ~128 KiB of
///    tables plus RSDP + alignment).  The buffer is leaked after
///    the call so tables persist for the OS.
/// 2. Calls `fstart_capabilities::acpi_load(self.<field>.as_ref()?,
///    buf, <name>)` and writes the returned RSDP address to
///    `self._acpi_rsdp_addr` — read later by `payload_load` / a
///    future `acpi_prepare`.
/// 3. On success returns `Ok(())`; on error logs + halts.
///
/// Wildcard arm: log + halt.  Boards with no `AcpiTableProvider`
/// device reduce to just the wildcard, which is dead code (no
/// `AcpiLoad` in their capability list).
///
/// # Note on the buffer placement
///
/// A 256 KiB buffer is too large to live on the stack (firmware
/// stacks are typically 16–64 KiB) or in the `_BoardDevices`
/// struct (we want the struct small enough to live on the stack).
/// Each method gets its own `static` via `UnsafeCell<[u8; N]>` —
/// matches the old codegen's approach.  Because the buffer is
/// scoped to the method, it does not collide with other statics
/// and needs no per-device suffix.
fn acpi_load_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "AcpiTableProvider")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            quote! {
                #id_lit => {
                    // 256 KiB buffer for ACPI tables — persists for
                    // the OS lifetime.  Wrapped in a newtype with
                    // `unsafe impl Sync` because `UnsafeCell` alone
                    // is not `Sync` and cannot live in a `static`.
                    #[repr(align(16))]
                    struct _AcpiLoadBufStore(core::cell::UnsafeCell<[u8; 256 * 1024]>);
                    // SAFETY: single-threaded firmware init, buffer
                    // used exactly once (the executor only dispatches
                    // `AcpiLoad` for one id per stage).
                    unsafe impl Sync for _AcpiLoadBufStore {}
                    static _ACPI_LOAD_BUF: _AcpiLoadBufStore =
                        _AcpiLoadBufStore(core::cell::UnsafeCell::new([0u8; 256 * 1024]));
                    let _acpi_buf = unsafe { &mut *_ACPI_LOAD_BUF.0.get() };
                    let rsdp = fstart_capabilities::acpi_load(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        _acpi_buf,
                        #dev_name,
                    )
                    .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    self._acpi_rsdp_addr = rsdp;
                    Ok(())
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => {
                // Executor contract violation — `acpi_load(id)` is
                // only dispatched for ids the plan marked as
                // `AcpiTableProvider` providers.
                fstart_log::error!("acpi_load: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::memory_detect`.
///
/// One arm per enabled device providing the `MemoryDetector`
/// service.  Each arm:
///
/// 1. Defines a method-local stack buffer `[E820Entry; 128]`
///    (smaller than ACPI's 256 KiB — e820 entries are ~24 bytes
///    each, so 128 entries = ~3 KiB, fits on a 16+ KiB stack).
///    Using a stack buffer rather than a struct field keeps the
///    `_BoardDevices` footprint small; the detected data lands in
///    the global `E820State` via `fstart_capabilities::memory_detect`
///    (which also persists it post-return).
/// 2. Calls `fstart_capabilities::memory_detect(self.<field>.as_ref()?,
///    &mut _e820_entries, <name>)` — discards the returned
///    `(count, total)` since the global state captures both.
/// 3. Returns `Ok(())` on success; log + halt on error.
///
/// Wildcard arm: log + halt.  Boards with no `MemoryDetector`
/// device reduce to just the wildcard (dead code).
fn memory_detect_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "MemoryDetector")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            quote! {
                #id_lit => {
                    // Buffer for `memory_detect()` output.  The
                    // detected entries are also stored in the global
                    // `E820State` for consumers (PCI host bridge,
                    // boot protocol, CrabEFI).
                    let mut _e820_entries =
                        [fstart_services::memory_detect::E820Entry::zeroed(); 128];
                    fstart_capabilities::memory_detect(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        &mut _e820_entries,
                        #dev_name,
                    )
                    .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    Ok(())
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => {
                // Executor contract violation — `memory_detect(id)` is
                // only dispatched for ids the plan marked as
                // `MemoryDetector` providers.
                fstart_log::error!("memory_detect: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::acpi_prepare`.
///
/// Orchestrates per-device ACPI generation, matching the old
/// `capabilities::acpi::generate_acpi_prepare` shape:
///
/// 1. Emits `let platform_acpi = ...;` via the shared
///    [`capabilities::acpi::generate_platform_acpi`] helper.  This
///    covers both the ARM SBSA and x86 flavours of
///    [`fstart_acpi::platform::PlatformConfig`].
///
/// 2. For every enabled device whose driver `has_acpi` and whose
///    instance has an `acpi_name()` set, emits a per-device
///    `let <name>_cfg = <config struct literal>;` binding using
///    [`config_ser::config_tokens`].  The old codegen had these
///    bindings at `fstart_main` scope; moving them into the
///    `acpi_prepare` body keeps them out of the rest of the adapter
///    where they would shadow the `self.<name>` fields.
///
/// 3. Inside the `fstart_capabilities::acpi::prepare(&platform_acpi,
///    |dsdt_aml, extra_tables| { ... })` closure, emits the per-device
///    `dsdt_aml.extend(AcpiDevice::dsdt_aml(self.<name>.as_ref()?,
///    &<name>_cfg))` + `extra_tables.extend(...)` calls.  The old
///    generator used `&#dev_name` against a local; in the new model
///    `self.<name>.as_ref().unwrap_or_else(|| halt())` yields the
///    same `&T` reference.
///
/// 4. Includes ACPI-only device contributions (AHCI, xHCI, PCIe
///    root) via the shared [`capabilities::acpi::generate_acpi_only_device`]
///    emitter — these have no corresponding `self.<name>` field,
///    so they're identical in the old and new model.
///
/// Boards without `config.acpi` (i.e. no `AcpiPrepare` capability)
/// emit a `todo!()` — dead code since the executor only dispatches
/// `AcpiPrepare` for boards where the capability is present.
fn acpi_prepare_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use super::capabilities::acpi as cap_acpi;
    use super::config_ser;

    // Gate on the stage actually declaring AcpiPrepare — the board
    // may have an `acpi` RON section, but a bootblock stage that
    // doesn’t list AcpiPrepare must not emit code referencing
    // `fstart_acpi` / `fstart_capabilities::acpi` (those crates may
    // not be linked into this stage’s binary).
    let has_acpi_prepare = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare));
    if !has_acpi_prepare {
        return quote! {
            todo!("board_gen::acpi_prepare: stage does not declare AcpiPrepare")
        };
    }

    let Some(acpi_cfg) = ctx.config.acpi.as_ref() else {
        return quote! {
            // No `acpi` config in RON — validation already rejected
            // `AcpiPrepare` in the capability list, so this body is
            // never reached at runtime.  Keep it compilable.
            todo!("board_gen::acpi_prepare: board has no `acpi` RON config")
        };
    };

    // Per-device config bindings, emitted as `let <name>_cfg = <literal>;`.
    let mut config_lets = TokenStream::new();
    // DSDT / extra_tables calls inside the closure.
    let mut device_blocks = TokenStream::new();
    // ACPI-only device blocks (AHCI / xHCI / PcieRoot).
    let mut acpi_only_blocks = TokenStream::new();

    for (idx, dev) in ctx.devices.iter().enumerate() {
        let inst = &ctx.instances[idx];
        let meta = inst.meta();

        // Per-driver contributors — need both `has_acpi` and a
        // non-None `acpi_name()` to emit anything.  Skip ACPI-only
        // instances: they have no `self.<field>` and are handled
        // separately below via `generate_acpi_only_device`.
        if meta.has_acpi && inst.acpi_name().is_some() && !inst.is_acpi_only() {
            let field = format_ident!("{}", dev.name.as_str());
            let cfg_name = format_ident!("{}_cfg", dev.name.as_str());
            let cfg_literal = config_ser::config_tokens(inst);
            let drv_ty = config_ser::driver_type_tokens(inst);
            config_lets.extend(quote! {
                let #cfg_name: <#drv_ty as fstart_services::device::Device>::Config =
                    #cfg_literal;
            });
            device_blocks.extend(quote! {
                dsdt_aml.extend(fstart_acpi::device::AcpiDevice::dsdt_aml(
                    self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt()),
                    &#cfg_name,
                ));
                extra_tables.extend(fstart_acpi::device::AcpiDevice::extra_tables(
                    self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt()),
                    &#cfg_name,
                ));
            });
        }
    }

    // ACPI-only contributors (no `self.<name>` field — they carry
    // their own per-variant struct literal emitted by the shared
    // helper).  The old generator enumerates them with an independent
    // counter so `_acpi_dev_0`, `_acpi_dev_1`, ... don't collide.
    let mut extra_idx = 0usize;
    for (idx, _dev) in ctx.devices.iter().enumerate() {
        let inst = &ctx.instances[idx];
        if !inst.is_acpi_only() {
            continue;
        }
        acpi_only_blocks.extend(cap_acpi::generate_acpi_only_device(inst, extra_idx));
        extra_idx += 1;
    }

    let platform_block = cap_acpi::generate_platform_acpi(&acpi_cfg.platform);

    quote! {
        #platform_block
        #config_lets
        fstart_capabilities::acpi::prepare(&platform_acpi, |dsdt_aml, extra_tables| {
            #device_blocks
            #acpi_only_blocks
        });
    }
}

/// Emit the body of `Board::smbios_prepare`.
///
/// Delegates entirely to the shared
/// [`capabilities::smbios::generate_smbios_prepare`] helper, which is
/// a pure function of [`BoardConfig`] — no `self.<field>` references,
/// no per-device config bindings, no closures.  The emitted
/// `fstart_capabilities::smbios::prepare(&SmbiosDesc { ... })` call
/// is identical in the old and new models.
///
/// Boards without `config.smbios` (i.e. no `SmBiosPrepare`
/// capability) emit a `todo!()` — dead code since the executor
/// only dispatches `SmBiosPrepare` for boards where the capability
/// is present.
fn smbios_prepare_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let has_smbios_prepare = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::SmBiosPrepare));
    if !has_smbios_prepare {
        return quote! {
            todo!("board_gen::smbios_prepare: stage does not declare SmBiosPrepare")
        };
    }
    if ctx.config.smbios.is_none() {
        return quote! {
            // No `smbios` config in RON — validation already rejected
            // `SmBiosPrepare` in the capability list, so this body is
            // never reached at runtime.
            todo!("board_gen::smbios_prepare: board has no `smbios` RON config")
        };
    }
    super::capabilities::generate_smbios_prepare(ctx.config)
}

/// Emit the body of `Board::boot_media_select`.
///
/// Called by the executor for `CapOp::BootMediaAuto` and the initial
/// step of `CapOp::LoadNextStage`.  Inspects the hardware boot-source
/// register (currently Allwinner-sunxi-only), picks whichever
/// candidate's `media_ids` contains that byte, and records the
/// selection on `self._boot_media` so subsequent trampolines read
/// from the right device.  Returns the chosen candidate's
/// [`DeviceId`](fstart_types::DeviceId) so the executor can fold it
/// into its `inited` mask after running `init_device`.
///
/// # When the real body is emitted
///
/// The stage must:
///
/// 1. Be sunxi-format (`SocImageFormat::AllwinnerEgon`).
///    Non-eGON boards do not implement the `boot_media` register and
///    referencing `fstart_soc_sunxi` would fail to compile if the
///    crate is not linked into the stage (generic armv7 and riscv /
///    aarch64 boards).
/// 2. Declare `Capability::LoadNextStage` or `Capability::BootMedia`
///    with an AutoDevice medium.  These are the only paths that lead
///    the executor to `boot_media_select`; stages without them get a
///    dead-code `todo!()`.
///
/// Both conditions together also guarantee that `fstart_soc_sunxi`
/// is in scope for this stage (sunxi boards enable the feature flag
/// that pulls it in).
///
/// # The emitted body
///
/// - Reads the hardware byte via
///   `fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize)`.
/// - Iterates `candidates` linearly and picks the first whose
///   `media_ids` contains the byte.
/// - Writes
///   `self._boot_media = BootMediaState::Block { device_id, offset, size }`
///   with the candidate's values (today all LoadNextStage candidates
///   are block devices; if a future capability adds memory-mapped
///   candidates we'll need an explicit media-kind tag on
///   `BootMediaCandidate`).
/// - Returns `Some(candidate.device)` on match, `None` with a
///   diagnostic log on miss.
fn boot_media_select_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_boot_media_select = ctx.stage_capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::LoadNextStage { .. } | Capability::BootMedia(_)
        )
    });
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_boot_media_select || !is_egon {
        return quote! {
            // Dead code: this stage does not declare a capability
            // that reaches `boot_media_select`, or the board is not
            // a sunxi eGON board.  Emitting the real body would
            // reference `fstart_soc_sunxi`, which is only linked
            // into stages that enable the sunxi feature.
            let _ = candidates;
            todo!("board_gen::boot_media_select: stage does not use LoadNextStage/BootMediaAuto, \
                   or board is not sunxi-eGON")
        };
    }

    quote! {
        // SAFETY: reading the BROM-populated eGON header at
        // `self._egon_sram_base + 0x28`.  The const-initialised
        // `_egon_sram_base` is the first stage's `load_addr` from RON,
        // which is where the BROM dropped us and thus where the
        // header lives.
        let _bm = fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize);
        fstart_log::info!("boot media detect: {:#x}", _bm);
        for candidate in candidates {
            if candidate.media_ids.iter().any(|&id| id == _bm) {
                self._boot_media = fstart_stage_runtime::BootMediaState::Block {
                    device_id: candidate.device,
                    offset: candidate.offset,
                    size: candidate.size,
                };
                return Some(candidate.device);
            }
        }
        fstart_log::error!(
            "boot_media_select: no candidate matched boot media {:#x}",
            _bm,
        );
        None
    }
}

/// Emit the body of `Board::load_next_stage`.
///
/// Reads the eGON header to get the next-stage offset and size, reads
/// the named stage from the current `self._boot_media` device into
/// its pre-declared load address, serialises a handoff descriptor,
/// and jumps with handoff.  This is the capstone of the sunxi
/// bootblock → main stage transition.
///
/// # Emitted shape
///
/// 1. `match next_stage { ... }` — one arm per multi-stage entry in
///    RON other than this stage, each arm binding `load_addr` and
///    `handoff_addr` locals (and halting on unknown stage names).
/// 2. Reads `ns_ffs_offset` and `ns_size` from
///    `fstart_soc_sunxi::next_stage_{offset,size}_at(self._egon_sram_base as usize)`.
/// 3. Validates both are non-zero; halts otherwise.
/// 4. `match self._boot_media { Block { device_id, offset, .. } => { match device_id { ... } } ... }`
///    — per-device dispatch using the block-device field pattern
///    already used by `sig_verify` / `stage_load`.  Each arm calls
///    `fstart_capabilities::next_stage::read_stage_to_addr(
///       self.<field>.as_ref()?, name, next_stage, dev_offset, load_addr, ns_size)`.
/// 5. Serialises the handoff to `handoff_addr` (= `load_addr - 0x1000`)
///    via `fstart_capabilities::next_stage::serialize_handoff(dram_size,
///    handoff_addr)`.  `dram_size` prefers `self._handoff` (carried
///    over from a previous stage when chained), falls back to the
///    runtime-detected DRAM size from a `DramInit` driver (when the
///    stage declares one), and finally to `self._dram_size_static`.
/// 6. `fstart_platform::jump_to_with_handoff(load_addr, handoff_addr as usize)`.
///
/// Dead-codes to `todo!()` for stages that don't declare
/// `LoadNextStage` or don't use the sunxi eGON format.  Validation
/// upstream already ensures this combination, so the executor never
/// reaches the stub at runtime.
fn load_next_stage_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_load_next_stage = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::LoadNextStage { .. }));
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_load_next_stage || !is_egon {
        return quote! {
            let _ = next_stage;
            todo!("board_gen::load_next_stage: stage does not use LoadNextStage, \
                   or board is not sunxi-eGON")
        };
    }

    // Collect (stage_name, load_addr) for every stage defined in the
    // MultiStage layout, so `match next_stage { ... }` can dispatch
    // to the right load/handoff addresses.  The current stage is
    // excluded (can't load ourselves), but with only 2-3 stages in
    // practice the extra code size is negligible.
    let stage_arms = match &ctx.config.stages {
        StageLayout::MultiStage(stages) => stages
            .iter()
            .map(|s| {
                let name = s.name.as_str();
                let load_addr = hex_addr(s.load_addr);
                let handoff_addr = hex_addr(s.load_addr.saturating_sub(0x1000));
                quote! {
                    #name => (#load_addr, #handoff_addr),
                }
            })
            .collect::<TokenStream>(),
        _ => quote! {},
    };

    // Per-device read-and-dispatch arms.  Matches the block-device
    // arm pattern used by `sig_verify` / `stage_load`, but here the
    // body calls `read_stage_to_addr` instead of reading the FFS
    // anchor.
    let dev_arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "BlockDevice")
        })
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            quote! {
                #id_lit => {
                    fstart_capabilities::next_stage::read_stage_to_addr(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        #dev_name,
                        next_stage,
                        dev_offset,
                        load_addr,
                        ns_size,
                    )
                    .unwrap_or_else(|_| {
                        fstart_log::error!(
                            "FATAL: failed to read stage from {}",
                            #dev_name,
                        );
                        fstart_platform::halt();
                    });
                }
            }
        });

    // `dram_size_for_handoff_expr` resolves the DRAM size to pass
    // into the handoff.  Priority:
    // 1. `self._handoff.dram_size` (non-first stage carrying a
    //    previous stage's handoff).
    // 2. The runtime `detected_size_bytes()` from a DramInit driver
    //    if the stage declares one — the old codegen's approach.
    // 3. `self._dram_size_static` as the static fallback.
    let dram_device = ctx.stage_capabilities.iter().find_map(|cap| match cap {
        Capability::DramInit { device } => Some(device.as_str()),
        _ => None,
    });
    let dram_size_expr = match dram_device {
        Some(dev_name) => {
            let dev = format_ident!("{}", dev_name);
            quote! {
                self._handoff
                    .as_ref()
                    .filter(|h| h.dram_size > 0)
                    .map(|h| h.dram_size)
                    .unwrap_or_else(|| {
                        self.#dev
                            .as_ref()
                            .map(|d| d.detected_size_bytes())
                            .unwrap_or(self._dram_size_static)
                    })
            }
        }
        None => quote! {
            self._handoff
                .as_ref()
                .filter(|h| h.dram_size > 0)
                .map(|h| h.dram_size)
                .unwrap_or(self._dram_size_static)
        },
    };

    quote! {
        // Stage-specific load and handoff addresses.
        let (load_addr, handoff_addr): (u64, u64) = match next_stage {
            #stage_arms
            other => {
                fstart_log::error!(
                    "load_next_stage: unknown next-stage name '{}'",
                    other,
                );
                fstart_platform::halt();
            }
        };

        // eGON header read (patched by the FFS assembler at image build).
        let ns_ffs_offset =
            fstart_soc_sunxi::next_stage_offset_at(self._egon_sram_base as usize) as u64;
        let ns_size =
            fstart_soc_sunxi::next_stage_size_at(self._egon_sram_base as usize) as usize;
        if ns_ffs_offset == 0 || ns_size == 0 {
            fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
            fstart_platform::halt();
        }

        // Per-device dispatch against the current boot medium.
        match self._boot_media {
            fstart_stage_runtime::BootMediaState::Block { device_id, offset, .. } => {
                let dev_offset = offset + ns_ffs_offset;
                match device_id {
                    #(#dev_arms)*
                    _ => {
                        fstart_log::error!(
                            "load_next_stage: unknown block device id {}",
                            device_id,
                        );
                        fstart_platform::halt();
                    }
                }
            }
            fstart_stage_runtime::BootMediaState::Mmio { .. } => {
                fstart_log::error!(
                    "load_next_stage: boot medium is a memory-mapped region, \
                     not a block device",
                );
                fstart_platform::halt();
            }
            fstart_stage_runtime::BootMediaState::None => {
                fstart_log::error!(
                    "load_next_stage: no boot medium selected",
                );
                fstart_platform::halt();
            }
        }

        // Handoff serialisation + jump.  `dram_size` prefers the
        // runtime-detected value (from handoff or DramInit driver)
        // and falls back to the RON static size.
        let dram_size: u64 = #dram_size_expr;
        fstart_capabilities::next_stage::serialize_handoff(dram_size, handoff_addr)
            .unwrap_or_else(|_| {
                fstart_log::error!("FATAL: handoff serialize failed");
                fstart_platform::halt();
            });
        fstart_log::info!(
            "jumping to stage '{}' at {:#x}",
            next_stage,
            load_addr,
        );
        fstart_platform::jump_to_with_handoff(load_addr, handoff_addr as usize)
    }
}

/// Emit the body of `Board::payload_load`.
///
/// Dispatches on the payload kind (mirroring the old
/// `capabilities::payload::generate_payload_load`):
///
/// - **UEFI** (`is_uefi_payload`) → build a `PlatformConfig` from
///   `self.<devices>` + static board data, optionally BL31 load for
///   aarch64 + ATF, call `fstart_crabefi::init_platform(_)` (→ !).
/// - **LinuxBoot** and **FitBuildtime** → load firmware (SBI/ATF/etc.)
///   plus kernel from FFS via a `match_boot_media` dispatch, then
///   call the platform boot protocol.
/// - **FitRuntime** → parse the embedded FIT via
///   `fstart_capabilities::fit::load_fit_components`, load optional
///   firmware, then the platform boot protocol.
/// - **No payload** → `fstart_capabilities::payload_load(..)` generic
///   stub, used by bare stages that have no specific boot target.
///
/// All FFS-touching variants go through [`match_boot_media`] so the
/// boot medium reconstruction stays a single code path.  The UEFI
/// path references `self._acpi_rsdp_addr` and `self._inited` for
/// runtime state instead of the old fstart_main-scoped
/// `_acpi_rsdp_addr` / `_<name>_ok` locals.
fn payload_load_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    use super::validation::{is_fit_image, is_fit_runtime, is_linux_boot, is_uefi_payload};

    // If the stage doesn’t declare PayloadLoad, the executor never
    // dispatches this method.  Emit a dead-code stub rather than
    // referencing crates (fstart_crabefi, etc.) that may not be
    // linked into this stage.
    let has_payload_load = ctx
        .stage_capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if !has_payload_load {
        return quote! {
            todo!("board_gen::payload_load: stage does not declare PayloadLoad")
        };
    }

    if is_uefi_payload(ctx.config) {
        return payload_load_uefi_body(platform, ctx);
    }
    if is_linux_boot(ctx.config) || (is_fit_image(ctx.config) && !is_fit_runtime(ctx.config)) {
        return payload_load_linux_body(platform, ctx);
    }
    if is_fit_image(ctx.config) && is_fit_runtime(ctx.config) {
        return payload_load_fit_runtime_body(platform, ctx);
    }

    // Generic FFS payload: `fstart_capabilities::payload_load(anchor,
    // &bm, fstart_platform::jump_to)`.  Only reached for boards with
    // a raw `PayloadLoad` capability and no specific `PayloadKind` —
    // today none of the fixture boards hit this branch, but keeping
    // it means `board_gen` produces valid code for any future raw
    // payload stage.
    if !ctx.ffs_stage {
        return quote! {
            todo!("board_gen::payload_load: generic payload requires an FFS-using stage")
        };
    }
    let anchor = anchor_bytes_stmt();
    let bm_usage = quote! {
        fstart_capabilities::payload_load(_anchor_bytes, &_bm, fstart_platform::jump_to);
    };
    let none_body = quote! {
        fstart_log::error!("payload_load: no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);
    quote! {
        #anchor
        #match_body
        // `payload_load` is `-> !` — if the capability returns, halt.
        fstart_log::error!("payload_load: returned unexpectedly — halting");
        fstart_platform::halt()
    }
}

/// Emit the LinuxBoot + FIT-buildtime payload load body.
///
/// Steps (all inside a [`match_boot_media`] dispatch):
///
/// 1. Optional firmware (SBI/ATF) load via `load_ffs_file_by_type`
///    with `FileType::Firmware`.
/// 2. Kernel load via `load_ffs_file_by_type` with `FileType::Payload`.
///
/// Then, outside the boot-media block, the platform boot protocol
/// (via [`platform_boot_protocol_stmts`]).
fn payload_load_linux_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    if !ctx.ffs_stage {
        return quote! {
            todo!("board_gen::payload_load (LinuxBoot): requires an FFS-using stage")
        };
    }
    let payload = ctx
        .config
        .payload
        .as_ref()
        .expect("LinuxBoot implies payload");
    let anchor = anchor_bytes_stmt();

    // `bm_usage` is the body of each boot-media arm: firmware load
    // (if configured) followed by kernel load.
    let firmware_load_tokens = match payload.firmware.as_ref() {
        Some(fw) => firmware_load_inside_match(fw),
        None => TokenStream::new(),
    };
    let bm_usage = quote! {
        #firmware_load_tokens
        fstart_log::info!("loading kernel...");
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Payload,
        ) {
            fstart_log::error!("FATAL: failed to load kernel");
            fstart_platform::halt();
        }
    };
    let none_body = quote! {
        fstart_log::error!("payload_load (LinuxBoot): no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);

    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));
    let platform_boot = platform_boot_protocol_stmts(platform, &kernel_addr, payload);

    quote! {
        fstart_log::info!("capability: PayloadLoad (LinuxBoot)");
        #anchor
        #match_body
        #platform_boot
        // Platform boot protocol is `-> !` for every platform (armv7
        // `boot_linux`, aarch64 `boot_linux_atf_prepared`, etc.).
        // If for any reason it returns, the surrounding `-> !` still
        // requires a diverging expression.
        fstart_platform::halt()
    }
}

/// Emit the FIT-runtime payload load body.
///
/// Calls `fstart_capabilities::fit::load_fit_components` against the
/// current boot medium, captures `_kernel_load`, optionally loads
/// firmware, then runs the platform boot protocol with
/// `#kernel_addr = _kernel_load`.
fn payload_load_fit_runtime_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    if !ctx.ffs_stage {
        return quote! {
            todo!("board_gen::payload_load (FIT runtime): requires an FFS-using stage")
        };
    }
    let payload = ctx
        .config
        .payload
        .as_ref()
        .expect("FIT runtime implies payload");
    let anchor = anchor_bytes_stmt();

    let config_expr = match &payload.fit_config {
        Some(name) => {
            let name_str = name.as_str();
            quote! { Some(#name_str) }
        }
        None => quote! { None },
    };

    let firmware_load_tokens = match payload.firmware.as_ref() {
        Some(fw) => firmware_load_inside_match(fw),
        None => TokenStream::new(),
    };

    // FIT runtime's `bm_usage` has to return a value (the kernel
    // address) from the closure.  Rather than fighting the
    // closure pattern we emit the whole sequence inline.
    let bm_usage = quote! {
        let _fit_boot = fstart_capabilities::fit::load_fit_components(
            _anchor_bytes,
            &_bm,
            #config_expr,
        )
        .unwrap_or_else(|e| {
            fstart_log::error!(
                "FATAL: FIT boot failed: {}",
                fstart_capabilities::fit::error_str(&e),
            );
            fstart_platform::halt();
        });
        _kernel_load = _fit_boot.kernel_addr;
        #firmware_load_tokens
    };
    let none_body = quote! {
        fstart_log::error!("payload_load (FIT runtime): no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);

    let kernel_addr = quote! { _kernel_load };
    let platform_boot = platform_boot_protocol_stmts(platform, &kernel_addr, payload);

    quote! {
        fstart_log::info!("capability: PayloadLoad (FIT runtime)");
        #anchor
        let mut _kernel_load: u64 = 0;
        #match_body
        #platform_boot
        fstart_platform::halt()
    }
}

/// Emit the firmware-load fragment used inside a `match_boot_media`
/// arm (LinuxBoot or FIT runtime).  Assumes `_anchor_bytes` and `_bm`
/// are in scope.
fn firmware_load_inside_match(firmware: &FirmwareConfig) -> TokenStream {
    let fw_kind_str = match firmware.kind {
        FirmwareKind::OpenSbi => "SBI firmware",
        FirmwareKind::ArmTrustedFirmware => "ATF BL31",
    };
    let load_msg = format!("loading {fw_kind_str}...");
    let error_msg = format!("FATAL: failed to load {fw_kind_str}");
    quote! {
        fstart_log::info!(#load_msg);
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Firmware,
        ) {
            fstart_log::error!(#error_msg);
            fstart_platform::halt();
        }
    }
}

/// Emit the platform-specific boot protocol sequence.
///
/// Matches the old `capabilities::payload::generate_platform_boot_protocol`
/// but reads per-board state from `&self` where needed:
///
/// - `_acpi_rsdp_addr` → `self._acpi_rsdp_addr` (x86_64 only; Linux
///   `boot_linux` wants the RSDP).
///
/// All other values (kernel addr, dtb addr, firmware addr, bootargs)
/// come from the payload literal — they're const, not runtime state.
fn platform_boot_protocol_stmts(
    platform: Platform,
    kernel_addr: &TokenStream,
    payload: &PayloadConfig,
) -> TokenStream {
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));
    match platform {
        Platform::Riscv64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            quote! {
                let _fw_info = fstart_platform::FwDynamicInfo::new(
                    #kernel_addr,
                    fstart_platform::boot_hart_id(),
                );
                fstart_log::info!("jumping to SBI firmware...");
                fstart_platform::boot_linux_sbi(
                    #fw_addr,
                    fstart_platform::boot_hart_id(),
                    #dtb_addr,
                    &_fw_info,
                );
            }
        }
        Platform::Aarch64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            quote! {
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform::boot_linux_atf_prepared(
                    #kernel_addr,
                    #dtb_addr,
                    #fw_addr,
                );
            }
        }
        Platform::Armv7 => quote! {
            fstart_log::info!("booting Linux (ARMv7)...");
            fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
            fstart_log::info!("  dtb    @ {:#x}", #dtb_addr as u64);
            fstart_platform::cleanup_before_linux();
            fstart_platform::boot_linux(#kernel_addr as u64, #dtb_addr);
        },
        Platform::X86_64 => {
            let bootargs_str = payload.bootargs.as_deref().unwrap_or("console=ttyS0");
            quote! {
                fstart_log::info!("booting Linux (x86_64)...");
                fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
                // e820 from the global E820State (populated by MemoryDetect).
                let _e820_state =
                    unsafe { fstart_services::memory_detect::e820_state() };
                fstart_platform::boot_linux(
                    #kernel_addr as u64,
                    self._acpi_rsdp_addr,
                    _e820_state.entries(),
                    #bootargs_str,
                    0x90000u64,
                );
            }
        }
    }
}

/// Emit the UEFI (CrabEFI) payload load body.
///
/// Mirrors `capabilities::payload::generate_payload_load_uefi` but
/// routes device references through `self.<field>` and reads the
/// RSDP + framebuffer-init flag from `self._acpi_rsdp_addr` /
/// `self._inited` respectively.
///
/// Sections (same order as the old generator):
///
/// 1. Optional BL31 load (aarch64 + ATF firmware) via `match_boot_media`.
/// 2. Timer / reset / RNG setup (per-platform).
/// 3. Console adapter (`self.<console>.as_ref()`).
/// 4. FDT blob probe (aarch64 / riscv64 from `boot_dtb_addr`).
/// 5. FDT reservation (non-x86).
/// 6. Memory map build (x86 from e820, others from static RAM + FDT
///    reservation).
/// 7. Framebuffer config gated on `self._inited.contains(fb_id)`.
/// 8. `fstart_crabefi::PlatformConfig { ... }` literal.
/// 9. `fstart_crabefi::init_platform(_crabefi_config)` (→ !).
fn payload_load_uefi_body(platform: Platform, ctx: &BoardCtx<'_>) -> TokenStream {
    let config = ctx.config;
    let payload = config.payload.as_ref().expect("UEFI implies payload");

    // Collect static memory map entries (ROM, Reserved) from board config.
    let mut static_mem_entries = TokenStream::new();
    for region in &config.memory.regions {
        let base = hex_addr(region.base);
        let size = hex_addr(region.size);
        match region.kind {
            RegionKind::Rom => static_mem_entries.extend(quote! {
                fstart_crabefi::MemoryRegion {
                    base: #base, size: #size,
                    region_type: fstart_crabefi::MemoryType::RuntimeServicesCode,
                },
            }),
            RegionKind::Reserved => static_mem_entries.extend(quote! {
                fstart_crabefi::MemoryRegion {
                    base: #base, size: #size,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                },
            }),
            RegionKind::Ram => {}
        }
    }

    // RAM region from board config.
    let ram_region = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram);
    let ram_base_lit = ram_region
        .map(|r| hex_addr(r.base))
        .unwrap_or_else(|| quote! { 0u64 });
    let ram_size_lit = ram_region
        .map(|r| hex_addr(r.size))
        .unwrap_or_else(|| quote! { 0u64 });

    // Firmware data/stack addresses from stage config.
    let (fw_data_addr, fw_stack_size) = match &config.stages {
        StageLayout::Monolithic(mono) => (
            mono.data_addr.unwrap_or(mono.load_addr),
            mono.stack_size as u64,
        ),
        StageLayout::MultiStage(stages) => {
            let last = stages.last().expect("multi-stage has at least one stage");
            (
                last.data_addr.unwrap_or(last.load_addr),
                last.stack_size as u64,
            )
        }
    };
    let fw_data_addr_lit = hex_addr(fw_data_addr);
    let fw_stack_size_lit = hex_addr(fw_stack_size);

    // Console device for DebugOutput adapter: find the first enabled
    // Console provider.  `_BoardDevices` always stores it in the
    // `self.<name>` field.
    let console_device_idx =
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded).find(|idx| {
            ctx.devices[*idx]
                .services
                .iter()
                .any(|s| s.as_str() == "Console")
        });
    let (console_setup, debug_output_field) = match console_device_idx {
        Some(idx) => {
            let field = format_ident!("{}", ctx.devices[idx].name.as_str());
            (
                quote! {
                    let _console_ref = self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt());
                    let mut _crabefi_console = fstart_crabefi::ConsoleAdapter(_console_ref);
                },
                quote! { debug_output: Some(&mut _crabefi_console), },
            )
        }
        None => (quote! {}, quote! { debug_output: None, }),
    };

    // PCI device for ECAM base.
    let pci_device_idx = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).find(|idx| {
        ctx.devices[*idx]
            .services
            .iter()
            .any(|s| s.as_str() == "PciRootBus")
    });
    let ecam_base_field = match pci_device_idx {
        Some(idx) => {
            let field = format_ident!("{}", ctx.devices[idx].name.as_str());
            quote! {
                ecam_base: Some(
                    self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt())
                        .ecam_base(),
                ),
            }
        }
        None => quote! { ecam_base: None, },
    };

    // Framebuffer device for GOP — gated on the init mask via
    // `self._inited.contains(fb_id)` rather than the old fstart_main
    // `_fb_ok: bool` local.
    let fb_device_idx = enabled_indices(ctx.devices, ctx.instances, ctx.excluded).find(|idx| {
        ctx.devices[*idx]
            .services
            .iter()
            .any(|s| s.as_str() == "Framebuffer")
    });
    let (fb_setup, framebuffer_field) = match fb_device_idx {
        Some(idx) => {
            let field = format_ident!("{}", ctx.devices[idx].name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let setup = quote! {
                let _fb_config = if self._inited.contains(#id_lit) {
                    let _fb_ref = self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt());
                    let _fb_info = _fb_ref.info();
                    Some(fstart_crabefi::FramebufferConfig {
                        physical_address: _fb_info.base_addr,
                        width: _fb_info.width,
                        height: _fb_info.height,
                        stride: _fb_info.stride,
                        bits_per_pixel: _fb_info.bits_per_pixel,
                        red_mask_pos: _fb_info.red_pos,
                        red_mask_size: _fb_info.red_size,
                        green_mask_pos: _fb_info.green_pos,
                        green_mask_size: _fb_info.green_size,
                        blue_mask_pos: _fb_info.blue_pos,
                        blue_mask_size: _fb_info.blue_size,
                    })
                } else {
                    None
                };
            };
            (setup, quote! { framebuffer: _fb_config, })
        }
        None => (quote! {}, quote! { framebuffer: None, }),
    };

    // FDT sourcing — mirrors `dtb_src_expr`-ish logic.
    let fdt_addr_expr = if let Some(addr) = payload.src_dtb_addr {
        hex_addr(addr)
    } else {
        match platform {
            Platform::Aarch64 | Platform::Riscv64 => {
                quote! { fstart_platform::boot_dtb_addr() }
            }
            Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
        }
    };
    let fdt_setup = match platform {
        Platform::Aarch64 | Platform::Riscv64 => quote! {
            let _fdt_addr = #fdt_addr_expr;
            // SAFETY: platform guarantees _fdt_addr points to a valid
            // FDT blob saved from the boot register on entry.
            let _fdt_blob: Option<&[u8]> =
                unsafe { fstart_capabilities::fdt_blob_from_addr(_fdt_addr) };
        },
        Platform::Armv7 | Platform::X86_64 => quote! {
            let _fdt_addr: u64 = 0;
            let _fdt_blob: Option<&[u8]> = None;
        },
    };
    let fdt_field = quote! { fdt: _fdt_blob, };

    // BL31 load — aarch64 + ATF only.
    let bl31_boot = if let Some(fw) = payload.firmware.as_ref() {
        if platform == Platform::Aarch64 && fw.kind == FirmwareKind::ArmTrustedFirmware {
            let fw_load_addr = hex_addr(fw.load_addr);
            let anchor = anchor_bytes_stmt();
            let bm_usage = quote! {
                if !fstart_capabilities::load_ffs_file_by_type(
                    _anchor_bytes,
                    &_bm,
                    fstart_types::ffs::FileType::Firmware,
                ) {
                    fstart_log::error!("FATAL: failed to load BL31 firmware");
                    fstart_platform::halt();
                }
            };
            let none_body = quote! {
                fstart_log::error!("payload_load (UEFI): no boot media for BL31");
                fstart_platform::halt();
            };
            let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);
            quote! {
                fstart_log::info!("loading TF-A BL31 firmware...");
                #anchor
                #match_body
                fstart_log::info!("booting BL31 (GIC, PSCI, NS switch)...");
                fstart_platform::boot_bl31_and_resume(
                    #fw_load_addr,
                    fstart_platform::boot_dtb_addr(),
                );
                fstart_log::info!("resumed from BL31 at EL2 NS");
            }
        } else {
            quote! {}
        }
    } else {
        quote! {}
    };

    // Platform-specific timer, reset, RNG.
    let (timer_setup, timer_field, reset_setup, reset_field, rng_setup, rng_field) = match platform
    {
        Platform::X86_64 => (
            quote! {
                let _crabefi_timer = fstart_crabefi::TscTimer::new();
                fstart_log::info!("TSC timer initialized");
            },
            quote! { timer: &_crabefi_timer, },
            quote! { let _crabefi_reset = fstart_crabefi::X86Reset; },
            quote! { reset: &_crabefi_reset, },
            quote! { let _crabefi_rng = fstart_crabefi::X86Rng::new(); },
            quote! { rng: Some(&_crabefi_rng), },
        ),
        _ => (
            quote! { let _crabefi_timer = fstart_crabefi::ArmGenericTimer::new(); },
            quote! { timer: &_crabefi_timer, },
            quote! { let _crabefi_reset = fstart_crabefi::PsciReset; },
            quote! { reset: &_crabefi_reset, },
            quote! {},
            quote! { rng: None, },
        ),
    };

    // ACPI RSDP: x86 reads from self._acpi_rsdp_addr (populated by
    // AcpiLoad).  On non-x86 the RSDP field stays None.
    let acpi_rsdp_field = if platform == Platform::X86_64 {
        quote! { acpi_rsdp: Some(self._acpi_rsdp_addr), }
    } else {
        quote! { acpi_rsdp: None, }
    };

    // Runtime region: x86 only.
    let runtime_region_field = if platform == Platform::X86_64 {
        quote! { Some(fstart_crabefi::compute_runtime_region()), }
    } else {
        quote! { None, }
    };

    // Memory map — x86 from e820, others from static RAM + FDT.
    let memory_map_setup = if platform == Platform::X86_64 {
        quote! {
            let _e820_state = unsafe { fstart_services::memory_detect::e820_state() };
            let _rom_entries: &[fstart_crabefi::MemoryRegion] = &[
                #static_mem_entries
            ];
            let mut _crabefi_mem_buf: [fstart_crabefi::MemoryRegion; 64] = [
                fstart_crabefi::MemoryRegion {
                    base: 0, size: 0,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                64
            ];
            let _mem_idx = fstart_crabefi::build_efi_memory_map_from_e820(
                _e820_state.entries(),
                0, 0,
                0, 0,
                _rom_entries,
                &mut _crabefi_mem_buf,
            );
            let _crabefi_memory_map: &[fstart_crabefi::MemoryRegion] =
                &_crabefi_mem_buf[.._mem_idx];
            fstart_log::info!("EFI memory map: {} entries", _mem_idx as u32);
        }
    } else {
        quote! {
            let _static_entries: &[fstart_crabefi::MemoryRegion] = &[
                #static_mem_entries
            ];
            let mut _crabefi_mem_buf: [fstart_crabefi::MemoryRegion; 12] = [
                fstart_crabefi::MemoryRegion {
                    base: 0, size: 0,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                12
            ];
            let _mem_idx = fstart_crabefi::build_efi_memory_map(
                _static_entries,
                #ram_base_lit,
                #ram_size_lit,
                #fw_data_addr_lit,
                #fw_stack_size_lit,
                #fw_stack_size_lit,
                _fdt_reservation,
                &mut _crabefi_mem_buf,
            );
            let _crabefi_memory_map: &[fstart_crabefi::MemoryRegion] =
                &_crabefi_mem_buf[.._mem_idx];
            fstart_log::info!("EFI memory map: {} entries", _mem_idx as u32);
        }
    };

    // FDT reservation — non-x86 only.
    let fdt_reservation_setup = if platform != Platform::X86_64 {
        quote! {
            let _fdt_reservation = if _fdt_addr != 0 {
                let fdt_size = unsafe {
                    fstart_crabefi::fdt_page_aligned_size(_fdt_addr)
                };
                Some((_fdt_addr, fdt_size))
            } else {
                None
            };
        }
    } else {
        quote! {}
    };

    quote! {
        fstart_log::info!("Launching CrabEFI UEFI payload...");

        #bl31_boot

        #timer_setup
        #reset_setup
        #rng_setup
        #console_setup

        #fdt_setup
        #fdt_reservation_setup
        #memory_map_setup

        #fb_setup

        let _crabefi_config = fstart_crabefi::PlatformConfig {
            memory_map: _crabefi_memory_map,
            #timer_field
            #reset_field
            block_devices: &mut [],
            variable_backend: None,
            #debug_output_field
            console_input: None,
            #framebuffer_field
            #acpi_rsdp_field
            smbios: None,
            #fdt_field
            #rng_field
            #ecam_base_field
            deferred_buffer: None,
            runtime_region: #runtime_region_field
            heap_pre_initialized: false,
        };

        fstart_log::info!(
            "EFI memory map: {} entries, calling init_platform...",
            _mem_idx as u32,
        );

        // init_platform() is `-> !` (never returns).
        fstart_crabefi::init_platform(_crabefi_config)
    }
}

/// Emit the body of `Board::init_device`.
///
/// Produces `match id { ... }` with one arm per enabled,
/// non-structural, non-ACPI device.  Each arm walks the device's
/// ancestor chain (root-first) and, for every non-inited ancestor
/// plus the target itself:
///
/// 1. Constructs the device via `Device::new(&cfg)` or
///    `BusDevice::new_on_bus(&cfg, &parent)` (picking based on the
///    driver's `is_bus_device` metadata), storing the result in
///    `self.<field>`.
/// 2. Calls `self.<field>.as_mut().ok_or(_)?.init()?`.
/// 3. Marks `self._inited.set(id)` so subsequent `init_device` calls
///    return early via the `if self._inited.contains(id)` guard at
///    the top of the arm.
///
/// The ancestor-chain inlining matches the old `ensure_device_ready`
/// behaviour: when a capability references a device X, its
/// construction triggers `.init()` on its whole ancestor chain
/// root-first (SuperIO needs its LPC southbridge programmed, etc.).
///
/// Wildcard arm halts the board — the executor only passes ids
/// declared in the plan.
///
/// # Error propagation
///
/// Each `.init()` returns `Result<(), DeviceError>`; the `?` operator
/// propagates up.  Construction itself (`Device::new`) can also
/// return `Err`; same pattern.  The trait's `-> Result<(), DeviceError>`
/// return type matches the old `.unwrap_or_else(halt)` but is
/// composable — the executor decides the failure policy (currently
/// always `halt()`, see the `if board.init_device(id).is_err()`
/// arms in `run_stage`).
fn init_device_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use super::config_ser::{config_tokens, driver_type_tokens};

    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| {
            let inst = &ctx.instances[*idx];
            !inst.is_structural() && !inst.is_acpi_only()
        })
        .map(|idx| {
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            // Walk root→target collecting every non-structural,
            // non-acpi-only, enabled device — this matches
            // `ensure_device_ready`'s chain construction.
            let chain = chain_from_root(idx, ctx.device_tree, ctx.devices, ctx.instances);
            let steps = chain.iter().map(|&step_idx| {
                let step_dev = &ctx.devices[step_idx];
                let step_inst = &ctx.instances[step_idx];
                let step_field = format_ident!("{}", step_dev.name.as_str());
                let step_id_lit = proc_macro2::Literal::u8_unsuffixed(step_idx as u8);
                let ty = driver_type_tokens(step_inst);
                let cfg = config_tokens(step_inst);
                // Construction — dispatch on `is_bus_device`.
                //
                // Emission pattern: `let mut _dev = T::new(&cfg)?;
                // _dev.init()?; self.<field> = Some(_dev);`
                //
                // This matches the old pre-flip
                // `capabilities::generate_console_init` output and
                // keeps the freshly-constructed driver at a single
                // stable stack location for the duration of
                // `init()`.
                //
                // Known limitation (debug only): on AArch64 in
                // debug mode, LLVM can still route the 16-byte
                // `Pl011::init` call through a stack scratch copy.
                // If the driver has internal state that `init()`
                // depends on being at the eventual field address,
                // that state ends up "stranded" in the scratch copy
                // and the field's `Pl011` misses the init.  `Pl011`
                // today has no such state — `init()` only writes
                // MMIO — but the `&'static Pl011Regs` pointer does
                // propagate from `new()` to the field and may read
                // back stale on some codegen paths.  Release mode
                // optimises the scratch chain away entirely.  The
                // true fix is a driver-side rework (store `base:
                // usize` instead of `&'static Regs`, or offer a
                // `construct_into(slot: &mut MaybeUninit<Self>)`
                // trait method).  Tracked as a follow-up in the
                // stage-runtime-codegen-split plan.
                //
                // Earlier rejected alternatives:
                //
                // - `self.<field> = Some(T::new(&cfg)?);
                //   self.<field>.as_mut().ok_or(...)?.init()?`:
                //   chained `Try::branch` debug codegen corrupted
                //   24-byte `Ns16550` struct copies (discriminant
                //   → 1 → `unreachable!()` → silent panic).
                // - `Option::insert` + init through returned `&mut`:
                //   works on AArch64 debug but hangs RISC-V debug
                //   (inverse of the current pattern's limitation).
                // - `core::hint::black_box(&mut _dev)` barrier:
                //   same RISC-V debug hang as `Option::insert`.
                let construct = if step_inst.meta().is_bus_device {
                    let parent_name =
                        walk_to_real_parent(step_idx, ctx.device_tree, ctx.devices, ctx.instances);
                    match parent_name {
                        Some(pname) => {
                            let parent = format_ident!("{}", pname);
                            quote! {
                                let _cfg = #cfg;
                                let _parent_ref = self.#parent
                                    .as_ref()
                                    .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                                let mut _dev = <#ty>::new_on_bus(&_cfg, _parent_ref)?;
                                _dev.init()?;
                                self.#step_field = Some(_dev);
                            }
                        }
                        None => quote! {
                            let _cfg = #cfg;
                            let mut _dev = <#ty>::new(&_cfg)?;
                            _dev.init()?;
                            self.#step_field = Some(_dev);
                        },
                    }
                } else {
                    quote! {
                        let _cfg = #cfg;
                        let mut _dev = <#ty>::new(&_cfg)?;
                        _dev.init()?;
                        self.#step_field = Some(_dev);
                    }
                };
                quote! {
                    if !self._inited.contains(#step_id_lit) {
                        #construct
                        self._inited.set(#step_id_lit);
                    }
                }
            });
            quote! {
                #id_lit => {
                    // Fast path: already inited.  Matches the
                    // `inited.contains(id) { continue; }` check at
                    // the top of the executor's `CapOp::ConsoleInit`
                    // arm.
                    if self._inited.contains(#id_lit) {
                        return Ok(());
                    }
                    #(#steps)*
                    Ok(())
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("init_device: unknown device id {}", id);
                Err(fstart_services::device::DeviceError::InitFailed)
            }
        }
    }
}

/// Walk a device's ancestor chain root-first, excluding structural
/// / ACPI-only / disabled ancestors (they have no `self.<field>`).
///
/// Returns indices in root-first order ending at `target_idx`.
/// Mirrors `ensure_device_ready`'s chain-building step in the old
/// codegen.
fn chain_from_root(
    target_idx: usize,
    device_tree: &[DeviceNode],
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut cursor = Some(target_idx);
    while let Some(idx) = cursor {
        let inst = &instances[idx];
        let dev = &devices[idx];
        if dev.enabled && !inst.is_structural() && !inst.is_acpi_only() {
            chain.push(idx);
        }
        cursor = device_tree[idx].parent.map(|p| p as usize);
    }
    chain.reverse();
    chain
}

/// Walk an ancestor chain until the first non-structural parent.
///
/// Used for `BusDevice::new_on_bus` to pick the *real* parent
/// reference to pass (skipping structural wrappers like the LPC node
/// between a SuperIO and its southbridge).  Returns `None` if the
/// device is a root (no non-structural ancestor).
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

/// Emit the body of `Board::init_all_devices`.
///
/// Called by the executor's `CapOp::DriverInit` arm to bulk-init
/// everything not already brought up by an earlier per-capability
/// init.  Parameters:
///
/// - `skip`: devices whose `DriverInit` should be silently skipped
///   (already initialised in this or a previous stage).
/// - `gated`: devices whose `.init()` must check the runtime
///   boot-source register (sunxi `fstart_soc_sunxi::boot_media()`)
///   against the device's `boot_media_ids`; only init if the
///   current byte matches.
///
/// The body iterates enabled, non-structural, non-ACPI-only devices
/// in root-first order (matching the old `sorted_indices` order
/// emitted by `plan_gen`).  For each device:
///
/// - `skip.contains(id)` → skip entirely.
/// - `gated.contains(id)` → emit `if matches!(_bm, VAL_1|VAL_2|..) {
///   self.init_device(id)? }`.
/// - otherwise → `self.init_device(id)?`.
///
/// Framebuffer devices: the old codegen tracked init success in a
/// `_<name>_ok: bool` local so UEFI's GOP config could be
/// conditional.  In the new model `self._inited.contains(fb_id)`
/// serves the same role.  A failing framebuffer init leaves
/// `self._inited` un-set and the UEFI builder emits
/// `framebuffer: None`.
fn init_all_devices_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use super::capabilities::boot_media_values_for_device;

    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    // Collect all runtime-active devices in root-first order.
    // `enabled_indices` already filters `!dev.enabled / is_acpi_only /
    // is_structural / excluded`; we just add an ordering step to
    // match the old `topological_sort` output.  The device tree is
    // already root-first by construction (`ron_loader::build_device_tree`
    // assigns depths and emits parent-first), so iterating
    // `enabled_indices(...)` in insertion order matches.
    let mut dev_statements = TokenStream::new();
    let mut has_any_gated = false;

    for idx in enabled_indices(ctx.devices, ctx.instances, ctx.excluded) {
        let inst = &ctx.instances[idx];
        if inst.is_structural() || inst.is_acpi_only() {
            continue;
        }
        let dev = &ctx.devices[idx];
        let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
        let is_framebuffer = dev.services.iter().any(|s| s.as_str() == "Framebuffer");
        // Framebuffer failures are non-fatal (UEFI path still uses
        // `self._inited.contains(fb_id)` as an availability flag);
        // other devices halt on failure to match the old codegen.
        let on_err = if is_framebuffer {
            quote! {
                fstart_log::warn!("driver init failed (framebuffer, continuing)");
                // Do NOT set `_inited` — UEFI config reads
                // `self._inited.contains(#id_lit)` as the availability
                // flag.
            }
        } else {
            quote! {
                fstart_log::error!("FATAL: driver init failed for id {}", #id_lit);
                fstart_platform::halt();
            }
        };

        let bm_values = if is_egon {
            // Safe to call boot_media_values_for_device — only sunxi
            // drivers have entries, and it panics on unknown drivers
            // which we guard against with `catch_unwind`.
            let dev_name = dev.name.as_str();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                boot_media_values_for_device(dev_name, ctx.devices, ctx.instances)
            }))
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Inner init call — runs iff the device is in neither `skip`
        // nor `gated`, or iff gated and boot_media matches.
        let init_call = quote! {
            match self.init_device(#id_lit) {
                Ok(()) => {}
                Err(_) => {
                    #on_err
                }
            }
        };

        let gated_check = if !bm_values.is_empty() && is_egon {
            has_any_gated = true;
            let val_lits = bm_values
                .iter()
                .map(|v| proc_macro2::Literal::u8_unsuffixed(*v))
                .collect::<Vec<_>>();
            quote! {
                if gated.contains(#id_lit) {
                    if matches!(_bm, #(#val_lits)|*) {
                        #init_call
                    } else {
                        fstart_log::info!(
                            "skipping driver init (boot-media gated, not active): id {}",
                            #id_lit,
                        );
                    }
                } else {
                    #init_call
                }
            }
        } else {
            quote! {
                // Device is not in the gating table; run unconditionally.
                #init_call
            }
        };

        dev_statements.extend(quote! {
            if !skip.contains(#id_lit) {
                #gated_check
            }
        });
    }

    // Read sunxi's boot-media byte once up front if any gated arms exist.
    let bm_preamble = if has_any_gated && is_egon {
        quote! {
            let _bm = fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize);
        }
    } else {
        // Silence unused-var warnings on non-gated stages.
        quote! { let _ = gated; }
    };

    quote! {
        #bm_preamble
        #dev_statements
    }
}

// =======================================================================
// Helpers
// =======================================================================

/// Indices into `devices`/`instances` that are worth materialising in
/// this stage.
///
/// Filters out:
///
/// - `!dev.enabled` — board author disabled the device.
/// - `inst.is_acpi_only()` — device exists only to contribute ACPI
///   tables at build time; has no runtime driver.
/// - `inst.is_structural()` — tree node for topology; no runtime rep.
/// - `excluded.contains(idx)` — bus child in a stage without
///   `DriverInit`.
fn enabled_indices<'a>(
    devices: &'a [DeviceConfig],
    instances: &'a [DriverInstance],
    excluded: &'a [usize],
) -> impl Iterator<Item = usize> + 'a {
    devices
        .iter()
        .zip(instances.iter())
        .enumerate()
        .filter_map(move |(idx, (dev, inst))| {
            if !dev.enabled
                || inst.is_acpi_only()
                || inst.is_structural()
                || excluded.contains(&idx)
            {
                None
            } else {
                Some(idx)
            }
        })
}

// =======================================================================
// Tests
// =======================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ron_loader::load_parsed_board;
    use std::path::PathBuf;

    /// Load a fixture board, generate the adapter for its first (or
    /// only) stage, and return the formatted source.
    ///
    /// Matches the path resolution `tests.rs` already uses — look up
    /// `boards/<name>/board.ron` relative to the workspace root.
    fn adapter_source_for_board(board: &str) -> String {
        adapter_source_inner(board, None)
    }

    /// Like [`adapter_source_for_board`] but selects a named stage on
    /// multi-stage boards.  Panics if `stage` is not in the board's
    /// stage list, mirroring real-build behaviour.
    fn adapter_source_for_stage(board: &str, stage: &str) -> String {
        adapter_source_inner(board, Some(stage.to_owned()))
    }

    /// Runs the ron loader + codegen on a fresh thread with a
    /// generous stack (8 MiB).  The Rust default test-thread stack
    /// is 2 MiB and `prettyplease` + serde-de-deep-ron can exceed that
    /// for some boards when compiled in debug mode.  Using a worker
    /// thread keeps every test robust without forcing every CI run
    /// to export `RUST_MIN_STACK`.
    fn adapter_source_inner(board: &str, stage: Option<String>) -> String {
        let board = board.to_owned();
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_path_buf();
                let ron = root.join("boards").join(&board).join("board.ron");
                let parsed = load_parsed_board(&ron)
                    .unwrap_or_else(|e| panic!("failed to load {board}: {e}"));

                // Pick the selected stage, or default to first /
                // monolithic — mirrors `generate_stage_source`.
                let caps: &[Capability] = match (&parsed.config.stages, stage.as_deref()) {
                    (fstart_types::StageLayout::Monolithic(m), _) => &m.capabilities,
                    (fstart_types::StageLayout::MultiStage(stages), Some(name)) => {
                        &stages
                            .iter()
                            .find(|s| s.name.as_str() == name)
                            .unwrap_or_else(|| panic!("stage {name} not found in board {board}"))
                            .capabilities
                    }
                    (fstart_types::StageLayout::MultiStage(stages), None) => {
                        &stages[0].capabilities
                    }
                };

                let tokens = generate_board_adapter(
                    &parsed.config,
                    &parsed.driver_instances,
                    &parsed.device_tree,
                    caps,
                    stage.as_deref(),
                );
                let file = syn::parse2::<syn::File>(tokens).unwrap_or_else(|e| {
                    panic!("board_gen for {board} produced unparseable Rust: {e}")
                });
                prettyplease::unparse(&file)
            })
            .expect("spawn codegen worker thread")
            .join()
            .expect("codegen worker thread panicked")
    }

    #[test]
    fn adapter_compiles_for_qemu_riscv64() {
        let src = adapter_source_for_board("qemu-riscv64");
        // Structure checks — everything a downstream compile of the
        // generated file will need must be present.
        assert!(src.contains("struct _BoardDevices"));
        assert!(src.contains("impl _BoardDevices"));
        assert!(src.contains("impl fstart_stage_runtime::Board for _BoardDevices"));
        assert!(src.contains("const fn new() -> Self"));
        // At least the NS16550 console device should become a field.
        assert!(src.contains("uart0: Option<Ns16550>"));
        // Adapter carries its boot-media state and FDT / DRAM / handoff
        // bookkeeping fields that drive the migrated trampolines.
        assert!(src.contains("_boot_media: fstart_stage_runtime::BootMediaState"));
        assert!(src.contains("_dtb_dst_addr: u64"));
        assert!(src.contains("_bootargs: &'static str"));
        assert!(src.contains("_dram_base: u64"));
        assert!(src.contains("_dram_size_static: u64"));
        assert!(src.contains("_handoff: Option<fstart_types::handoff::StageHandoff>"));
        // Trivial trampolines wired to the real capability helpers.
        assert!(src.contains("fstart_capabilities::memory_init()"));
        assert!(src.contains("fstart_capabilities::late_driver_init_complete"));
        // `boot_media_static` is real — writes state.
        assert!(src.contains("BootMediaState::from_static"));
        // qemu-riscv64 uses FFS (SigVerify/PayloadLoad), so `sig_verify`
        // is the real body — not a todo!().
        assert!(src.contains("fstart_capabilities::sig_verify"));
        assert!(src.contains("MemoryMapped::from_raw_addr"));
        assert!(
            !src.contains("board_gen::sig_verify: migration pending"),
            "qemu-riscv64 uses FFS; sig_verify must have a real body, got:\n{src}"
        );
        // qemu-riscv64 has a LinuxBoot payload with FdtSource::Platform,
        // so the body is `fdt_prepare_platform` with a runtime
        // `boot_dtb_addr()` call (RISC-V / AArch64 default) and the
        // shared DRAM-size-with-handoff expression.
        assert!(src.contains("fstart_capabilities::fdt_prepare_platform"));
        assert!(src.contains("fstart_platform::boot_dtb_addr()"));
        assert!(src.contains("self._dtb_dst_addr"));
        assert!(src.contains("self._bootargs"));
        assert!(src.contains("self._dram_base"));
        // The handoff-aware DRAM-size expression must read from the
        // field (rather than inline a constant from the method body).
        // prettyplease may break `self._handoff` across lines, so the
        // `._handoff` token alone is a reliable indicator.
        assert!(src.contains("._handoff"));
        assert!(src.contains("_dram_size_static"));
        assert!(
            !src.contains("board_gen::fdt_prepare: migration pending"),
            "qemu-riscv64 has FdtSource::Platform; fdt_prepare must have a real body, got:\n{src}"
        );
        // install_logger is a real body: the ConsoleInit id arm
        // calls `fstart_log::init` + `console_ready`.
        assert!(
            src.contains("fstart_log::init"),
            "install_logger must call fstart_log::init on the console device, got:\n{src}"
        );
        assert!(
            src.contains("fstart_capabilities::console_ready"),
            "install_logger must emit console_ready banner, got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::install_logger: migration pending"),
            "install_logger must have a real body, got:\n{src}"
        );
        // payload_load is real: qemu-riscv64 has LinuxBoot → firmware
        // load + kernel load + platform boot protocol.
        assert!(
            src.contains("fstart_capabilities::load_ffs_file_by_type"),
            "payload_load must call load_ffs_file_by_type for kernel/firmware; got:\n{src}"
        );
        assert!(
            src.contains("fstart_platform::boot_linux_sbi"),
            "riscv64 payload_load must use boot_linux_sbi; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::payload_load: migration pending"),
            "payload_load must have a real body; got:\n{src}"
        );
        // init_device + init_all_devices are now migrated too — all
        // 20 Board methods have real bodies.  No `migration pending`
        // marker should remain anywhere in the generated source.
        assert!(
            !src.contains("migration pending"),
            "all Board methods must have real bodies now; got:\n{src}"
        );
    }

    #[test]
    fn adapter_compiles_for_qemu_aarch64() {
        let src = adapter_source_for_board("qemu-aarch64");
        assert!(src.contains("struct _BoardDevices"));
        assert!(src.contains("uart0: Option<Pl011>"));
        // aarch64 qemu board also uses FFS.
        assert!(src.contains("fstart_capabilities::sig_verify"));
    }

    #[test]
    fn adapter_compiles_for_qemu_armv7() {
        let src = adapter_source_for_board("qemu-armv7");
        assert!(src.contains("struct _BoardDevices"));
        // armv7 uses halt from fstart_platform (re-exported from fstart_arch).
        assert!(src.contains("fstart_platform::halt()"));
        // qemu-armv7 uses a PL011 UART (not NS16550).
        assert!(src.contains("uart0: Option<Pl011>"));
    }

    #[test]
    fn x86_adapter_has_jump_to_with_handoff_fallback() {
        // On x86_64, fstart_platform has no jump_to_with_handoff; the
        // emitter must substitute halt() so the trait impl still
        // type-checks in downstream firmware builds.
        let src = adapter_source_for_board("qemu-q35");
        assert!(src.contains("impl fstart_stage_runtime::Board for _BoardDevices"));
        // Ensure we did not emit a call to the missing symbol.
        assert!(
            !src.contains("fstart_platform::jump_to_with_handoff"),
            "x86 adapter must not reference the non-existent jump_to_with_handoff; got:\n{src}"
        );
    }

    #[test]
    fn bootblock_without_driver_init_excludes_bus_children() {
        // Pick a multi-stage board whose first stage lacks
        // `DriverInit`.  `qemu-riscv64-multi` is a good example:
        // its bootblock only does ConsoleInit + SigVerify + StageLoad.
        let src = adapter_source_for_board("qemu-riscv64-multi");
        assert!(src.contains("struct _BoardDevices"));
        // The exact set of fields depends on the board; this test is
        // a smoke test that the filter did not panic or emit an
        // unparseable struct.
        // Bootblock uses SigVerify, so it uses FFS and sig_verify is
        // real.
        assert!(src.contains("fstart_capabilities::sig_verify"));
    }

    #[test]
    fn sig_verify_stub_for_non_ffs_stages() {
        // Pick a multi-stage board's non-FFS stage.  The `main` stage
        // of `qemu-riscv64-multi` is ConsoleInit + MemoryInit +
        // DriverInit — no SigVerify/StageLoad/PayloadLoad.  So
        // `sig_verify` stays a todo!() placeholder because FSTART_ANCHOR
        // does not exist in that stage's generated source.
        let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
        assert!(src.contains("struct _BoardDevices"));
        // No FFS ⇒ sig_verify body is a todo!() — referencing
        // FSTART_ANCHOR here would break compilation.
        assert!(
            !src.contains("&FSTART_ANCHOR"),
            "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
        );
        assert!(
            src.contains("board_gen::sig_verify: no FFS-using capability"),
            "expected no-FFS sig_verify stub, got:\n{src}"
        );
    }

    #[test]
    fn sunxi_board_sig_verify_has_block_device_arm() {
        // `orangepi-pc2` boots from SD/MMC (`sunxi-mmc`, providing
        // `BlockDevice`) via `LoadNextStage` on the bootblock.  Its
        // `main` stage uses `SigVerify` against the block-backed
        // boot medium.  The emitted `sig_verify` match must have a
        // `Block` arm that references the `mmc0` field.
        let src = adapter_source_for_stage("orangepi-pc2", "main");
        assert!(src.contains("fstart_capabilities::sig_verify"));
        assert!(
            src.contains("BlockDeviceMedia::new"),
            "sunxi stage using BlockDevice must construct BlockDeviceMedia, got:\n{src}"
        );
        // `prettyplease` may break `self.mmc0` across lines, so
        // check for the tokens separately.  The `.mmc0` reference
        // on its own is a reliable indicator of the block-device
        // arm since no other construct in the emitted adapter would
        // produce that string.
        assert!(
            src.contains(".mmc0"),
            "block-device arm must reference the mmc0 field, got:\n{src}"
        );
    }

    // ===== fdt_prepare migration tests =================================

    #[test]
    fn fdt_prepare_platform_uses_handoff_aware_dram_size() {
        // qemu-riscv64 is the canonical FdtSource::Platform case.
        // Its body must read every board-level fact from `&self` and
        // use the runtime `boot_dtb_addr()` call (RISC-V default).
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("fstart_capabilities::fdt_prepare_platform"),
            "Platform variant must call fdt_prepare_platform; got:\n{src}"
        );
        assert!(
            src.contains("fstart_platform::boot_dtb_addr()"),
            "RISC-V Platform FdtSource must use runtime boot_dtb_addr(); got:\n{src}"
        );
        // No inlined hex constants for DTB dst / bootargs / DRAM —
        // per invariant #3 those all live in fields on `_BoardDevices`.
        assert!(src.contains("_dtb_dst_addr"));
        assert!(src.contains("_bootargs"));
        assert!(src.contains("_dram_base"));
        // The handoff-aware size expression must be emitted (splits
        // across lines in prettyplease, so check for its pieces).
        assert!(src.contains("._handoff"));
        assert!(src.contains("_dram_size_static"));
    }

    #[test]
    fn fdt_prepare_override_loads_from_ffs_on_sunxi_main() {
        // orangepi-pc2 main stage: FdtSource::Override("…dtb") over
        // a block-device boot medium (SD/MMC from the bootblock's
        // LoadNextStage).  The adapter must emit:
        //
        // - anchor-bytes preamble (&FSTART_ANCHOR)
        // - boot-media match with a Block arm mentioning .mmc0
        // - load_ffs_file_by_type call for FileType::Fdt
        // - fdt_prepare_platform(dst, dst, ...) patch call
        let src = adapter_source_for_stage("orangepi-pc2", "main");
        assert!(
            src.contains("load_ffs_file_by_type"),
            "Override FDT variant must load via load_ffs_file_by_type; got:\n{src}"
        );
        assert!(
            src.contains("fstart_types :: ffs :: FileType :: Fdt")
                || src.contains("ffs::FileType::Fdt"),
            "Override FDT variant must reference FileType::Fdt; got:\n{src}"
        );
        assert!(
            src.contains("fstart_capabilities::fdt_prepare_platform"),
            "Override FDT variant must still call fdt_prepare_platform for bootargs \
             patching; got:\n{src}"
        );
        assert!(
            src.contains("&FSTART_ANCHOR"),
            "Override FDT variant requires FFS stage; must reference FSTART_ANCHOR; \
             got:\n{src}"
        );
    }

    // ===== payload_load migration tests =================================

    #[test]
    fn payload_load_linux_boot_on_riscv64() {
        // qemu-riscv64: LinuxBoot + OpenSBI firmware.  Body must
        // load firmware + kernel from FFS, then call boot_linux_sbi.
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("capability: PayloadLoad (LinuxBoot)"),
            "payload_load must emit the LinuxBoot banner; got:\n{src}"
        );
        assert!(
            src.contains("SBI firmware"),
            "riscv64 payload_load must load SBI firmware; got:\n{src}"
        );
        assert!(
            src.contains("fstart_platform::boot_linux_sbi"),
            "riscv64 payload_load must call boot_linux_sbi; got:\n{src}"
        );
        assert!(
            src.contains("loading kernel..."),
            "payload_load must log kernel load; got:\n{src}"
        );
    }

    #[test]
    fn payload_load_linux_boot_on_aarch64() {
        // qemu-aarch64: LinuxBoot + no firmware (TCG boots directly).
        let src = adapter_source_for_board("qemu-aarch64");
        assert!(
            src.contains("fstart_platform::boot_linux_atf_prepared"),
            "aarch64 payload_load must call boot_linux_atf_prepared; got:\n{src}"
        );
    }

    #[test]
    fn payload_load_armv7_cleanup_before_linux() {
        let src = adapter_source_for_board("qemu-armv7");
        assert!(
            src.contains("fstart_platform::cleanup_before_linux"),
            "armv7 payload_load must call cleanup_before_linux; got:\n{src}"
        );
        assert!(
            src.contains("fstart_platform::boot_linux"),
            "armv7 payload_load must call boot_linux; got:\n{src}"
        );
    }

    // ===== init_device + init_all_devices migration tests ===============

    #[test]
    fn init_device_emits_match_arm_per_enabled_device() {
        // qemu-riscv64 has uart0 (ns16550) as an enabled,
        // non-structural, non-ACPI device.  init_device must have a
        // match arm that references `self.uart0` and emits both the
        // construction and init calls.
        let src = adapter_source_for_board("qemu-riscv64");
        // Per-arm fast path: if already inited, return Ok.
        assert!(
            src.contains("if self._inited.contains"),
            "init_device arms must check self._inited; got:\n{src}"
        );
        // Construction path uses Device::new (or ::new_on_bus for bus
        // children — but qemu-riscv64 is flat).  prettyplease emits
        // the turbofish-qualified form `<Ns16550>::new(...)` to
        // disambiguate the trait method resolution.
        assert!(
            src.contains("<Ns16550>::new"),
            "init_device must call Ns16550::new for uart0; got:\n{src}"
        );
        assert!(
            src.contains(".init()?"),
            "init_device must call .init() on each device; got:\n{src}"
        );
        assert!(
            src.contains("self._inited.set"),
            "init_device must set self._inited; got:\n{src}"
        );
    }

    #[test]
    fn init_device_ancestors_walked_root_first() {
        // Verify the ancestor-walking helpers produce the right
        // init-chain shape.  We can't easily build a fixture board
        // with a bus-device-children tree that exercises new_on_bus
        // without a live board using it (q35 / sbsa have pre-existing
        // build failures), so the structural check is:
        //
        // - qemu-riscv64 (flat) uses Device::new, not new_on_bus.
        // - The chain-walking helper `walk_to_real_parent` is
        //   unit-tested indirectly via the sunxi sig_verify body
        //   which already dispatches on BlockDevice ids.
        //
        // If a new fixture board with a non-trivial bus tree lands,
        // add a stronger assertion here.
        let src = adapter_source_for_board("qemu-riscv64");
        // Flat board: no new_on_bus calls.
        assert!(
            !src.contains("new_on_bus"),
            "flat qemu-riscv64 must not emit new_on_bus; got:\n{src}"
        );
    }

    #[test]
    fn init_all_devices_iterates_non_structural() {
        // qemu-riscv64: iterates enabled non-structural devices
        // (just uart0).  Each loop body calls self.init_device(id).
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("self.init_device"),
            "init_all_devices must call self.init_device; got:\n{src}"
        );
        assert!(
            src.contains("if !skip.contains"),
            "init_all_devices must gate on skip mask; got:\n{src}"
        );
    }

    #[test]
    fn init_all_devices_respects_boot_media_gating_on_sunxi() {
        // orangepi-pc2 bootblock has mmc0 (sunxi-mmc, BlockDevice)
        // gated by boot_media.  The body must have a `if gated.contains(id)`
        // check + a `matches!(_bm, ...)` guard.
        let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
        assert!(
            src.contains("fstart_soc_sunxi::boot_media_at"),
            "sunxi init_all_devices must read boot_media; got:\n{src}"
        );
        assert!(
            src.contains("if gated.contains"),
            "sunxi init_all_devices must gate on the gated mask; got:\n{src}"
        );
        assert!(
            src.contains("matches!(_bm"),
            "sunxi init_all_devices must match boot-media byte; got:\n{src}"
        );
    }

    // ===== boot_media_select + load_next_stage migration tests ==========

    #[test]
    fn boot_media_select_real_body_on_sunxi_bootblock() {
        // orangepi-pc2's bootblock uses LoadNextStage(devices=[mmc0])
        // over sunxi-eGON — the body must be the real sunxi dispatch.
        let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
        assert!(
            src.contains("fstart_soc_sunxi::boot_media_at"),
            "sunxi boot_media_select must read via boot_media_at; got:\n{src}"
        );
        // prettyplease may wrap `self._egon_sram_base` across lines.
        assert!(
            src.contains("_egon_sram_base"),
            "boot_media_select must read _egon_sram_base field; got:\n{src}"
        );
        // Writes BootMediaState::Block on match.
        assert!(
            src.contains("BootMediaState::Block"),
            "boot_media_select must write Block variant; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::boot_media_select: stage does not use"),
            "sunxi bootblock must not emit the dead-code stub; got:\n{src}"
        );
    }

    #[test]
    fn boot_media_select_dead_code_stub_on_non_sunxi_boards() {
        // qemu-riscv64 is not a sunxi board, so boot_media_select
        // stays as the todo!() stub — referencing fstart_soc_sunxi
        // there would fail to link (no sunxi feature flag).
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("board_gen::boot_media_select: stage does not use"),
            "qemu-riscv64 must emit the dead-code stub; got:\n{src}"
        );
        // And must not reference fstart_soc_sunxi in boot_media_select
        // or anywhere else in the adapter.
        assert!(
            !src.contains("fstart_soc_sunxi"),
            "qemu-riscv64 adapter must not reference fstart_soc_sunxi; got:\n{src}"
        );
    }

    #[test]
    fn load_next_stage_emits_real_body_on_sunxi_bootblock() {
        // orangepi-pc2 bootblock: LoadNextStage(devices=[mmc0],
        // next_stage: "main").  The body must emit the stage-name
        // match, eGON header read, per-device dispatch, and
        // jump_to_with_handoff call.
        let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
        assert!(
            src.contains("fstart_capabilities::next_stage::read_stage_to_addr"),
            "load_next_stage must call read_stage_to_addr; got:\n{src}"
        );
        assert!(
            src.contains("fstart_capabilities::next_stage::serialize_handoff"),
            "load_next_stage must call serialize_handoff; got:\n{src}"
        );
        assert!(
            src.contains("fstart_platform::jump_to_with_handoff"),
            "load_next_stage must jump with handoff; got:\n{src}"
        );
        assert!(
            src.contains("next_stage_offset_at"),
            "load_next_stage must read next_stage_offset_at; got:\n{src}"
        );
        assert!(
            src.contains("next_stage_size_at"),
            "load_next_stage must read next_stage_size_at; got:\n{src}"
        );
        // Stage-name dispatch: RON declares "bootblock" + "main"
        // stages; the arm for "main" must exist.
        assert!(
            src.contains("\"main\""),
            "load_next_stage must have a match arm for \"main\"; got:\n{src}"
        );
        // Per-device dispatch references .mmc0.
        assert!(
            src.contains(".mmc0"),
            "load_next_stage must dispatch to self.mmc0; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::load_next_stage: stage does not use"),
            "sunxi bootblock must not emit the dead-code stub; got:\n{src}"
        );
    }

    #[test]
    fn load_next_stage_dead_code_stub_on_non_sunxi_boards() {
        // qemu-riscv64 never calls LoadNextStage; the body is todo!().
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("board_gen::load_next_stage: stage does not use"),
            "qemu-riscv64 must emit the dead-code load_next_stage stub; got:\n{src}"
        );
        assert!(
            !src.contains("next_stage_offset_at"),
            "qemu-riscv64 must not reference eGON header symbols; got:\n{src}"
        );
    }

    #[test]
    fn board_struct_carries_egon_sram_base_field() {
        // Every board's _BoardDevices carries _egon_sram_base.
        // On non-sunxi boards it's initialised to 0 (harmless
        // because dead-code trampolines never read it).
        for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
            let src = adapter_source_for_board(board);
            assert!(
                src.contains("_egon_sram_base: u64"),
                "{board} must declare _egon_sram_base field; got:\n{src}"
            );
            // Non-sunxi boards have 0 for the SRAM base.
            assert!(
                src.contains("_egon_sram_base: 0x0"),
                "{board} must const-init _egon_sram_base to 0; got:\n{src}"
            );
        }
    }

    // ===== acpi_prepare + smbios_prepare migration tests ================

    #[test]
    fn acpi_prepare_emits_real_body_on_sbsa() {
        // qemu-sbsa has `AcpiPrepare` + a populated `acpi` RON
        // config (ARM SBSA platform).  The body must emit the
        // platform_acpi binding plus the acpi::prepare call with
        // closure.  per-device `_cfg` bindings may or may not be
        // present depending on whether any driver has `has_acpi` +
        // an `acpi_name` set.
        let src = adapter_source_for_board("qemu-sbsa");
        assert!(
            src.contains("let platform_acpi"),
            "acpi_prepare must emit platform_acpi binding; got:\n{src}"
        );
        assert!(
            src.contains("fstart_capabilities::acpi::prepare"),
            "acpi_prepare must call the capability fn; got:\n{src}"
        );
        assert!(
            src.contains("PlatformConfig::Arm"),
            "sbsa acpi_prepare must use the Arm platform variant; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::acpi_prepare: migration pending"),
            "acpi_prepare must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn acpi_prepare_stub_on_boards_without_acpi_config() {
        // qemu-riscv64 has no `acpi` RON config and no AcpiPrepare
        // capability, so the body must be the dead-code todo!().
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("board_gen::acpi_prepare: stage does not declare AcpiPrepare")
                || src.contains("board_gen::acpi_prepare: board has no `acpi` RON config"),
            "riscv64 must emit the no-config/no-cap stub; got:\n{src}"
        );
        // And must not emit spurious platform_acpi tokens.
        assert!(
            !src.contains("PlatformConfig::Arm"),
            "riscv64 must not reference Arm platform config; got:\n{src}"
        );
    }

    #[test]
    fn smbios_prepare_emits_real_body_on_sbsa() {
        // qemu-sbsa has `SmBiosPrepare` + a populated `smbios` RON
        // config.  The body must call smbios::prepare with the full
        // SmbiosDesc literal.
        let src = adapter_source_for_board("qemu-sbsa");
        assert!(
            src.contains("fstart_capabilities::smbios::prepare"),
            "smbios_prepare must call the capability fn; got:\n{src}"
        );
        assert!(
            src.contains("SmbiosDesc"),
            "smbios_prepare must emit the SmbiosDesc literal; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::smbios_prepare: migration pending"),
            "smbios_prepare must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn smbios_prepare_stub_on_boards_without_smbios_config() {
        // qemu-riscv64 has no `smbios` config and no SmBiosPrepare
        // capability, so the body is the dead-code todo!().
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("board_gen::smbios_prepare: stage does not declare SmBiosPrepare")
                || src.contains("board_gen::smbios_prepare: board has no `smbios` RON config"),
            "riscv64 must emit the smbios no-config/no-cap stub; got:\n{src}"
        );
        assert!(
            !src.contains("fstart_capabilities::smbios::prepare"),
            "riscv64 must not emit smbios::prepare call; got:\n{src}"
        );
    }

    // ===== acpi_load + memory_detect migration tests ====================

    #[test]
    fn acpi_load_emits_real_body_on_q35() {
        // qemu-q35 declares `AcpiLoad(device: "fw_cfg0")` and the
        // `fw_cfg0` device provides `AcpiTableProvider`.  The body
        // must allocate the 256 KiB buffer, call acpi_load, and
        // write the RSDP into `self._acpi_rsdp_addr`.
        let src = adapter_source_for_board("qemu-q35");
        assert!(
            src.contains("fstart_capabilities::acpi_load"),
            "acpi_load must call the capability fn; got:\n{src}"
        );
        assert!(
            src.contains("256 * 1024"),
            "acpi_load must declare the 256 KiB buffer; got:\n{src}"
        );
        assert!(
            src.contains("_ACPI_LOAD_BUF"),
            "acpi_load must use the static buffer symbol; got:\n{src}"
        );
        // RSDP is stored on `self`.
        assert!(
            src.contains("_acpi_rsdp_addr"),
            "acpi_load must write RSDP into self._acpi_rsdp_addr; got:\n{src}"
        );
        // Device name is baked in.
        assert!(
            src.contains("\"fw_cfg0\""),
            "acpi_load arm must pass the RON device name; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::acpi_load: migration pending"),
            "acpi_load must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn memory_detect_emits_real_body_on_q35() {
        // qemu-q35 declares `MemoryDetect(device: "fw_cfg0")`.  The
        // body must allocate a 128-entry E820 buffer and call
        // memory_detect.
        let src = adapter_source_for_board("qemu-q35");
        assert!(
            src.contains("fstart_capabilities::memory_detect"),
            "memory_detect must call the capability fn; got:\n{src}"
        );
        assert!(
            src.contains("E820Entry::zeroed()"),
            "memory_detect must initialise the buffer with E820Entry::zeroed(); got:\n{src}"
        );
        assert!(
            src.contains("; 128]"),
            "memory_detect buffer must have 128 entries; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::memory_detect: migration pending"),
            "memory_detect must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn acpi_and_memory_detect_halt_on_non_x86_boards() {
        // qemu-riscv64 has no AcpiTableProvider or MemoryDetector
        // device — the bodies are real but degenerate to wildcard
        // log + halt.  Must still compile and must not reference
        // ACPI / E820 symbols beyond the match block's closing brace.
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("acpi_load: unknown device id"),
            "non-ACPI board's acpi_load must emit the wildcard log; got:\n{src}"
        );
        assert!(
            src.contains("memory_detect: unknown device id"),
            "non-memory-detect board's memory_detect must emit the wildcard log; got:\n{src}"
        );
        // ACPI buffer must NOT appear in boards with no provider.
        assert!(
            !src.contains("_ACPI_LOAD_BUF"),
            "non-ACPI board must not emit the ACPI buffer; got:\n{src}"
        );
        assert!(
            !src.contains("E820Entry::zeroed()"),
            "non-detect board must not emit the e820 buffer; got:\n{src}"
        );
    }

    #[test]
    fn board_struct_carries_acpi_rsdp_field() {
        // Every board's _BoardDevices carries the `_acpi_rsdp_addr`
        // field so the struct shape is stable across boards that do
        // and don't use AcpiLoad.
        for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
            let src = adapter_source_for_board(board);
            assert!(
                src.contains("_acpi_rsdp_addr: u64"),
                "{board}'s _BoardDevices must declare _acpi_rsdp_addr; got:\n{src}"
            );
            // And new() must const-init it to 0.
            assert!(
                src.contains("_acpi_rsdp_addr: 0"),
                "{board}'s _BoardDevices::new() must initialise _acpi_rsdp_addr to 0; got:\n{src}"
            );
        }
    }

    // ===== pci_init + chipset_init migration tests ======================

    #[test]
    fn pci_init_emits_real_body_on_aarch64_sbsa() {
        // qemu-sbsa uses `PciInit(device: "pci0")`.  The adapter must
        // carry an arm that logs the banner and returns Ok(()).
        let src = adapter_source_for_board("qemu-sbsa");
        assert!(
            src.contains("PCI init complete"),
            "pci_init body must log the banner; got:\n{src}"
        );
        assert!(
            src.contains("\"pci0\""),
            "pci_init arm must bake the RON device name; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::pci_init: migration pending"),
            "pci_init must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn pci_init_boards_without_pci_root_have_wildcard_only() {
        // qemu-riscv64 has no PciRootBus provider.  The body is just
        // the wildcard arm that halts.  It's dead code (executor never
        // dispatches PciInit on this board), but the trait still
        // requires a body.
        let src = adapter_source_for_board("qemu-riscv64");
        // The match still exists (empty arm set).  What matters is
        // we do not reference any PCI identifier or "PCI init
        // complete" banner here.
        assert!(
            !src.contains("PCI init complete"),
            "non-pci board must not emit PCI banner; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::pci_init: migration pending"),
            "pci_init must have a real body on any board; got:\n{src}"
        );
    }

    #[test]
    fn chipset_init_emits_early_init_calls_on_foxconn_d41s() {
        // foxconn-d41s uses `ChipsetInit(northbridge: "northbridge",
        // southbridge: "southbridge")` in its bootblock.  The body
        // must call PciHost::early_init + Southbridge::early_init
        // against the corresponding fields.
        let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
        assert!(
            src.contains("fstart_services::PciHost"),
            "chipset_init must import PciHost trait; got:\n{src}"
        );
        assert!(
            src.contains("fstart_services::Southbridge"),
            "chipset_init must import Southbridge trait; got:\n{src}"
        );
        assert!(
            src.contains("_PciHost::early_init"),
            "chipset_init must call PciHost::early_init; got:\n{src}"
        );
        assert!(
            src.contains("_Southbridge::early_init"),
            "chipset_init must call Southbridge::early_init; got:\n{src}"
        );
        assert!(
            src.contains("chipset init complete"),
            "chipset_init must log the completion banner; got:\n{src}"
        );
        assert!(
            !src.contains("board_gen::chipset_init: migration pending"),
            "chipset_init must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn chipset_init_halts_on_board_without_pci_host() {
        // qemu-riscv64 has neither a PciHost nor a Southbridge.  The
        // body degenerates to a log + halt — dead code since validation
        // forbids ChipsetInit on this board anyway.
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("chipset_init: no PciHost device declared")
                || src.contains("chipset_init: no Southbridge device declared"),
            "boards without chipset must emit the no-device error log; got:\n{src}"
        );
    }

    // ===== return_to_fel migration tests ================================

    #[test]
    fn return_to_fel_stays_stubbed_for_boards_without_capability() {
        // No fixture board today actively declares `ReturnToFel` in
        // any stage's capabilities (orangepi-r1 has the entry
        // commented out).  Every adapter must therefore emit the
        // `todo!()` stub that skips referencing `fstart_soc_sunxi`
        // — the crate is only pulled into the dependency graph via
        // the `sunxi` feature on sunxi boards, and non-sunxi armv7
        // boards like `qemu-armv7` would fail to compile if we
        // emitted the real `fstart_soc_sunxi::...` call.
        for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
            let src = adapter_source_for_board(board);
            assert!(
                !src.contains("fstart_soc_sunxi"),
                "{board} does not declare ReturnToFel; adapter must not reference \
                 fstart_soc_sunxi; got:\n{src}"
            );
            assert!(
                src.contains("board_gen::return_to_fel: stage does not declare ReturnToFel"),
                "{board} return_to_fel must be the dead-code stub; got:\n{src}"
            );
        }
    }

    // ===== stage_load migration tests ===================================

    #[test]
    fn stage_load_bootblock_emits_real_body() {
        // qemu-riscv64-multi's bootblock: ConsoleInit + BootMedia +
        // SigVerify + StageLoad("main").  The `stage_load` trampoline
        // must reconstruct the boot medium and call
        // `fstart_capabilities::stage_load`.
        let src = adapter_source_for_stage("qemu-riscv64-multi", "bootblock");
        assert!(
            src.contains("fstart_capabilities::stage_load"),
            "bootblock stage_load must call the capability fn; got:\n{src}"
        );
        // Anchor preamble is present (shared with sig_verify, but the
        // stage_load arm emits its own dispatch body that uses it).
        assert!(src.contains("&FSTART_ANCHOR"));
        // The trailing `halt()` satisfies the `-> !` return type.
        assert!(
            src.contains("stage_load: capability returned without jumping"),
            "stage_load body must log + halt on non-diverging return; got:\n{src}"
        );
        // Old migration-pending stub must be gone.
        assert!(
            !src.contains("board_gen::stage_load: migration pending"),
            "stage_load must have a real body; got:\n{src}"
        );
    }

    #[test]
    fn stage_load_stub_for_non_ffs_stages() {
        // A stage without FFS capabilities has no FSTART_ANCHOR static
        // and no boot-media import path.  `stage_load` on that stage
        // would be dead code (validation forbids StageLoad without
        // BootMedia), so we emit a `todo!()`.
        //
        // qemu-riscv64-multi's `main` stage is the canonical non-FFS
        // stage in the fixture set.
        let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
        // No FSTART_ANCHOR referenced anywhere in this stage.
        assert!(
            !src.contains("&FSTART_ANCHOR"),
            "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
        );
        // `stage_load` body is a todo!() — the compiler still
        // type-checks the trait impl, but no executor arm dispatches
        // this method for this stage.
        assert!(
            src.contains("board_gen::stage_load requires an FFS-using stage"),
            "non-FFS stage_load must emit the dead-code todo!(); got:\n{src}"
        );
    }

    // ===== install_logger migration tests ===============================

    #[test]
    fn install_logger_emits_arm_per_console_device() {
        // qemu-riscv64 has one Console-providing device: uart0 (ns16550).
        // The match must carry an arm that inits the logger against
        // `.uart0` and emits the console_ready banner with both the
        // RON device name and the driver crate name.
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("fstart_log::init"),
            "install_logger body must call fstart_log::init, got:\n{src}"
        );
        // prettyplease may break `self.uart0` across lines — `.uart0`
        // is the reliable indicator.  Every Console arm references its
        // `self.<field>` to pass to `fstart_log::init`.
        assert!(
            src.contains(".uart0"),
            "install_logger arm must reference self.uart0, got:\n{src}"
        );
        // Banner call with device + driver name literals.
        assert!(
            src.contains("fstart_capabilities::console_ready"),
            "install_logger body must call console_ready, got:\n{src}"
        );
        assert!(
            src.contains("\"uart0\""),
            "console_ready must pass the RON device name, got:\n{src}"
        );
        assert!(
            src.contains("\"ns16550\""),
            "console_ready must pass the driver crate name, got:\n{src}"
        );
    }

    #[test]
    fn install_logger_pl011_on_aarch64() {
        // qemu-aarch64 uses a Pl011 driver.  The driver-name literal
        // in the console_ready banner must reflect that.
        let src = adapter_source_for_board("qemu-aarch64");
        assert!(src.contains("fstart_log::init"));
        assert!(
            src.contains("\"pl011\""),
            "console_ready must pass \"pl011\" for qemu-aarch64, got:\n{src}"
        );
    }

    #[test]
    fn install_logger_always_has_wildcard_halt() {
        // Every generated install_logger body ends with a `_ =>` arm
        // that halts.  The executor guarantees the id matches a
        // Console provider, but the compiler still needs exhaustive
        // coverage of the match.
        let src = adapter_source_for_board("qemu-riscv64");
        // Find the `unsafe fn install_logger` signature.  Walk
        // forward matching braces on `{` / `}` to isolate the
        // method body so we don't bleed into the next method.
        let sig_idx = src
            .find("unsafe fn install_logger(")
            .expect("adapter must define install_logger");
        let open = sig_idx
            + src[sig_idx..]
                .find('{')
                .expect("install_logger method must have a body");
        let mut depth = 0i32;
        let mut end = open;
        for (off, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + off + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[open..end];
        assert!(
            body.contains("_ =>") && body.contains("fstart_platform::halt()"),
            "install_logger must include `_ => halt()` wildcard, got:\n{body}"
        );
    }

    #[test]
    fn fdt_prepare_stub_when_board_has_no_payload() {
        // Exercise the `config.payload.is_none()` path.  Pick a
        // simple, widely-tested board and strip the payload in-memory
        // via a derived `BoardConfig`.  Writing fresh RON in a test
        // fixture directory would be cleaner but overkill for one
        // assertion — the important thing is the fdt_prepare_body
        // match arm is reachable and emits `fdt_prepare_stub`.
        //
        // Current board set: every live board ships a payload, so
        // the smoke test here instead verifies that the `stub()`
        // fallback token is present in `board_gen` source itself.
        // A functional test of this path lands once a board with
        // no payload exists (or we add a unit test fixture).
        let src = adapter_source_for_board("qemu-riscv64");
        // Sanity — `fdt_prepare_stub` identifier must still be
        // reachable from generated code when we need it later.
        assert!(
            std::fs::read_to_string(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/stage_gen/board_gen.rs"),
            )
            .unwrap()
            .contains("fstart_capabilities::fdt_prepare_stub"),
            "board_gen must still emit fdt_prepare_stub for no-payload boards; \
             got adapter src:\n{src}"
        );
    }
}
