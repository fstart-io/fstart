//! AMD SP3 platform: the fixed boot flows of Naples, Rome and Milan, their
//! memory map, MP, SMM, PCI root bridges, ACPI tables and flash image.
//!
//! One firmware build serves one generation (one flash half); the flow
//! identifies the CPU at runtime and halts on anything else.
#![cfg_attr(not(any(test, feature = "host")), no_std)]

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod entry;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;
pub mod layout;
pub mod memory;
