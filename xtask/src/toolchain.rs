//! Toolchain policy for firmware builds.

use fstart_core::Platform;

/// Build-time target information derived from a board platform.
#[derive(Debug, Clone, Copy)]
pub struct TargetSpec {
    /// Rust target triple used by Cargo.
    pub triple: &'static str,
    /// Cargo feature selecting the platform crate.
    pub platform_feature: &'static str,
    /// Whether the ELF must be converted to a flat binary for booting/packaging.
    pub needs_flat_binary: bool,
}

impl TargetSpec {
    /// Construct target build policy for a platform.
    pub fn for_platform(platform: Platform) -> Self {
        Self {
            triple: platform.target_triple(),
            platform_feature: platform.as_str(),
            needs_flat_binary: matches!(
                platform,
                Platform::Aarch64 | Platform::Riscv64 | Platform::Armv7 | Platform::X86_64
            ),
        }
    }
}

/// Base rustflags for a firmware target triple.
pub fn rustflags_for_triple(triple: &str) -> String {
    if triple.starts_with("x86_64") {
        // x86_64 firmware: static relocation, large code model.
        // Large model is needed because ROM at the top of 4 GiB and RAM/BSS
        // can be ~4 GiB apart, exceeding small/medium/kernel model ±2 GiB
        // limits.
        //
        // Force curve25519-dalek to use the scalar backend.  This must be in
        // RUSTFLAGS because Cargo features cannot set --cfg on transitive
        // dependencies and a global .cargo/config.toml would affect host builds.
        "-Zub-checks=no -Crelocation-model=static -Ccode-model=large \
         --cfg curve25519_dalek_backend=\"serial\""
            .to_string()
    } else {
        "-Zub-checks=no".to_string()
    }
}
