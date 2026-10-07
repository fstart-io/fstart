#![no_std]
#![allow(clippy::redundant_locals)]

mod generic;
mod ldn;
pub use ldn::{DmaResource, IoResource, IrqResource, LogicalDevice};
pub mod ite8721f;
pub mod pc87382;
pub mod pc87392;
pub mod smsc_lpc47m15x;

pub use generic::*;
