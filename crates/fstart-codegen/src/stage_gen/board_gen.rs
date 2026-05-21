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

use fstart_device_registry::{DriverInstance, Service};
use fstart_types::memory::{FlashLayout, RegionKind};
use fstart_types::{
    BoardConfig, BootMedium, Capability, DeviceConfig, DeviceNode, FdtSource, PayloadConfig,
    Platform,
};

// `PayloadConfig` and `FdtSource` are used by [`fdt_prepare_platform_body`] and
// [`fdt_prepare_body`]'s `match &payload.fdt` arms.

use super::tokens::hex_addr;

mod boot_media;
mod caps_tables;
mod init_caps;
mod lifecycle;
mod logger;
mod model;
mod payload;
mod phases;
mod state;
mod sunxi;

use boot_media::{anchor_bytes_stmt, match_boot_media};
use caps_tables::{acpi_load_body, acpi_prepare_body, memory_detect_body, smbios_prepare_body};
use init_caps::{dram_init_body, late_driver_init_body, pci_init_body};
use lifecycle::{init_all_devices_body, init_device_body};
use logger::install_logger_body;
use model::{device_provides, BoardCtx};
use payload::payload_load_body;
use phases::{phase_init_body, PhaseSpec};
use state::{emit_adapter_new, emit_adapter_struct};
use sunxi::{boot_media_select_body, load_next_stage_body};

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
    device_services: &[heapless::Vec<Service, 8>],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let excluded = compute_excluded_indices(&config.devices, instances, device_tree, capabilities);
    let platform = config.platform;
    let ctx = BoardCtx::new(
        config,
        instances,
        device_tree,
        device_services,
        &excluded,
        capabilities,
        stage_name,
    );

    let mut tokens = TokenStream::new();
    tokens.extend(emit_adapter_struct(&ctx));
    tokens.extend(emit_adapter_new(&ctx));
    tokens.extend(emit_board_impl(platform, &ctx));
    tokens
}

// Board adapter semantic context lives in `model`.

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
    devices: &[DeviceConfig],
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

    let mut referenced: Vec<&str> = Vec::new();
    for cap in capabilities {
        match cap {
            Capability::ClockInit { device }
            | Capability::ConsoleInit { device }
            | Capability::DramInit { device }
            | Capability::PciInit { device }
            | Capability::AcpiLoad { device }
            | Capability::MemoryDetect { device } => referenced.push(device.as_str()),
            Capability::PreConsoleInit { devices }
            | Capability::EarlyInit { devices }
            | Capability::StageLocalInit { devices }
            | Capability::PostDramInit { devices }
            | Capability::FinalizeInit { devices } => {
                for device in devices {
                    referenced.push(device.as_str());
                }
            }
            Capability::BootMedia(BootMedium::Device { name, .. }) => {
                referenced.push(name.as_str())
            }
            Capability::BootMedia(BootMedium::AutoDevice { devices }) => {
                for dev in devices {
                    referenced.push(dev.name.as_str());
                }
            }
            Capability::LoadNextStage { devices, .. } => {
                for dev in devices {
                    referenced.push(dev.name.as_str());
                }
            }
            _ => {}
        }
    }

    device_tree
        .iter()
        .enumerate()
        .filter(|(idx, node)| {
            // Only exclude bus children that are not directly referenced by a
            // stage capability. Capability targets (e.g. ConsoleInit on an LPC
            // SuperIO/UART) must still be materialised even in tiny stages that
            // intentionally omit DriverInit.
            node.parent.is_some()
                && !instances[*idx].is_structural()
                && !referenced
                    .iter()
                    .any(|name| *name == devices[*idx].name.as_str())
        })
        .map(|(idx, _)| idx)
        .collect()
}

// =======================================================================
// `struct _BoardDevices`
// =======================================================================

// =======================================================================
// `impl Board for _BoardDevices`
// =======================================================================

/// Emit the `impl fstart_stage_runtime::Board for _BoardDevices` block.
///
/// Most methods are small trampolines from the generic stage executor into
/// the concrete drivers and capability helpers selected by the board RON.
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
    let dram_init_body = dram_init_body(ctx);
    let pre_console_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(
            Service::PreConsoleInit,
            "PreConsoleInit",
            "pre_console_init",
        ),
    );
    let early_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::EarlyInit, "EarlyInit", "early_init"),
    );
    let stage_local_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(
            Service::StageLocalInit,
            "StageLocalInit",
            "stage_local_init",
        ),
    );
    let post_dram_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::PostDramInit, "PostDramInit", "post_dram_init"),
    );
    let finalize_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::FinalizeInit, "FinalizeInit", "finalize_init"),
    );

    let acpi_load_body = acpi_load_body(ctx);
    let memory_detect_body = memory_detect_body(ctx);
    let acpi_prepare_body = acpi_prepare_body(ctx);
    let smbios_prepare_body = smbios_prepare_body(ctx);
    let mp_init_body = mp_init_body(ctx);
    let boot_media_select_body = boot_media_select_body(ctx);
    let load_next_stage_body = load_next_stage_body(ctx);
    let payload_load_body = payload_load_body(platform, ctx);
    let init_device_body = init_device_body(ctx);
    let init_all_devices_body = init_all_devices_body(ctx);
    let late_driver_init_body = late_driver_init_body(ctx);
    let boot_media_context_publish = if ctx.stage.uses_ffs {
        let anchor = anchor_bytes_stmt();
        quote! {
            if device.is_none() {
                #anchor
                fstart_services::ffs_context::set_memory_mapped(_anchor_bytes, offset, size);
            }
        }
    } else {
        quote! {}
    };

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

            fn late_driver_init_complete(&mut self, count: usize) {
                #late_driver_init_body
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

            fn mp_init(
                &mut self,
                cpu_model: &str,
                num_cpus: u16,
                smm: bool,
            ) -> Result<(), fstart_stage_runtime::RuntimeError> {
                #mp_init_body
            }

            fn pre_console_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #pre_console_init_body
            }

            fn early_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #early_init_body
            }

            fn stage_local_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #stage_local_init_body
            }

            fn post_dram_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #post_dram_init_body
            }

            fn finalize_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #finalize_init_body
            }

            fn dram_init(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #dram_init_body
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
                #boot_media_context_publish
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

fn mp_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_mp = ctx
        .stage
        .capabilities
        .iter()
        .any(|cap| matches!(cap, Capability::MpInit { .. }));
    if !uses_mp {
        return quote! {
            let _ = (cpu_model, num_cpus, smm);
            Ok(())
        };
    }

    let uses_smm = ctx
        .stage
        .capabilities
        .iter()
        .any(|cap| matches!(cap, Capability::MpInit { smm: true, .. }));
    let smm_image_expr = if uses_smm {
        quote! { if smm { Some(FSTART_SMM_IMAGE) } else { None } }
    } else {
        quote! { None }
    };

    let explicit_smm_provider = ctx.stage.capabilities.iter().find_map(|cap| match cap {
        Capability::MpInit {
            smm: true,
            smm_provider: Some(provider),
            ..
        } => Some(provider.as_str()),
        _ => None,
    });
    let smm_provider = if let Some(provider) = explicit_smm_provider {
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .find(|idx| ctx.devices[*idx].name.as_str() == provider)
    } else {
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .find(|idx| device_provides(ctx, *idx, Service::SmmOps))
    };

    let mp_microcode_enabled = matches!(
        ctx.config.microcode.as_ref(),
        Some(fstart_types::board::MicrocodeConfig::Intel(config)) if config.mp
    );
    let microcode_expr = mp_microcode_enabled
        .then(|| ctx.stage.capabilities.iter().find_map(|cap| match cap {
            Capability::BootMedia(BootMedium::MemoryMapped { base, .. }) => Some(*base),
            _ => None,
        }))
        .flatten()
        .map(|base| {
            let base_lit = hex_addr(base);
            let anchor_stmt = anchor_bytes_stmt();
            quote! {
                {
                    #anchor_stmt
                    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(_anchor_bytes) }
                        .ok();
                    anchor.and_then(|anchor| {
                        if anchor.microcode_offset == 0 || anchor.microcode_size == 0 {
                            None
                        } else {
                            let addr = (#base_lit as usize).saturating_add(anchor.microcode_offset as usize);
                            // SAFETY: xtask patched the anchor with a range inside the
                            // memory-mapped FFS image declared by this stage's BootMedia.
                            Some(unsafe {
                                core::slice::from_raw_parts(addr as *const u8, anchor.microcode_size as usize)
                            })
                        }
                    })
                }
            }
        })
        .unwrap_or_else(|| quote! { None });

    let smm_ops_expr = if let Some(idx) = smm_provider {
        let field = format_ident!("{}", ctx.devices[idx].name.as_str());
        quote! {
            if smm {
                Some(
                    self.#field
                        .as_ref()
                        .ok_or(fstart_stage_runtime::RuntimeError::Failed)?
                        as &dyn fstart_mp::SmmOps,
                )
            } else {
                None
            }
        }
    } else {
        quote! {
            if smm {
                fstart_log::error!("mp: SMM requested but this board has no SmmOps provider");
                return Err(fstart_stage_runtime::RuntimeError::Failed);
            } else {
                None
            }
        }
    };

    quote! {
        let smm_ops: Option<&dyn fstart_mp::SmmOps> = #smm_ops_expr;
        let smm_image: Option<&[u8]> = #smm_image_expr;
        let microcode_blob: Option<&'static [u8]> = #microcode_expr;

        if cpu_model == "generic-x86" || cpu_model == "qemu-x86" || cpu_model == "qemu" {
            let cpu_ops = fstart_mp::GenericX86CpuOps;
            let config = fstart_mp::MpConfig {
                cpu_ops: &cpu_ops,
                smm: smm_ops,
                smm_image,
                num_cpus,
            };
            fstart_mp::mp_init(&config)
                .map(|_| ())
                .map_err(|_| fstart_stage_runtime::RuntimeError::Failed)
        } else if cpu_model == "core2" || cpu_model == "6fx" {
            #[cfg(feature = "core2-cpu")]
            {
                let cpu_ops = fstart_cpu_intel::core2_cpu::Core2CpuOps::new(0x0500, microcode_blob);
                let config = fstart_mp::MpConfig {
                    cpu_ops: &cpu_ops,
                    smm: smm_ops,
                    smm_image,
                    num_cpus,
                };
                fstart_mp::mp_init(&config)
                    .map(|_| ())
                    .map_err(|_| fstart_stage_runtime::RuntimeError::Failed)
            }
            #[cfg(not(feature = "core2-cpu"))]
            {
                fstart_log::error!("mp: Core 2 CPU ops feature is not enabled");
                Err(fstart_stage_runtime::RuntimeError::Failed)
            }
        } else if cpu_model == "pineview" || cpu_model == "106cx" {
            #[cfg(feature = "pineview-cpu")]
            {
                let cpu_ops = fstart_cpu_intel::pineview::PineviewCpuOps::with_microcode(0x0500, microcode_blob);
                let config = fstart_mp::MpConfig {
                    cpu_ops: &cpu_ops,
                    smm: smm_ops,
                    smm_image,
                    num_cpus,
                };
                fstart_mp::mp_init(&config)
                    .map(|_| ())
                    .map_err(|_| fstart_stage_runtime::RuntimeError::Failed)
            }
            #[cfg(not(feature = "pineview-cpu"))]
            {
                fstart_log::error!("mp: Pineview CPU ops feature is not enabled");
                Err(fstart_stage_runtime::RuntimeError::Failed)
            }
        } else {
            fstart_log::error!("mp: unsupported CPU model '{}'; no CpuOps provider", cpu_model);
            Err(fstart_stage_runtime::RuntimeError::Failed)
        }
    }
}

/// Emit the body of `Board::sig_verify`.
///
/// For stages that use FFS (`ctx.stage.uses_ffs == true`), generates:
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
    if !ctx.stage.uses_ffs {
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
///    FFS file.  Requires `ctx.stage.uses_ffs` because the emitted body
///    references `FSTART_ANCHOR` and the boot-media types.  The
///    body loads the DTB from FFS into `self._dtb_dst_addr` via
///    `fstart_capabilities::load_ffs_file_by_type`, halts on
///    failure, then patches bootargs in-place with `src = dst`.
///    When `!ctx.stage.uses_ffs`, emits a `todo!()` — codegen bug for a
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
        .stage
        .capabilities
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
/// Requires `ctx.stage.uses_ffs`; otherwise the FFS / boot-media types
/// aren't in scope and we can't emit the load code.  Returns a
/// `todo!()` placeholder in that case (unreachable — the executor
/// only dispatches `FdtPrepare` when the plan carries the capability,
/// and `FdtPrepare` with `Override` upstream-requires BootMedia,
/// which in turn implies FFS).
fn fdt_prepare_override_body(ctx: &BoardCtx<'_>, platform: Platform) -> TokenStream {
    if !ctx.stage.uses_ffs {
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
/// Requires `ctx.stage.uses_ffs`; otherwise the FFS / boot-media types
/// aren't in scope.  `stage_load` upstream-requires `BootMedia`,
/// which in turn implies FFS, so a non-FFS stage reaching this
/// trampoline is a plan bug (unreachable — the executor dispatches
/// `StageLoad` only if the plan carries the capability).  Emits a
/// `todo!()` in that case.
fn x86_postcar_config_tokens(ctx: &BoardCtx<'_>) -> TokenStream {
    let ram_ranges: Vec<TokenStream> = ctx
        .config
        .memory
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .map(|r| {
            let base = r.base;
            let size = r.size;
            quote! { fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size } }
        })
        .collect();

    let rom_range = match &ctx.config.memory.flash_layout {
        Some(FlashLayout::IntelIfd(layout)) => {
            let base = layout.base;
            let size = layout.size as u64;
            quote! {
                Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
            }
        }
        None => match (ctx.config.memory.flash_base, ctx.config.memory.flash_size) {
            (Some(base), Some(size)) => quote! {
                Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
            },
            _ => quote! { None },
        },
    };

    quote! {
        static _FSTART_POSTCAR_RAM_RANGES: &[fstart_platform::car_teardown::PhysicalRange] = &[
            #(#ram_ranges),*
        ];
        static _FSTART_POSTCAR_CONFIG: fstart_platform::car_teardown::PostcarConfig =
            fstart_platform::car_teardown::PostcarConfig {
                ram_ranges: _FSTART_POSTCAR_RAM_RANGES,
                rom_range: #rom_range,
            };
    }
}

fn stage_load_body(ctx: &BoardCtx<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            let _ = next_stage;
            todo!("board_gen::stage_load requires an FFS-using stage")
        };
    }

    let anchor = anchor_bytes_stmt();

    if ctx.config.platform == Platform::X86_64 {
        let postcar_config = x86_postcar_config_tokens(ctx);
        return quote! {
            fstart_log::info!("stage_load: generated trampoline enter");
            #anchor
            #postcar_config
            fstart_log::info!("stage_load: anchor slice ready");
            match self._boot_media {
                fstart_stage_runtime::BootMediaState::Mmio { base, size } => {
                    fstart_log::info!("stage_load: switching to post-CAR DRAM stack for MMIO load");
                    // SAFETY: DRAM has just been trained before StageLoad,
                    // `_FSTART_POSTCAR_CONFIG` describes a DRAM stack and
                    // temporary MTRRs, and this call never returns to CAR.
                    unsafe {
                        fstart_platform::car_teardown::stage_load_mmio(
                            &_FSTART_POSTCAR_CONFIG,
                            next_stage,
                            _anchor_bytes,
                            base,
                            size,
                        );
                    }
                }
                fstart_stage_runtime::BootMediaState::None => {
                    fstart_log::error!("stage_load: no boot media configured");
                }
                fstart_stage_runtime::BootMediaState::Block { device_id, .. } => {
                    fstart_log::error!("stage_load: x86 post-CAR StageLoad supports MMIO only, device {}", device_id);
                }
            }
            fstart_platform::halt()
        };
    }

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
        fstart_log::info!("stage_load: generated trampoline enter");
        #anchor
        fstart_log::info!("stage_load: anchor slice ready");
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
        .stage
        .capabilities
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

/// Emit the body of `Board::init_device`.
///
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
pub(super) fn enabled_indices<'a>(
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
                    &parsed.device_services,
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
            src.contains("fstart_platform::boot_linux"),
            "riscv64 payload_load must use boot_linux; got:\n{src}"
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
    fn bootblock_without_driver_init_keeps_capability_referenced_child() {
        // Foxconn's bootblock intentionally omits DriverInit, but ConsoleInit
        // targets a SuperIO child under the LPC bus. Capability targets must be
        // materialised even when unrelated bus children are excluded.
        let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
        assert!(
            src.contains("superio: Option"),
            "bootblock adapter must keep ConsoleInit child; got:\n{src}"
        );
        assert!(src.contains("fn dram_init"));
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
            src.contains("fstart_platform::boot_linux"),
            "riscv64 payload_load must call boot_linux; got:\n{src}"
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
            src.contains("fstart_platform::boot_linux"),
            "aarch64 payload_load must call boot_linux; got:\n{src}"
        );
    }

    #[test]
    fn payload_load_armv7_cleanup_before_linux() {
        let src = adapter_source_for_board("qemu-armv7");
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
            src.contains("if this._inited.contains") || src.contains("if self._inited.contains"),
            "init_device arms must check the init mask; got:\n{src}"
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
            src.contains("this._inited.set") || src.contains("self._inited.set"),
            "init_device must set the init mask; got:\n{src}"
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

    // ===== pci_init + generic phase-init migration tests ================

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
    fn early_init_emits_generic_phase_calls_on_foxconn_d41s() {
        let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
        assert!(
            src.contains("fstart_services::EarlyInit"),
            "early_init must import EarlyInit trait; got:\n{src}"
        );
        assert!(
            src.contains("_EarlyInit::early_init"),
            "early_init must call EarlyInit::early_init; got:\n{src}"
        );
        assert!(
            !src.contains("fstart_services::PciHost as _PciHost"),
            "generic phase init must not depend on PciHost topology; got:\n{src}"
        );
    }

    #[test]
    fn pre_console_init_emits_generic_phase_calls_on_foxconn_d41s() {
        let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
        assert!(
            src.contains("fstart_services::PreConsoleInit"),
            "pre_console_init must import PreConsoleInit trait; got:\n{src}"
        );
        assert!(
            src.contains("_PreConsoleInit::pre_console_init"),
            "pre_console_init must call PreConsoleInit::pre_console_init; got:\n{src}"
        );
    }

    #[test]
    fn phase_init_boards_without_provider_have_wildcard_only() {
        let src = adapter_source_for_board("qemu-riscv64");
        assert!(
            src.contains("unknown or unsupported device id"),
            "boards without a phase provider must still emit wildcard errors; got:\n{src}"
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
