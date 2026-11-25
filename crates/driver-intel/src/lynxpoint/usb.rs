//! USB port routing, overcurrent maps, per-port PHY tuning, and post-DRAM
//! EHCI/xHCI handoff (`early_usb_native.c`, `usb_ehci.c`, `usb_xhci.c`).

use super::{
    hsio, rcba_regs,
    regs::{FD, RcbaRegs, dev},
};
use fstart_core::{mmio::MmioWindow, services::ServiceError};
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    BackPanel,
    FrontPanel,
    Dock,
    MiniPcie,
    Flex,
    Internal,
    Skip,
}

#[derive(Clone, Copy, Debug)]
pub struct Usb2Port {
    pub length: u16,
    pub enabled: bool,
    pub oc_pin: Option<u8>,
    pub location: Location,
}

#[derive(Clone, Copy, Debug)]
pub struct Usb3Port {
    pub enabled: bool,
    pub oc_pin: Option<u8>,
}

fn tune(p: Usb2Port, lp: bool, mobile: bool) -> u32 {
    use Location::*;
    let len = p.length;
    let (a, b) = if !mobile && !lp {
        if p.location == BackPanel {
            (
                4,
                if len < 0x80 {
                    2
                } else if len < 0x130 {
                    3
                } else {
                    4
                },
            )
        } else {
            (3, 2)
        }
    } else if p.location == Dock {
        (4, if len < 0x50 { 1 } else { 2 })
    } else if (lp && !matches!(p.location, BackPanel | MiniPcie)) || p.location == Internal {
        (5, 2)
    } else {
        (
            if len < 0x70 { 5 } else { 6 },
            if len < 0x100 { 2 } else { 3 },
        )
    };
    (b << 11) | (a << 8)
}

const fn physical_hs(lp: bool) -> usize {
    if lp { 10 } else { 14 }
}

fn oc_shift(lp: bool, kind: usize, index: usize, pin: u8) -> Option<(usize, usize)> {
    let bank = usize::from(pin / 4);
    if bank > usize::from(!lp) || (kind == 0 && index / 8 != bank) {
        return None;
    }
    // Each OC pin owns one byte. U2OCM2 (0xc4) uses its low six bits
    // for physical ports 8..13, not bits 8..13 of the preceding byte.
    let shift = usize::from(pin % 4) * 8 + (index % 8);
    (shift < 32).then_some((bank, shift))
}

pub(super) fn validate(lp: bool, hs: &[Usb2Port], ss: &[Usb3Port]) -> Result<(), ServiceError> {
    if hs.len() > physical_hs(lp)
        || ss.len() > if lp { 4 } else { 6 }
        || hs.iter().enumerate().any(|(i, p)| {
            p.enabled
                && p.oc_pin
                    .is_some_and(|pin| oc_shift(lp, 0, i, pin).is_none())
        })
        || ss.iter().enumerate().any(|(i, p)| {
            p.enabled
                && p.oc_pin
                    .is_some_and(|pin| oc_shift(lp, 1, i, pin).is_none())
        })
    {
        return Err(ServiceError::InvalidParam);
    }
    Ok(())
}

fn masks(lp: bool, hs: &[Usb2Port], ss: &[Usb3Port]) -> [[u32; 2]; 2] {
    let mut out = [[0; 2]; 2];
    for (kind, index, pin) in hs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.enabled)
        .filter_map(|(i, p)| Some((0, i, p.oc_pin?)))
        .chain(
            ss.iter()
                .enumerate()
                .filter(|(_, p)| p.enabled)
                .filter_map(|(i, p)| Some((1, i, p.oc_pin?))),
        )
    {
        if let Some((bank, shift)) = oc_shift(lp, kind, index, pin) {
            out[kind][bank] |= 1 << shift;
        } else {
            fstart_log::warn!("USB OC: invalid port {} pin {}, skipping", index, pin);
        }
    }
    out
}

/// PCI `USB2PR` (`0xd0`) / `USB3PR` (`0xd8`), 32 bits. The corresponding
/// routing masks live in the next dword of configuration space.
const HS_ROUTE: (u16, u32) = (0xd0, 0x7fff);
const SS_ROUTE: (u16, u32) = (0xd8, 0x3f);
/// LPC `PMIR` / `ETR3` bit 16: xHCI Smart Auto scratch bit.
const SMART_AUTO: u32 = 1 << 16;

/// Physical USB 2.0 port bit in `XUSB2PR` / `XUSB2PRM` wired to `devfn`'s EHCI
/// debug port (8 Series / Lynx Point-LP EDS: EHCI1 -> USB2 Port 1,
/// EHCI2 -> USB2 Port 9).
const fn debug_hs_port_mask(debug_devfn: Option<u8>) -> u32 {
    match debug_devfn {
        Some(dev::EHCI1) => 1 << 1,
        Some(dev::EHCI2) => 1 << 9,
        _ => 0,
    }
}

/// Ensure the selected EHCI debug controller is visible and its physical USB2
/// port is routed to EHCI (not xHCI) before `fstart-driver-usbdebug` probes it.
/// On a warm reboot from an OS with xHCI routing active, `XUSB2PR` may still
/// point USB2 Port 1 (or Port 9) at xHCI.
pub(super) fn prepare_ehci_debug(debug_devfn: u8, rcba: &RcbaRegs, xhci: EcamDevice) {
    match debug_devfn {
        dev::EHCI1 => rcba.fd.modify(FD::EHCI1::CLEAR),
        dev::EHCI2 => rcba.fd.modify(FD::EHCI2::CLEAR),
        _ => return,
    }
    let mask = debug_hs_port_mask(Some(debug_devfn));
    if mask != 0 && xhci.is_present() {
        xhci.and32(HS_ROUTE.0, !mask);
        xhci.and32(HS_ROUTE.0 + 4, !mask);
    }
}

/// Minimal 32-bit MMIO interface for early xHCI port routing so unit tests can
/// intercept the ordering of MMIO writes relative to PCI `USB2PR`/`USB3PR`.
trait XhciBar {
    fn read32(&self, off: u32) -> u32;
    fn write32(&self, off: u32, val: u32);
    fn setbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }
    fn clrbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }
    fn clrsetbits32(&self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }
}

impl XhciBar for MmioWindow {
    fn read32(&self, off: u32) -> u32 {
        (*self).read32(off)
    }

    fn write32(&self, off: u32, val: u32) {
        (*self).write32(off, val);
    }
}

fn set_route(x: EcamDevice, (reg, mask): (u16, u32), xhci: bool) {
    x.modify32(reg, !mask, if xhci { x.read32(reg + 4) & mask } else { 0 });
}

fn reset_early_ports(b: &impl XhciBar, start: u32, count: u32, reset: u32, skip_mask: u32) {
    for n in 0..count {
        if skip_mask & (1 << n) == 0 {
            b.clrsetbits32(start + n * 16, 2, reset);
        }
    }
    if !fstart_arch::x86::wait_us(100_000, || {
        (0..count).all(|n| skip_mask & (1 << n) != 0 || b.read32(start + n * 16) & (1 << 4) == 0)
    }) {
        fstart_log::warn!("lynxpoint: USB port reset timeout");
    }
}

#[allow(clippy::too_many_arguments)]
fn early_routing(
    x: EcamDevice,
    lpc: EcamDevice,
    b: &impl XhciBar,
    hs_count: u32,
    ss_count: u32,
    ss_base: u32,
    route_xhci: bool,
    debug_hs_mask: u32,
) {
    let self_refresh = lpc.read8(0xa2) & 32 != 0;
    // Keep any active EHCI debug port unroutable in XUSB2PRM so neither
    // set_route nor a downstream OS xHCI driver steals it from EHCI.
    x.modify32(
        HS_ROUTE.0 + 4,
        !HS_ROUTE.1,
        ((1 << hs_count) - 1) & !debug_hs_mask,
    );
    x.modify32(SS_ROUTE.0 + 4, !SS_ROUTE.1, (1 << ss_count) - 1);
    // Restore the switch driver's state before resetting ports, then consume
    // the scratch bit. It is not a substitute for the requested final route.
    if self_refresh && lpc.read32(0xac) & SMART_AUTO != 0 {
        set_route(x, HS_ROUTE, true);
        set_route(x, SS_ROUTE, true);
    }
    lpc.and32(0xac, !SMART_AUTO);
    if route_xhci {
        if self_refresh {
            reset_early_ports(b, ss_base, ss_count, 1 << 31, 0);
        }
        set_route(x, SS_ROUTE, true);
        set_route(x, HS_ROUTE, true);
    } else {
        for (route, start, count, reset, skip_mask) in [
            (HS_ROUTE, 0x480, hs_count, 1 << 4, debug_hs_mask),
            (SS_ROUTE, ss_base, ss_count, 1 << 31, 0),
        ] {
            reset_early_ports(b, start, count, reset, skip_mask);
            set_route(x, route, false);
            for n in 0..count {
                if skip_mask & (1 << n) == 0 {
                    b.clrsetbits32(start + n * 16, 2, 0x7f << 17);
                }
            }
        }
        b.setbits32(0x80, 1);
        b.clrbits32(0x80, 1);
    }
}

pub(super) fn init(
    c: &super::LynxPointConfig,
    mobile: bool,
    resume: bool,
    ehci_debug: Option<(u8, u32)>,
) {
    let (lp, hs, ss) = (c.gpio.lp(), c.usb2, c.usb3);
    let route_xhci = if resume {
        c.usb_xhci_on_resume
    } else {
        c.xhci_default
    };
    let x = EcamDevice::from_devfn(0, dev::XHCI);
    let x_present = c.xhci_enabled && x.is_present();
    let f = if x_present { x.read32(0xe0) } else { 0 };
    let usbr = (f >> 5) & 1;
    let hs_count = if lp { 8 } else { 14 - 2 * ((f >> 1) & 3) };
    let ss_count = if lp { 4 } else { 6 - 2 * ((f >> 3) & 3) };
    let oc = masks(lp, hs, &ss[..ss.len().min(ss_count as usize)]);
    for (i, devfn) in [dev::EHCI1, dev::EHCI2]
        .into_iter()
        .take(if lp { 1 } else { 2 })
        .enumerate()
    {
        let d = EcamDevice::from_devfn(0, devfn);
        if !c.ehci_enabled || !d.is_present() {
            continue;
        }
        let debug_bar = ehci_debug.filter(|&(f, _)| f == devfn).map(|(_, bar)| bar);
        let is_debug = debug_bar.is_some();
        let cmd = d.read16(4);
        let saved = d.read32(0x10);
        // When EHCI debug already opened BAR0 and memory decode, reuse it
        // in place without dropping decode or issuing HCRESET.
        let live_debug = is_debug && (cmd & 2 != 0) && (saved & !0xf != 0);
        let base = if live_debug {
            saved & !0xf
        } else {
            debug_bar.unwrap_or(0xe800_0000 + 0x400 * i as u32)
        };
        if !live_debug {
            d.and16(4, !6);
            d.write32(0x10, base);
            d.or16(4, 2);
        }
        // SAFETY: 1 KiB EHCI BAR0 (either already live for EHCI debug or
        // temporarily enabled above during pre-RAM init).
        let bar = unsafe { MmioWindow::new(base as usize, 0x400) };
        d.and16(0x78, !3);
        if !is_debug {
            bar.setbits32(0x20, 2);
        }
        crate::southbridge::with_ehci_unlocked(d, || {
            bar.clrbits32(4, 0xf000);
            bar.clrsetbits32(4, 15, 2 + usbr);
        });
        d.or16(0x78, 4);
        d.or16(0x7c, 0x4080);
        d.modify32(0x8c, !0xf00, 0x400);
        d.modify32(0x8c, !(1 << 26), 1 << 17);
        if !live_debug {
            d.and16(4, !6);
            d.write32(0x10, saved);
            d.write16(4, cmd);
        }
    }
    hsio::update(0xe500_7f04, u32::MAX, 0x4481);
    for port in 0..physical_hs(lp) as u32 {
        hsio::update(0xe500_400f + port * 0x100, 1, 0);
    }
    hsio::update(0xe500_7f14, 3 << 19, 3 << 19);
    if lp {
        hsio::update(0xe500_7f02, 3 << 22, 0);
    }
    let program_ports = || {
        for (bank, devfn) in [dev::EHCI1, dev::EHCI2]
            .into_iter()
            .take(if lp { 1 } else { 2 })
            .enumerate()
        {
            let ehci = EcamDevice::from_devfn(0, devfn);
            if c.ehci_enabled && ehci.is_present() {
                ehci.write32(0x74, oc[0][bank]);
            }
        }
        if x_present {
            for bank in 0..if lp { 1 } else { 2 } {
                x.write32(0xc0 + 4 * bank as u16, oc[0][bank]);
                x.write32(0xc8 + 4 * bank as u16, oc[1][bank]);
            }
        }
        for (i, p) in hs.iter().enumerate().filter(|(_, p)| p.enabled) {
            hsio::update(0xe500_4100 + 0x100 * i as u32, 0x7f00, tune(*p, lp, mobile));
        }
    };
    if !x_present {
        if c.xhci_enabled {
            fstart_log::warn!("LPT: xHCI absent, skipping controller setup");
        }
        program_ports();
        return;
    }
    let cmd = x.read16(4);
    let saved = [x.read32(0x10), x.read32(0x14)];
    let base = 0xe810_0000;
    x.and16(4, !6);
    x.write32(0x14, 0);
    x.write32(0x10, base);
    x.or16(4, 2);
    // SAFETY: temporary 64 KiB xHCI BAR0 enabled above in pre-RAM init.
    let b = unsafe { MmioWindow::new(base as usize, 0x1_0000) };
    if !lp {
        // HCSPARAMS1.MaxPorts is bits 31:24 (the C port shifts by 28).
        b.clrsetbits32(4, 0xff << 24, max_ports(ss_count));
    }
    b.clrsetbits32(0xc, 0xffff_00ff, 0x0200_000a);
    b.clrsetbits32(0x10, 32, 0x600);
    if !lp {
        b.clrbits32(0x8008, 1 << 19);
    }
    for (off, clear, set) in [
        (0x8058, 256, if lp { 0x1_0000 } else { 0x11_0000 }),
        (0x8060, 0, (1 << 25) | (1 << 18)),
        (0x8090, 0, 0x4100),
        (0x8094, 0, 0x00a0_4000),
        (0x80e0, 1 << 16, 64),
        (0x80ec, 0x7e00, 6 << 9),
        (0x80f0, 1 << 20, 0),
    ] {
        b.clrsetbits32(off, clear, set);
    }
    if lp {
        b.setbits32(0x80fc, 1 << 25);
    }
    b.clrsetbits32(0x8110, if lp { 260 } else { 4 }, 0x0010_0800);
    b.write32(0x8140, if lp { 0xff00_f03c } else { 0xff03_c132 });
    b.clrsetbits32(
        0x8154,
        if lp { 0x20_0000 } else { 0x20_2000 },
        if lp { 0x2000 } else { 0 },
    );
    b.clrbits32(0x8154, 8);
    if lp {
        b.setbits32(0x8164, 3);
        for (off, v) in [
            (0x8174, 0x0140_0c0a),
            (0x817c, 0x0332_00a3),
            (0x8180, 0x00cb_0028),
            (0x8184, 0x0064_001e),
        ] {
            b.write32(off, v);
        }
    }
    x.or16(0x44, 0xc401);
    x.or8(0x46, 15);
    b.setbits32(0x8188, if lp { 0x0500_0000 } else { 0x0100_0000 });
    early_routing(
        x,
        crate::southbridge::lpc::device(),
        &b,
        hs_count + usbr,
        ss_count,
        if lp { 0x510 } else { 0x570 },
        route_xhci,
        debug_hs_port_mask(ehci_debug.map(|(d, _)| d)),
    );
    program_ports();
    x.and16(4, !6);
    x.write32(0x14, saved[1]);
    x.write32(0x10, saved[0]);
    x.write16(4, cmd);
}

/// xHCI `HCSPARAMS1` (`0x04`, 32 bits), `MaxPorts` in the upper byte.
const fn max_ports(ss_count: u32) -> u32 {
    (15 + ss_count) << 24
}

/// Apply post-DRAM EHCI clock gating and RMH wake control only to enabled,
/// present EHCI controllers (coreboot `usb_ehci_init`).
fn init_post_dram_ehci(
    ehci_enabled: bool,
    lp: bool,
    debug_devfn: Option<u8>,
    rcba: &RcbaRegs,
    device: impl Fn(u8) -> EcamDevice,
) {
    if !ehci_enabled && debug_devfn.is_none() {
        return;
    }
    let mut active = false;
    for devfn in [dev::EHCI1, dev::EHCI2]
        .into_iter()
        .take(if lp { 1 } else { 2 })
    {
        if !ehci_enabled && debug_devfn != Some(devfn) {
            continue;
        }
        let ehci = device(devfn);
        if !ehci.is_present() {
            continue;
        }
        ehci.or32(0xdc, 0x26);
        // Keep 0x78[1:0] clear on an active EHCI debug controller: BWG shared
        // EHCI/xHCI port workaround while the debug port stays on EHCI and
        // sibling ports route to xHCI.
        if debug_devfn != Some(devfn) {
            ehci.or32(0x78, 3);
        }
        active = true;
    }
    if active {
        rcba.rmhwkctl.set(rcba.rmhwkctl.get() | 0x22);
    }
}

/// Route all ports to xHCI and disable any remaining EHCI controllers on a cold
/// boot when xHCI is the default controller (coreboot `usb_xhci_route_all`).
///
/// Skipping when `FD.EHCI1` is already set must only skip the EHCI handoff,
/// never the subsequent SuperSpeed port recovery in `post_dram`. When EHCI
/// debug is active on `debug_devfn`, keep its physical USB2 port unrouted in
/// `XUSB2PRM`/`XUSB2PR` and leave that EHCI controller live.
fn route_all_to_xhci(
    xhci: EcamDevice,
    lp: bool,
    debug_devfn: Option<u8>,
    rcba: &RcbaRegs,
    device: impl Fn(u8) -> EcamDevice,
) {
    if debug_devfn.is_none() && rcba.fd.is_set(FD::EHCI1) {
        return;
    }
    let debug_hs_mask = debug_hs_port_mask(debug_devfn);
    if debug_hs_mask != 0 {
        xhci.and32(HS_ROUTE.0 + 4, !debug_hs_mask);
    }
    set_route(xhci, SS_ROUTE, true);
    set_route(xhci, HS_ROUTE, true);
    for (devfn, bit) in [(dev::EHCI1, FD::EHCI1::SET), (dev::EHCI2, FD::EHCI2::SET)]
        .into_iter()
        .take(if lp { 1 } else { 2 })
    {
        if debug_devfn == Some(devfn) {
            continue;
        }
        let ehci = device(devfn);
        if ehci.is_present() {
            ehci.or32(0xdc, 1);
            ehci.write16(0x54, (ehci.read16(0x54) & !0x103) | 3);
            ehci.and16(4, !7);
            ehci.write32(0x10, 0);
        }
        rcba.fd.modify(bit);
    }
}

struct XhciHandoff {
    lp: bool,
    resume: bool,
    default_xhci: bool,
    debug_devfn: Option<u8>,
}

/// Complete xHCI handoff (optional EHCI disable + SuperSpeed PORTSC recovery).
fn finish_post_dram_xhci(
    xhci: EcamDevice,
    handoff: XhciHandoff,
    rcba: &RcbaRegs,
    device: impl Fn(u8) -> EcamDevice,
    read_port: impl Fn(u32) -> u32,
    write_port: impl Fn(u32, u32),
    delay: impl Fn(u32),
) {
    if !handoff.resume && handoff.default_xhci {
        route_all_to_xhci(xhci, handoff.lp, handoff.debug_devfn, rcba, device);
    }
    if handoff.resume || handoff.default_xhci {
        let count = if handoff.lp {
            4
        } else {
            6 - 2 * ((xhci.read32(0xe0) >> 3) & 3)
        };
        recover_ss(
            count,
            !xhci.read32(0xe8),
            !handoff.resume,
            read_port,
            write_port,
            delay,
        );
    }
}

/// Clock gating and R/WO completion after the PCI allocator assigned BAR0.
pub(super) fn post_dram(
    c: &super::LynxPointConfig,
    resume: bool,
    debug_devfn: Option<u8>,
) -> Result<(), ServiceError> {
    let lp = c.gpio.lp();
    let rcba = rcba_regs();
    hsio::update(0xe500_4001, 0, 0xc0);
    init_post_dram_ehci(c.ehci_enabled, lp, debug_devfn, rcba, |devfn| {
        EcamDevice::from_devfn(0, devfn)
    });
    if !c.xhci_enabled {
        return Ok(());
    }
    let d = EcamDevice::from_devfn(0, dev::XHCI);
    let Some(base) = d.memory_bar(0x10) else {
        fstart_log::warn!("LPT: no xHCI BAR, skipping init");
        return Ok(());
    };
    // SAFETY: xHCI BAR0 (64 KiB) was assigned by the PCI allocator.
    let b = unsafe { MmioWindow::new(base as usize, 0x1_0000) };
    d.and16(0x74, !3);
    let cfg = (d.read32(0x40) & !(1 << 23)) | 0x0036_0100;
    let cfg = if lp { cfg & !(1 << 19) } else { cfg };
    d.write16(0x40, cfg as u16);
    d.write8(0x42, (cfg >> 16) as u8);
    d.or16(0x44, 0x288);
    d.or32(0xa0, if lp { 1 << 18 } else { 64 });
    d.and32(0xa4, !(1 << 13));
    b.clrsetbits32(0x8144, 0x1c0, if lp { 0x1c0 } else { 0x100 });
    if lp {
        b.clrsetbits32(0x816c, 0xfffff, 0xe_0038);
        d.modify32(0xb0, !0x6000, 1 << 17);
        d.modify32(0x50, !0x1fff_ffff, 0x0fce_2e5f);
    } else {
        d.modify32(0x50, !0x07ff_ffff, 0x0788_6e9f);
    }
    d.or32(0x44, 1 << 31);
    d.modify32(0x40, !(1 << 23), 1 << 31);
    let start = if lp { 0x510 } else { 0x570 };
    finish_post_dram_xhci(
        d,
        XhciHandoff {
            lp,
            resume,
            default_xhci: c.xhci_default,
            debug_devfn,
        },
        rcba,
        |devfn| EcamDevice::from_devfn(0, devfn),
        |n| b.read32(start + 16 * n),
        |n, v| b.write32(start + 16 * n, v),
        fstart_arch::x86::udelay,
    );
    Ok(())
}

/// `PORTSC` recovery shared with the LP SMM sleep path.
///
/// Only reset ports in RxDetect or Polling on resume; wait for `WRC` only on
/// the ports we actually reset.
#[inline(always)]
pub(super) fn recover_ss(
    count: u32,
    enabled: u32,
    all: bool,
    read: impl Fn(u32) -> u32,
    write: impl Fn(u32, u32),
    delay: impl Fn(u32),
) {
    for _ in 0..100 {
        if (0..count).all(|n| enabled & (1 << n) == 0 || ((read(n) >> 5) & 15) != 7) {
            break;
        }
        delay(1000);
    }
    let mut reset = 0u32;
    for n in 0..count {
        if enabled & (1 << n) == 0 {
            continue;
        }
        let sc = read(n);
        if all || matches!((sc >> 5) & 15, 5 | 7) {
            write(n, (sc & !2) | (1 << 31));
            reset |= 1 << n;
        }
    }
    for _ in 0..100 {
        if (0..count).all(|n| reset & (1 << n) == 0 || read(n) & (1 << 19) != 0) {
            break;
        }
        delay(1000);
    }
    for n in 0..count {
        write(n, (read(n) & !2) | (0x7f << 17));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use fstart_pci::{PciAddress, PciEcam, PciRoot, PciRootInfo};

    fn fake_ecam(backing: &mut [u8]) -> PciEcam {
        let root = PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: backing.as_mut_ptr().addr().next_multiple_of(1 << 20) as u64,
                bus_start: 0,
                bus_end: 0,
            },
            [],
        )
        .unwrap();
        PciEcam::new([root]).unwrap()
    }

    #[test]
    fn early_switch_restores_smart_auto_before_returning_ports_to_ehci() {
        let mut backing = alloc::vec![0u8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let x = pci.device(PciAddress::new(0, 0, 20, 0)).unwrap();
        let lpc = pci.device(PciAddress::new(0, 0, 31, 0)).unwrap();

        struct Trace<'a> {
            x: EcamDevice,
            writes: &'a RefCell<alloc::vec::Vec<(u32, u32, u32, u32)>>,
        }
        impl XhciBar for Trace<'_> {
            fn read32(&self, _: u32) -> u32 {
                0 // Controller completes each reset immediately.
            }
            fn write32(&self, off: u32, v: u32) {
                self.writes.borrow_mut().push((
                    off,
                    v,
                    self.x.read32(HS_ROUTE.0),
                    self.x.read32(SS_ROUTE.0),
                ));
            }
        }
        let writes = RefCell::default();
        let b = Trace { x, writes: &writes };
        // Cold boot to xHCI needs no port resets.
        early_routing(x, lpc, &b, 1, 1, 0x510, true, 0);
        assert!(writes.borrow().is_empty());
        assert_eq!((x.read32(0xd0), x.read32(0xd8)), (1, 1));
        // Lost PR registers in self refresh, retained SMART_AUTO. Acknowledge
        // each port's change bits only after it has actually switched away.
        x.write32(0xd0, 0);
        x.write32(0xd8, 0);
        lpc.write8(0xa2, 32);
        lpc.write32(0xac, SMART_AUTO | 0x40);
        early_routing(x, lpc, &b, 1, 1, 0x510, false, 0);
        assert_eq!(lpc.read32(0xac), 0x40);
        assert_eq!(
            &*writes.borrow(),
            &[
                (0x480, 16, 1, 1),
                (0x480, 0xfe_0000, 0, 1),
                (0x510, 1 << 31, 0, 1),
                (0x510, 0xfe_0000, 0, 0),
                (0x80, 1, 0, 0),
                (0x80, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn post_dram_ehci_skips_disabled_or_absent_controllers() {
        let mut backing = alloc::vec![0xffu8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let ehci1 = pci.device(PciAddress::new(0, 0, 29, 0)).unwrap();
        let ehci2 = pci.device(PciAddress::new(0, 0, 26, 0)).unwrap();
        ehci1.write16(0, 0x8086);
        ehci1.write32(0xdc, 0);
        ehci1.write32(0x78, 0);
        // EHCI2 stays 0xffff (absent).

        let mut rcba_buf = alloc::vec![0u32; 0x4000 / 4];
        // SAFETY: aligned 16 KiB buffer matching RcbaRegs.
        let rcba = unsafe { &*rcba_buf.as_mut_ptr().cast::<RcbaRegs>() };

        // Disabled in config: neither EHCI nor RCBA+0x35b0 is touched.
        init_post_dram_ehci(false, false, None, rcba, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert_eq!(ehci1.read32(0xdc), 0);
        assert_eq!(ehci1.read32(0x78), 0);
        assert_eq!(rcba.rmhwkctl.get(), 0);

        // Enabled in config: present EHCI1 is updated; absent EHCI2 is left untouched.
        init_post_dram_ehci(true, false, None, rcba, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert_eq!(ehci1.read32(0xdc), 0x26);
        assert_eq!(ehci1.read32(0x78), 3);
        assert_eq!(ehci2.read32(0xdc), 0xffff_ffff);
        assert_eq!(rcba.rmhwkctl.get(), 0x22);
    }

    #[test]
    fn post_dram_xhci_runs_recover_ss_even_when_ehci_already_disabled() {
        let mut backing = alloc::vec![0u8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let xhci = pci.device(PciAddress::new(0, 0, 20, 0)).unwrap();
        xhci.write16(0, 0x8086);
        // LP: 4 SS ports, port 0..=2 enabled (0xe8 is a disable mask).
        xhci.write32(0xe8, !0b0111u32);

        let mut rcba_buf = alloc::vec![0u32; 0x4000 / 4];
        // SAFETY: aligned 16 KiB buffer matching RcbaRegs.
        let rcba = unsafe { &*rcba_buf.as_mut_ptr().cast::<RcbaRegs>() };
        // Simulate ehci_enabled == false: enable_devices() already set FD.EHCI1.
        rcba.fd.modify(FD::EHCI1::SET);

        let resets = Cell::new(0u32);
        finish_post_dram_xhci(
            xhci,
            XhciHandoff {
                lp: true,
                resume: false,
                default_xhci: true,
                debug_devfn: None,
            },
            rcba,
            |devfn| {
                pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                    .unwrap()
            },
            |_| 1 << 19,
            |n, v| {
                if v >> 31 != 0 {
                    resets.set(resets.get() | (1 << n));
                }
            },
            |_| (),
        );
        // Bug 1b regression check: recover_ss must still warm-reset enabled SS ports.
        assert_eq!(resets.get(), 0b0111);
    }

    #[test]
    fn cold_reset_all_resume_only_disconnected() {
        for (cold, expected) in [(true, 0b1011), (false, 0b1000)] {
            let resets = Cell::new(0);
            recover_ss(
                4,
                0b1011,
                cold,
                |n| (1 << 19) | if n >= 2 { 5 << 5 } else { 0 },
                |n, v| {
                    if v >> 31 != 0 {
                        resets.set(resets.get() | (1 << n));
                    }
                },
                |_| (),
            );
            assert_eq!(resets.get(), expected);
        }
    }

    #[test]
    fn ehci_debug_keeps_port_and_controller_alive_across_xhci_handoff() {
        let mut backing = alloc::vec![0u8; 2 << 20];
        let pci = fake_ecam(&mut backing);
        let xhci = pci.device(PciAddress::new(0, 0, 20, 0)).unwrap();
        let lpc = pci.device(PciAddress::new(0, 0, 31, 0)).unwrap();
        let ehci1 = pci.device(PciAddress::new(0, 0, 29, 0)).unwrap();
        xhci.write16(0, 0x8086);
        ehci1.write16(0, 0x8086);
        ehci1.write16(4, 0x0006);
        ehci1.write32(0x10, 0xfef0_0000);
        // Simulate a warm reset from an OS that routed all 8 LP ports to xHCI.
        xhci.write32(0xd0, 0xff);
        xhci.write32(0xd4, 0xff);

        let mut rcba_buf = alloc::vec![0u32; 0x4000 / 4];
        // SAFETY: aligned 16 KiB buffer matching RcbaRegs.
        let rcba = unsafe { &*rcba_buf.as_mut_ptr().cast::<RcbaRegs>() };
        rcba.fd.modify(FD::EHCI1::SET);

        prepare_ehci_debug(dev::EHCI1, rcba, xhci);
        assert!(!rcba.fd.is_set(FD::EHCI1));
        assert_eq!(xhci.read32(0xd0), 0xfd);
        assert_eq!(xhci.read32(0xd4), 0xfd);

        struct NoopBar;
        impl XhciBar for NoopBar {
            fn read32(&self, _: u32) -> u32 {
                0
            }
            fn write32(&self, _: u32, _: u32) {}
        }
        early_routing(
            xhci,
            lpc,
            &NoopBar,
            8,
            4,
            0x510,
            true,
            debug_hs_port_mask(Some(dev::EHCI1)),
        );
        // Physical USB2 port 1 (bit 1) remains routed to EHCI1 in both XUSB2PR and XUSB2PRM.
        assert_eq!(xhci.read32(0xd4), 0xfd);
        assert_eq!(xhci.read32(0xd0), 0xfd);

        init_post_dram_ehci(true, true, Some(dev::EHCI1), rcba, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert_eq!(ehci1.read32(0xdc), 0x26);
        assert_eq!(ehci1.read32(0x78), 0);

        route_all_to_xhci(xhci, true, Some(dev::EHCI1), rcba, |devfn| {
            pci.device(PciAddress::new(0, 0, devfn >> 3, devfn & 7))
                .unwrap()
        });
        assert!(!rcba.fd.is_set(FD::EHCI1));
        assert_eq!(ehci1.read16(4), 0x0006);
        assert_eq!(ehci1.read32(0x10), 0xfef0_0000);
        assert_eq!(ehci1.read32(0xdc), 0x26);
        assert_eq!(xhci.read32(0xd0), 0xfd);
        assert_eq!(xhci.read32(0xd4), 0xfd);
    }

    #[test]
    fn connectors() {
        assert_eq!(max_ports(6), 0x1500_0000);
        let p = Usb2Port {
            enabled: true,
            length: 0x150,
            oc_pin: Some(2),
            location: Location::MiniPcie,
        };
        assert_eq!(tune(p, true, true), (3 << 11) | (6 << 8));
        assert_eq!(masks(true, &[p], &[])[0][0], 1 << 16);
        assert!(validate(true, &[Usb2Port { oc_pin: None, ..p }; 10], &[]).is_ok());
        for i in [8, 13] {
            assert_eq!(oc_shift(false, 0, i, 4), Some((1, i - 8)));
            assert_eq!(oc_shift(false, 0, i, 7), Some((1, 24 + i - 8)));
        }
        assert_eq!(oc_shift(false, 1, 5, 7), Some((1, 29)));
    }
}
