//! Multiprocessor/SMM trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Capability;

use super::boot_media::anchor_bytes_stmt;
use super::model::BoardEmitModel;

/// Emit the body of `Board::mp_init`.
pub(super) fn mp_init_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
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
        ctx.runtime_devices
            .runtime()
            .find(|device| device.name == provider)
            .map(|device| device.index)
    } else {
        ctx.runtime_devices
            .providers(Service::SmmOps)
            .next()
            .map(|device| device.index)
    };

    let mp_microcode_enabled = matches!(
        ctx.config.microcode.as_ref(),
        Some(fstart_types::board::MicrocodeConfig::Intel(config)) if config.mp
    );
    let microcode_expr = if mp_microcode_enabled {
        let anchor_stmt = anchor_bytes_stmt();
        quote! {
            {
                #anchor_stmt
                let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(_anchor_bytes) }
                    .ok();
                anchor.and_then(|anchor| {
                    if anchor.microcode_offset == 0 || anchor.microcode_size == 0 {
                        return None;
                    }
                    let offset = anchor.microcode_offset as u64;
                    let size = anchor.microcode_size as u64;
                    let addr = match self._boot_media {
                        fstart_stage_runtime::BootMediaState::FirmwareImage { image, temp_ram_buffer: _ } => {
                            let end = offset.checked_add(size)?;
                            let first = image.translate(offset)?;
                            if size != 0 {
                                let last = image.translate(end.checked_sub(1)?)?;
                                if last.checked_sub(first)? != size - 1 {
                                    return None;
                                }
                            }
                            first
                        }
                        fstart_stage_runtime::BootMediaState::None
                        | fstart_stage_runtime::BootMediaState::FirmwareImageBlock { .. } => return None,
                    };
                    // SAFETY: xtask patched the anchor with a range inside the active
                    // boot-media image. The checks above verify the selected memory
                    // mapping contains that byte range contiguously.
                    Some(unsafe {
                        core::slice::from_raw_parts(addr as *const u8, anchor.microcode_size as usize)
                    })
                })
            }
        }
    } else {
        quote! { None }
    };

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
