//! Legacy Intel CPU support shared by the GMCH northbridges.

#[cfg(all(
    feature = "acpi",
    any(
        feature = "gm45",
        feature = "gm965",
        feature = "i945",
        feature = "pineview"
    )
))]
pub(crate) mod core2_aml;
mod cstates;
#[cfg(all(feature = "acpi", feature = "ironlake"))]
pub mod model_2065x_aml;
#[cfg(all(feature = "acpi", feature = "baytrail"))]
pub(crate) mod silvermont_aml;

pub use cstates::{CORE2_MOBILE_CSTATES, CState, Core2Cstates};
