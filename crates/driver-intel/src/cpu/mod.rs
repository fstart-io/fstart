//! Legacy Intel CPU support shared by the GMCH northbridges.

#[cfg(feature = "acpi")]
pub(crate) mod core2_aml;
mod cstates;

pub use cstates::{CORE2_MOBILE_CSTATES, CState, Core2Cstates};
