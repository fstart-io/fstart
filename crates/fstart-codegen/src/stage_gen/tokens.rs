//! Token stream utility functions for codegen.
//!
//! Small helpers that produce commonly-used [`proc_macro2::TokenStream`]
//! fragments.  The emission layer that used to live here (halt
//! expressions, anchor-block casts) moved into `board_gen` as the
//! board adapter absorbed all per-capability token emission.

use proc_macro2::TokenStream;

/// Create a hex-formatted u64 literal token (e.g., `0x80000000`).
pub(super) fn hex_addr(val: u64) -> TokenStream {
    let s = format!("{val:#x}");
    s.parse().expect("hex literal should parse as TokenStream")
}
