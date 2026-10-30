//! QEMU SiFive U board support package.

#![no_std]

pub mod config;

/// QEMU SiFive U board marker.
pub struct Board;

pub use config::*;
