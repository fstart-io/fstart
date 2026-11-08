//! Plain Rust board and build metadata helpers.

use crate::meta::hvec;
use crate::{
    Compression, DigestAlgorithm, FdtSource, PayloadConfig, PayloadKind, SecurityConfig,
    SignatureAlgorithm,
    payload_manifest::{X86_LINUX_BOOTARGS_TOO_LONG, X86_LINUX_MAX_BOOTARGS},
};

/// Direct x86 Linux launch policy for PC-compatible boards.
///
/// The single constructor for the manifest-backed Linux payload: `bootargs`
/// is checked against [`X86_LINUX_MAX_BOOTARGS`] (the `PayloadConfig::bootargs`
/// capacity) so platform hosts never hand-roll the 15-field literal or their
/// own length error. An empty `bootargs` means no command line: there is
/// deliberately no implicit serial-console default, so boards cannot inherit
/// silent policy — callers pass `--linux-bootargs` explicitly.
/// `compression` is how the FFS stores the kernel: the platform's constant,
/// which its stage decodes.
// Note: `Result` is `#[must_use]` already; no extra attribute needed.
pub fn x86_linux_payload(
    kernel_load_addr: u64,
    zero_page_addr: u64,
    bootargs: &str,
    print_x86_mtrrs: bool,
    compression: Compression,
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
        compression,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    })
}

/// Default x86 UEFI payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_uefi_payload(compression: Compression) -> PayloadConfig {
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
        compression,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Default x86 coreboot payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_coreboot_payload(compression: Compression) -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::Coreboot,
        ..x86_uefi_payload(compression)
    }
}

/// Ed25519 signatures over SHA-256 file digests.
#[must_use]
pub fn default_security_config() -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        required_digests: hvec([DigestAlgorithm::Sha256]),
    }
}
