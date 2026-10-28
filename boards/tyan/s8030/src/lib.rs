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
//! | `platform` | the `fstart_platform_sp3::board::Board` implementation over the above |
//! | `smm` | the SMM image's handler binding (SMM unit only) |
//!
//! One build serves both generations: the reset image identifies the CPU
//! and the [`fstart_platform_sp3::board::Board`] hooks take its generation.
//! The same image goes into both 16 MiB halves of the 32 MiB flash.
//!
//! Where the pieces go in the SP3 boot flow (NATIVE_PORT_PLAN §4):
//!
//! | Step | Board input |
//! |---|---|
//! | 3 FCH early | [`bmc::open_lpc_decodes`], [`bmc::early_init`], console [`bmc::CONSOLE`], [`policy::fch_env`] |
//! | 6 DF | [`policy::df`] |
//! | 7 board early hook | [`gpio::m2_detect_pads`] + read, [`gpio::rome_oem_pads`], then [`gpio::CPM_GPIO`] |
//! | 8 NBIO / 12 MP | [`policy::nbio`], [`policy::ccx`] |
//! | 9 SMU | [`policy::power`] |
//! | 10 DXIO | [`dxio::engines`] with [`dxio::Options`] and [`dxio::Detected`] (M.2 levels, [`bmc::m2_gen3_cap`]), [`policy::dxio`] |
//! | 14 ACPI | [`bmc::acpi_board`] over [`bmc::lpc_aml`] |
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmc;
pub mod dxio;
pub mod gpio;
mod platform;
pub mod policy;
#[cfg(fstart_stage_env = "smm")]
mod smm;

use fstart_driver_amd::sp3::soc::Generation;
use fstart_platform_sp3::layout::Sp3BoardFacts;

/// The Tyan S8030 board marker.
pub struct Board;

/// Rome and Milan; the PSP boots the vendor ROM's Rome (lower) or Milan
/// (upper) half and both carry this build. Their platform features are
/// enabled on the `fstart-platform-sp3` dependency.
impl Sp3BoardFacts for Board {
    const GENERATIONS: &'static [Generation] = &[Generation::Rome, Generation::Milan];
    /// The vendor 32 MiB dual image (Rome lower half, Milan upper half);
    /// the user supplies it (see `vendor/README.md`).
    const VENDOR_IMAGE: &'static str = "boards/tyan/s8030/vendor/8030V404.ROM";
    /// 1P, 64 cores with SMT (Rome and Milan alike).
    const MAX_CPUS: u16 = 128;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_generation_is_compiled_in() {
        assert!(
            Board::GENERATIONS
                .iter()
                .all(|&g| fstart_platform_sp3::layout::compiled(g))
        );
    }
}
