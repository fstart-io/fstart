// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCHs of the other dies of SP4 parts: the vendor FchMultiFchPei
//! (ffdcb817) and FchMultiFchDxe (0348) with their defaults (sp4r2 report
//! FCH_TAISHAN section 4). These FCHs have no LPC, SPI, SMBus, HPET, RTC,
//! ACPI PM or SMI setup; their xHCI, SATA and xGbE are reached through the
//! SMN space of their die.
//!
//! The loops take all dies (die 0 first, as [`crate::df::dies`]
//! yields them) and the board's [`Devices`] per die, indexed by die.
use super::{
    Devices, NbifStrap, sata,
    usb::{self, ResetControl},
};
use crate::ccx::tsc::DelayNs;
use crate::cpu::Package;
use crate::fch::{Acpimmio, Bus, Fch, PM_INDEX, alink, indexed_io, iomux, misc};
use crate::{
    df::map::{self, IoBase, IoLimit},
    smn::{Smn, SmnReg},
    soc::zeppelin::Die,
    table,
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
/// [`crate::soc::zeppelin::NONPCI_MMIO_SIZE`]), at `nonpci_mmio_base`.
pub const fn acpimmio_base(die: u8, nonpci_mmio_base: u32) -> Acpimmio {
    if die == 0 {
        Acpimmio::DIE0
    } else {
        Acpimmio(nonpci_mmio_base + ACPIMMIO_OFFSET)
    }
}

register_bitfields![u8,
    /// PMxD5: decode the ACPI MMIO base in PMxD6.
    AcpimmioSelect [SELECT OFFSET(0) NUMBITS(2) [Base = 1]]
];
/// Through the PM index/data pair: PMxD5, PMxD6..D7 (base >> 16) and the
/// A-Link I/O base PMxE0..E1.
const PM_ACPIMMIO_SELECT: u8 = 0xd5;
const PM_ACPIMMIO_BASE: u8 = 0xd6;
const PM_AB_REG_BAR: u8 = 0xe0;

/// Vendor ffdcb7de, run while die 0's DF sends I/O 0-0xfff to the die: move
/// the die's ACPI MMIO block, then IOMUX, A-Link, clock generator and legacy
/// SD through it. Nothing may print here; the console is on die 0's LPC bus.
fn secondary_legacy<B: Bus>(fch: &Fch<B>, package: Package) {
    let bus = fch.bus();
    let pm = |register| indexed_io::<B, ()>(bus, PM_INDEX, register);
    let pm_word = |register, value: u16| {
        let [low, high] = value.to_le_bytes();
        pm(register).set(low);
        pm(register + 1).set(high);
    };
    pm_word(PM_ACPIMMIO_BASE, (fch.acpimmio().0 >> 16) as u16);
    indexed_io::<B, AcpimmioSelect::Register>(bus, PM_INDEX, PM_ACPIMMIO_SELECT)
        .modify(AcpimmioSelect::SELECT::Base);

    // SP4: IOMUX pin 2 function 1
    if package == Package::Sp4 {
        fch.reg(iomux::at::<u8, ()>(2)).set(1);
    }

    // A-Link index at 0xcd8 just for this write (low PMxE0 word only).
    pm_word(PM_AB_REG_BAR, alink::INDEX);
    alink::apply(bus, &[table::rmw(0x10050, 1 << 2, 0)]);
    pm_word(PM_AB_REG_BAR, 0);

    // Clock generator; these differ from die 0's values.
    fch.apply::<u32>([
        table::rmw(misc::at(0x10), 0x3, 0x1),
        table::rmw(misc::at(0x10), 0x1ff0, 0x40),
        table::rmw(misc::at(0x34), 0xff80_0000, 0x1e00_0000),
        table::rmw(misc::at(0x14), 0x00ff_ff00, 0x3d700),
        table::rmw(misc::at(0x18), 0xffff_0000, 0x3e_0000),
        table::rmw(misc::at(0x1c), 0x1e000, 0x2000),
        table::rmw(misc::at(0x1c), 0x1e_0000, 0),
        table::rmw(misc::at(0x1c), 0x0c00_0000, 0x0800_0000),
        table::rmw(misc::at(0x1c), 0x1e00, 0x400),
        table::rmw(misc::at(0x40), 1 << 25, 1 << 25),
        table::rmw(misc::at(0x40), 1 << 30, 1 << 30),
    ]);

    fch.legacy_sd_off();
}

/// FchMultiFchPei: the legacy part through I/O routed to each other die,
/// then the SATA reset task of the die. `nonpci_mmio_base` gives a die's
/// non-PCI MMIO pool (DF code). Runs after [`super::reset_init`].
pub fn reset<P: ConfigRegionAccess + Copy>(
    bus: &impl Bus,
    dies: &[Die<P>],
    package: Package,
    nonpci_mmio_base: impl Fn(u8) -> u32,
    delay: &mut impl DelayNs,
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

        // Route I/O 0-0xfff (pair 0) to the die's IOMS.
        let (io_base, io_limit) = (df0.reg(map::io_base(0)), df0.reg(map::io_limit(0)));
        let saved = (io_base.get(), io_limit.get());
        io_base.write(IoBase::RE::SET + IoBase::WE::SET);
        io_limit.write(IoLimit::DST_FABRIC_ID.val(die.ioms_fabric_id().into()));
        secondary_legacy(&Fch::new(bus, mmio), package);
        io_base.set(saved.0);
        io_limit.set(saved.1);

        // Vendor PCD 0x8d: SATA enabled on every die.
        sata::reset(&die.smn(), delay);
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
    delay: &mut impl DelayNs,
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
            usb::init(smn, die.index(), package, delay);
        } else {
            strap_off(smn, usb::NBIF_STRAP);
        }
    }
}

const AMD_VENDOR: u32 = 0x1022;

/// FchMultiFchDxe: SATA and xGbE of the other dies whose internal bridge 8.1
/// exists. Runs after [`super::env_init`] of die 0.
pub fn env<P: ConfigRegionAccess + Copy>(
    bus: &impl Bus,
    dies: &[Die<P>],
    board: &[Devices],
    nonpci_mmio_base: impl Fn(u8) -> u32,
    subsystem_id: u32,
    delay: &mut impl DelayNs,
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
            sata::init(&Fch::new(bus, mmio), die.index(), smn, subsystem_id, delay);
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
