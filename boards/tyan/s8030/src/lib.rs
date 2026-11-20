//! Tyan S8030 (S8030GM2NE, S8030GM4NE-2T): one AMD SP3 socket, EPYC 7002
//! (Rome) or 7003 (Milan).
//!
//! The board is data and policy for the SP3 platform (BOARD_S8030 in the
//! sp3 analysis workspace):
//!
//! | Module | What |
//! |---|---|
//! | [`dxio`] | the `$A26` lane map as typed engines and the OEM runtime patches |
//! | [`gpio`] | the CPM `$A10` pad table, the M.2 detect pads, Rome GPIO4/5 |
//! | [`policy`] | silicon policy from the S8030 setup defaults |
//! | [`bmc`] | AST2500 LPC decodes, Super I/O and SCU, COM1 console, IPMI KCS, ACPI |
//! | `platform` | the `fstart_platform_amd::sp3::board::Board` implementation over the above |
//! | `smm` | the SMM image's handler binding (SMM unit only) |
//!
//! One build serves both generations: the reset image identifies the CPU
//! and the [`fstart_platform_amd::sp3::board::Board`] hooks take its generation.
//! The same image goes into both 16 MiB halves of the 32 MiB flash.
//!
//! Where the board supplies policy to the SP3 flow:
//!
//! | Phase | Board input |
//! |---|---|
//! | Early FCH | [`bmc::open_lpc_decodes`], [`bmc::early_init`], console [`bmc::CONSOLE`], [`policy::fch_env`] |
//! | Data fabric | [`policy::df`] |
//! | Board early hook | [`gpio::m2_detect_pads`] + read, [`gpio::rome_oem_pads`], then [`gpio::CPM_GPIO`] |
//! | NBIO and MP | [`policy::nbio`], [`policy::ccx`] |
//! | SMU | [`policy::power`] |
//! | DXIO | [`dxio::engines`] with [`dxio::Options`] and [`dxio::Detected`] (M.2 levels, [`bmc::m2_gen3_cap`]), [`policy::dxio`] |
//! | ACPI | [`bmc::acpi_board`] |
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmc;
pub mod dxio;
pub mod gpio;
pub mod platform;
pub mod policy;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;

/// The Tyan S8030 board marker.
pub struct Board;
