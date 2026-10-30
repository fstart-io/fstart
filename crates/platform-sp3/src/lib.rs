//! AMD SP3 platform: the fixed boot flows of Naples, Rome and Milan, their
//! memory map, MP, SMM, PCI root bridges, ACPI tables and flash image.
//!
//! One firmware build serves every generation the board supports (the
//! platform's additive `naples`/`rome`/`milan` features) with one reset
//! image that every generation's `0x62` entry names; the CPU picks the
//! flow's silicon steps at runtime.
//!
//! A board crate implements [`board::Board`] (and
//! [`layout::Sp3BoardFacts`]), its facts and silicon policy, and owns the
//! reset image's `main`: a [`Flow`] that does the board's work between the
//! phases ([`Reset`], [`Stage`]), started with [`start`] (feature
//! `bundle-reset`).
#![cfg_attr(not(any(test, feature = "host")), no_std)]

extern crate ufmt;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod board;
pub mod entry;
#[cfg(feature = "bundle-reset")]
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
#[cfg(feature = "bundle-reset")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;

#[cfg(feature = "bundle-reset")]
pub use flow::{Flow, Reset, Silicon, Stage, start};

/// Declare the board's reset-image `main`:
/// `fstart_platform_sp3::entry!(main);`, where `main` calls
/// [`start`] with the board's [`Flow`].
#[cfg(feature = "bundle-reset")]
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::stage_runtime::stage_bin!(main: $main);
    };
}
