// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows: the PSP-booted ("noncar") SoCs from Family 17h on
//! ([`noncar`]) start in PSP-trained DRAM, never Intel CAR. Each SoC module
//! implements the flow's [`noncar::Soc`].
#![no_std]
#[cfg(feature = "noncar")]
pub mod acpi;
#[cfg(feature = "noncar")]
pub mod noncar;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
// Each board's host adapter enables exactly one SoC.
#[cfg(all(feature = "host", feature = "picasso", not(feature = "zeppelin")))]
pub use picasso::host::Plan;
#[cfg(all(feature = "host", feature = "zeppelin"))]
pub use zeppelin::host::Plan;
/// SMM image ABI and handler binding: a board's `smm.rs` binds its handler
/// with `fstart_platform_amd::smm::smm_bin!`.
#[cfg(feature = "smm")]
pub use fstart_smm as smm;
#[cfg(feature = "bundle")]
#[doc(hidden)]
pub use fstart_stage as __stage;
