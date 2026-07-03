//! Fixed-flow stage runtime support for the fstart firmware framework.
//!
//! Runtime control flow is handwritten in [`fixed_flow`]. Board/device
//! participation is expressed by concrete `HardwareInit` device containers, not by
//! generated `StagePlan` tables.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod fixed_flow;

pub use fixed_flow::{run as run_fixed_flow, StageFlow};
