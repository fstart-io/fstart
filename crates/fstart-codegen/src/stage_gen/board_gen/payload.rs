//! Payload-load primitive descriptor emission.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::{Capability, Platform};

use super::model::BoardEmitModel;
use super::payload_uefi::payload_load_uefi_body;

/// Emit the body of primitive `Board::payload_load_desc`.
pub(super) fn payload_load_desc_body(_platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::validation::{
        is_fit_image, is_fit_runtime, is_linux_boot, is_uefi_payload,
    };

    let has_payload_load = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if !has_payload_load {
        return quote! { None };
    }

    let kind = if is_uefi_payload(ctx.config) {
        quote! { fstart_stage_runtime::PayloadLoadKind::Uefi }
    } else if is_linux_boot(ctx.config) || (is_fit_image(ctx.config) && !is_fit_runtime(ctx.config))
    {
        quote! { fstart_stage_runtime::PayloadLoadKind::LinuxBoot }
    } else if is_fit_image(ctx.config) && is_fit_runtime(ctx.config) {
        let payload = ctx
            .config
            .payload
            .as_ref()
            .expect("FIT runtime implies payload");
        let config_expr = match &payload.fit_config {
            Some(name) => {
                let name = name.as_str();
                quote! { Some(#name) }
            }
            None => quote! { None },
        };
        quote! { fstart_stage_runtime::PayloadLoadKind::FitRuntime { config: #config_expr } }
    } else {
        quote! { fstart_stage_runtime::PayloadLoadKind::GenericFfs }
    };

    let payload = ctx.config.payload.as_ref();
    let kernel_addr = payload
        .and_then(|payload| payload.kernel_load_addr)
        .unwrap_or(0);
    let dtb_addr = payload.and_then(|payload| payload.dtb_addr).unwrap_or(0);
    let fw_addr = payload
        .and_then(|payload| payload.firmware.as_ref())
        .map(|firmware| firmware.load_addr)
        .unwrap_or(0);
    let bootargs = payload
        .and_then(|payload| payload.bootargs.as_deref())
        .unwrap_or("");
    let load_firmware = payload.is_some_and(|payload| payload.firmware.is_some());
    let print_x86_mtrrs = payload.is_some_and(|payload| payload.print_x86_mtrrs);

    quote! {
        Some(fstart_stage_runtime::PayloadLoadDesc {
            kind: #kind,
            kernel_addr: #kernel_addr,
            dtb_addr: #dtb_addr,
            fw_addr: #fw_addr,
            bootargs: #bootargs,
            load_firmware: #load_firmware,
            print_x86_mtrrs: #print_x86_mtrrs,
        })
    }
}

/// Emit the temporary UEFI payload-load body.
pub(super) fn uefi_payload_load_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::validation::is_uefi_payload;

    let has_payload_load = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if has_payload_load && is_uefi_payload(ctx.config) {
        return payload_load_uefi_body(platform, ctx);
    }
    quote! { unreachable!("board_gen::uefi_payload_load: stage does not use UEFI PayloadLoad") }
}
