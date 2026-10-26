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
pub mod config;
pub mod mainboard;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;
#[cfg(all(
    not(test),
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    )
))]
mod stage;
pub struct Board;
pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::T6_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::T6Mainboard;
