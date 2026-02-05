//! Hardware driver implementations.
//!
//! Each driver is feature-gated so only the drivers a board needs are compiled.
//! In Rigid mode, unused drivers are completely eliminated.
//!
//! Drivers implement the `Device` trait (from `fstart-services`) with a typed
//! `Config` associated type, and one or more service traits (`Console`,
//! `BlockDevice`, `Timer`).
//!
//! See [docs/driver-model.md](../../docs/driver-model.md) for the full
//! driver model architecture.

#![no_std]

pub mod uart;
