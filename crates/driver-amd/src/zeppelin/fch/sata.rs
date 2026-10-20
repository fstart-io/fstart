// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SATA controller of a die (behind 8.1 of the die's root bus, function
//! 2). The vendor FchTaishan code programs
//! it through its SMN aperture at 0x03100000, with the AHCI registers at
//! 0x03101000 (sp4r2 report FCH_TAISHAN, tasks af47, af57 and af77, with the
//! vendor policy defaults).
use crate::fam17h::tsc::DelayNs;
use super::{NbifStrap, irq};
use crate::fch::{Bus, Fch, iomux};
use crate::smn::{Smn, SmnReg};
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub const PORTS: u32 = 8;

register_bitfields![u32,
    /// PxSCTL/PxSSTS device detection.
    Det [DET OFFSET(0) NUMBITS(4) [Idle = 0, Comreset = 1, Present = 3, Offline = 4]],
    pub(super) Control [
        /// Set to switch off a controller without lanes.
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) [],
        BIT3 OFFSET(3) NUMBITS(1) [],
        BIT11 OFFSET(11) NUMBITS(1) [],
        BIT24 OFFSET(24) NUMBITS(1) []
    ],
    /// Bit n: PHY n in reset.
    ResetStatus [PHYS OFFSET(0) NUMBITS(8) []],
    Misc [
        /// Unlocks the read-only AHCI and PCI registers.
        UNLOCK OFFSET(0) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) [],
        PORT_DISABLE OFFSET(16) NUMBITS(8) []
    ],
    Capabilities [PORTS OFFSET(0) NUMBITS(5) []],
    /// The PCI MSI capability (function 0xa0) and its copy in the controller.
    MsiCap [MULTIPLE_CAPABLE OFFSET(17) NUMBITS(3) []],
    MsiControl [MULTIPLE_CAPABLE OFFSET(24) NUMBITS(3) []],
    R1024 [BIT0 OFFSET(0) NUMBITS(1) []],
    /// SGPIO control.
    Sgpio10f4 [SGPIO0 OFFSET(4) NUMBITS(1) [], SGPIO1 OFFSET(8) NUMBITS(1) []],
    Sgpio10f8 [BYTE0 OFFSET(0) NUMBITS(8) [], BYTE2 OFFSET(16) NUMBITS(8) [],
        BIT20 OFFSET(20) NUMBITS(1) [], BYTE3 OFFSET(24) NUMBITS(8) []],
    PortConfig [PORT OFFSET(8) NUMBITS(3) []],
    NbifStrap0c [BIT18 OFFSET(18) NUMBITS(1) []]
];

const BASE: u32 = 0x0310_0000;
pub(super) const CONTROL: SmnReg<Control::Register> = SmnReg::new(BASE);
const RESET_STATUS: SmnReg<ResetStatus::Register> = SmnReg::new(BASE + 0x4);
const MSI: SmnReg<MsiControl::Register> = SmnReg::new(BASE + 0x8);
const AHCI_CAP: SmnReg<Capabilities::Register> = SmnReg::new(BASE + 0x1000);
const AHCI_PI: SmnReg = SmnReg::new(BASE + 0x100c);
const R1024: SmnReg<R1024::Register> = SmnReg::new(BASE + 0x1024);
const R10F4: SmnReg<Sgpio10f4::Register> = SmnReg::new(BASE + 0x10f4);
const R10F8: SmnReg<Sgpio10f8::Register> = SmnReg::new(BASE + 0x10f8);
const MISC: SmnReg<Misc::Register> = SmnReg::new(BASE + 0x1800);
const fn port_config(port: u32) -> SmnReg<PortConfig::Register> {
    SmnReg::new(BASE + 0x188c + port * 0x20)
}
const fn ahci_port<R: tock_registers::RegisterLongName>(port: u32, register: u32) -> SmnReg<R> {
    SmnReg::new(BASE + 0x1100 + port * 0x80 + register)
}
const fn px_is(port: u32) -> SmnReg {
    ahci_port(port, 0x10)
}
const fn px_ssts(port: u32) -> SmnReg<Det::Register> {
    ahci_port(port, 0x28)
}
const fn px_sctl(port: u32) -> SmnReg<Det::Register> {
    ahci_port(port, 0x2c)
}
const fn px_serr(port: u32) -> SmnReg {
    ahci_port(port, 0x30)
}
/// Vendor values of unknown meaning for the controller registers
/// 0x10fc, 0x180c, 0x1810 and 0x1814: (offset, clear, set).
const VENDOR: [(u32, u32, u32); 4] = [
    (0x10fc, 0x0808_0000, 0x0400_0c02),
    (0x180c, 0x0080_0000, 0xf000_006a),
    (0x1810, 0x0200_c304, 0x0200_c304),
    (0x1814, 0x4000_01e0, 0x4000_01e0),
];

/// NBIF straps of the SATA function.
pub(crate) const NBIF_STRAP: SmnReg<NbifStrap::Register> = SmnReg::new(0x1023_4400);
const NBIF_STRAP_0C: SmnReg<NbifStrap0c::Register> = SmnReg::new(0x1023_440c);
const NBIF_CLASS: SmnReg = SmnReg::new(0x1023_4434);
const NBIF_SSID: SmnReg = SmnReg::new(0x1024_204c);

const AHCI_DEVICE_ID: u32 = 0x7901;
const AHCI_CLASS: u32 = 0x01_0601;

/// Polls of the PHY-offline wait, 10 us apart: the vendor code gives up
/// silently after 1 ms.
const OFFLINE_POLLS: u32 = 100;

/// Reset task: take the PHYs offline. `smn` is the die's.
pub fn reset(smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) {
    let status = smn.reg(RESET_STATUS);
    for phy in 0..8 {
        let mut value = status.extract();
        value.modify(ResetStatus::PHYS.val(value.read(ResetStatus::PHYS) & !(1 << phy)));
        status.set(value.get());
    }
    smn.reg(CONTROL).modify(Control::BIT0::CLEAR);
    for port in 0..PORTS {
        smn.reg(px_sctl(port)).modify(Det::DET::Offline);
        let offline = || smn.reg(px_ssts(port)).matches_all(Det::DET::Offline);
        let mut polls = 0;
        while !offline() && polls < OFFLINE_POLLS {
            delay.delay_us(10);
            polls += 1;
        }
    }
}

/// Env task: enable the ports and set the controller up as AHCI. `die`
/// selects the die (the PCI IRQ router belongs to die 0's FCH), `smn` is its
/// SMN and `fch` its FCH ([`super::acpimmio_base`]). `subsystem_id` is the
/// board's (vendor PCD 0x15f).
pub fn init(
    fch: &Fch<impl Bus>,
    die: u8,
    smn: &Smn<impl ConfigRegionAccess>,
    subsystem_id: u32,
    delay: &mut impl DelayNs,
) {
    let misc = smn.reg(MISC);
    misc.modify(Misc::UNLOCK::SET);

    // Enable each port with a COMRESET.
    for port in 0..PORTS {
        let mut value = misc.extract();
        value.modify(Misc::PORT_DISABLE.val(value.read(Misc::PORT_DISABLE) & !(1 << port)));
        misc.set(value.get());
        smn.reg(px_sctl(port)).modify(Det::DET::Comreset);
        delay.delay_us(1000);
        smn.reg(px_sctl(port)).modify(Det::DET::Idle);
    }

    smn.reg(CONTROL).modify(
        Control::BIT1::CLEAR
            + Control::BIT2::SET
            + Control::BIT3::CLEAR
            + Control::BIT24::CLEAR,
    );
    let vendor = |(offset, clear, set): (u32, u32, u32)| {
        super::super::nbio::rmw(BASE + offset, clear, set)
    };
    super::super::nbio::apply(smn, [vendor(VENDOR[0])]);
    smn.reg(R10F8)
        .modify(Sgpio10f8::BYTE3.val(0xff));
    smn.reg(R1024).modify(R1024::BIT0::CLEAR);
    super::super::nbio::apply(smn, VENDOR[1..].iter().copied().map(vendor));
    misc.modify(Misc::BIT13::SET);
    smn.reg(NBIF_STRAP_0C)
        .modify(NbifStrap0c::BIT18::SET);
    // No disabled ports
    misc.modify(Misc::PORT_DISABLE.val(0));

    // No eSATA ports, no SGPIO: its pins (IOMUX 5 and 6 of the die's FCH)
    // are GPIOs.
    fch.reg(iomux::at::<u8, ()>(5)).set(0);
    fch.reg(iomux::at::<u8, ()>(6)).set(0);
    smn.reg(R10F8)
        .modify(Sgpio10f8::BYTE0.val(0) + Sgpio10f8::BYTE2.val(0));
    // The vendor code clears bit 20 again.
    smn.reg(R10F8).modify(Sgpio10f8::BIT20::CLEAR);
    // SGPIO0/1 port select (vendor PCD 0x15c) is 0 for both, so bit 8 twice.
    smn.reg(R10F4)
        .modify(Sgpio10f4::SGPIO0::CLEAR + Sgpio10f4::SGPIO1::CLEAR);

    for port in 0..PORTS {
        smn.reg(port_config(port))
            .modify(PortConfig::PORT.val(port));
        smn.reg(px_serr(port)).set(u32::MAX);
        smn.reg(px_is(port)).set(0);
    }

    smn.reg(NBIF_CLASS).set(AHCI_CLASS);
    smn.reg(NBIF_STRAP)
        .modify(NbifStrap::DEVICE_ID.val(AHCI_DEVICE_ID));
    smn.reg(NBIF_SSID).set(subsystem_id);

    if die == 0 {
        let misc = irq::read(fch.bus(), irq::MISC, irq::Mode::Pic);
        irq::write(fch.bus(), irq::MISC, irq::Mode::Pic, (misc & 0xf) | 0xf0);
    }

    misc.modify(Misc::UNLOCK::CLEAR);
}

/// Late task (ReadyToBoot in the vendor firmware): disable the ports without
/// a device and report only the remaining ones, and copy the MSI capability
/// of the SATA function `sata` into the controller. `smn` is the SMN of die
/// `die`. Returns the ports implemented.
pub fn late<P: ConfigRegionAccess>(
    smn: &Smn<impl ConfigRegionAccess>,
    die: u8,
    pci: P,
    sata: PciAddress,
) -> u32 {
    let mut implemented = smn.reg(AHCI_PI).get();
    let misc = smn.reg(MISC);
    misc.modify(Misc::UNLOCK::SET);
    for port in 0..PORTS {
        if smn.reg(px_ssts(port)).matches_all(Det::DET::Present) {
            continue;
        }
        let mut value = misc.extract();
        value.modify(Misc::PORT_DISABLE.val(value.read(Misc::PORT_DISABLE) | 1 << port));
        misc.set(value.get());
        implemented &= !(1 << port);
    }
    if implemented == 0 {
        implemented = 1;
    }
    smn.reg(AHCI_PI).set(implemented);
    smn.reg(AHCI_CAP)
        .modify(Capabilities::PORTS.val(implemented.count_ones() - 1));

    // Vendor PCD 0x167: MSI capability multiple message capable.
    // SAFETY: `sata` is the enumerated SATA function.
    let msi = unsafe { ConfigRegister::<_, MsiCap::Register>::new(pci, sata, 0xa0) };
    smn.reg(MSI).modify(
        MsiControl::MULTIPLE_CAPABLE.val(msi.read(MsiCap::MULTIPLE_CAPABLE)),
    );
    misc.modify(Misc::UNLOCK::CLEAR);

    fstart_log::debug!(
        "SATA {}: ports implemented {}",
        die,
        fstart_log::Hex(implemented.into())
    );
    implemented
}
