// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SATA controller of a die (behind 8.1 of the die's root bus, function
//! 2). The vendor FchTaishan code programs
//! it through its SMN aperture at 0x03100000, with the AHCI registers at
//! 0x03101000 (sp4r2 report FCH_TAISHAN, tasks af47, af57 and af77, with the
//! vendor policy defaults).
use super::{Acpimmio, Bus, irq, smn_modify};
use crate::fam17h::registers::Smn;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{LocalRegisterCopy, register_bitfields};

pub const PORTS: u32 = 8;

const BASE: u32 = 0x0310_0000;
pub(super) const CONTROL: u32 = BASE;
const RESET_STATUS: u32 = BASE + 0x4;
const MSI: u32 = BASE + 0x8;
const AHCI_CAP: u32 = BASE + 0x1000;
const AHCI_PI: u32 = BASE + 0x100c;
const R1024: u32 = BASE + 0x1024;
/// SGPIO
const R10F4: u32 = BASE + 0x10f4;
const R10F8: u32 = BASE + 0x10f8;
const R10FC: u32 = BASE + 0x10fc;
const MISC: u32 = BASE + 0x1800;
const R180C: u32 = BASE + 0x180c;
const R1810: u32 = BASE + 0x1810;
const R1814: u32 = BASE + 0x1814;
const fn port_config(port: u32) -> u32 {
    BASE + 0x188c + port * 0x20
}
const fn ahci_port(port: u32, register: u32) -> u32 {
    BASE + 0x1100 + port * 0x80 + register
}
const PX_IS: u32 = 0x10;
const PX_SSTS: u32 = 0x28;
const PX_SCTL: u32 = 0x2c;
const PX_SERR: u32 = 0x30;

/// NBIF straps of the SATA function.
pub(super) const NBIF_STRAP: u32 = 0x1023_4400;
const NBIF_STRAP_0C: u32 = 0x1023_440c;
const NBIF_CLASS: u32 = 0x1023_4434;
const NBIF_SSID: u32 = 0x1024_204c;

const AHCI_DEVICE_ID: u32 = 0x7901;
const AHCI_CLASS: u32 = 0x01_0601;

register_bitfields![u32,
    /// PxSCTL/PxSSTS device detection.
    Det [DET OFFSET(0) NUMBITS(4) [Idle = 0, Comreset = 1, Present = 3, Offline = 4]],
    pub(super) Misc [
        /// Unlocks the read-only AHCI and PCI registers.
        UNLOCK OFFSET(0) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) [],
        PORT_DISABLE OFFSET(16) NUMBITS(8) []
    ],
    Capabilities [PORTS OFFSET(0) NUMBITS(5) []],
    /// The PCI MSI capability (function 0xa0) and its copy in the controller.
    MsiCap [MULTIPLE_CAPABLE OFFSET(17) NUMBITS(3) []],
    MsiControl [MULTIPLE_CAPABLE OFFSET(24) NUMBITS(3) []],
    /// NBIF function strap: [15:0] device ID, bit 28 enable.
    pub(super) NbifStrap [DEVICE_ID OFFSET(0) NUMBITS(16) [], ENABLE OFFSET(28) NUMBITS(1) []]
];

/// Polls of the PHY-offline wait, 10 us apart: the vendor code gives up
/// silently after 1 ms.
const OFFLINE_POLLS: u32 = 100;

/// Reset task: take the PHYs offline. `smn` is the die's.
pub fn reset(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) {
    for bit in 0..8 {
        smn.update(RESET_STATUS, 1 << bit, 0);
    }
    smn.update(CONTROL, 1, 0);
    for port in 0..PORTS {
        smn_modify(smn, ahci_port(port, PX_SCTL), Det::DET::Offline);
        let offline = |smn: &mut _| {
            LocalRegisterCopy::<u32, Det::Register>::new(Smn::read(smn, ahci_port(port, PX_SSTS)))
                .matches_all(Det::DET::Offline)
        };
        let mut polls = 0;
        while !offline(smn) && polls < OFFLINE_POLLS {
            delay_us(10);
            polls += 1;
        }
    }
}

/// Env task: enable the ports and set the controller up as AHCI. `die`
/// selects the die (the PCI IRQ router belongs to die 0's FCH), `smn` is its
/// SMN and `acpimmio` its FCH's ACPI MMIO ([`super::acpimmio_base`]).
/// `subsystem_id` is the board's (vendor PCD 0x15f).
pub fn init(
    bus: &mut impl Bus,
    die: u8,
    smn: &mut impl Smn,
    acpimmio: Acpimmio,
    subsystem_id: u32,
    delay_us: &mut impl FnMut(u32),
) {
    smn_modify(smn, MISC, Misc::UNLOCK::SET);

    // Enable each port with a COMRESET.
    for port in 0..PORTS {
        smn.update(MISC, Misc::PORT_DISABLE.val(1 << port).value, 0);
        smn_modify(smn, ahci_port(port, PX_SCTL), Det::DET::Comreset);
        delay_us(1000);
        smn_modify(smn, ahci_port(port, PX_SCTL), Det::DET::Idle);
    }

    smn.update(CONTROL, (1 << 1) | (1 << 2) | (1 << 3) | (1 << 24), 1 << 2);
    smn.update(R10FC, 0x0808_0000, 0x0400_0c02);
    smn.update(R10F8, 0xff00_0000, 0xff00_0000);
    smn.update(R1024, 1, 0);
    smn.update(R180C, 0x0080_0000, 0xf000_006a);
    smn.update(R1810, 0x0200_c304, 0x0200_c304);
    smn.update(R1814, 0x4000_01e0, 0x4000_01e0);
    smn_modify(smn, MISC, Misc::BIT13::SET);
    smn.update(NBIF_STRAP_0C, 1 << 18, 1 << 18);
    // No disabled ports
    smn_modify(smn, MISC, Misc::PORT_DISABLE.val(0));

    // No eSATA ports, no SGPIO: its pins (IOMUX 5 and 6 of the die's FCH)
    // are GPIOs.
    bus.set(acpimmio.iomux::<u8, ()>(5), 0);
    bus.set(acpimmio.iomux::<u8, ()>(6), 0);
    smn.update(R10F8, 0x00ff_00ff, 0);
    smn.update(R10F8, 1 << 20, 0);
    // SGPIO0/1 port select (vendor PCD 0x15c) is 0 for both, so bit 8 twice.
    smn.update(R10F4, (1 << 4) | (1 << 8), 0);

    for port in 0..PORTS {
        smn.update(port_config(port), 0x700, port << 8);
        smn.write(ahci_port(port, PX_SERR), u32::MAX);
        smn.write(ahci_port(port, PX_IS), 0);
    }

    smn.write(NBIF_CLASS, AHCI_CLASS);
    smn_modify(smn, NBIF_STRAP, NbifStrap::DEVICE_ID.val(AHCI_DEVICE_ID));
    smn.write(NBIF_SSID, subsystem_id);

    if die == 0 {
        let misc = irq::read(bus, irq::MISC, irq::Mode::Pic);
        irq::write(bus, irq::MISC, irq::Mode::Pic, (misc & 0xf) | 0xf0);
    }

    smn_modify(smn, MISC, Misc::UNLOCK::CLEAR);
}

/// Late task (ReadyToBoot in the vendor firmware): disable the ports without
/// a device and report only the remaining ones, and copy the MSI capability
/// of the SATA function `sata` into the controller. `smn` is the SMN of die
/// `die`. Returns the ports implemented.
pub fn late(smn: &mut impl Smn, die: u8, pci: &impl ConfigRegionAccess, sata: PciAddress) -> u32 {
    let mut implemented = smn.read(AHCI_PI);
    smn_modify(smn, MISC, Misc::UNLOCK::SET);
    for port in 0..PORTS {
        let status =
            LocalRegisterCopy::<u32, Det::Register>::new(smn.read(ahci_port(port, PX_SSTS)));
        if status.matches_all(Det::DET::Present) {
            continue;
        }
        smn.update(MISC, 0, Misc::PORT_DISABLE.val(1 << port).value);
        implemented &= !(1 << port);
    }
    if implemented == 0 {
        implemented = 1;
    }
    smn.write(AHCI_PI, implemented);
    smn_modify(
        smn,
        AHCI_CAP,
        Capabilities::PORTS.val(implemented.count_ones() - 1),
    );

    // Vendor PCD 0x167: MSI capability multiple message capable.
    // SAFETY: `sata` is the enumerated SATA function.
    let msi = LocalRegisterCopy::<u32, MsiCap::Register>::new(unsafe { pci.read(sata, 0xa0) });
    smn_modify(
        smn,
        MSI,
        MsiControl::MULTIPLE_CAPABLE.val(msi.read(MsiCap::MULTIPLE_CAPABLE)),
    );
    smn_modify(smn, MISC, Misc::UNLOCK::CLEAR);

    fstart_log::debug!(
        "SATA {}: ports implemented {}",
        die,
        fstart_log::Hex(implemented.into())
    );
    implemented
}
