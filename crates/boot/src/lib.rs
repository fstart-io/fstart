//! Payload launch support: FIT/Linux and CrabEFI.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "fit")]
pub mod fit;

#[cfg(feature = "crabefi")]
pub mod crabefi;
