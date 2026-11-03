//! AMD SP3 platform: the fixed boot flows of Naples, Rome and Milan, their
//! memory map, MP, SMM, PCI root bridges, ACPI tables and flash image.
//!
//! One firmware build serves every generation the board supports (the
//! platform's additive `naples`/`rome`/`milan` features) with one reset
//! image that every generation's `0x62` entry names; the CPU picks the
//! flow's silicon steps at runtime.
//!
//! A board crate implements [`board::Board`] (and
//! [`layout::Sp3BoardFacts`]), its facts, and owns the stage `main`: it
//! matches the family [`start`] returns and does its own work between the
//! phases ([`Identified`], [`Early`], then [`Stage`] in each phase),
//! passing its policy, console, GPIO, DXIO lanes and tables to them
//! (feature `bundle-stage`).
#![cfg_attr(not(any(test, feature = "host")), no_std)]

extern crate ufmt;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod board;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The payload's files, loaded by the stage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

pub mod entry;
#[cfg(feature = "bundle-stage")]
mod flow;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;
pub mod einj;
#[cfg(target_arch = "x86_64")]
pub mod hw;
pub mod layout;
pub mod memory;
#[cfg(target_arch = "x86_64")]
pub mod mp;
pub mod pci;
#[cfg(feature = "acpi")]
pub mod smbios;
pub mod smm;

#[cfg(feature = "smm")]
pub use fstart_smm as smm_runtime;
#[cfg(feature = "bundle-stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;

#[cfg(all(feature = "bundle-stage", feature = "naples"))]
pub use flow::NaplesSilicon;
#[cfg(all(feature = "bundle-stage", any(feature = "rome", feature = "milan")))]
pub use flow::RomeMilanSilicon;
#[cfg(all(feature = "bundle-stage", feature = "acpi"))]
pub use flow::Tables;
#[cfg(feature = "bundle-stage")]
pub use flow::{
    Chipset, Configured, Cpus, Early, Enumerated, Identified, Mapped, Published, Silicon, Stage,
    Start, Trained, start,
};

/// Declare the board's stage `main`:
/// `fstart_platform_sp3::entry!(main);`, where `main` matches the family
/// [`start`] returns and runs the board's flow.
#[cfg(feature = "bundle-stage")]
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::stage_runtime::stage_bin!(main: $main);
    };
}
