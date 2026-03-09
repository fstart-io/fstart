//! Token stream utility functions for codegen.
//!
//! Small helpers that produce commonly-used [`proc_macro2::TokenStream`]
//! fragments: hex literals, platform halt expressions, and anchor block
//! casts.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::Platform;

/// Create a hex-formatted u64 literal token (e.g., `0x80000000`).
pub(super) fn hex_addr(val: u64) -> TokenStream {
    let s = format!("{val:#x}");
    s.parse().expect("hex literal should parse as TokenStream")
}

/// Generate the platform halt expression.
///
/// Uses the `fstart_platform` alias emitted by `generate_platform_externs()`,
/// so this is platform-agnostic — no match needed.
pub(super) fn halt_expr(_platform: Platform) -> TokenStream {
    quote! { fstart_platform::halt() }
}

/// The `unsafe` expression that casts `&FSTART_ANCHOR` to `&[u8]` for
/// capability functions that read the anchor at runtime.
///
/// Used by first/monolithic stages that have the anchor embedded in
/// their own binary (patched by the FFS builder).
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

/// Reference to the `scanned_anchor_data` local variable.
///
/// Used by non-first stages that scan the boot media for the anchor
/// at runtime (the bootblock's patched anchor is in the FFS image
/// copy in DRAM).
pub(super) fn scanned_anchor_bytes_expr() -> TokenStream {
    quote! { &scanned_anchor_data[..] }
}

/// Select the appropriate anchor bytes expression based on whether
/// this stage embeds the anchor or scans boot media for it.
pub(super) fn anchor_expr(embed_anchor: bool) -> TokenStream {
    if embed_anchor {
        anchor_as_bytes_expr()
    } else {
        scanned_anchor_bytes_expr()
    }
}
