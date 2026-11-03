//! SiFive FU740: the single-stage flow from the reset vector to the payload,
//! as phases a board's `main` calls, and its build plan.

#![no_std]

extern crate ufmt;

mod config;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The payload's files (kernel, OpenSBI), loaded by the stage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "stage", target_arch = "riscv64"))]
mod stage;

pub use config::{FU740_ECAM_BASE, FU740_MMIO, Fu740Config};
pub use fstart_driver_sifive::fu740_ddr::{FU740_DRAM_BASE, Fu740DdrConfig};
pub use fstart_driver_sifive::fu740_prci::Fu740PrciConfig;
#[cfg(feature = "host")]
pub use host::Plan;
#[cfg(all(feature = "stage", target_arch = "riscv64"))]
pub use stage::{Clocks, Console, Dram, Fu740, Ready, resume_sbi};

#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;

/// A board built on the FU740. Its configuration is the one both the
/// firmware and the host build plan read.
pub trait Fu740Board {
    const CONFIG: &'static Fu740Config;
    /// The Linux device tree among the payload inputs.
    const DEVICE_TREE: &'static str;
}

/// Declare the board's stage `main` and the function OpenSBI returns to
/// before CrabEFI: `fstart_platform_sifive::entry!(main, sbi_resume: resume);`.
#[macro_export]
macro_rules! entry {
    ($main:path, sbi_resume: $resume:path) => {
        $crate::stage_runtime::stage_bin!(main: $main, sbi_resume: $resume);
    };
}
