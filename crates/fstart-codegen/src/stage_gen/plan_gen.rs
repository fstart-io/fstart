//! Compile a [`ParsedBoard`] + stage selection into a
//! `fstart_stage_runtime::StagePlan` literal.
//!
//! Phase 1 of the stage-runtime/codegen split: the plan is emitted
//! alongside the existing `fstart_main()` body but not yet consumed.
//! The existing generator keeps doing its job; this module just makes
//! the resolved stage semantics available as data.
//!
//! The emitted shape mirrors the types in
//! `crates/fstart-stage-runtime/src/plan.rs`.  When that crate's types
//! change, this module must change with them — which is the whole
//! point of keeping plan construction in one place.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::{
    AutoBootDevice, BoardConfig, BootMedium, Capability, DeviceConfig, DeviceId, LoadDevice,
    StageLayout,
};

use super::capabilities::boot_media_values_for_device;
use super::tokens::hex_addr;

// =======================================================================
// Public entry point
// =======================================================================

/// Emit the `static PLAN: StagePlan = ...;` literal for a stage.
///
/// Called by `generate_stage_source` after validation.  The emitted
/// static is exposed with `#[no_mangle]` so later phases can wire it
/// up to `fstart_stage_runtime::run_stage` without codegen changes.
///
/// Phase 1 keeps the plan `#[allow(dead_code)]` — the existing
/// `fstart_main()` is still the code path that actually runs.
pub(super) fn generate_stage_plan(
    config: &BoardConfig,
    instances: &[DriverInstance],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let device_id = DeviceIdMap::new(&config.devices);
    let ctx = PlanCtx {
        ids: &device_id,
        devices: &config.devices,
        instances,
    };

    let stage_name_str = stage_name.unwrap_or("");
    let is_first_stage = is_first_stage(config, stage_name);
    let ends_with_jump = ends_with_jump(capabilities);

    // ---- capabilities → CapOp literals --------------------------------
    let cap_ops = capabilities.iter().map(|c| cap_to_capop_tokens(c, &ctx));
    let cap_ops_len = capabilities.len();
    let caps_ident = format_ident!("_FSTART_PLAN_CAPS");

    // ---- persistent_inited: devices a prior stage already handled ----
    let persistent = persistent_inited_ids(config, stage_name, &device_id);
    let persistent_lits = persistent.iter().map(|id| Literal::u8_unsuffixed(*id));
    let persistent_ident = format_ident!("_FSTART_PLAN_PERSISTENT");

    // ---- boot_media_gated: (DeviceId, &[u8]) pairs --------------------
    let gated = collect_boot_media_gated(capabilities, &config.devices, instances, &device_id);
    let gated_idents = (0..gated.len()).map(|i| format_ident!("_FSTART_PLAN_GATED_IDS_{i}"));
    let gated_values_ids = gated.iter().zip(gated_idents.clone()).map(|((_, v), id)| {
        let lits = v.iter().map(|b| Literal::u8_unsuffixed(*b));
        quote! { static #id: &[u8] = &[#(#lits),*]; }
    });
    let gated_entries = gated
        .iter()
        .zip(gated_idents)
        .map(|((dev_id, _), values_ident)| {
            let dev_lit = Literal::u8_unsuffixed(*dev_id);
            quote! { (#dev_lit, #values_ident) }
        });
    let gated_ident = format_ident!("_FSTART_PLAN_GATED");
    let gated_len = gated.len();

    // ---- all_devices: enabled + non-structural + non-acpi-only ------
    let all_devs = all_runtime_devices(&config.devices, instances, &device_id);
    let all_devs_lits = all_devs.iter().map(|id| Literal::u8_unsuffixed(*id));
    let all_devs_ident = format_ident!("_FSTART_PLAN_ALL_DEVICES");
    let all_devs_len = all_devs.len();

    quote! {
        // --- Auxiliary slices for the PLAN static. These are split out
        // to statics because StagePlan fields are `&'static [T]` and the
        // literal has to reference something with a stable address.

        #[allow(dead_code)]
        static #caps_ident: [fstart_stage_runtime::CapOp; #cap_ops_len] = [
            #(#cap_ops,)*
        ];

        #[allow(dead_code)]
        static #persistent_ident: &[fstart_types::DeviceId] = &[
            #(#persistent_lits,)*
        ];

        #(#gated_values_ids)*

        #[allow(dead_code)]
        static #gated_ident: [(fstart_types::DeviceId, &'static [u8]); #gated_len] = [
            #(#gated_entries,)*
        ];

        #[allow(dead_code)]
        static #all_devs_ident: [fstart_types::DeviceId; #all_devs_len] = [
            #(#all_devs_lits,)*
        ];

        /// Compiled stage plan.  Consumed by `fstart_stage_runtime::run_stage`
        /// via the codegen-emitted `fstart_main` shim (still pending — see
        /// `.opencode/plans/stage-runtime-codegen-split.md` §"Work breakdown").
        ///
        /// Module-local (no `#[no_mangle]`, no `pub`) so that a future
        /// multi-platform codegen can emit several named plans in the
        /// same stage binary (`STAGE_PLAN_ICH7`, `STAGE_PLAN_Q35`, ...)
        /// without symbol collisions.  See plan doc §Invariant 1.
        #[allow(dead_code)]
        static STAGE_PLAN: fstart_stage_runtime::StagePlan = fstart_stage_runtime::StagePlan {
            stage_name: #stage_name_str,
            is_first_stage: #is_first_stage,
            ends_with_jump: #ends_with_jump,
            caps: &#caps_ident,
            persistent_inited: #persistent_ident,
            boot_media_gated: &#gated_ident,
            all_devices: &#all_devs_ident,
        };
    }
}

// =======================================================================
// Plan emission context
// =======================================================================

/// Everything the capability-lowering helpers need, bundled together
/// so function signatures don't list three references every time.
struct PlanCtx<'a> {
    ids: &'a DeviceIdMap<'a>,
    devices: &'a [DeviceConfig],
    instances: &'a [DriverInstance],
}

// =======================================================================
// Device-name → DeviceId resolution
// =======================================================================

/// Map device names to stable `DeviceId`s.
///
/// Indices match the order of `config.devices` — i.e. the same order
/// everything else in `stage_gen` already uses.  `DeviceId` is `u8`;
/// more than 256 devices per board causes a `compile_error!` in the
/// emitted source.
struct DeviceIdMap<'a> {
    devices: &'a [DeviceConfig],
}

impl<'a> DeviceIdMap<'a> {
    fn new(devices: &'a [DeviceConfig]) -> Self {
        assert!(
            devices.len() <= 256,
            "more than 256 devices per board ({} present) — DeviceId is u8",
            devices.len()
        );
        Self { devices }
    }

    /// Returns the `DeviceId` for `name`, or `None` if not present.
    fn get(&self, name: &str) -> Option<DeviceId> {
        self.devices
            .iter()
            .position(|d| d.name.as_str() == name)
            .map(|i| i as DeviceId)
    }

    /// Emit a `DeviceId` literal.  Panics with a clear codegen-time
    /// message if the name is missing — validation upstream should
    /// already have caught this with `compile_error!`, so reaching here
    /// indicates a bug in the generator itself.
    fn lit(&self, name: &str, context: &str) -> Literal {
        let id = self.get(name).unwrap_or_else(|| {
            panic!(
                "plan_gen {context}: device '{name}' not in board — validation should have rejected this"
            )
        });
        Literal::u8_unsuffixed(id)
    }
}

// =======================================================================
// Capability → CapOp token emission
// =======================================================================

fn cap_to_capop_tokens(cap: &Capability, ctx: &PlanCtx<'_>) -> TokenStream {
    use Capability as C;

    match cap {
        C::ClockInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ClockInit");
            quote! { fstart_stage_runtime::CapOp::ClockInit(#id) }
        }
        C::ConsoleInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ConsoleInit");
            quote! { fstart_stage_runtime::CapOp::ConsoleInit(#id) }
        }
        C::MemoryInit => quote! { fstart_stage_runtime::CapOp::MemoryInit },
        C::DramInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "DramInit");
            quote! { fstart_stage_runtime::CapOp::DramInit(#id) }
        }
        C::ChipsetInit {
            northbridge,
            southbridge,
        } => {
            let nb = ctx.ids.lit(northbridge.as_str(), "ChipsetInit.northbridge");
            let sb = ctx.ids.lit(southbridge.as_str(), "ChipsetInit.southbridge");
            quote! {
                fstart_stage_runtime::CapOp::ChipsetInit {
                    nb: #nb,
                    sb: #sb,
                }
            }
        }
        C::MpInit {
            cpu_model,
            num_cpus,
            smm,
        } => {
            let model_str = cpu_model.as_str();
            let nc = *num_cpus;
            quote! {
                fstart_stage_runtime::CapOp::MpInit {
                    cpu_model: #model_str,
                    num_cpus: #nc,
                    smm: #smm,
                }
            }
        }
        C::PciInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "PciInit");
            quote! { fstart_stage_runtime::CapOp::PciInit(#id) }
        }
        C::DriverInit => quote! { fstart_stage_runtime::CapOp::DriverInit },
        C::LateDriverInit => quote! { fstart_stage_runtime::CapOp::LateDriverInit },
        C::SigVerify => quote! { fstart_stage_runtime::CapOp::SigVerify },
        C::FdtPrepare => quote! { fstart_stage_runtime::CapOp::FdtPrepare },
        C::PayloadLoad => quote! { fstart_stage_runtime::CapOp::PayloadLoad },
        C::StageLoad { next_stage } => {
            let name = next_stage.as_str();
            quote! {
                fstart_stage_runtime::CapOp::StageLoad { next_stage: #name }
            }
        }
        C::AcpiPrepare => quote! { fstart_stage_runtime::CapOp::AcpiPrepare },
        C::SmBiosPrepare => quote! { fstart_stage_runtime::CapOp::SmBiosPrepare },
        C::AcpiLoad { device } => {
            let id = ctx.ids.lit(device.as_str(), "AcpiLoad");
            quote! { fstart_stage_runtime::CapOp::AcpiLoad(#id) }
        }
        C::MemoryDetect { device } => {
            let id = ctx.ids.lit(device.as_str(), "MemoryDetect");
            quote! { fstart_stage_runtime::CapOp::MemoryDetect(#id) }
        }
        C::ReturnToFel => quote! { fstart_stage_runtime::CapOp::ReturnToFel },

        C::BootMedia(medium) => boot_medium_to_capop(medium, ctx),

        C::LoadNextStage {
            devices: load_devs,
            next_stage,
        } => load_next_stage_to_capop(load_devs, next_stage.as_str(), ctx),
    }
}

fn boot_medium_to_capop(medium: &BootMedium, ctx: &PlanCtx<'_>) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { base, size, .. } => {
            // Memory-mapped flash isn't a device; the adapter reads the
            // offset/size from the CapOp and sets up a MemoryMapped
            // BootMedia accessor.  device=None is the marker for
            // "memory-mapped, not a block device".
            let base_lit = hex_addr(*base);
            let size_lit = hex_addr(*size);
            quote! {
                fstart_stage_runtime::CapOp::BootMediaStatic {
                    device: None,
                    offset: #base_lit,
                    size: #size_lit,
                }
            }
        }
        BootMedium::Device { name, offset, size } => {
            let id = ctx.ids.lit(name.as_str(), "BootMedia::Device");
            let off_lit = hex_addr(*offset);
            let sz_lit = hex_addr(*size);
            quote! {
                fstart_stage_runtime::CapOp::BootMediaStatic {
                    device: Some(#id),
                    offset: #off_lit,
                    size: #sz_lit,
                }
            }
        }
        BootMedium::AutoDevice {
            devices: candidates,
        } => auto_device_candidates_static(candidates.as_slice(), ctx),
    }
}

/// Emit a `CapOp::LoadNextStage { candidates, next_stage }` literal
/// wrapped in a block that hosts the `&'static` candidate table.
///
/// `BootMediaCandidate::size` is reported as `0` for LoadNextStage:
/// today's generator reads the actual size from the eGON header at
/// runtime, and the adapter's `load_next_stage` trampoline does the
/// same.  The `size` field in the plan serves `BootMediaAuto` only.
fn load_next_stage_to_capop(
    load_devs: &[LoadDevice],
    next_stage: &str,
    ctx: &PlanCtx<'_>,
) -> TokenStream {
    let candidates = load_devs
        .iter()
        .map(|ld| {
            let id = ctx.ids.lit(ld.name.as_str(), "LoadNextStage");
            let off_lit = hex_addr(ld.base_offset);
            let media_ids = media_ids_tokens(ld.name.as_str(), ctx);
            quote! {
                fstart_stage_runtime::BootMediaCandidate {
                    device: #id,
                    offset: #off_lit,
                    size: 0,
                    media_ids: #media_ids,
                }
            }
        })
        .collect::<Vec<_>>();
    let n = candidates.len();

    quote! {
        {
            static _LNS_CANDIDATES: [fstart_stage_runtime::BootMediaCandidate; #n] = [
                #(#candidates),*
            ];
            fstart_stage_runtime::CapOp::LoadNextStage {
                candidates: &_LNS_CANDIDATES,
                next_stage: #next_stage,
            }
        }
    }
}

/// Emit an inline `{ static X = ...; CapOp::BootMediaAuto { candidates: &X } }`
/// block.  Wrapping the static in a block scope means multiple
/// `BootMedia(AutoDevice)` entries in the same stage don't collide on
/// the static's name.
fn auto_device_candidates_static(candidates: &[AutoBootDevice], ctx: &PlanCtx<'_>) -> TokenStream {
    let n = candidates.len();
    let items = candidates.iter().map(|c| {
        let id = ctx.ids.lit(c.name.as_str(), "BootMedia::AutoDevice");
        let off = hex_addr(c.offset);
        let sz = hex_addr(c.size);
        let media_ids = media_ids_tokens(c.name.as_str(), ctx);
        quote! {
            fstart_stage_runtime::BootMediaCandidate {
                device: #id,
                offset: #off,
                size: #sz,
                media_ids: #media_ids,
            }
        }
    });
    quote! {
        {
            static _AUTO_CANDIDATES: [fstart_stage_runtime::BootMediaCandidate; #n] = [
                #(#items),*
            ];
            fstart_stage_runtime::CapOp::BootMediaAuto {
                candidates: &_AUTO_CANDIDATES,
            }
        }
    }
}

/// Produce a `&[u8]` literal of the SoC boot-source register values
/// that select the named device.  For devices where the runtime
/// doesn't have a known mapping yet (non-sunxi boards where boot-media
/// auto-select doesn't apply), this yields an empty slice — the
/// `boot_media_select` adapter method on those platforms picks by a
/// different rule.
fn media_ids_tokens(device_name: &str, ctx: &PlanCtx<'_>) -> TokenStream {
    // Today `boot_media_values_for_device` panics if the driver has
    // no known mapping.  We only call it for devices that already
    // passed `collect_boot_media_gated` or appear in an AutoDevice /
    // LoadNextStage candidate list, and those paths require the
    // sunxi boot-media mapping in the old codegen.  For the new
    // codegen we broaden the reach: catch the panic case and emit
    // an empty slice.
    //
    // This is pragmatic, not principled.  The fully-correct answer
    // is to add a generic `boot_source_values_for_device` that
    // returns `Option<Vec<u8>>` so non-sunxi platforms emit `None`
    // and the adapter reads its own boot-source register directly.
    // Deferred until an x86 board actually needs BootMediaAuto.
    let values = std::panic::catch_unwind(|| {
        boot_media_values_for_device(device_name, ctx.devices, ctx.instances)
    })
    .unwrap_or_default();
    let lits = values.iter().map(|b| Literal::u8_unsuffixed(*b));
    quote! { &[#(#lits),*] }
}

// =======================================================================
// Helpers: mirror existing stage_gen logic
// =======================================================================

fn is_first_stage(config: &BoardConfig, stage_name: Option<&str>) -> bool {
    match (&config.stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    }
}

fn ends_with_jump(capabilities: &[Capability]) -> bool {
    capabilities.last().is_some_and(|cap| {
        matches!(
            cap,
            Capability::StageLoad { .. }
                | Capability::PayloadLoad
                | Capability::LoadNextStage { .. }
                | Capability::ReturnToFel
        )
    })
}

/// Mirror of `stage_gen::previous_stages_inited_devices`, except it
/// emits `DeviceId` values instead of name strings.  The two
/// implementations will converge when the plan becomes the source of
/// truth (Phase 4).
fn persistent_inited_ids(
    config: &BoardConfig,
    stage_name: Option<&str>,
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    let stages = match &config.stages {
        StageLayout::MultiStage(stages) => stages,
        _ => return Vec::new(),
    };
    let Some(name) = stage_name else {
        return Vec::new();
    };
    let Some(our_idx) = stages.iter().position(|s| s.name.as_str() == name) else {
        return Vec::new();
    };
    if our_idx == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for stage in &stages[..our_idx] {
        for cap in &stage.capabilities {
            match cap {
                Capability::ClockInit { device } | Capability::DramInit { device } => {
                    if let Some(id) = ids.get(device.as_str()) {
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn collect_boot_media_gated(
    capabilities: &[Capability],
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    ids: &DeviceIdMap<'_>,
) -> Vec<(DeviceId, Vec<u8>)> {
    // The existing implementation only gates on armv7.  Here we keep
    // the plan platform-neutral — it lists what the board says to gate
    // on, regardless of whether the executor actually honours it.  The
    // executor does that filtering by looking at the board platform,
    // which arrives via the board adapter.
    let mut out: Vec<(DeviceId, Vec<u8>)> = Vec::new();
    for cap in capabilities {
        match cap {
            Capability::LoadNextStage {
                devices: load_devs, ..
            } if load_devs.len() > 1 => {
                for ld in load_devs.iter() {
                    if let Some(id) = ids.get(ld.name.as_str()) {
                        if out.iter().any(|(d, _)| *d == id) {
                            continue;
                        }
                        let vals =
                            boot_media_values_for_device(ld.name.as_str(), devices, instances);
                        out.push((id, vals));
                    }
                }
            }
            Capability::BootMedia(BootMedium::AutoDevice {
                devices: candidates,
            }) if candidates.len() > 1 => {
                for c in candidates.iter() {
                    if let Some(id) = ids.get(c.name.as_str()) {
                        if out.iter().any(|(d, _)| *d == id) {
                            continue;
                        }
                        let vals =
                            boot_media_values_for_device(c.name.as_str(), devices, instances);
                        out.push((id, vals));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Enabled, non-structural, non-ACPI-only devices in `config.devices`
/// order.  Matches what `generate_driver_init` iterates today.
fn all_runtime_devices(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    devices
        .iter()
        .zip(instances.iter())
        .filter_map(|(dev, inst)| {
            if !dev.enabled || inst.is_acpi_only() || inst.is_structural() {
                return None;
            }
            ids.get(dev.name.as_str())
        })
        .collect()
}
