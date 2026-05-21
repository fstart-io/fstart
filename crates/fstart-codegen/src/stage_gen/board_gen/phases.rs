//! Generic phase-init trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Capability;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Description of one phase-init capability and service trait.
#[derive(Debug, Clone, Copy)]
pub(super) struct PhaseSpec {
    service: Service,
    trait_name: &'static str,
    method_name: &'static str,
}

impl PhaseSpec {
    pub(super) const fn new(
        service: Service,
        trait_name: &'static str,
        method_name: &'static str,
    ) -> Self {
        Self {
            service,
            trait_name,
            method_name,
        }
    }
}

/// Emit the body for a generic phase-init trampoline.
pub(super) fn phase_init_body(ctx: &BoardCtx<'_>, spec: PhaseSpec) -> TokenStream {
    let stage_declares_phase = ctx.stage.capabilities.iter().any(|capability| {
        matches!(
            (spec.service, capability),
            (Service::PreConsoleInit, Capability::PreConsoleInit { .. })
                | (Service::EarlyInit, Capability::EarlyInit { .. })
                | (Service::StageLocalInit, Capability::StageLocalInit { .. })
                | (Service::PostDramInit, Capability::PostDramInit { .. })
                | (Service::FinalizeInit, Capability::FinalizeInit { .. })
        )
    });
    if spec.service == Service::PostDramInit && !stage_declares_phase {
        let msg = format!(
            "board_gen::{}: stage does not declare {}",
            spec.method_name,
            spec.service.as_str()
        );
        return quote! { todo!(#msg) };
    }

    let trait_ident = format_ident!("{}", spec.trait_name);
    let trait_alias = format_ident!("_{}", spec.trait_name);
    let method_ident = format_ident!("{}", spec.method_name);

    let southbridge_field = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .find(|idx| device_provides(ctx, *idx, Service::Southbridge))
        .map(|idx| format_ident!("{}", ctx.devices[idx].name.as_str()));

    let method_name = spec.method_name;
    let arms: Vec<TokenStream> = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, spec.service))
        .map(|idx| {
            let field = format_ident!("{}", ctx.devices[idx].name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let is_mainboard = device_provides(ctx, idx, Service::Mainboard);
            if spec.service == Service::PreConsoleInit && is_mainboard {
                if let Some(sb_field) = southbridge_field.as_ref() {
                    quote! {
                        #id_lit => {
                            use fstart_services::Mainboard as _Mainboard;
                            let dev = self.#field
                                .as_mut()
                                .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                            let sb = self.#sb_field
                                .as_mut()
                                .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                            _Mainboard::pre_console_init_with_southbridge(dev, sb)
                                .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                        }
                    }
                } else {
                    quote! {
                        #id_lit => {
                            use fstart_services::Mainboard as _Mainboard;
                            let dev = self.#field
                                .as_mut()
                                .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                            _Mainboard::pre_console_init(dev)
                                .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                        }
                    }
                }
            } else {
                quote! {
                    #id_lit => {
                        use fstart_services::#trait_ident as #trait_alias;
                        let dev = self.#field
                            .as_mut()
                            .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                        #trait_alias::#method_ident(dev)
                            .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    }
                }
            }
        })
        .collect();

    quote! {
        for id in ids {
            match *id {
                #(#arms)*
                _ => {
                    fstart_log::error!(
                        "{}: unknown or unsupported device id {}",
                        #method_name,
                        *id,
                    );
                    return Err(fstart_services::device::DeviceError::InitFailed);
                }
            }
        }
        Ok(())
    }
}
