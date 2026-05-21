//! Multiprocessor/SMM trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::{BootMedium, Capability};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::anchor_bytes_stmt;
use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the body of `Board::mp_init`.
pub(super) fn mp_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
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
        .then(|| {
            ctx.stage.capabilities.iter().find_map(|cap| match cap {
                Capability::BootMedia(BootMedium::MemoryMapped { base, .. }) => Some(*base),
                _ => None,
            })
        })
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
