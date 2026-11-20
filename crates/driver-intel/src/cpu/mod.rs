//! Legacy Intel CPU support shared by the GMCH northbridges.

pub(crate) mod core2_aml;
mod cstates;
pub mod model_2065x_aml;
pub mod model_206ax_aml;
pub mod options;
pub(crate) mod silvermont_aml;

pub use cstates::{CORE2_MOBILE_CSTATES, CState, Core2Cstates};
