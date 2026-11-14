//! The SP3 socket: what its SoCs (Naples, Rome, Milan) share. Its flash
//! geometry and layout, its memory reservations, the flow's services for
//! the socket ([`soc`]: the flash windows, the APOB, options, TPM, SMM with
//! RAS and EINJ, the tables), the shared silicon steps and context
//! ([`silicon`]) and its host plan's flash halves ([`host`]).
//!
//! One firmware build serves every SoC the board lists
//! ([`crate::board::Board::SOCS`]) with one reset image that every
//! generation's `0x62` entry names; the board's stage matches the SoC
//! [`crate::flow::start`] returns and runs the flow's phases with its own
//! work in between, passing its policy, console, GPIO, DXIO lanes and
//! tables to them (feature `bundle`).
pub mod acpi;
pub mod board;

pub mod einj;
pub mod entry;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub(crate) mod flow;
#[cfg(feature = "host")]
pub mod host;
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
pub use soc::{Engines, Socket};
