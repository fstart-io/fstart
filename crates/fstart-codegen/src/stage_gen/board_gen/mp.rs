//! Multiprocessor/SMM primitive service emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Capability;

use super::model::BoardEmitModel;

/// Emit the body of `Board::with_mp_services`.
pub(super) fn mp_init_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let mp_cap = ctx.stage.capabilities.iter().find_map(|cap| match cap {
        Capability::MpInit { cpu_model, .. } => Some(cpu_model.as_str()),
        _ => None,
    });
    let Some(cpu_model) = mp_cap else {
        return quote! {
            let _ = (smm, microcode_blob, run);
            Err(fstart_stage_runtime::RuntimeError::Failed)
        };
    };

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

    let cpu_driver_body = if cpu_model == "generic-x86"
        || cpu_model == "qemu-x86"
        || cpu_model == "qemu"
    {
        quote! {
            let cpu_driver = fstart_mp::GenericX86CpuDriver;
            let cpu_drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu_driver];
            Ok(run(fstart_stage_runtime::MpServices {
                cpu_drivers: &cpu_drivers,
                smm_ops,
                smm_image,
            }))
        }
    } else if cpu_model == "core2" || cpu_model == "6fx" {
        quote! {
            #[cfg(feature = "intel-cpu")]
            {
                let cpu_driver = fstart_cpu_intel::core2_cpu::Core2CpuDriver::new(0x0500, microcode_blob);
                let cpu_drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu_driver];
                Ok(run(fstart_stage_runtime::MpServices {
                    cpu_drivers: &cpu_drivers,
                    smm_ops,
                    smm_image,
                }))
            }
            #[cfg(not(feature = "intel-cpu"))]
            {
                fstart_log::error!("mp: Intel CPU driver support is not enabled");
                Err(fstart_stage_runtime::RuntimeError::Failed)
            }
        }
    } else if cpu_model == "pineview" || cpu_model == "106cx" {
        quote! {
            #[cfg(feature = "intel-cpu")]
            {
                let cpu_driver = fstart_cpu_intel::pineview::PineviewCpuDriver::with_microcode(0x0500, microcode_blob);
                let cpu_drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu_driver];
                Ok(run(fstart_stage_runtime::MpServices {
                    cpu_drivers: &cpu_drivers,
                    smm_ops,
                    smm_image,
                }))
            }
            #[cfg(not(feature = "intel-cpu"))]
            {
                fstart_log::error!("mp: Intel CPU driver support is not enabled");
                Err(fstart_stage_runtime::RuntimeError::Failed)
            }
        }
    } else {
        let model = cpu_model;
        quote! {
            fstart_log::error!("mp: unsupported CPU model '{}'; no CpuDriver provider", #model);
            Err(fstart_stage_runtime::RuntimeError::Failed)
        }
    };

    quote! {
        let smm_ops: Option<&dyn fstart_mp::SmmOps> = #smm_ops_expr;
        let smm_image: Option<&[u8]> = #smm_image_expr;
        let _ = microcode_blob;
        #cpu_driver_body
    }
}
