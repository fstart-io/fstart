//! SiFive FU740: the LIM bootblock and DRAM mainstage flows, and the
//! build plan that lays both out.

#![no_std]

extern crate ufmt;

mod config;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The DRAM mainstage, loaded by the LIM bootblock from the
    /// memory-mapped flash: LZ4, decoded slice-to-slice with no
    /// allocation in the bootblock.
    pub const MAIN_STAGE: Compression = Compression::Lz4;
    /// The payload's files (kernel, OpenSBI), loaded by the mainstage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "stage", target_arch = "riscv64"))]
mod stage;

pub use config::{FU740_ECAM_BASE, FU740_MMIO, Fu740Config};
pub use fstart_driver_sifive::fu740_ddr::{FU740_DRAM_BASE, Fu740DdrConfig};
pub use fstart_driver_sifive::fu740_prci::{Fu740PrciConfig, PllConfig, PllSettings};
#[cfg(feature = "host")]
pub use host::Plan;
#[cfg(all(feature = "stage", target_arch = "riscv64"))]
pub use stage::{Clocks, Console, Dram, Fu740, main_stage, resume_sbi};

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

/// Declare a board stage binary's entry: a plain `main` for the LIM
/// bootblock, and for the DRAM mainstage a `main` taking the bootblock's
/// handoff address plus the function OpenSBI returns to before CrabEFI.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::stage_runtime::stage_bin!(main: $main);
    };
    ($main:path, handoff, sbi_resume: $resume:path) => {
        $crate::stage_runtime::stage_bin!(main_handoff: $main);
        #[cfg(target_arch = "riscv64")]
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_sbi_resume(hart_id: u64, dtb_addr: u64) -> ! {
            $resume(hart_id, dtb_addr)
        }
        #[cfg(not(target_arch = "riscv64"))]
        const _: fn(u64, u64) -> ! = $resume;
    };
}
