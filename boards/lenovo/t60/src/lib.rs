//! Experimental ThinkPad T60/R60/Z61t bindings, with 32-bit Yonah/Merom stages.
//!
//! No hardware/thermal validation. Intel graphics only: ATI initialization and
//! brightness, TI PCI1x2x vendor setup, S3/MRC cache, IR and dock parallel ports
//! are not implemented. Docking/Ultrabay, panel/VBT, audio and PCIe behavior
//! remain unvalidated. A laptop without a dock/module has no serial console.
#![no_std]
#[cfg(test)]
extern crate std;
#[cfg(all(feature = "r60", feature = "z61t"))]
compile_error!("r60 and z61t are distinct board selections");
#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod dock;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
pub struct Board;
pub use config::*;
