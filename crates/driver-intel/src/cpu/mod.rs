#[cfg(any(feature = "i945", feature = "gm965", feature = "pineview", test))]
pub(crate) mod core2_aml;
#[cfg(feature = "baytrail")]
pub(crate) mod silvermont_aml;
