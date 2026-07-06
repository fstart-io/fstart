//! Plain Rust board and build metadata helpers.

use heapless::{String as HString, Vec as HVec};

use crate::{
    Compression, DigestAlgorithm, FdtSource, PayloadConfig, PayloadKind, SecurityConfig,
    SignatureAlgorithm,
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

/// Default x86 LinuxBoot payload policy used by simple PC-compatible boards.
#[must_use]
pub fn x86_linuxboot_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("bzImage")),
        kernel_load_addr: Some(0x0200_0000),
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: Some(hstr(
            "console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 ignore_loglevel loglevel=8",
        )),
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Default x86 UEFI payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_uefi_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::UefiPayload,
        kernel_file: None,
        kernel_load_addr: None,
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
