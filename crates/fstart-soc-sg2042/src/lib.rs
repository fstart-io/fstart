//! Sophgo SG2042 ("Mango") SoC runtime support.
//!
//! This crate provides drivers and utilities for the Sophgo SG2042
//! heterogeneous SoC used on the Milk-V Pioneer board. The SoC contains
//! a single ARM Cortex-A53 SCP that handles all platform initialization
//! (clock, DDR, CMN-600 coherent mesh, PCIe, PLIC) before releasing
//! 64 × RISC-V C920 application cores.
//!
//! # Boot flow
//!
//! Silicon BootROM → fstart BL2 (this crate's drivers, on A53 EL3) →
//! RISC-V ZSBL (loaded by [`riscv::Sg2042RvRelease`]) → OpenSBI → Linux
//!
//! # Hardware reference
//!
//! All register addresses and init sequences are derived from the
//! Sophgo open-source TF-A port at:
//! `bootloader-arm64/trusted-firmware-a/plat/sophgo/mango/`

#![no_std]

pub mod cmn;
pub mod ddr;
pub mod pcie;
pub mod plic;
pub mod riscv;
pub mod spi_part;
pub mod top;
pub mod wdt;

#[cfg(feature = "std")]
pub mod fip;

// Re-export all driver types at crate root so generated stage code can use
// `use fstart_soc_sg2042::*;` and find types without module qualification.
pub use cmn::{Sg2042Cmn, Sg2042CmnConfig};
pub use ddr::{Sg2042Ddr, Sg2042DdrConfig};
pub use pcie::{Sg2042Pcie, Sg2042PcieConfig};
pub use plic::{Sg2042Plic, Sg2042PlicConfig};
pub use riscv::{Sg2042RvRelease, Sg2042RvReleaseConfig};
pub use top::{Sg2042Top, Sg2042TopConfig};
pub use wdt::{Sg2042Wdt, Sg2042WdtConfig};
