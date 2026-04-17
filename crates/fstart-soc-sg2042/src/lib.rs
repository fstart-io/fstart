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
