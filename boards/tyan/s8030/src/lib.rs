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
//!
//! One build serves one generation: the `rome` or `milan` feature, selected
//! by the `tyan-s8030-rome` / `tyan-s8030-milan` variant.
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

#[cfg(not(any(feature = "rome", feature = "milan")))]
compile_error!("select a variant: tyan-s8030-rome or tyan-s8030-milan");
#[cfg(all(feature = "rome", feature = "milan"))]
compile_error!("the rome and milan variants are separate builds");

pub mod bmc;
pub mod dxio;
pub mod gpio;
pub mod policy;

use fstart_driver_zen::soc::Generation;
use fstart_platform_sp3::layout::{FlashHalf, Sp3BoardFacts};

/// The Tyan S8030 board marker.
pub struct Board;

/// The vendor 32 MiB dual image (Rome lower half, Milan upper half). Both
/// variants take their half's prefix from it; the user supplies it (see
/// `vendor/README.md`).
const VENDOR_IMAGE: &str = "boards/tyan/s8030/vendor/8030V404.ROM";
/// 1P, 64 cores with SMT.
const MAX_CPUS: u16 = 128;

#[cfg(feature = "rome")]
impl Sp3BoardFacts for Board {
    const GENERATION: Generation = Generation::Rome;
    const VENDOR_IMAGE: &'static str = VENDOR_IMAGE;
    const FLASH_HALF: FlashHalf = FlashHalf::Lower;
    const MAX_CPUS: u16 = MAX_CPUS;
}

#[cfg(feature = "milan")]
impl Sp3BoardFacts for Board {
    const GENERATION: Generation = Generation::Milan;
    const VENDOR_IMAGE: &'static str = VENDOR_IMAGE;
    const FLASH_HALF: FlashHalf = FlashHalf::Upper;
    const MAX_CPUS: u16 = MAX_CPUS;
}

/// The generation of this build.
pub const GENERATION: Generation = <Board as Sp3BoardFacts>::GENERATION;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_half_matches_generation() {
        let half = match GENERATION {
            Generation::Rome => FlashHalf::Lower,
            Generation::Milan => FlashHalf::Upper,
            Generation::Naples => unreachable!("no Naples variant"),
        };
        assert_eq!(<Board as Sp3BoardFacts>::FLASH_HALF, half);
    }
}
