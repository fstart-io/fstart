//! Generic phase-init trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Capability;

use super::model::BoardEmitModel;
use crate::stage_gen::config_ser;

/// Description of one phase-init capability and service trait.
#[derive(Debug, Clone, Copy)]
pub(super) struct PhaseSpec {
    service: Service,
    method_name: &'static str,
}

impl PhaseSpec {
    pub(super) const fn new(service: Service, method_name: &'static str) -> Self {
        Self {
            service,
            method_name,
        }
    }
}

/// Emit the body for a generic phase-init trampoline.
pub(super) fn phase_init_body(ctx: &BoardEmitModel<'_>, spec: PhaseSpec) -> TokenStream {
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
        return quote! { unreachable!(#msg) };
    }

    let trait_name = spec.service.as_str();
    let trait_ident = format_ident!("{}", trait_name);
    let trait_alias = format_ident!("_{}", trait_name);
    let method_ident = format_ident!("{}", spec.method_name);

    let southbridge_field = ctx
        .runtime_devices
        .providers(Service::Southbridge)
        .next()
        .map(|device| format_ident!("{}", device.name));

    let method_name = spec.method_name;
    let arms: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(spec.service)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let is_mainboard = device.provides(Service::Mainboard);
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
                let verify_flash_layout = if spec.service == Service::EarlyInit
                    && device.provides(Service::FlashLayoutVerifier)
                {
                    ctx.config.memory.flash_layout.as_ref().map(|layout| {
                        let layout_tokens = config_ser::serialize_to_tokens(layout);
                        quote! {
                            let expected_flash_layout = #layout_tokens;
                            fstart_services::FlashLayoutVerifier::verify_flash_layout(
                                dev,
                                &expected_flash_layout,
                            )
                            .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                        }
                    })
                } else {
                    None
                };
                quote! {
                    #id_lit => {
                        use fstart_services::#trait_ident as #trait_alias;
                        let dev = self.#field
                            .as_mut()
                            .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                        #verify_flash_layout
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
