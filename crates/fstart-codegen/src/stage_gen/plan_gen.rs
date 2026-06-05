//! Emit data-only `StagePlan` statics for the handwritten executor.
//!
//! This module lowers board RON capabilities to static facts only. The
//! handwritten executor owns behavior.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::{DriverInstance, PlatformBootMediaCandidate, Service, ServiceSet};
use fstart_types::{
    BoardConfig, BootMedium, Capability, DeviceConfig, DeviceId, LoadDevice, StageLayout,
    TempRamBuffer,
};

use super::capabilities::boot_media_values_for_device;
use super::tokens::hex_addr;

/// Emit the `STAGE_PLAN` literal for a stage build.
pub(super) fn generate_stage_plan(
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let ids = DeviceIdMap::new(&config.devices);
    let ctx = PlanCtx {
        config,
        ids: &ids,
        devices: &config.devices,
        instances,
        device_services,
    };

    let mut helper_tokens = Vec::new();
    let mut guard_tokens = Vec::new();
    let op_tokens: Vec<_> = capabilities
        .iter()
        .enumerate()
        .map(|(idx, cap)| {
            let lowered = capability_tokens(idx, cap, &ctx);
            helper_tokens.extend(lowered.helpers);
            guard_tokens.extend(lowered.guards);
            lowered.op
        })
        .collect();
    let ops_len = capabilities.len();

    let persistent = persistent_inited_ids(config, stage_name, &ids);
    let persistent_lits = persistent.iter().map(|id| Literal::u8_unsuffixed(*id));

    let all_devices = all_runtime_devices(&config.devices, instances, device_services, &ids);
    let all_device_lits = all_devices.iter().map(|id| Literal::u8_unsuffixed(*id));
    let all_device_len = all_devices.len();

    let optional_devices = optional_runtime_devices(&config.devices, device_services, &ids);
    let optional_device_lits = optional_devices
        .iter()
        .map(|id| Literal::u8_unsuffixed(*id));
    let optional_device_len = optional_devices.len();

    let gated = collect_boot_media_gated(config, capabilities, &config.devices, instances, &ids);
    let gated_len = gated.len();

    let stage_name = stage_name.unwrap_or("");

    quote! {
        #(#guard_tokens)*
        #(#helper_tokens)*

        static _FSTART_STAGE_PLAN_OPS: [fstart_stage_runtime::StageOp; #ops_len] = [
            #(#op_tokens,)*
        ];

        static _FSTART_STAGE_PLAN_PERSISTENT_INITED: &[fstart_types::DeviceId] = &[
            #(#persistent_lits,)*
        ];

        static _FSTART_STAGE_PLAN_ALL_DEVICES: [fstart_types::DeviceId; #all_device_len] = [
            #(#all_device_lits,)*
        ];

        static _FSTART_STAGE_PLAN_OPTIONAL_DEVICES: [fstart_types::DeviceId; #optional_device_len] = [
            #(#optional_device_lits,)*
        ];

        static _FSTART_STAGE_PLAN_BOOT_MEDIA_GATED: [fstart_stage_runtime::BootMediaCandidate; #gated_len] = [
            #(#gated,)*
        ];

        static STAGE_PLAN: fstart_stage_runtime::StagePlan = fstart_stage_runtime::StagePlan {
            stage_name: #stage_name,
            ops: &_FSTART_STAGE_PLAN_OPS,
            persistent_inited: _FSTART_STAGE_PLAN_PERSISTENT_INITED,
            all_devices: &_FSTART_STAGE_PLAN_ALL_DEVICES,
            optional_devices: &_FSTART_STAGE_PLAN_OPTIONAL_DEVICES,
            boot_media_gated: &_FSTART_STAGE_PLAN_BOOT_MEDIA_GATED,
        };
    }
}

struct PlanCtx<'a> {
    config: &'a BoardConfig,
    ids: &'a DeviceIdMap<'a>,
    devices: &'a [DeviceConfig],
    instances: &'a [DriverInstance],
    device_services: &'a [ServiceSet],
}

struct LoweredOp {
    helpers: Vec<TokenStream>,
    guards: Vec<TokenStream>,
    op: TokenStream,
}

impl LoweredOp {
    fn new(op: TokenStream) -> Self {
        Self {
            helpers: Vec::new(),
            guards: Vec::new(),
            op,
        }
    }

    fn guarded(mut self, feature: &'static str, capability: &'static str) -> Self {
        self.guards.push(feature_guard(feature, capability));
        self
    }

    fn with_helper(mut self, helper: TokenStream) -> Self {
        self.helpers.push(helper);
        self
    }
}

struct DeviceIdMap<'a> {
    devices: &'a [DeviceConfig],
}

impl<'a> DeviceIdMap<'a> {
    fn new(devices: &'a [DeviceConfig]) -> Self {
        if devices.len() > 256 {
            panic!(
                "codegen: board has {} devices but DeviceId is u8 (max 256). Reduce device count or widen DeviceId.",
                devices.len()
            );
        }
        Self { devices }
    }

    fn get(&self, name: &str) -> Option<DeviceId> {
        self.devices
            .iter()
            .position(|device| device.name.as_str() == name)
            .map(|index| index as DeviceId)
    }

    fn lit(&self, name: &str, context: &str) -> Literal {
        let id = self.get(name).unwrap_or_else(|| {
            panic!(
                "plan_gen {context}: device '{name}' not in board — validation should have rejected this"
            )
        });
        Literal::u8_unsuffixed(id)
    }
}

fn capability_tokens(idx: usize, cap: &Capability, ctx: &PlanCtx<'_>) -> LoweredOp {
    use Capability as C;

    match cap {
        C::ClockInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ClockInit");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::ClockInit(#id) })
                .guarded("stage-flow-clock-init", "ClockInit")
        }
        C::ConsoleInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ConsoleInit");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::ConsoleInit(#id) })
                .guarded("stage-flow-console-init", "ConsoleInit")
        }
        C::MemoryInit => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::MemoryInit })
            .guarded("stage-flow-memory-init", "MemoryInit"),
        C::DramInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "DramInit");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::DramInit(#id) })
                .guarded("stage-flow-dram-init", "DramInit")
        }
        C::PreConsoleInit { devices } => phase_op(idx, "PreConsoleInit", devices, ctx),
        C::EarlyInit { devices } => phase_op(idx, "EarlyInit", devices, ctx),
        C::StageLocalInit { devices } => phase_op(idx, "StageLocalInit", devices, ctx),
        C::PostDramInit { devices } => phase_op(idx, "PostDramInit", devices, ctx),
        C::FinalizeInit { devices } => phase_op(idx, "FinalizeInit", devices, ctx),
        C::DriverInit => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::DriverInit })
            .guarded("stage-flow-driver-init", "DriverInit"),
        C::PciInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "PciInit");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::PciInit(#id) })
                .guarded("stage-flow-pci", "PciInit")
        }
        C::MemoryDetect { device } => {
            let id = ctx.ids.lit(device.as_str(), "MemoryDetect");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::MemoryDetect(#id) })
                .guarded("stage-flow-memory-detect", "MemoryDetect")
        }
        C::BootMedia(medium) => boot_media_op(idx, medium, ctx),
        C::SigVerify => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::SigVerify })
            .guarded("stage-flow-ffs", "SigVerify"),
        C::FdtPrepare => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::FdtPrepare })
            .guarded("stage-flow-fdt", "FdtPrepare"),
        C::PayloadLoad => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::PayloadLoad })
            .guarded("stage-flow-ffs", "PayloadLoad"),
        C::StageLoad { next_stage } => {
            let next_stage = next_stage.as_str();
            LoweredOp::new(
                quote! { fstart_stage_runtime::StageOp::StageLoad { next_stage: #next_stage } },
            )
            .guarded("stage-flow-ffs", "StageLoad")
        }
        C::AcpiPrepare => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::AcpiPrepare })
            .guarded("stage-flow-acpi", "AcpiPrepare"),
        C::SmBiosPrepare => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::SmBiosPrepare })
            .guarded("stage-flow-smbios", "SmBiosPrepare"),
        C::AcpiLoad { device } => {
            let id = ctx.ids.lit(device.as_str(), "AcpiLoad");
            LoweredOp::new(quote! { fstart_stage_runtime::StageOp::AcpiLoad(#id) })
                .guarded("stage-flow-acpi", "AcpiLoad")
        }
        C::MpInit {
            cpu_model,
            num_cpus,
            smm,
            ..
        } => {
            let cpu_model = cpu_model.as_str();
            LoweredOp::new(quote! {
                fstart_stage_runtime::StageOp::MpInit {
                    cpu_model: #cpu_model,
                    num_cpus: #num_cpus,
                    smm: #smm,
                }
            })
            .guarded("stage-flow-mp", "MpInit")
        }
        C::ReturnToFel => LoweredOp::new(quote! { fstart_stage_runtime::StageOp::ReturnToFel })
            .guarded("stage-flow-fel", "ReturnToFel"),
        C::LoadNextStage {
            devices,
            next_stage,
        } => load_next_stage_op(idx, devices.as_slice(), next_stage.as_str(), ctx),
    }
}

fn phase_op(
    idx: usize,
    variant: &'static str,
    devices: &[heapless::String<32>],
    ctx: &PlanCtx<'_>,
) -> LoweredOp {
    let ids: Vec<Literal> = devices
        .iter()
        .map(|device| ctx.ids.lit(device.as_str(), variant))
        .collect();
    let len = ids.len();
    let ident = format_ident!(
        "_FSTART_STAGE_PLAN_{}_IDS_{idx}",
        variant.to_ascii_uppercase()
    );
    let op = format_ident!("{}", variant);
    LoweredOp::new(quote! { fstart_stage_runtime::StageOp::#op(&#ident) })
        .guarded("stage-flow-phases", variant)
        .with_helper(quote! {
            static #ident: [fstart_types::DeviceId; #len] = [#(#ids,)*];
        })
}

fn boot_media_op(idx: usize, medium: &BootMedium, ctx: &PlanCtx<'_>) -> LoweredOp {
    match medium {
        BootMedium::FirmwareImage {
            provider,
            temp_ram_buffer,
        } => firmware_image_boot_media_op(
            idx,
            provider.as_ref().map(|provider| provider.as_str()),
            *temp_ram_buffer,
            ctx,
        ),
    }
}

fn firmware_image_boot_media_op(
    idx: usize,
    provider: Option<&str>,
    temp_ram_buffer: Option<TempRamBuffer>,
    ctx: &PlanCtx<'_>,
) -> LoweredOp {
    let Some(resolved) = resolve_firmware_provider(provider, ctx) else {
        return LoweredOp::new(quote! {
            compile_error!("BootMedia(FirmwareImage(...)) requires exactly one enabled FirmwareImageProvider, an explicit provider, Rust platform firmware-image support, or Rust platform boot-source candidates")
        });
    };
    let temp_ram_buffer = temp_ram_buffer_tokens(temp_ram_buffer);
    match resolved {
        ResolvedFirmwareProvider::Device(provider_name) => {
            let id = ctx.ids.lit(provider_name, "BootMedia::FirmwareImage");
            LoweredOp::new(quote! {
                fstart_stage_runtime::StageOp::BootMediaFirmwareProvider {
                    provider: #id,
                    temp_ram_buffer: #temp_ram_buffer,
                }
            })
            .guarded("stage-flow-boot-media", "BootMedia(FirmwareImage)")
        }
        ResolvedFirmwareProvider::Platform(image) => {
            let ident = format_ident!("_FSTART_STAGE_PLAN_PLATFORM_IMAGE_{idx}");
            let image = firmware_image_tokens(image);
            LoweredOp::new(quote! {
                fstart_stage_runtime::StageOp::BootMediaPlatformFirmwareImage {
                    image: &#ident,
                    temp_ram_buffer: #temp_ram_buffer,
                }
            })
            .guarded("stage-flow-boot-media", "BootMedia(FirmwareImage)")
            .with_helper(quote! {
                static #ident: fstart_services::FirmwareImage = #image;
            })
        }
        ResolvedFirmwareProvider::PlatformBootSource(candidates) => {
            let ident = format_ident!("_FSTART_STAGE_PLAN_BOOT_CANDIDATES_{idx}");
            let candidate_tokens = boot_source_candidates(candidates, ctx);
            let len = candidate_tokens.len();
            LoweredOp::new(quote! {
                fstart_stage_runtime::StageOp::BootMediaPlatformBootSource {
                    candidates: &#ident,
                    temp_ram_buffer: #temp_ram_buffer,
                }
            })
            .guarded("stage-flow-boot-media", "BootMedia(FirmwareImage)")
            .with_helper(quote! {
                static #ident: [fstart_stage_runtime::BootMediaCandidate; #len] = [
                    #(#candidate_tokens,)*
                ];
            })
        }
    }
}

fn load_next_stage_op(
    idx: usize,
    devices: &[LoadDevice],
    next_stage: &str,
    ctx: &PlanCtx<'_>,
) -> LoweredOp {
    let ident = format_ident!("_FSTART_STAGE_PLAN_LNS_CANDIDATES_{idx}");
    let candidates = load_candidates(devices, ctx);
    let len = candidates.len();
    LoweredOp::new(quote! {
        fstart_stage_runtime::StageOp::LoadNextStage {
            candidates: &#ident,
            next_stage: #next_stage,
        }
    })
    .guarded("stage-flow-fel", "LoadNextStage")
    .with_helper(quote! {
        static #ident: [fstart_stage_runtime::BootMediaCandidate; #len] = [
            #(#candidates,)*
        ];
    })
}

fn temp_ram_buffer_tokens(value: Option<TempRamBuffer>) -> TokenStream {
    match value {
        Some(value) => {
            let base = hex_addr(value.base);
            let size = hex_addr(value.size);
            quote! { Some(fstart_types::TempRamBuffer { base: #base, size: #size }) }
        }
        None => quote! { None },
    }
}

fn firmware_image_tokens(image: fstart_services::FirmwareImage) -> TokenStream {
    let size = hex_addr(image.size);
    let windows = image.windows.iter().map(|window| {
        let flash_offset = hex_addr(window.flash_offset);
        let cpu_base = hex_addr(window.cpu_base);
        let size = hex_addr(window.size);
        quote! { fstart_services::FirmwareWindow::new(#flash_offset, #cpu_base, #size) }
    });
    let window_count = image.window_count as usize;
    quote! {
        fstart_services::FirmwareImage {
            size: #size,
            windows: [#(#windows,)*],
            window_count: #window_count as u8,
        }
    }
}

fn feature_guard(feature: &'static str, capability: &'static str) -> TokenStream {
    let message = format!("{capability} requires fstart-stage feature `{feature}`");
    quote! {
        #[cfg(not(feature = #feature))]
        compile_error!(#message);
    }
}

enum ResolvedFirmwareProvider<'a> {
    Device(&'a str),
    Platform(fstart_services::FirmwareImage),
    PlatformBootSource(&'static [PlatformBootMediaCandidate]),
}

fn resolve_firmware_provider<'a>(
    provider: Option<&'a str>,
    ctx: &PlanCtx<'a>,
) -> Option<ResolvedFirmwareProvider<'a>> {
    if let Some(provider) = provider {
        return Some(ResolvedFirmwareProvider::Device(provider));
    }

    let mut matches = ctx
        .devices
        .iter()
        .zip(ctx.device_services.iter())
        .filter(|(device, services)| {
            device.enabled && services.contains(Service::FirmwareImageProvider)
        })
        .map(|(device, _)| device.name.as_str());
    if let Some(first) = matches.next() {
        if matches.next().is_none() {
            return Some(ResolvedFirmwareProvider::Device(first));
        }
        return None;
    }

    if let Some(image) = fstart_device_registry::platform_firmware_image(
        ctx.config.name.as_str(),
        ctx.config.platform,
    ) {
        return Some(ResolvedFirmwareProvider::Platform(image));
    }

    let candidates = fstart_device_registry::platform_boot_media_candidates(
        ctx.config.name.as_str(),
        ctx.config.platform,
    );
    if !candidates.is_empty() {
        return Some(ResolvedFirmwareProvider::PlatformBootSource(candidates));
    }

    None
}

fn boot_source_candidates(
    candidates: &[PlatformBootMediaCandidate],
    ctx: &PlanCtx<'_>,
) -> Vec<TokenStream> {
    candidates
        .iter()
        .map(|candidate| {
            let id = ctx.ids.lit(candidate.device, "BootMedia::FirmwareImage");
            let offset = hex_addr(candidate.offset);
            let size = hex_addr(candidate.size);
            let media_ids = media_ids_tokens(candidate.device, ctx);
            quote! {
                fstart_stage_runtime::BootMediaCandidate {
                    device: #id,
                    offset: #offset,
                    size: #size,
                    media_ids: #media_ids,
                }
            }
        })
        .collect()
}

fn load_candidates(candidates: &[LoadDevice], ctx: &PlanCtx<'_>) -> Vec<TokenStream> {
    candidates
        .iter()
        .map(|candidate| {
            let id = ctx.ids.lit(candidate.name.as_str(), "LoadNextStage");
            let offset = hex_addr(candidate.base_offset);
            let media_ids = media_ids_tokens(candidate.name.as_str(), ctx);
            quote! {
                fstart_stage_runtime::BootMediaCandidate {
                    device: #id,
                    offset: #offset,
                    size: 0,
                    media_ids: #media_ids,
                }
            }
        })
        .collect()
}

fn media_ids_tokens(device_name: &str, ctx: &PlanCtx<'_>) -> TokenStream {
    let values = boot_media_values_for_device(device_name, ctx.devices, ctx.instances);
    let lits = values.iter().map(|b| Literal::u8_unsuffixed(*b));
    quote! { &[#(#lits),*] }
}

fn persistent_inited_ids(
    config: &BoardConfig,
    stage_name: Option<&str>,
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    let StageLayout::MultiStage(stages) = &config.stages else {
        return Vec::new();
    };
    let Some(stage_name) = stage_name else {
        return Vec::new();
    };
    let Some(pos) = stages
        .iter()
        .position(|stage| stage.name.as_str() == stage_name)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for stage in &stages[..pos] {
        for cap in &stage.capabilities {
            let dev = match cap {
                Capability::ClockInit { device } | Capability::DramInit { device } => {
                    Some(device.as_str())
                }
                _ => None,
            };
            if let Some(dev) = dev.and_then(|name| ids.get(name)) {
                if !out.contains(&dev) {
                    out.push(dev);
                }
            }
        }
    }
    out
}

fn collect_boot_media_gated(
    config: &BoardConfig,
    capabilities: &[Capability],
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    ids: &DeviceIdMap<'_>,
) -> Vec<TokenStream> {
    let mut out = Vec::new();
    for cap in capabilities {
        match cap {
            Capability::LoadNextStage {
                devices: load_devs, ..
            } if load_devs.len() > 1 => {
                for load_dev in load_devs {
                    if let Some(id) = ids.get(load_dev.name.as_str()) {
                        if out.iter().any(|(existing, _)| *existing == id) {
                            continue;
                        }
                        let values = boot_media_values_for_device(
                            load_dev.name.as_str(),
                            devices,
                            instances,
                        );
                        if !values.is_empty() {
                            out.push((id, driver_init_candidate_tokens(id, values.as_slice())));
                        }
                    }
                }
            }
            Capability::BootMedia(BootMedium::FirmwareImage { provider: None, .. }) => {
                let candidates = fstart_device_registry::platform_boot_media_candidates(
                    config.name.as_str(),
                    config.platform,
                );
                if candidates.len() > 1 {
                    for candidate in candidates {
                        if let Some(id) = ids.get(candidate.device) {
                            if out.iter().any(|(existing, _)| *existing == id) {
                                continue;
                            }
                            let values =
                                boot_media_values_for_device(candidate.device, devices, instances);
                            if !values.is_empty() {
                                out.push((id, driver_init_candidate_tokens(id, values.as_slice())));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.into_iter().map(|(_, tokens)| tokens).collect()
}

fn driver_init_candidate_tokens(id: DeviceId, media_ids: &[u8]) -> TokenStream {
    let id = Literal::u8_unsuffixed(id);
    let media_ids = media_ids.iter().map(|value| Literal::u8_unsuffixed(*value));
    quote! {
        fstart_stage_runtime::BootMediaCandidate {
            device: #id,
            offset: 0,
            size: 0,
            media_ids: &[#(#media_ids),*],
        }
    }
}

fn all_runtime_devices(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    devices
        .iter()
        .zip(instances.iter())
        .zip(device_services.iter())
        .filter_map(|((device, instance), services)| {
            if !device.enabled || !instance.has_runtime_driver() {
                return None;
            }
            if services.contains(Service::PciRootBus) {
                return None;
            }
            ids.get(device.name.as_str())
        })
        .collect()
}

fn optional_runtime_devices(
    devices: &[DeviceConfig],
    device_services: &[ServiceSet],
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    devices
        .iter()
        .zip(device_services.iter())
        .filter_map(|(device, services)| {
            if !device.enabled || !services.contains(Service::Framebuffer) {
                return None;
            }
            ids.get(device.name.as_str())
        })
        .collect()
}
