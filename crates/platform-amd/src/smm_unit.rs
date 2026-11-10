// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host plan's SMM unit, shared by the noncar and Rs flows: the board's
//! SMM handler (its `smm.rs`, `smm_bin!`) as a standalone image with one
//! entry per CPU, which the reset stage embeds (`FSTART_SMM_IMAGE`).
extern crate std;
use fstart_image_build::build_plan::{ArtifactBinding, CargoTarget, CompilationUnit, UnitOutput};
use std::{collections::BTreeMap, string::ToString};

pub const NAME: &str = "smm";
pub const IMAGE_ENV: &str = "FSTART_SMM_IMAGE";

/// The SMM unit for up to `max_cpus` CPUs.
pub fn unit(max_cpus: u16) -> CompilationUnit {
    CompilationUnit {
        name: NAME.into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: "x86_64-unknown-none".into(),
        features: std::vec!["smm".into(), "bundle-smm".into()],
        build_std: None,
        release_only: true,
        linker_script: None,
        rustflags: [
            "-Cpanic=abort",
            "-Copt-level=s",
            // Position independent, as SP3's: lookup tables and the
            // addresses of constants are RIP-relative, calls direct
            // (`-Zplt=yes`); the image builder still rejects any
            // load-base-dependent reference.
            "-Crelocation-model=pie",
            "-Zplt=yes",
            "-Cno-redzone=yes",
            "-Clinker-plugin-lto=no",
            "-Cembed-bitcode=no",
            "-Zfunction-sections=yes",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        environment_values: BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), max_cpus.to_string())]),
        bindings: std::vec![],
        file_bindings: std::vec![],
        // The board's SMM handler module, outside the stage `main`.
        board_features: std::vec!["smm".into()],
        output: UnitOutput::SmmImage {
            entry_count: max_cpus,
            stack_size: 0x800,
            coreboot_module_args: false,
            coreboot_header: false,
        },
    }
}

/// The stage's binding of the SMM image.
pub fn binding() -> ArtifactBinding {
    ArtifactBinding {
        producer: NAME.into(),
        artifact: "image".into(),
        environment: IMAGE_ENV.into(),
    }
}
