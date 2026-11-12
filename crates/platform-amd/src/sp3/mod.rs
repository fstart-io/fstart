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
#[cfg(feature = "acpi")]
pub mod acpi;
pub mod board;

pub mod entry;
#[cfg(feature = "bundle-stage")]
mod flow;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;
pub mod einj;
pub mod layout;
pub mod memory;
pub mod pci;
#[cfg(feature = "acpi")]
pub mod smbios;
pub mod smm;

#[cfg(feature = "smm")]
pub use fstart_smm as smm_runtime;

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
