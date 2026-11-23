//! QEMU q35 RO/A/B hardware facts and policy.

use fstart_platform_qemu::{
    facts::{VirtBoardFacts, VirtMachine},
    q35_ab::Q35AbRoConfig,
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Q35Ab;
}

/// Development/CI-only X.509 DER certificate matching
/// `fstart_image_build::pkcs7_sign::DEV_CAPSULE_RSA2048_PEM`.
#[cfg(feature = "dev-keys")]
pub const DEV_CAPSULE_TRUST_CERT_DER: &[u8] = include_bytes!("../dev-capsule-cert.der");

#[cfg(feature = "dev-keys")]
pub static CAPSULE_TRUST_CERTS: &[&[u8]] = &[DEV_CAPSULE_TRUST_CERT_DER];

/// Without `dev-keys`, no development certificate is trusted (all capsules
/// require an explicit production trust store).
#[cfg(not(feature = "dev-keys"))]
pub static CAPSULE_TRUST_CERTS: &[&[u8]] = &[];

pub static QEMU_Q35_AB_RO: Q35AbRoConfig = Q35AbRoConfig::new();

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub static QEMU_Q35_AB_RAMSTAGE: fstart_platform_qemu::q35_ab::Q35AbRamstageConfig =
    fstart_platform_qemu::q35_ab::Q35AbRamstageConfig::new(CAPSULE_TRUST_CERTS)
        .with_fail_trial(cfg!(all(feature = "fail-trial", feature = "dev-keys")));
