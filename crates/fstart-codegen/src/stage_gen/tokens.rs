//! Token stream utility functions for codegen.
//!
//! Small helpers that produce commonly-used [`proc_macro2::TokenStream`]
//! fragments: hex literals, platform halt expressions, and anchor block
//! casts.

use proc_macro2::TokenStream;
use quote::quote;

/// Create a hex-formatted u64 literal token (e.g., `0x80000000`).
pub(super) fn hex_addr(val: u64) -> TokenStream {
    let s = format!("{val:#x}");
    s.parse().expect("hex literal should parse as TokenStream")
}

/// Generate the platform halt expression (e.g., `fstart_platform_riscv64::halt()`).
pub(super) fn halt_expr(platform: &str) -> TokenStream {
    match platform {
        "riscv64" => quote! { fstart_platform_riscv64::halt() },
        "aarch64" => quote! { fstart_platform_aarch64::halt() },
        "armv7" => quote! { fstart_platform_armv7::halt() },
        _ => quote! { loop { core::hint::spin_loop() } },
    }
}

/// The `unsafe` expression that casts `&FSTART_ANCHOR` to `&[u8]` for
/// capability functions that read the anchor at runtime.
pub(super) fn anchor_as_bytes_expr() -> TokenStream {
    quote! {
        unsafe {
            core::slice::from_raw_parts(
                &FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock as *const u8,
                core::mem::size_of::<fstart_types::ffs::AnchorBlock>(),
            )
        }
    }
}
