//! Emit the `impl Board for _BoardDevices` adapter used by generated stage
//! codeflow.
//!
//! This is the board-specific half of stage generation.  The generated
//! `fstart_main` emits direct per-stage codeflow for code size and calls this
//! adapter through the `fstart_stage_runtime::Board` trait.
//!
//! The adapter owns concrete driver fields and provides the typed lifecycle and
//! service/capability trampolines that direct codeflow calls through the
//! `fstart_stage_runtime::Board` trait.
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

use fstart_device_registry::{DriverInstance, Service, ServiceSet};
use fstart_types::{
    acpi::AcpiExtraDevice, BoardConfig, BootMedium, Capability, DeviceConfig, DeviceNode,
};
use proc_macro2::TokenStream;

mod board_impl;
mod boot_media;
mod caps_tables;
mod fdt;
mod init_caps;
mod lifecycle;
mod logger;
mod model;
mod mp;
mod payload;
mod payload_uefi;
mod phases;
mod platform;
mod security;
mod state;

use board_impl::emit_board_impl;
use model::{BoardEmitInputs, BoardEmitModel};
use state::{emit_adapter_new, emit_adapter_struct};

// =======================================================================
// Public entry point
// =======================================================================

/// Emit the complete board adapter: `_BoardDevices` struct, `new()`,
/// and `impl Board for _BoardDevices`.
///
/// Callers wire this into `generate_stage_source` before emitting the direct
/// `fstart_main` codeflow.
///
/// `stage_name` is used to derive stage-local facts (monolithic or named first
/// stage in a `MultiStage` layout). Non-first stages are the ones that receive
/// a serialised [`StageHandoff`] from the previous stage; currently only the
/// `fdt_prepare` trampoline uses this fact (to prefer a runtime DRAM size over
/// the static board-config value).
///
/// [`StageHandoff`]: fstart_types::handoff::StageHandoff
pub(super) fn generate_board_adapter(
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    device_services: &[ServiceSet],
    acpi_only_devices: &[AcpiExtraDevice],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let excluded = compute_excluded_indices(
        config,
        &config.devices,
        instances,
        device_tree,
        device_services,
        capabilities,
    );
    let platform = config.platform;
    let ctx = BoardEmitModel::new(BoardEmitInputs {
        config,
        instances,
        device_tree,
        device_services,
        acpi_only_devices,
        excluded: &excluded,
        capabilities,
        stage_name,
    });

    let mut tokens = TokenStream::new();
    tokens.extend(emit_adapter_struct(&ctx));
    tokens.extend(emit_adapter_new(&ctx));
    tokens.extend(emit_board_impl(platform, &ctx));
    tokens
}

// Board adapter semantic context lives in `model`.

// =======================================================================
// Excluded devices for stages without DriverInit
// =======================================================================

/// Which device indices are not materialised in this stage.
///
/// Bus children require their parent bus to be initialised before
/// construction (`new_on_bus` reads the parent's BARs).  In stages
/// without a `DriverInit` capability, no parent ever initialises, so
/// bus children that are not capability targets are omitted.
fn compute_excluded_indices(
    config: &BoardConfig,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    device_tree: &[DeviceNode],
    device_services: &[ServiceSet],
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
            Capability::BootMedia(BootMedium::FirmwareImage { provider, .. }) => {
                if let Some(provider) = provider {
                    referenced.push(provider.as_str());
                } else if let Some(provider) =
                    sole_enabled_firmware_provider(devices, device_services)
                {
                    referenced.push(provider);
                } else {
                    for candidate in fstart_device_registry::platform_boot_media_candidates(
                        config.name.as_str(),
                        config.platform,
                    ) {
                        referenced.push(candidate.device);
                    }
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
                && instances[*idx].has_runtime_driver()
                && !referenced
                    .iter()
                    .any(|name| *name == devices[*idx].name.as_str())
        })
        .map(|(idx, _)| idx)
        .collect()
}

fn sole_enabled_firmware_provider<'a>(
    devices: &'a [DeviceConfig],
    device_services: &[ServiceSet],
) -> Option<&'a str> {
    let mut providers = devices
        .iter()
        .zip(device_services.iter())
        .filter(|(device, services)| {
            device.enabled && services.contains(Service::FirmwareImageProvider)
        })
        .map(|(device, _)| device.name.as_str());
    let first = providers.next()?;
    if providers.next().is_none() {
        Some(first)
    } else {
        None
    }
}

// =======================================================================
// `struct _BoardDevices`
// =======================================================================

// =======================================================================
// `impl Board for _BoardDevices`
// =======================================================================

// =======================================================================
// Tests
// =======================================================================

#[cfg(test)]
mod tests;
