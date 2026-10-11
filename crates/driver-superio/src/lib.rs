#![no_std]
#![allow(clippy::redundant_locals)]

mod generic;
mod ldn;
pub use ldn::{DmaResource, IoResource, IrqResource, LogicalDevice};
pub mod ite8718f;
pub mod ite8720f;
pub mod ite8721f;
pub mod ite_env;
pub mod ite_gpio;
pub mod pc87382;
pub mod pc87392;
pub mod smsc_lpc47m15x;
mod w83627_gpio;
pub mod w83627dhg;
pub mod w83627ehg;
pub mod w83627thg;
pub mod winbond_hwm;

pub use generic::*;
