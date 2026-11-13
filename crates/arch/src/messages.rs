//! Lines the bring-up logs that the QEMU boot tests wait for. They are
//! constants so a platform's boot tests (its host module) and the code that
//! prints them cannot drift apart.

/// `mp: initialization complete (N CPUs)`.
pub const MP_COMPLETE: &str = "mp: initialization complete";
/// `... N CPUs relocated`, once SMM relocation ran on every CPU.
pub const SMM_RELOCATED: &str = "CPUs relocated";
pub const SMRAM_LOCKED: &str = "SMM: permanent SMI enabled and SMRAM locked";
pub const SMI_ENABLE_LOCKED: &str = "SMM: global SMI enable locked and verified";
/// `SMM: permanent SMI returned on all N CPUs`.
pub const SMI_RETURNED: &str = "SMM: permanent SMI returned on all";
