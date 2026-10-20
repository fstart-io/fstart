// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCHs of the other dies of SP4 parts: the vendor FchMultiFchPei
//! (ffdcb817) and FchMultiFchDxe (0348) with their defaults (sp4r2 report
//! FCH_TAISHAN section 4). These FCHs have no LPC, SPI, SMBus, HPET, RTC,
//! ACPI PM or SMI setup; their xHCI, SATA and xGbE are reached through the
//! SMN space of their die.
//!
//! The loops take all dies (die 0 first, as [`crate::zeppelin::topology::dies`]
//! yields them) and the board's [`Devices`] per die, indexed by die.
use super::{
    AB_INDEX, Acpimmio, Bus, Devices, NbifStrap, PM_INDEX, abcfg_update, index_read, index_write,
    legacy_sd_off, sata,
    usb::{self, ResetControl},
};
use crate::{
    fam17h::registers::{DfReg, Smn, SmnReg},
    zeppelin::{Die, Package},
};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// The vendor code allocates 8 KiB of the die's non-PCI pool; use the 64 KiB
/// at +1 MiB.
const ACPIMMIO_OFFSET: u32 = 1 << 20;

/// The ACPI MMIO of die `die`'s FCH. Die 0's is at 0xfed80000, the others
/// are moved into the die's non-PCI MMIO pool (DF window top - 8 MiB, see
/// [`crate::zeppelin::topology::NONPCI_MMIO_SIZE`]), at `nonpci_mmio_base`.
pub const fn acpimmio_base(die: u8, nonpci_mmio_base: u32) -> Acpimmio {
    if die == 0 {
        Acpimmio::DIE0
    } else {
        Acpimmio(nonpci_mmio_base + ACPIMMIO_OFFSET)
    }
}

/// PMxD5[1:0]: decode the ACPI MMIO base in PMxD6.
const PM_ACPIMMIO_SELECT: u8 = 0xd5;
/// PMxD6: 16 bits, base >> 16.
const PM_ACPIMMIO_BASE: u8 = 0xd6;
const PM_AB_REG_BAR: u8 = 0xe0;
register_bitfields![u32,
    DfIoBase [READ_ENABLE OFFSET(0) NUMBITS(1) [], WRITE_ENABLE OFFSET(1) NUMBITS(1) []],
    DfIoLimit [DESTINATION OFFSET(0) NUMBITS(8) []]
];
/// D18F0xC0/C4: DF I/O space base/limit 0.
const DF_IO_BASE0: DfReg<DfIoBase::Register> = DfReg::new(0, 0xc0);
const DF_IO_LIMIT0: DfReg<DfIoLimit::Register> = DfReg::new(0, 0xc4);

/// Vendor ffdcb7de, run while die 0's DF sends I/O 0-0xfff to the die: move
/// the die's ACPI MMIO block, then IOMUX, A-Link, clock generator and legacy
/// SD through it. Nothing may print here; the console is on die 0's LPC bus.
fn secondary_legacy(bus: &mut impl Bus, mmio: Acpimmio, package: Package) {
    let base = mmio.0 >> 16;
    index_write(bus, PM_INDEX, PM_ACPIMMIO_BASE, base as u8);
    index_write(bus, PM_INDEX, PM_ACPIMMIO_BASE + 1, (base >> 8) as u8);
    let select = index_read(bus, PM_INDEX, PM_ACPIMMIO_SELECT);
    index_write(bus, PM_INDEX, PM_ACPIMMIO_SELECT, (select & 0xfc) | 1);

    // SP4: IOMUX pin 2 function 1
    if package == Package::Sp4 {
        bus.set(mmio.iomux::<u8, ()>(2), 1);
    }

    // A-Link index at 0xcd8 just for this write (low PMxE0 word only).
    index_write(bus, PM_INDEX, PM_AB_REG_BAR, AB_INDEX as u8);
    index_write(bus, PM_INDEX, PM_AB_REG_BAR + 1, (AB_INDEX >> 8) as u8);
    abcfg_update(bus, 0x10050, 1 << 2, 0);
    index_write(bus, PM_INDEX, PM_AB_REG_BAR, 0);
    index_write(bus, PM_INDEX, PM_AB_REG_BAR + 1, 0);

    // Clock generator; these differ from die 0's values.
    for (offset, clear, set) in [
        (0x10, 0x3, 0x1),
        (0x10, 0x1ff0, 0x40),
        (0x34, 0xff80_0000, 0x1e00_0000),
        (0x14, 0x00ff_ff00, 0x3d700),
        (0x18, 0xffff_0000, 0x3e_0000),
        (0x1c, 0x1e000, 0x2000),
        (0x1c, 0x1e_0000, 0),
        (0x1c, 0x0c00_0000, 0x0800_0000),
        (0x1c, 0x1e00, 0x400),
        (0x40, 1 << 25, 1 << 25),
        (0x40, 1 << 30, 1 << 30),
    ] {
        bus.update(mmio.misc::<u32, ()>(offset), clear, set);
    }

    legacy_sd_off(bus, mmio);
}

/// FchMultiFchPei: the legacy part through I/O routed to each other die,
/// then the SATA reset task of the die. `nonpci_mmio_base` gives a die's
/// non-PCI MMIO pool (DF code). Runs after [`super::reset_init`].
pub fn reset<P: ConfigRegionAccess + Copy>(
    bus: &mut impl Bus,
    dies: &[Die<P>],
    package: Package,
    nonpci_mmio_base: impl Fn(u8) -> u32,
    delay_us: &mut impl FnMut(u32),
) {
    let Some(die0) = dies.first() else { return };
    let df0 = die0.df();
    for die in &dies[1..] {
        let mmio = acpimmio_base(die.index(), nonpci_mmio_base(die.index()));
        fstart_log::debug!(
            "FCH {}: ACPI MMIO at {}",
            die.index(),
            fstart_log::Hex(mmio.0.into())
        );

        // Route I/O 0-0xfff to the die's IOMS.
        let (io_base, io_limit) = (df0.reg(DF_IO_BASE0), df0.reg(DF_IO_LIMIT0));
        let saved = (io_base.get(), io_limit.get());
        io_base.write(DfIoBase::READ_ENABLE::SET + DfIoBase::WRITE_ENABLE::SET);
        io_limit.write(DfIoLimit::DESTINATION.val(die.ioms_fabric_id().into()));
        secondary_legacy(bus, mmio, package);
        io_base.set(saved.0);
        io_limit.set(saved.1);

        // Vendor PCD 0x8d: SATA enabled on every die.
        sata::reset(&die.smn(), delay_us);
    }
}

register_bitfields![u32,
    XgbePort [DISABLE OFFSET(0) NUMBITS(1) []]
];
const fn nbif_xgbe_strap(port: u32) -> SmnReg<NbifStrap::Register> {
    SmnReg::new(0x1023_4800 + 0x200 * port)
}
const fn xgbe_port_control(port: u32) -> SmnReg<XgbePort::Register> {
    SmnReg::new(0x1631_f030 + 4 * port)
}
fn strap_enabled(smn: &Smn<impl ConfigRegionAccess>, strap: SmnReg<NbifStrap::Register>) -> bool {
    smn.reg(strap).is_set(NbifStrap::ENABLE)
}
fn strap_off(smn: &Smn<impl ConfigRegionAccess>, strap: SmnReg<NbifStrap::Register>) {
    smn.reg(strap).modify(NbifStrap::ENABLE::CLEAR);
}

/// af49 (FchTaishanDxe) and the xGbE part of FchMultiFchDxe: the xGbE ports
/// without an NBIF strap are switched off. `smn` is the die's.
pub fn xgbe_init(smn: &Smn<impl ConfigRegionAccess>) {
    for port in 0..4 {
        if !strap_enabled(smn, nbif_xgbe_strap(port)) {
            smn.reg(xgbe_port_control(port))
                .modify(XgbePort::DISABLE::SET);
        }
    }
}

fn devices(devices: &[Devices], die: u8) -> Devices {
    devices.get(usize::from(die)).copied().unwrap_or(Devices {
        xhci: false,
        sata: false,
    })
}

/// The USB part of FchMultiFchPei for the other dies: odd die pairs (2, 3,
/// 6, 7) are held in reset, the others brought up if the board enables
/// their xHCI. Runs after [`usb::init`] of die 0.
pub fn usb<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    board: &[Devices],
    package: Package,
    delay_us: &mut impl FnMut(u32),
) {
    for die in dies.iter().skip(1) {
        let smn = &die.smn();
        smn.reg(usb::R14C).modify(usb::R14c::BIT31::SET);
        if die.index() & 2 != 0 {
            strap_off(smn, usb::NBIF_STRAP);
            smn.reg(usb::RESET_CONTROL).modify(
                ResetControl::BIT0::SET + ResetControl::BIT1::SET + ResetControl::BIT2::SET,
            );
        } else if devices(board, die.index()).xhci {
            usb::init(smn, die.index(), package, delay_us);
        } else {
            strap_off(smn, usb::NBIF_STRAP);
        }
    }
}

const AMD_VENDOR: u32 = 0x1022;

/// FchMultiFchDxe: SATA and xGbE of the other dies whose internal bridge 8.1
/// exists. Runs after [`super::env_init`] of die 0.
pub fn env<P: ConfigRegionAccess + Copy>(
    bus: &mut impl Bus,
    dies: &[Die<P>],
    board: &[Devices],
    nonpci_mmio_base: impl Fn(u8) -> u32,
    subsystem_id: u32,
    delay_us: &mut impl FnMut(u32),
) {
    for die in dies.iter().skip(1) {
        let smn = &die.smn();
        let bridge = PciAddress::new(0, die.bus(), 8, 1);
        // SAFETY: a config read of a possibly absent function reads all ones.
        if unsafe { die.pci().read(bridge, 0) } & 0xffff != AMD_VENDOR {
            strap_off(smn, sata::NBIF_STRAP);
            (0..4).for_each(|port| strap_off(smn, nbif_xgbe_strap(port)));
            continue;
        }

        if !strap_enabled(smn, sata::NBIF_STRAP) {
            // No SATA lanes on this die: switch the controller off.
            smn.reg(sata::CONTROL).modify(sata::Control::BIT0::SET);
            smn.reg(sata::CONTROL).modify(sata::Control::BIT11::SET);
        } else if !devices(board, die.index()).sata {
            strap_off(smn, sata::NBIF_STRAP);
        } else {
            let mmio = acpimmio_base(die.index(), nonpci_mmio_base(die.index()));
            sata::init(bus, die.index(), smn, mmio, subsystem_id, delay_us);
        }

        xgbe_init(smn);
    }
}

/// The SATA late task of the other dies (vendor SMM SW SMI 0xb4).
/// `sata_function` gives the enumerated SATA function of a die, `None` if
/// the board disables it or it wasn't found. Runs before
/// [`super::late_init`].
pub fn late<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    sata_function: impl Fn(u8) -> Option<PciAddress>,
) {
    for die in dies.iter().skip(1) {
        if let Some(sata) = sata_function(die.index()) {
            sata::late(&die.smn(), die.index(), die.pci(), sata);
        }
    }
}
