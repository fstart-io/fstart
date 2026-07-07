#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

#[cfg(feature = "acpi")]
mod core2_aml;

pub mod ck505;
pub mod gm965;
pub mod gpio_ich;
pub mod ich7;
pub mod ich8;
pub mod pineview;
pub use fstart_arch::cpu_intel::microcode;
pub mod pmio_ich;
pub mod smbus;
