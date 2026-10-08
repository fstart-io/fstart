//! ThinkPad X200 (8-MiB 6D flash layout, integrated GM45 graphics).
#![no_std]

pub mod config;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
mod mainboard;
#[cfg(fstart_stage_env = "smm")]
mod smm;
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
