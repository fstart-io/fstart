//! Security capability trampoline emission.

use proc_macro2::TokenStream;
use quote::quote;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;

/// Emit the body of `Board::sig_verify`.
pub(super) fn sig_verify_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            todo!("board_gen::sig_verify: no FFS-using capability in this stage")
        };
    }

    let bm_usage = quote! {
        fstart_capabilities::sig_verify(_anchor_bytes, &_bm);
    };
    let none_body = quote! {
        fstart_log::info!("sig verify: no boot media configured, skipping");
    };
    let anchor = anchor_bytes_stmt();
    let match_body = match_boot_media(ctx, &bm_usage, "sig_verify", &none_body);

    quote! {
        #anchor
        #match_body
    }
}
