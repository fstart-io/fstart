//! Emit a direct per-stage `fstart_main` from the ordered capability list.
//!
//! This is deliberately stage-specific codeflow rather than a generic
//! interpreter. The generated body is small and mechanical, while
//! capability/device details still live in the generated board adapter and
//! handwritten helper crates. There is no second runtime stage-creation path.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::{
    AutoBootDevice, BoardConfig, BootMedium, Capability, DeviceConfig, DeviceId, LoadDevice,
    StageLayout,
};

use super::capabilities::boot_media_values_for_device;
use super::tokens::hex_addr;

/// Emit the stage entry point as straight-line code for this stage's declared
/// capabilities.
pub(super) fn generate_fstart_main(
    config: &BoardConfig,
    instances: &[DriverInstance],
    capabilities: &[Capability],
    stage_name: Option<&str>,
) -> TokenStream {
    let ids = DeviceIdMap::new(&config.devices);
    let ctx = DirectCtx {
        ids: &ids,
        devices: &config.devices,
        instances,
        capabilities,
    };

    let persistent = persistent_inited_ids(config, stage_name, &ids);
    let persistent_lits = persistent.iter().map(|id| Literal::u8_unsuffixed(*id));
    let ops = capabilities
        .iter()
        .enumerate()
        .map(|(idx, cap)| capability_tokens(idx, cap, &ctx));

    quote! {
        /// Stage entry point.  Called by the platform's `_start`
        /// after register setup + BSS zero + stack pointer load.
        #[no_mangle]
        #[allow(unreachable_code, unused_variables, unused_mut)]
        pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
            let _ = handoff_ptr;
            let mut board = _BoardDevices::new();
            let mut _inited = fstart_stage_runtime::DeviceMask::from_slice(&[
                #(#persistent_lits,)*
            ]);

            #(#ops)*

            fstart_stage_runtime::Board::halt(&board)
        }
    }
}

struct DirectCtx<'a> {
    ids: &'a DeviceIdMap<'a>,
    devices: &'a [DeviceConfig],
    instances: &'a [DriverInstance],
    capabilities: &'a [Capability],
}

struct DeviceIdMap<'a> {
    devices: &'a [DeviceConfig],
}

impl<'a> DeviceIdMap<'a> {
    fn new(devices: &'a [DeviceConfig]) -> Self {
        if devices.len() > 256 {
            panic!(
                "codegen: board has {} devices but DeviceId is u8 (max 256). \
                 Reduce device count or widen DeviceId.",
                devices.len()
            );
        }
        Self { devices }
    }

    fn get(&self, name: &str) -> Option<DeviceId> {
        self.devices
            .iter()
            .position(|d| d.name.as_str() == name)
            .map(|i| i as DeviceId)
    }

    fn lit(&self, name: &str, context: &str) -> Literal {
        let id = self.get(name).unwrap_or_else(|| {
            panic!(
                "direct_flow {context}: device '{name}' not in board — validation should have rejected this"
            )
        });
        Literal::u8_unsuffixed(id)
    }
}

fn capability_tokens(idx: usize, cap: &Capability, ctx: &DirectCtx<'_>) -> TokenStream {
    use Capability as C;

    match cap {
        C::ClockInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ClockInit");
            quote! {
                if !_inited.contains(#id) {
                    if fstart_stage_runtime::Board::init_device(&mut board, #id).is_err() {
                        fstart_stage_runtime::Board::halt(&board);
                    }
                    _inited.set(#id);
                }
            }
        }
        C::ConsoleInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "ConsoleInit");
            quote! {
                if fstart_stage_runtime::Board::init_device(&mut board, #id).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
                // SAFETY: capability validation guarantees this device provides
                // Console; init_device has just constructed it.
                unsafe {
                    fstart_stage_runtime::Board::install_logger(&board, #id);
                }
                _inited.set(#id);
            }
        }
        C::MemoryInit => quote! {
            fstart_stage_runtime::Board::memory_init(&board);
        },
        C::DramInit { device } => {
            let id = ctx.ids.lit(device.as_str(), "DramInit");
            quote! {
                if fstart_stage_runtime::Board::init_device(&mut board, #id).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
                if fstart_stage_runtime::Board::dram_init(&mut board, #id).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
                _inited.set(#id);
            }
        }
        C::PreConsoleInit { devices } => phase_tokens(
            "PreConsoleInit",
            devices,
            ctx,
            |ids| quote! { fstart_stage_runtime::Board::pre_console_init(&mut board, &[#(#ids),*]) },
        ),
        C::EarlyInit { devices } => phase_tokens(
            "EarlyInit",
            devices,
            ctx,
            |ids| quote! { fstart_stage_runtime::Board::early_init(&mut board, &[#(#ids),*]) },
        ),
        C::StageLocalInit { devices } => phase_tokens(
            "StageLocalInit",
            devices,
            ctx,
            |ids| quote! { fstart_stage_runtime::Board::stage_local_init(&mut board, &[#(#ids),*]) },
        ),
        C::PostDramInit { devices } => phase_tokens(
            "PostDramInit",
            devices,
            ctx,
            |ids| quote! { fstart_stage_runtime::Board::post_dram_init(&mut board, &[#(#ids),*]) },
        ),
        C::FinalizeInit { devices } => phase_tokens(
            "FinalizeInit",
            devices,
            ctx,
            |ids| quote! { fstart_stage_runtime::Board::finalize_init(&mut board, &[#(#ids),*]) },
        ),
        C::MpInit {
            cpu_model,
            num_cpus,
            smm,
            ..
        } => {
            let cpu_model = cpu_model.as_str();
            quote! {
                if fstart_stage_runtime::Board::mp_init(&mut board, #cpu_model, #num_cpus, #smm).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
            }
        }
        C::PciInit { device } => single_device_call(device.as_str(), "PciInit", ctx, |id| {
            quote! { fstart_stage_runtime::Board::pci_init(&mut board, #id) }
        }),
        C::DriverInit => driver_init_tokens(cap, ctx),
        C::LateDriverInit => quote! {
            fstart_stage_runtime::Board::late_driver_init_complete(&mut board, 0);
        },
        C::SigVerify => quote! {
            fstart_stage_runtime::Board::sig_verify(&board);
        },
        C::FdtPrepare => quote! {
            fstart_stage_runtime::Board::fdt_prepare(&board);
        },
        C::PayloadLoad => quote! {
            fstart_stage_runtime::Board::payload_load(&board);
        },
        C::StageLoad { next_stage } => {
            let next_stage = next_stage.as_str();
            quote! {
                fstart_stage_runtime::Board::stage_load(&board, #next_stage);
            }
        }
        C::AcpiPrepare => quote! {
            fstart_stage_runtime::Board::acpi_prepare(&mut board);
        },
        C::SmBiosPrepare => quote! {
            fstart_stage_runtime::Board::smbios_prepare(&board);
        },
        C::AcpiLoad { device } => single_device_call(device.as_str(), "AcpiLoad", ctx, |id| {
            quote! { fstart_stage_runtime::Board::acpi_load(&mut board, #id) }
        }),
        C::MemoryDetect { device } => {
            single_device_call(device.as_str(), "MemoryDetect", ctx, |id| {
                quote! { fstart_stage_runtime::Board::memory_detect(&mut board, #id) }
            })
        }
        C::ReturnToFel => quote! {
            fstart_stage_runtime::Board::return_to_fel(&board);
        },
        C::BootMedia(medium) => boot_media_tokens(idx, medium, ctx),
        C::LoadNextStage {
            devices,
            next_stage,
        } => load_next_stage_tokens(idx, devices.as_slice(), next_stage.as_str(), ctx),
    }
}

fn phase_tokens(
    context: &str,
    devices: &[heapless::String<32>],
    ctx: &DirectCtx<'_>,
    call: impl FnOnce(Vec<Literal>) -> TokenStream,
) -> TokenStream {
    let ids: Vec<Literal> = devices
        .iter()
        .map(|device| ctx.ids.lit(device.as_str(), context))
        .collect();
    let init_ids = ids.iter();
    let set_ids = ids.iter();
    let phase_call = call(ids.clone());

    quote! {
        #(
            if fstart_stage_runtime::Board::init_device(&mut board, #init_ids).is_err() {
                fstart_stage_runtime::Board::halt(&board);
            }
        )*
        if #phase_call.is_err() {
            fstart_stage_runtime::Board::halt(&board);
        }
        #(
            _inited.set(#set_ids);
        )*
    }
}

fn single_device_call(
    device: &str,
    context: &str,
    ctx: &DirectCtx<'_>,
    call: impl FnOnce(Literal) -> TokenStream,
) -> TokenStream {
    let id = ctx.ids.lit(device, context);
    let op_call = call(id.clone());
    quote! {
        if fstart_stage_runtime::Board::init_device(&mut board, #id).is_err() {
            fstart_stage_runtime::Board::halt(&board);
        }
        if #op_call.is_err() {
            fstart_stage_runtime::Board::halt(&board);
        }
        _inited.set(#id);
    }
}

fn driver_init_tokens(_cap: &Capability, ctx: &DirectCtx<'_>) -> TokenStream {
    let gated = collect_boot_media_gated(ctx.capabilities, ctx.devices, ctx.instances, ctx.ids);
    let gated_lits = gated.iter().map(|id| Literal::u8_unsuffixed(*id));
    let all_devs = all_runtime_devices(ctx.devices, ctx.instances, ctx.ids);
    let all_devs_lits = all_devs.iter().map(|id| Literal::u8_unsuffixed(*id));

    quote! {
        let mut _gated = fstart_stage_runtime::DeviceMask::new();
        #(
            _gated.set(#gated_lits);
        )*
        let _no_skip = fstart_stage_runtime::DeviceMask::new();
        fstart_stage_runtime::Board::init_all_devices(&mut board, &_no_skip, &_gated);
        #(
            _inited.set(#all_devs_lits);
        )*
    }
}

fn boot_media_tokens(idx: usize, medium: &BootMedium, ctx: &DirectCtx<'_>) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { base, size, .. } => {
            let base = hex_addr(*base);
            let size = hex_addr(*size);
            quote! {
                fstart_stage_runtime::Board::boot_media_static(&mut board, None, #base, #size);
            }
        }
        BootMedium::Device { name, offset, size } => {
            let id = ctx.ids.lit(name.as_str(), "BootMedia::Device");
            let offset = hex_addr(*offset);
            let size = hex_addr(*size);
            quote! {
                if fstart_stage_runtime::Board::init_device(&mut board, #id).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
                _inited.set(#id);
                fstart_stage_runtime::Board::boot_media_static(&mut board, Some(#id), #offset, #size);
            }
        }
        BootMedium::AutoDevice { devices } => {
            let candidates_ident = format_ident!("_FSTART_AUTO_CANDIDATES_{idx}");
            let candidates = auto_candidates(devices.as_slice(), ctx);
            let n = devices.len();
            quote! {
                static #candidates_ident: [fstart_stage_runtime::BootMediaCandidate; #n] = [
                    #(#candidates,)*
                ];
                let Some(_boot_media_id) = fstart_stage_runtime::Board::boot_media_select(
                    &mut board,
                    &#candidates_ident,
                ) else {
                    fstart_stage_runtime::Board::halt(&board);
                };
                if fstart_stage_runtime::Board::init_device(&mut board, _boot_media_id).is_err() {
                    fstart_stage_runtime::Board::halt(&board);
                }
                _inited.set(_boot_media_id);
            }
        }
    }
}

fn load_next_stage_tokens(
    idx: usize,
    devices: &[LoadDevice],
    next_stage: &str,
    ctx: &DirectCtx<'_>,
) -> TokenStream {
    let candidates_ident = format_ident!("_FSTART_LNS_CANDIDATES_{idx}");
    let candidates = load_candidates(devices, ctx);
    let n = devices.len();
    quote! {
        static #candidates_ident: [fstart_stage_runtime::BootMediaCandidate; #n] = [
            #(#candidates,)*
        ];
        let Some(_boot_media_id) = fstart_stage_runtime::Board::boot_media_select(
            &mut board,
            &#candidates_ident,
        ) else {
            fstart_stage_runtime::Board::halt(&board);
        };
        if fstart_stage_runtime::Board::init_device(&mut board, _boot_media_id).is_err() {
            fstart_stage_runtime::Board::halt(&board);
        }
        fstart_stage_runtime::Board::load_next_stage(&mut board, #next_stage);
    }
}

fn auto_candidates(candidates: &[AutoBootDevice], ctx: &DirectCtx<'_>) -> Vec<TokenStream> {
    candidates
        .iter()
        .map(|candidate| {
            let id = ctx
                .ids
                .lit(candidate.name.as_str(), "BootMedia::AutoDevice");
            let offset = hex_addr(candidate.offset);
            let size = hex_addr(candidate.size);
            let media_ids = media_ids_tokens(candidate.name.as_str(), ctx);
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

fn load_candidates(candidates: &[LoadDevice], ctx: &DirectCtx<'_>) -> Vec<TokenStream> {
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

fn media_ids_tokens(device_name: &str, ctx: &DirectCtx<'_>) -> TokenStream {
    // Unmapped drivers return an empty list, matching the plan metadata and
    // keeping candidate tables usable on platforms where this mapping is N/A.
    let values = boot_media_values_for_device(device_name, ctx.devices, ctx.instances);
    let lits = values.iter().map(|b| Literal::u8_unsuffixed(*b));
    quote! { &[#(#lits),*] }
}

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
) -> Vec<DeviceId> {
    let mut out = Vec::new();
    for cap in capabilities {
        match cap {
            Capability::LoadNextStage {
                devices: load_devs, ..
            } if load_devs.len() > 1 => {
                for load_dev in load_devs {
                    if let Some(id) = ids.get(load_dev.name.as_str()) {
                        if out.contains(&id) {
                            continue;
                        }
                        let values = boot_media_values_for_device(
                            load_dev.name.as_str(),
                            devices,
                            instances,
                        );
                        if !values.is_empty() {
                            out.push(id);
                        }
                    }
                }
            }
            Capability::BootMedia(BootMedium::AutoDevice {
                devices: candidates,
            }) if candidates.len() > 1 => {
                for candidate in candidates {
                    if let Some(id) = ids.get(candidate.name.as_str()) {
                        if out.contains(&id) {
                            continue;
                        }
                        let values = boot_media_values_for_device(
                            candidate.name.as_str(),
                            devices,
                            instances,
                        );
                        if !values.is_empty() {
                            out.push(id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn all_runtime_devices(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    ids: &DeviceIdMap<'_>,
) -> Vec<DeviceId> {
    devices
        .iter()
        .zip(instances.iter())
        .filter_map(|(dev, inst)| {
            if !dev.enabled || !inst.has_runtime_driver() {
                return None;
            }
            ids.get(dev.name.as_str())
        })
        .collect()
}
