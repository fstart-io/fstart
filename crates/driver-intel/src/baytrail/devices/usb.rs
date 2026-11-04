//! USB: the EHCI (00:1d.0) and xHCI (00:14.0) controllers, their PHYs and
//! port routing (coreboot `ehci.c` and `xhci.c`).

use super::{Bar, Function, poll_bar};
use crate::MmioBar;
use crate::baytrail::iosf::{Iosf, USBPHY, USHPHY};
use crate::baytrail::{Stepping, lpc, pmc};
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{Readable, Writeable};

/// One USB 2 port's PHY tuning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usb2Phy {
    pub lane: u32,
    pub rcomp_hs_pullup: u32,
}

/// USB policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbConfig {
    /// Route every port to the xHCI and hide the EHCI.
    pub route_to_xhci: bool,
    pub usb2_port_disable_mask: u16,
    pub usb3_port_disable_mask: u16,
    /// USB 2 PHY bandgap compensation; 0 means 0x4700.
    pub usb2_comp_bg: u32,
    pub usb2_phy: [Usb2Phy; 4],
}

impl UsbConfig {
    pub const fn new() -> Self {
        Self {
            route_to_xhci: false,
            usb2_port_disable_mask: 0,
            usb3_port_disable_mask: 0,
            usb2_comp_bg: 0,
            usb2_phy: [Usb2Phy {
                lane: 0,
                rcomp_hs_pullup: 0,
            }; 4],
        }
    }
}

impl Default for UsbConfig {
    fn default() -> Self {
        Self::new()
    }
}

// EHCI configuration and MMIO registers.
const EHCI_CMD_STS: u16 = 0x04;
const INTRDIS: u32 = 1 << 10;
const EHCI_SBRN_FLA_PWC: u16 = 0x60;
const PORTWKIMP: u32 = 1 << 16;
const PORTWKCAPMASK: u32 = 0x3ff << 17;
const EHCI_USB2PDO: u16 = 0x64;
const USB2CMD: u32 = 0x20;
const USB2CMD_ASE: u32 = 1 << 5;
const USB2CMD_PSE: u32 = 1 << 4;
const USB2CMD_HCRESET: u16 = 1 << 1;
const USB2CMD_RS: u32 = 1 << 0;
const USB2STS: u32 = 0x24;
const USB2STS_HCHALT: u32 = 1 << 12;
const RCBA_EHCI_DIS: u32 = 1 << 0;

// xHCI configuration and MMIO registers.
const XHCI_USB2PR: u16 = 0xd0;
const XHCI_USB2PRM: u16 = 0xd4;
const XHCI_USB3PR: u16 = 0xd8;
const XHCI_USB3PRM: u16 = 0xdc;
const XHCI_USB2PDO: u16 = 0xe4;
const XHCI_USB3PDO: u16 = 0xe8;
const fn usb3_portsc(port: u32) -> u32 {
    0x4e0 + port * 0x10
}
const PORTSC_CHST: u32 = 0x7f << 17;
const PORTSC_WRC: u32 = 1 << 19;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_WPR: u32 = 1 << 31;
/// The fuses are wrong on Bay Trail-M: fixed port maps.
const USB2_PORT_MAP: u32 = 0xf;
const USB3_PORT_COUNT: u32 = 1;
const USB3_PORT_MAP: u32 = 0x1;
/// 100 ms.
const RESET_TIMEOUT_US: u32 = 100_000;

/// Open the per-port disable override, run `f`, close it again.
fn with_port_disable_override(f: impl FnOnce()) {
    let pm = pmc::pm();
    pm.write16(pmc::UPRWC, pm.read16(pmc::UPRWC) | pmc::UPRWC_WR_EN);
    f();
    pm.write16(pmc::UPRWC, pm.read16(pmc::UPRWC) & !pmc::UPRWC_WR_EN);
}

fn usb2_phy_init<P: ConfigRegionAccess>(config: &UsbConfig, iosf: &Iosf<P>) {
    let comp_bg = if config.usb2_comp_bg == 0 {
        0x4700
    } else {
        config.usb2_comp_bg
    };
    iosf.write(USBPHY, 0x7f04, comp_bg);
    for (port, phy) in (0..4u32).zip(config.usb2_phy) {
        iosf.write(USBPHY, 0x4100 + 0x100 * port, phy.lane);
        iosf.write(USBPHY, 0x4122 + 0x100 * port, phy.rcomp_hs_pullup);
    }
}

/// EHCI init, or its shutdown when the ports go to the xHCI.
pub fn ehci_init<P: ConfigRegionAccess>(
    config: &UsbConfig,
    iosf: &Iosf<P>,
    resume: bool,
) -> Result<(), ServiceError> {
    let ehci = Function::Ehci.device();
    if !ehci.is_present() {
        return Ok(());
    }
    let bar = Bar::of(&ehci);
    // Reset the controller, except on S3 resume.
    if let (false, Some(bar)) = (resume, bar) {
        bar.setbits16(USB2CMD, USB2CMD_HCRESET);
    }
    let rcba = lpc::rcba();
    if config.route_to_xhci {
        if let Some(bar) = bar {
            bar.clrbits32(USB2CMD, USB2CMD_RS);
            poll_bar(bar, USB2STS, USB2STS_HCHALT, USB2STS_HCHALT, 10_000)?;
            ehci.or32(EHCI_CMD_STS, INTRDIS);
            bar.clrbits32(USB2CMD, USB2CMD_ASE | USB2CMD_PSE);
        }
        ehci.and32(EHCI_SBRN_FLA_PWC, !(PORTWKIMP | PORTWKCAPMASK));
        rcba.func_dis.set(rcba.func_dis.get() | RCBA_EHCI_DIS);
        super::set_enabled(Function::Ehci, false);
    } else {
        // S0 PLL shutdown, local clock gating and dynamic PHY clock gating.
        ehci.or16(0x7a, 0x14de);
        ehci.or32(0x7c, 0x0000_000c);
        ehci.or32(0x8c, 0x0000_0001);
        iosf.modify(USBPHY, 0x4001, !0xffff_ff00, 0xce);
        rcba.usb[0].set(1);
        rcba.usb[1].set(2);
        rcba.usb[2].set(0);
        rcba.usb_240.set(rcba.usb_240.get() & !0x1f);
        rcba.usb_318.set((rcba.usb_318.get() & !0x378) | 0x7);
        rcba.usb_31c.set((rcba.usb_31c.get() & !0xc) | 0x3);
        // Clock gating, B0 and later.
        ehci.or32(0x7c, 0x0000_4000);
        rcba.usb_284.set(0xbe);
        with_port_disable_override(|| {
            ehci.write8(EHCI_USB2PDO, config.usb2_port_disable_mask as u8)
        });
    }
    usb2_phy_init(config, iosf);
    Ok(())
}

fn usb3_phy_init<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    for (reg, clear, set) in [
        (0x03c0, 0x0070_0000, 0x0050_0000),
        (0x0054, 0x001f_0000, 0x000a_0000),
        (0x8040, 0x0000_000f, 0x0000_000b),
        (0x80a8, 0x0000_00f0, 0x0000_00f0),
        (0x80b0, 0x0000_01c0, 0x0000_0000),
        (0x8080, 0x0000_0070, 0x0000_0020),
        (0x80b8, 0x0000_0002, 0x0000_0002),
        (0x0000, 0x0000_0000, 0x0004_0000),
    ] {
        iosf.modify(USHPHY, reg, clear, set);
    }
}

/// `CommonXhciHcInit()`.
fn hc_init(xhci: &EcamDevice, bar: Bar) {
    bar.modify32(0x000c, 0x0000_ffff, 0x0200_0000);
    bar.modify32(0x000c, 0xffff_ff00, 0x0000_000a);
    bar.setbits32(0x8094, 0x00a0_4000);
    bar.modify32(0x8110, !0x0000_0104, 0x0010_0800);
    bar.setbits32(0x8144, 0x0000_01c0);
    bar.modify32(0x8154, !0x0020_0008, 0x8000_2000);
    bar.modify32(0x816c, 0xfff0_8000, 0x000e_0030);
    bar.setbits32(0x8188, 0x0500_0000);
    bar.modify32(0x8174, 0xfe00_0000, 0x0100_0c0a);
    bar.modify32(0x854c, !0x2000_0000, 0);
    bar.modify32(0x8178, !0xffff_e000, 0);
    bar.setbits32(0x8164, 0x0000_00ff);
    bar.modify32(0x0010, !0x0000_0020, 0x0000_0600);
    bar.modify32(0x8058, !0x0000_0100, 0x0011_0000);
    bar.setbits32(0x8060, 0x0200_0000);
    bar.modify32(0x80f0, !0x0010_0000, 0);
    // LPM.
    bar.setbits32(0x8008, 0x0008_0000);
    bar.setbits32(0x80fc, 0x0200_0000);
    // XHCC1/XHCC2 as bytes, to keep bit 31 alone; no IIL1E (USB wake).
    xhci.and8_or8(0x41, !0x06, 0x01);
    xhci.and8_or8(0x42, 0x3c, 0x04);
    xhci.and8_or8(0x44, 0x00, 0x8f);
    xhci.and8_or8(0x45, !0xcf, 0xc6);
    xhci.and8_or8(0x46, !0x0f, 0x0f);
    bar.write32(0x8140, 0xff00_f03c);
}

fn clock_gating(xhci: &EcamDevice) {
    xhci.write16(0x40, (xhci.read16(0x40) & !0x0600) | 0x0100);
    xhci.and8_or8(0x42, !0x38, 0x04);
    xhci.write16(0x44, (xhci.read16(0x44) & !0x0030) | 0x0008);
    xhci.modify32(0xa0, !0x0008_0000, 0x0004_0000);
    xhci.write16(0xa4, 0);
    xhci.and32(0xb0, !0x0037_6000);
    xhci.write32(0x50, 0x0bce_6e5f);
}

/// Route every port to the xHCI and warm-reset the enabled USB 3 ports.
fn route_all(xhci: &EcamDevice, bar: Bar, resume: bool) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: USB ports routed to xHCI");
    xhci.write32(XHCI_USB3PR, USB3_PORT_MAP);
    xhci.write32(XHCI_USB2PR, USB2_PORT_MAP);
    if resume {
        return Ok(());
    }
    let disabled = xhci.read32(XHCI_USB3PDO);
    for port in (0..USB3_PORT_COUNT).filter(|port| disabled & (1 << port) == 0) {
        let portsc = usb3_portsc(port);
        bar.setbits32(portsc, PORTSC_WPR);
        poll_bar(bar, portsc, PORTSC_WRC, PORTSC_WRC, RESET_TIMEOUT_US)?;
        bar.modify32(portsc, !PORTSC_PED, PORTSC_CHST);
    }
    Ok(())
}

/// xHCI init for boot or resume.
pub fn xhci_init<P: ConfigRegionAccess>(
    config: &UsbConfig,
    iosf: &Iosf<P>,
    resume: bool,
) -> Result<(), ServiceError> {
    let xhci = Function::Xhci.device();
    if !xhci.is_present() {
        return Ok(());
    }
    let Some(bar) = Bar::of(&xhci) else {
        fstart_log::warn!("baytrail: xHCI BAR unassigned");
        return Ok(());
    };
    usb3_phy_init(iosf);
    hc_init(&xhci, bar);
    if resume {
        // Leave bit 24 clear to keep the controller out of reset.
        bar.modify32(0x80e0, !0x0101_0200, 0x0000_0040);
    } else {
        // Toggle bit 24.
        bar.modify32(0x80e0, !0x0001_0200, 0x0100_0040);
        bar.modify32(0x80e0, !0x0100_0000, 0);
    }
    // C0 changes the iCLK/USB PLL VCO setting from 5 to 7.
    if crate::baytrail::stepping() == Stepping::C0 {
        iosf.modify(USHPHY, 0x03c0, 0, 0x0070_0000);
    }
    clock_gating(&xhci);
    // Finalize XHCC1 and XHCC2, port routing masks.
    xhci.or32(0x44, 0x83c0_0000);
    xhci.modify32(0x40, !0x0080_0000, 0x8000_0000);
    xhci.write32(XHCI_USB2PRM, USB2_PORT_MAP);
    xhci.write32(XHCI_USB3PRM, USB3_PORT_MAP);
    with_port_disable_override(|| {
        xhci.write32(XHCI_USB2PDO, u32::from(config.usb2_port_disable_mask));
        xhci.write32(XHCI_USB3PDO, u32::from(config.usb3_port_disable_mask));
    });
    if config.route_to_xhci {
        route_all(&xhci, bar, resume)?;
    }
    Ok(())
}
