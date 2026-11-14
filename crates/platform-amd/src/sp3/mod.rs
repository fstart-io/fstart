//! AMD SP3 platform: the fixed boot flows of Naples, Rome and Milan, their
//! memory map, MP, SMM, PCI root bridges, ACPI tables and flash image.
//!
//! One firmware build serves every generation the board supports
//! ([`layout::Sp3BoardFacts::GENERATIONS`]) with one reset image that every
//! generation's `0x62` entry names; the CPU picks the flow's silicon steps
//! at runtime, among the flows the board's stage calls.
//!
//! A board crate implements [`board::Board`] (and
//! [`layout::Sp3BoardFacts`]), its facts, and owns the stage `main`: it
//! matches the family `start` returns and runs the flow's phases
//! ([`crate::flow`]) with its own work in between, passing its policy,
//! console, GPIO, DXIO lanes and tables to them (feature `bundle`).
pub mod acpi;
pub mod board;

pub mod entry;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub(crate) mod flow;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;
pub mod einj;
pub mod layout;
pub mod memory;
pub mod silicon;
pub mod smbios;
pub mod smm;

#[cfg(feature = "smm")]
pub use fstart_smm as smm_runtime;

#[cfg(all(feature = "bundle", target_os = "none"))]
pub mod soc;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub use soc::{Engines, Identified, Socket, Start, start};
