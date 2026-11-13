// SPDX-License-Identifier: MIT OR Apache-2.0
//! DXIO engines: the fixed onboard ports and the variant's vendor CPM
//! template ($A26) for the x16 slot.
use crate::riser::Bifurcation;
use fstart_acpi::smbios::{Attached, OnboardDevice, OnboardType, SystemSlot};
use fstart_driver_amd::dxio::{Descriptor, SataChannel};

/// The x16 slot: Gen3, as wide electrically as physically (slot type B1h
/// with the x16 data bus width), 3.3 V; a riser splits it.
pub static PCIE_1: Attached<'static> = Attached::Slot(SystemSlot {
    designation: "PCIE_1",
    slot_type: 0xb1,
    data_bus_width: 0x0d,
    length: 0x02,
    id: 0,
    characteristics: [0x04, 0x08],
});

/// The M.2 socket: M key (Socket 3), x4.
pub static M2: Attached<'static> = Attached::Slot(SystemSlot {
    designation: "M.2",
    slot_type: 0x17,
    data_bus_width: 0x0a,
    length: 0x02,
    id: 0,
    characteristics: [0x04, 0],
});

/// The SlimSAS connector (Setup's `SL_SAS`), x4 PCIe: a cable goes in
/// it, and SMBIOS has no slot type of its own for it.
pub static SL_SAS: Attached<'static> = Attached::Slot(SystemSlot {
    designation: "SL_SAS",
    slot_type: 0x01,
    data_bus_width: 0x0a,
    length: 0x01,
    id: 0,
    characteristics: [0x04, 0],
});

/// The two I210s.
pub static LAN1: Attached<'static> = Attached::Onboard(OnboardDevice {
    designation: "LAN1",
    kind: OnboardType::Ethernet,
});
pub static LAN2: Attached<'static> = Attached::Onboard(OnboardDevice {
    designation: "LAN2",
    kind: OnboardType::Ethernet,
});

/// The BMC's display controller, behind the AST2500's own PCI bridge.
pub static BMC_VGA: Attached<'static> = Attached::Onboard(OnboardDevice {
    designation: "BMC VGA",
    kind: OnboardType::Video,
});

const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
    Descriptor {
        turn_off_unused_lanes: true,
        ..Descriptor::pcie(start, end, device, function)
    }
}

/// A link of [`PCIE_1`] at 3.`function`.
const fn slot(start: u8, end: u8, function: u8) -> Descriptor {
    pcie(start, end, 3, function).attached(&PCIE_1)
}

macro_rules! with_fixed_engines {
    ($($slot:expr),* $(,)?) => {
        &[
            Descriptor {
                turn_off_unused_lanes: true,
                ..Descriptor::sata(0, 3, SataChannel::Other)
            },
            pcie(4, 7, 1, 2).attached(&SL_SAS),
            pcie(8, 11, 1, 3).attached(&M2),
            pcie(12, 13, 1, 4).attached(&LAN1),
            pcie(14, 14, 1, 5).attached(&LAN2),
            pcie(15, 15, 1, 1).attached(&BMC_VGA),
            $($slot),*
        ]
    };
}

/// MJ11-EC1 (G431): the slot's reversed lanes split x8/x8 on 3.1 and 3.3.
#[cfg(not(feature = "ec0"))]
pub const DEFAULT: &[Descriptor] = with_fixed_engines!(slot(31, 24, 1), slot(23, 16, 3));
/// MJ11-EC0: one x16 slot.
#[cfg(feature = "ec0")]
pub const DEFAULT: &[Descriptor] = with_fixed_engines!(slot(31, 16, 1));

/// The engines for a riser's bifurcation of the slot (the vendor's engines
/// 6-9: lanes 31-16 reversed, functions 3.1-3.4).
pub const fn with_riser(mode: Bifurcation) -> &'static [Descriptor] {
    const X4X4X4X4: &[Descriptor] = with_fixed_engines!(
        slot(31, 28, 1),
        slot(27, 24, 2),
        slot(23, 20, 3),
        slot(19, 16, 4)
    );
    const X4X4: &[Descriptor] = with_fixed_engines!(slot(31, 28, 1), slot(27, 24, 2));
    const X8X4X4: &[Descriptor] =
        with_fixed_engines!(slot(31, 24, 1), slot(23, 20, 3), slot(19, 16, 4));
    const X8X8: &[Descriptor] = with_fixed_engines!(slot(31, 24, 1), slot(23, 16, 2));
    match mode {
        Bifurcation::X4x4x4x4 => X4X4X4X4,
        Bifurcation::X4x4 => X4X4,
        Bifurcation::X8x4x4 => X8X4X4,
        Bifurcation::X8x8 => X8X8,
    }
}
