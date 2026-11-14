//! Secondary FCHs: Naples dies 1..7 and the Rome/Milan socket-1 FCH
//! (FCH_SP3 §4).
//!
//! # Access
//!
//! Everything the donors do on a secondary FCH is ACPI-MMIO/PM or SMN
//! except the A-Link (ABCFG) program, which needs the `CD8h/CDCh` I/O
//! ports of that FCH. So:
//!
//! - ACPI-MMIO/PM through the FCH's SMN view ([`acpimmio::SMN_BASE`] on its
//!   die/socket bus), or, as the donors do, an ALTMMIO relocation
//!   ([`relocate`]) when byte-only registers (IOMUX) must be written;
//! - the A-Link program and PMIO with the DF legacy I/O route pointed at the
//!   FCH ([`with_legacy_io`], D18F0xC0/C4) for the duration only.
//!
//! The secondary's [`FchHw`] is built by the caller: `acpi` = its window,
//! `io` = the host ports (routed while needed), `bus` = its root bus. Bus
//! discovery is the DF's (`crate::df`): D(18+n)F0x84 on Naples, the IOMS3
//! FICAA read (`D19F4x5C = 001B0085h`) on Rome/Milan (§4.2).

use crate::fch::acpimmio::{PM_AB_REG_BAR, PM_SD_CONTROL, PM_SD_ENABLE};
use fstart_pci::{ConfigRegionAccess, PciAddress};

use super::acpimmio::{self, PmIo};
use super::{FchHw, acpimmio::AoacDevice, alink};
use crate::cpu::Silicon;
use crate::window::Window;
use crate::window::update;

/// DF IO base/limit pair 0 (D18F0xC0/xC4) used for the temporary route.
const DF_IO_BASE: u16 = 0xC0;
const DF_IO_LIMIT: u16 = 0xC4;
fn df_f0() -> PciAddress {
    PciAddress::new(0, 0, 0x18, 0)
}

/// The D18F0xC4 value routing legacy I/O to Naples die `die` (1..7) or to
/// the Rome/Milan socket-1 IOMS3 (FCH_SP3 §4, §9).
pub const fn route_target(generation: Silicon, die: u8) -> u32 {
    match generation {
        Silicon::Zp => 0x24 + 0x20 * (die as u32 - 1),
        Silicon::Ssp | Silicon::Gn => 0x3B,
    }
}

/// Run `f` with I/O pair 0 of the DF routing legacy I/O to `target`
/// (base RE|WE = 3, limit = `target`); the previous pair is restored after
/// (the donor restores 0, which only matches if the pair was unused).
pub fn with_legacy_io<P: ConfigRegionAccess, R>(pci: &P, target: u32, f: impl FnOnce() -> R) -> R {
    // SAFETY: the DF IO pair 0 is owned by the boot flow at this point.
    let saved = unsafe {
        (
            pci.read(df_f0(), DF_IO_BASE),
            pci.read(df_f0(), DF_IO_LIMIT),
        )
    };
    unsafe {
        pci.write(df_f0(), DF_IO_BASE, 3);
        pci.write(df_f0(), DF_IO_LIMIT, target);
    }
    let r = f();
    // SAFETY: as above.
    unsafe {
        pci.write(df_f0(), DF_IO_BASE, saved.0);
        pci.write(df_f0(), DF_IO_LIMIT, saved.1);
    }
    r
}

/// Relocate the routed FCH's ACPI-MMIO to `base` through PMIO (call inside
/// [`with_legacy_io`]); the DF must route `base` to that die/socket.
pub fn relocate<I: Window>(io: &I, base: u32) {
    acpimmio::relocate(&PmIo(io), base);
}

/// Legacy SD off on a secondary (the env af55 subset the multi-FCH PEIMs
/// run).
fn sd_off<W: Window>(acpi: &W) {
    update::<u8, _>(acpi, PM_SD_CONTROL.offset, 0x40, 0);
    update::<u8, _>(acpi, PM_SD_ENABLE.offset, 0x01, 0);
    acpimmio::aoac_power(acpi, AoacDevice::SD, false);
}

/// Naples secondary die policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaplesDie {
    /// Die 1..7 (socket 1 = 4..7).
    pub die: u8,
    /// ALTMMIO base if the caller relocates (donor path); `None` uses the
    /// SMN view that `hw.acpi()` already is.
    pub alt_mmio: Option<u32>,
    pub usb: crate::fch::taishan::usb::DieUsb,
    pub sata: bool,
}

impl NaplesDie {
    /// Donor defaults (FCH_SP3 §4.1): USB per die, SATA on die 2 only.
    pub const fn default_for(die: u8) -> Self {
        Self {
            die,
            alt_mmio: None,
            usb: crate::fch::taishan::usb::DieUsb::default_for(die),
            sata: die == 2,
        }
    }
}

/// FchMultiFchPei for one Naples die (FCH_SP3 §4.1). `hw` is that die's
/// FCH (`acpi` = SMN view or relocated window, `bus` = the die's bus).
pub fn naples_die<H: FchHw>(hw: &H, policy: &NaplesDie, resume: bool) {
    debug_assert!((1..8).contains(&policy.die));
    with_legacy_io(hw.pci(), route_target(Silicon::Zp, policy.die), || {
        if let Some(base) = policy.alt_mmio {
            relocate(hw.io(), base);
        }
        let pmio = PmIo(hw.io());
        pmio.write16(PM_AB_REG_BAR.offset, alink::INDEX);
        alink::modify(hw.io(), alink::MISC_CTL, alink::RESET_CPU_ON_SYNC_FLOOD, 0);
        pmio.write16(PM_AB_REG_BAR.offset, 0);
    });
    let acpi = hw.acpi();
    // IOMUX 02h = 1 (byte-only: see the SMN caveat in `gpio`).
    acpi.write8(acpimmio::IOMUX + 2, 1);
    crate::window::apply(acpi, super::tables::CLOCK_GEN_NAPLES_SECONDARY);
    sd_off(acpi);
    let smn = hw.smn();
    let delay = &mut fstart_core::services::TimerDelay(hw.timer());
    // S3 resume is not supported: the cold path.
    let _ = resume;
    let package = crate::cpu::Package::Sp3;
    crate::fch::taishan::usb::reset(&smn, policy.die, policy.usb, package, delay);
    if policy.sata {
        crate::fch::taishan::sata::reset(&smn, delay);
    } else {
        crate::fch::taishan::sata::hold(&smn);
    }
}

/// FchMultiFchDxe for one Naples die: a die without SATA hides its SATA
/// and xGbE functions.
pub fn naples_die_env<H: FchHw>(hw: &H, sata: bool) {
    if !sata {
        crate::fch::taishan::sata::hide(&hw.smn());
        crate::fch::taishan::multi::xgbe_hide(&hw.smn(), 0xF);
    }
}

/// FchHuashanMultiFchPei for socket 1 (FCH_SP3 §4.2). `hw` is the socket-1
/// FCH (`acpi` = SMN view, `bus` = socket 1's bus), `smu` socket 1's SMU.
pub fn huashan_socket1<H: FchHw, S: crate::smu::SmuMessages + ?Sized>(
    hw: &H,
    generation: Silicon,
    smu: &S,
    usb: &crate::fch::huashan::usb::UsbPolicy,
    sata: &crate::fch::huashan::sata::SataPolicy,
    resume: bool,
) -> Result<(), crate::smu::Error> {
    with_legacy_io(hw.pci(), route_target(generation, 1), || {
        let pmio = PmIo(hw.io());
        pmio.write16(PM_AB_REG_BAR.offset, alink::INDEX);
        alink::clock_gating_secondary(hw, generation);
        pmio.write16(PM_AB_REG_BAR.offset, 0);
        sd_off(hw.acpi());
    });
    crate::fch::huashan::usb::reset(hw, generation, 1, smu, usb, resume)?;
    crate::fch::huashan::sata::reset(hw, generation, sata);
    Ok(())
}

/// FchHuashanMultiFchDxe: socket 1's SATA env (all four controllers off at
/// the donor default). Its PCI_INTR write goes to socket 1's ports, so
/// the legacy I/O is routed there meanwhile.
pub fn huashan_socket1_env<H: FchHw>(
    hw: &H,
    generation: Silicon,
    sata: &crate::fch::huashan::sata::SataPolicy,
) -> bool {
    with_legacy_io(hw.pci(), route_target(generation, 1), || {
        crate::fch::huashan::sata::env(hw, generation, sata)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;
    use crate::fch::model::smn;

    #[test]
    fn route_targets() {
        assert_eq!(route_target(Silicon::Zp, 1), 0x24);
        assert_eq!(route_target(Silicon::Zp, 7), 0xE4);
        assert_eq!(route_target(Silicon::Gn, 1), 0x3B);
    }

    #[test]
    fn route_is_restored() {
        let hw = fch();
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x18, 0, 0xc0), 0x1003);
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x18, 0, 0xc4), 0x0fff_0000);
        let seen = with_legacy_io(&hw.pci, 0x3b, || {
            hw.pci.0.borrow().config[&(0, 0x18, 0, 0xc4)]
        });
        assert_eq!(seen, 0x3b);
        assert_eq!(hw.pci.0.borrow().config[&(0, 0x18, 0, 0xc0)], 0x1003);
    }

    #[test]
    fn rome_socket1() {
        let mut hw = fch();
        hw.bus = 0x80;
        let smu = crate::fch::model::Smu::default();
        huashan_socket1(
            &hw,
            Silicon::Ssp,
            &smu,
            &crate::fch::huashan::usb::UsbPolicy::default_for(Silicon::Ssp, 1),
            &crate::fch::huashan::sata::SataPolicy::socket1(),
            false,
        )
        .unwrap();
        assert_eq!(smu.ids().last(), Some(&0x0a));
        assert_eq!(smn(&hw.pci, 0x80, 0x02d0_001c), 1 << 31);
        assert_eq!(smn(&hw.pci, 0x80, 0x13e3_9804), 7);
        assert_eq!(hw.io.at(0xcdc, 0xc001_0050) & 4, 0);
        assert_eq!(hw.io.at(0xcdc, 0xc000_0094), 0, "no 94h row on socket 1");
        assert_eq!(hw.acpi.writes_to(0x1e72), vec![0]);
        assert_eq!(hw.io.at(0xcd7, 0xe0), 0);
    }

    #[test]
    fn naples_die2() {
        let mut hw = fch();
        hw.bus = 0x40;
        naples_die(&hw, &NaplesDie::default_for(2), false);
        assert_eq!(smn(&hw.pci, 0x40, 0x0310_112c), 4, "SATA on, ports offline");
        assert_eq!(smn(&hw.pci, 0x40, 0x16d8_0100), 7, "USB held in reset");
        assert_eq!(hw.acpi.get(0xe34, 4), 0x1e00_0000);
        assert_eq!(hw.acpi.get(0xd02, 1), 1);
    }
}
