// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit external compiler input in the immutable PSP-copied reset image.
//! No flash reads, hardware discovery, authentication or automatic fallback.
use fstart_driver_amd::picasso::cpu::microcode::{Bundle, Error, MAX_PATCHES, PATCH_BYTES};

const BYTES: usize = include_bytes!(env!("FSTART_AMD_MICROCODE_BUNDLE")).len();
const _: () =
    assert!(BYTES > 0 && BYTES.is_multiple_of(PATCH_BYTES) && BYTES <= PATCH_BYTES * MAX_PATCHES);
#[repr(align(16))]
struct Input([u8; BYTES]);
static INPUT: Input = Input(*include_bytes!(env!("FSTART_AMD_MICROCODE_BUNDLE")));

/// Select exactly the supplied CPUID stepping, after validating the entire
/// bounded bundle. The linker places these bytes with immutable image rodata.
/// Selection is not proof of physical decoding/coherence, vendor authentication
/// or security freshness. Staging still requires the caller's full unsafe
/// contracts; never submit this reset-image source address to PATCH_LOADER.
pub fn selected_source(signature: u32) -> Result<&'static [u8], Error> {
    Bundle::parse(&INPUT.0)?
        .select(signature)
        .map(|patch| patch.bytes())
}
