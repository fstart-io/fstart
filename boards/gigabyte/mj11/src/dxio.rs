// SPDX-License-Identifier: MIT OR Apache-2.0
//! DXIO engines: the fixed onboard ports and the variant's vendor CPM
//! template ($A26) for the x16 slot.
use crate::riser::Bifurcation;
use fstart_driver_amd::dxio::{Descriptor, SataChannel};

const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
    Descriptor {
        turn_off_unused_lanes: true,
        ..Descriptor::pcie(start, end, device, function)
    }
}

macro_rules! with_fixed_engines {
    ($($slot:expr),* $(,)?) => {
        &[
            Descriptor {
                turn_off_unused_lanes: true,
                ..Descriptor::sata(0, 3, SataChannel::Other)
            },
            pcie(4, 7, 1, 2),   // SlimSAS
            pcie(8, 11, 1, 3),  // M.2
            pcie(12, 13, 1, 4), // I210 #1
            pcie(14, 14, 1, 5), // I210 #2
            pcie(15, 15, 1, 1), // AST2500
            $($slot),*
        ]
    };
}

/// MJ11-EC1 (G431): the slot's reversed lanes split x8/x8 on 3.1 and 3.3.
#[cfg(not(feature = "ec0"))]
pub const DEFAULT: &[Descriptor] = with_fixed_engines!(pcie(31, 24, 3, 1), pcie(23, 16, 3, 3));
/// MJ11-EC0: one x16 slot.
#[cfg(feature = "ec0")]
pub const DEFAULT: &[Descriptor] = with_fixed_engines!(pcie(31, 16, 3, 1));

/// The engines for a riser's bifurcation of the slot (the vendor's engines
/// 6-9: lanes 31-16 reversed, functions 3.1-3.4).
pub const fn with_riser(mode: Bifurcation) -> &'static [Descriptor] {
    const X4X4X4X4: &[Descriptor] = with_fixed_engines!(
        pcie(31, 28, 3, 1),
        pcie(27, 24, 3, 2),
        pcie(23, 20, 3, 3),
        pcie(19, 16, 3, 4)
    );
    const X4X4: &[Descriptor] = with_fixed_engines!(pcie(31, 28, 3, 1), pcie(27, 24, 3, 2));
    const X8X4X4: &[Descriptor] =
        with_fixed_engines!(pcie(31, 24, 3, 1), pcie(23, 20, 3, 3), pcie(19, 16, 3, 4));
    const X8X8: &[Descriptor] = with_fixed_engines!(pcie(31, 24, 3, 1), pcie(23, 16, 3, 2));
    match mode {
        Bifurcation::X4x4x4x4 => X4X4X4X4,
        Bifurcation::X4x4 => X4X4,
        Bifurcation::X8x4x4 => X8X4X4,
        Bifurcation::X8x8 => X8X8,
    }
}
