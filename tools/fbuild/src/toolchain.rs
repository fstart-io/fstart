use fstart_core::Platform;

#[derive(Debug, Clone, Copy)]
pub struct TargetSpec {
    pub triple: &'static str,
    pub platform_feature: &'static str,
    pub needs_flat_binary: bool,
}

impl TargetSpec {
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

pub fn rustflags_for_triple(triple: &str) -> String {
    if triple.starts_with("x86_64") {
        "-Zub-checks=no -Crelocation-model=static -Ccode-model=large \
         --cfg curve25519_dalek_backend=\"serial\""
            .to_string()
    } else {
        "-Zub-checks=no".to_string()
    }
}
