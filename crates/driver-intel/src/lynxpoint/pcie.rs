//! PCIe root port configuration before writing the R/WO `RPFN` assignment.

use super::{
    LynxPoint, gpio, hsio, rcba_regs,
    regs::{FD, dev},
};
use crate::southbridge::lpc;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

fn grant(straps: u32) -> u16 {
    [0, 2, 0x22, 2][((straps >> 14) & 3) as usize]
}

fn permitted(lp: bool, owner: u32, f1: u32, f5: u32) -> u8 {
    let mut mask = if lp { 0x3f } else { 0xff };
    let group = |f: u32| [15u8, 13, 5, 1][((f >> 14) & 3) as usize];
    mask &= 0xf0 | group(f1);
    if !lp {
        mask &= 0x0f | (group(f5) << 4);
    }
    let (a, b) = if lp { (1, 4) } else { (0x11, 0x24) };
    if owner & a == 0 {
        mask &= !1;
    }
    if owner & b == 0 {
        mask &= !2;
    }
    if lp && owner & 0xf0 == 0 {
        mask &= !32;
    }
    if f1 & (1 << 19) != 0 {
        let sel = (f1 >> 16) & 7;
        let gbe = if lp {
            [2, 3, 4, 4, 4, 4, 8, 8][sel as usize]
        } else {
            sel
        };
        mask &= !(1u16 << gbe) as u8;
    }
    mask
}

fn tune(lp: bool, n: u8, owner: u32) {
    if lp {
        match n {
            0..=3 => hsio::add(
                [0xe900_2440, 0xe900_2640, 0xe900_0840, 0xe900_0a40][n as usize],
                0x2_0200,
            ),
            4 => {
                for i in 0..4 {
                    hsio::add(0xe900_0c40 + 0x200 * i, 0x2_0200);
                }
            }
            5 => {
                for i in 0..4 {
                    if owner & (16 << i) != 0 {
                        hsio::add(0xea00_2040 + 0x200 * i, 0x2_0200);
                    }
                }
            }
            _ => {}
        }
    } else {
        let addr = match n {
            0 if owner & 3 != 1 => 0xea00_2040,
            1 if owner & 12 != 4 => 0xea00_2240,
            _ => 0xe900_2e40 - 0x200 * u32::from(n),
        };
        hsio::add(addr, 0x2_0200);
    }
}

/// Derive both the `RPFN` write and the OS companions from the same set of
/// physically present, enabled ports. If port 1 is absent/disabled, coalesce so
/// that the first visible function is zero.
fn assignments(rpfn: u32, count: u8, present: u8, requested: u8) -> (u32, [Option<u8>; 8]) {
    let mask = present & requested;
    let mut next = rpfn;
    let mut visible = [None; 8];
    let (mut enabled, mut disabled) = (0, mask.count_ones());
    for n in 0..count {
        if present & (1 << n) == 0 {
            continue; // No RPFN field to program.
        }
        let on = mask & (1 << n) != 0;
        let func = if mask & 1 != 0 {
            (rpfn >> (4 * n)) & 7
        } else {
            let counter = if on { &mut enabled } else { &mut disabled };
            let function = *counter;
            *counter += 1;
            function
        };
        visible[n as usize] = on.then_some(func as u8);
        next = (next & !(15 << (4 * n))) | ((func | (u32::from(!on) << 3)) << (4 * n));
    }
    (next, visible)
}

impl LynxPoint {
    pub(super) fn configure_root_ports(&mut self) {
        let c = self.config;
        let lp = c.gpio.lp();
        let r = rcba_regs();
        let rpfn = r.rpfn.get();
        let port = |n: u8| EcamDevice::new(0, dev::PCIE, ((rpfn >> (4 * n)) & 7) as u8);
        let strap = |n, reg| {
            let d = port(n);
            if d.is_present() { d.read32(reg) } else { 0 }
        };
        let f1 = strap(0, 0xfc);
        let f5 = strap(4, 0xfc);
        let owner = strap(0, 0x410);
        let requested = self.requested_ports & permitted(lp, owner, f1, f5);
        let count = if lp || lpc::device().device_id() == 0x8c5c {
            6
        } else {
            8
        };
        let count = if lp {
            count
        } else {
            count.min([8, 8, 7, 5][((f5 >> 14) & 3) as usize])
        };
        let present = (0..count).fold(0u8, |bits, n| bits | (u8::from(port(n).is_present()) << n));
        let (next, visible) = assignments(rpfn, count, present, requested);
        let mask = present & requested;
        let straps = [strap(0, 0x32c), strap(4, 0x32c), strap(5, 0x32c)];
        for n in 0..count {
            let d = port(n);
            if mask & (1 << n) == 0 {
                continue;
            }
            let aspm = c.pcie_force_aspm & (1 << n) != 0
                || straps[if n < 4 {
                    0
                } else if lp && n == 5 {
                    2
                } else {
                    1
                }] & (1 << (28 + (n % 4)))
                    != 0;
            if aspm {
                d.modify32(0xd4, !12, 24);
                d.or32(0xd8, 7 << 18);
                tune(lp, n, owner);
                d.and32(0x338, !(1 << 26));
            }
            d.modify32(0x64, !(3 << 18), 1 << 11);
            d.write16(0x68, (d.read16(0x68) & !(3 << 13)) | (1 << 10));
            d.modify32(0x318, 0xffff, 0x1414_0000);
            d.modify32(
                0x4c,
                !(7 << 15),
                if aspm || d.read8(0xf5) & 1 != 0 {
                    4 << 15
                } else {
                    2 << 15
                },
            );
            d.write32(0x314, 0x743a_361b);
            d.modify32(0xd8, !(7 << 15), 3 << 15);
            d.modify32(0x33c, 0xff00_0000, 0x0085_4c74);
            d.or32(0xd8, 1 << 25);
            d.and8(0xf5, 15);
            if n == 0 || n == 4 || (lp && n == 5) {
                d.and8(0xf7, !12);
            }
            d.or32(0xd4, 2);
            d.modify32(0x100, !0x000f_ffff, 0x1_0001 | if lp { 1 << 29 } else { 0 });
            if lp {
                d.modify32(0x200, !0x000f_ffff, 0x1e);
                d.modify32(0x320, !((3 << 20) | (7 << 6)), (1 << 20) | (3 << 6));
            }
            d.or32(0x320, 3 << 23);
            for o in [0x34, 0x40, 0x80, 0x90] {
                d.or32(o, 0);
            }
        }
        for n in 0..count {
            if present & (1 << n) == 0 {
                continue;
            }
            let d = port(n);
            let on = mask & (1 << n) != 0;
            if on {
                d.or8(0xe1, 3);
                if lp {
                    d.or8(0xe2, 64);
                    d.and8_or8(0xe8, !12, 8);
                }
                d.or8(0xe8, 1);
                d.or32(0x324, 32);
            } else if lp {
                d.or8(0xe2, 0x30);
                d.or32(0x420, 1 << 31);
            }
            if lp && gpio::LpGpio::native_at(c.gpio_base(), 18 + n) {
                d.or32(0x420, 3 << 29);
            }
            if n == 0 || n == 4 || (lp && n == 5) {
                d.or8(0xe1, 0x3c);
            }
            if !on
                && ((n == 0 && mask & 15 == 0)
                    || (n == 4 && (lp || mask & 0xf0 == 0))
                    || (lp && n == 5))
            {
                d.or8(0xe2, 1);
                d.or8(0xe1, 128);
            }
        }
        if lp && present & 1 != 0 && mask == 0 {
            port(0).or8(0xe1, 1 << 6);
        }
        r.rpc.set(grant(f1) | if lp { 0 } else { grant(f5) << 8 });
        for n in 0..count {
            if present & !mask & (1 << n) == 0 {
                continue;
            }
            let d = port(n);
            d.and16(4, !7);
            d.or32(0x338, 1 << 26);
            if !fstart_arch::x86::wait_us(5_000, || d.read32(0x328) >> 24 == 1) {
                fstart_log::warn!(
                    "LPT RP{}: link disable timeout, 328={:#x}",
                    n + 1,
                    d.read32(0x328)
                );
            }
            d.or32(0x408, 1 << 27);
            let pcie_disabled = r.fd.read(FD::PCIE) | (1 << n);
            r.fd.modify(FD::PCIE.val(pcie_disabled));
        }
        r.rpfn.set(next);
        self.root_functions = visible;
    }

    pub(super) fn init_bridge_resources(&self) {
        for f in self.root_functions.iter().flatten() {
            let d = EcamDevice::new(0, dev::PCIE, *f);
            d.or16(4, 0x104);
            d.write8(0xc, 16);
            d.and16(0x3e, !1);
            for off in [6, 0x1e] {
                d.write16(off, d.read16(off));
            }
            let secondary = d.read8(0x19);
            if !(1..=63).contains(&secondary) || secondary > d.read8(0x1a) {
                continue;
            }
            let root_cap = d.find_capability(0x10);
            let root_ltr = d.read16(0x68) & (1 << 10) != 0;
            // Point-to-point root link: endpoint device 0, including multifunction.
            let mut endpoints = [None; 8];
            let mut ep_count = 0usize;
            for func in 0..8 {
                let ep = EcamDevice::new(secondary, 0, func);
                if !ep.is_present() {
                    continue;
                }
                Self::program_interrupt_line(ep);
                if let Some(cap) = ep.find_capability(0x10) {
                    endpoints[ep_count] = Some((ep, cap));
                    ep_count += 1;
                }
            }
            if let Some(rcap) = root_cap
                && ep_count != 0
            {
                let eps = &endpoints[..ep_count];
                // 1. Common Clock Configuration + Link Retrain (PCIe 3.0 §7.8.7).
                let root_slc = d.read16(rcap + 0x12) & (1 << 12) != 0;
                let all_slc = eps
                    .iter()
                    .flatten()
                    .all(|&(ep, cap)| ep.read16(cap + 0x12) & (1 << 12) != 0);
                if root_slc && all_slc {
                    let already_ccc = d.read16(rcap + 0x10) & (1 << 6) != 0
                        && eps
                            .iter()
                            .flatten()
                            .all(|&(ep, cap)| ep.read16(cap + 0x10) & (1 << 6) != 0);
                    if !already_ccc {
                        for &(ep, cap) in eps.iter().flatten() {
                            ep.or16(cap + 0x10, 1 << 6);
                        }
                        d.or16(rcap + 0x10, 1 << 6);
                        let _ = fstart_arch::x86::wait_us(100_000, || {
                            d.read16(rcap + 0x12) & (1 << 11) == 0
                        });
                        d.or16(rcap + 0x10, 1 << 5);
                        let _ = fstart_arch::x86::wait_us(100_000, || {
                            d.read16(rcap + 0x12) & (1 << 11) == 0
                        });
                    }
                }
                // 2. Intersect ASPM support across root port and all endpoint functions.
                let mut aspm = ((d.read32(rcap + 0x0c) >> 10) & 3) as u16;
                for &(ep, cap) in eps.iter().flatten() {
                    aspm &= ((ep.read32(cap + 0x0c) >> 10) & 3) as u16;
                }
                if aspm != 0 {
                    // PCIe spec: enable ASPM on upstream port before downstream endpoints.
                    d.write16(rcap + 0x10, (d.read16(rcap + 0x10) & !3) | aspm);
                    for &(ep, cap) in eps.iter().flatten() {
                        ep.write16(cap + 0x10, (ep.read16(cap + 0x10) & !3) | aspm);
                    }
                }
                // 3. Enable endpoint Clock PM when supported by LNKCAP[18].
                for &(ep, cap) in eps.iter().flatten() {
                    if ep.read32(cap + 0x0c) & (1 << 18) != 0 {
                        ep.or16(cap + 0x10, 1 << 8);
                    }
                }
            }
            if root_ltr {
                for &(ep, cap) in endpoints[..ep_count].iter().flatten() {
                    if ep.read16(cap + 2) & 15 < 2 || ep.read32(cap + 0x24) & (1 << 11) == 0 {
                        continue;
                    }
                    ep.or16(cap + 0x28, 1 << 10);
                    let mut off = 0x100;
                    while off <= 0xff8 {
                        let hdr = ep.read32(off);
                        if hdr as u16 == 0x18 {
                            ep.write16(off + 4, 0x1003);
                            ep.write16(off + 6, 0x1003);
                            break;
                        }
                        let next = (hdr >> 20) as u16;
                        if next <= off || next & 3 != 0 {
                            break;
                        }
                        off = next;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_first_port_is_hidden_not_an_aml_companion() {
        let (rpfn, visible) = assignments(0x7654_3210, 6, 0x3e, 0b1011);
        assert_eq!(
            visible,
            [None, Some(0), None, Some(1), None, None, None, None]
        );
        assert_eq!(rpfn, 0x76cb_1a00);
        let (_, none) = assignments(0x7654_3210, 6, 0x3f, 0);
        assert_eq!(none, [None; 8]);
    }

    #[test]
    fn width_and_owner_fuses_reduce_board_mask() {
        assert_eq!(permitted(true, 0xf5, 0, 0), 63);
        assert_eq!(permitted(true, 1, 3 << 14, 0), 17);
        assert_eq!(permitted(false, 0xff, 0, 3 << 14), 0x1f);
        assert_eq!(grant(2 << 14), 0x22);
    }
}
