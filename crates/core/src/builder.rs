//! Plain Rust board and build metadata helpers.

use heapless::{String as HString, Vec as HVec};

use crate::{
    Compression, DigestAlgorithm, FdtSource, PayloadConfig, PayloadKind, SecurityConfig,
    SignatureAlgorithm,
    payload_manifest::{X86_LINUX_BOOTARGS_TOO_LONG, X86_LINUX_MAX_BOOTARGS},
};

/// Construct a bounded heapless string for static board metadata.
///
/// This keeps board crates from each defining their own `HString::try_from`
/// wrappers while still failing fast when a literal exceeds the schema capacity.
#[must_use]
pub fn hstr<const N: usize>(value: &str) -> HString<N> {
    HString::try_from(value).expect("string exceeds heapless capacity")
}

/// Construct a bounded heapless vector for static board metadata.
///
/// The output capacity `C` is inferred from the destination field type.
#[must_use]
pub fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}

/// Direct x86 Linux launch policy for PC-compatible boards.
///
/// The single constructor for the manifest-backed Linux payload: `bootargs`
/// is checked against [`X86_LINUX_MAX_BOOTARGS`] (the `PayloadConfig::bootargs`
/// capacity) so platform hosts never hand-roll the 15-field literal or their
/// own length error. An empty `bootargs` means no command line: there is
/// deliberately no implicit serial-console default, so boards cannot inherit
/// silent policy — callers pass `--linux-bootargs` explicitly.
// Note: `Result` is `#[must_use]` already; no extra attribute needed.
pub fn x86_linux_payload(
    kernel_load_addr: u64,
    zero_page_addr: u64,
    bootargs: &str,
    print_x86_mtrrs: bool,
) -> Result<PayloadConfig, &'static str> {
    if bootargs.len() > X86_LINUX_MAX_BOOTARGS {
        return Err(X86_LINUX_BOOTARGS_TOO_LONG);
    }
    Ok(PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: None,
        kernel_load_addr: Some(kernel_load_addr),
        x86_zero_page_addr: Some(zero_page_addr),
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: Some(
            bootargs
                .try_into()
                .map_err(|_| X86_LINUX_BOOTARGS_TOO_LONG)?,
        ),
        print_x86_mtrrs,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    })
}

/// Default x86 UEFI payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_uefi_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::UefiPayload,
        kernel_file: None,
        kernel_load_addr: None,
        x86_zero_page_addr: None,
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Common development signing policy for board metadata.
#[must_use]
pub fn dev_security_config(pubkey_file: &str) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr(pubkey_file),
        required_digests: hvec([DigestAlgorithm::Sha256]),
    }
}
