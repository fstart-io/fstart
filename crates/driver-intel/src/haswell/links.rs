//! Haswell DMI, EPBAR, VCU mailbox, and ULT power-sharing link setup.
//!
//! PCH RCBA accesses go through `crate::southbridge::rcba_window(rcba_base)`
//! using the base passed from board/platform configuration so this module has
//! no compile-time coupling to any southbridge driver.

use super::system_agent::{DMI, EP, dmi, ep, mch};
use crate::southbridge::rcba_window;
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;

fn wait(us: u32, done: impl FnMut() -> bool) -> Result<(), ServiceError> {
    fstart_arch::x86::wait_us(us, done)
        .then_some(())
        .ok_or(ServiceError::Timeout)
}

tock_registers::register_bitfields![u32,
    /// VCU mailbox interface, MCHBAR `0x6c00` (32 bits).
    Vcu [
        RESPONSE OFFSET(0) NUMBITS(16) [],
        RUN_BUSY OFFSET(31) NUMBITS(1) []
    ]
];

const VCU_INTERFACE: u32 = 0x6c00;
const VCU_DATA: u32 = 0x6c04;

fn command(op: u32, value: u32) -> Result<u32, ServiceError> {
    let m = mch();
    for _ in 0..10 {
        let ready = || m.read32(VCU_INTERFACE) & Vcu::RUN_BUSY::SET.value == 0;
        if wait(1000, ready).is_err() {
            continue;
        }
        m.write32(VCU_DATA, value);
        m.write32(VCU_INTERFACE, Vcu::RUN_BUSY::SET.value | op);
        if wait(1000, ready).is_ok() && Vcu::RESPONSE.read(m.read32(VCU_INTERFACE)) == 0x40 {
            return Ok(m.read32(VCU_DATA));
        }
    }
    fstart_log::warn!(
        "VCU: op {:#x} data {:#x} failed, interface={:#x}",
        op,
        value,
        m.read32(VCU_INTERFACE)
    );
    Err(ServiceError::HardwareError)
}

fn vcu(
    addr: u32,
    write: Option<u32>,
    mut command: impl FnMut(u32, u32) -> Result<u32, ServiceError>,
) -> Result<u32, ServiceError> {
    let seq = if write.is_some() { 4 } else { 3 };
    command(2, seq)?;
    let result = command(if write.is_some() { 0x16 } else { 0x15 }, addr)
        .and_then(|_| command(if write.is_some() { 8 } else { 7 }, write.unwrap_or(0)));
    let closed = command(3, seq);
    result.and_then(|v| closed.map(|_| v))
}

fn sharing_levels(p: u32, ext: u32) -> (u32, u32) {
    let fields = |v: u32| (v & 31) | (((v >> 6) & 31) << 8) | (((v >> 12) & 31) << 16);
    // Match coreboot `configure_pch_power_sharing`: the first extended level
    // is consumed before shifting by six.
    (fields(p), ((p >> 18) & 31) | (fields(ext) << 8))
}

/// ULT C9/C10 voltage, 24 MHz wake clock calibration, and PMSYNC power sharing.
fn sharing_policy(
    mut issue: impl FnMut(u32, Option<u32>) -> Result<u32, ServiceError>,
) -> Option<(u32, u32)> {
    let mut pcode = |op, data| {
        issue(op, data)
            .inspect_err(|e| fstart_log::warn!("PCODE {:#x}: {:?}", op, e))
            .ok()
    };
    pcode(0x27, Some(0x1f1f));
    pcode(0x909, Some(u32::MAX));
    if let Some(cal) = pcode(0x509, None) {
        fstart_log::info!("PCODE: 24MHz BCLK calibration value: {:#010x}", cal);
    }
    let p = pcode(0xa, None);
    let ext = pcode(0xb, None);
    if let (Some(p), Some(ext)) = (p, ext) {
        fstart_log::info!("PCH Power: PCODE Levels {:#010x} {:#010x}", p, ext);
    }
    Some(sharing_levels(p?, ext?))
}

pub(super) fn ult_power_sharing(rcba_base: u32) {
    let Some((base, extra)) = sharing_policy(|op, data| mch().pcode(op, data)) else {
        return;
    };
    // SAFETY: `rcba_base` is the mapped 16 KiB PCH RCBA configured by the platform.
    let r = unsafe { rcba_window(rcba_base as usize) };
    r.clrsetbits32(0x33c4, 0x1f_1f1f, base);
    r.clrsetbits32(0x33cc, 0x1f1f_1f1f, extra);
}

/// Traditional (PCH-H) DMI PHY calibration, retraining, and VC negotiation.
///
/// LP uses the on-package link rather than the external DMI PHY.
pub(super) fn early_dmi(rcba_base: u32) -> Result<(), ServiceError> {
    let d = dmi();
    // SAFETY: `rcba_base` is the mapped 16 KiB PCH RCBA configured by the platform.
    let r = unsafe { rcba_window(rcba_base as usize) };
    for i in [0, 0x20] {
        for off in [0xa00, 0xa10] {
            d.clrsetbits32(off + i, 31, 12);
        }
    }
    for (off, clear, set) in [
        (0x90c, 63 << 5, 9 << 5),
        (0x910, 15 << 10, 0),
        (0x910, 7 << 18, 0),
    ] {
        for i in [0, 0x20] {
            d.clrsetbits32(off + i, clear, set);
        }
    }
    let addr = 0x0c0c_8001;
    if let Err(e) = vcu(addr, None, command).and_then(|v| vcu(addr, Some(v & !(63 << 23)), command))
    {
        fstart_log::warn!("DMI: skipping VCU update {:#x}: {:?}", addr, e);
    }
    d.clrsetbits32(0xc28, 31 << 18, 19 << 18);
    d.clrbits32(0xc38, 64);
    d.clrsetbits32(0x260, 3, 2);
    for i in [0, 0x20] {
        d.clrbits32(0x900 + i, 3 << 26);
    }
    for i in [0, 0x20] {
        d.setbits32(0x904 + i, 3 << 10);
    }
    d.setbits32(0x258, 1 << 29);
    d.clrsetbits32(0x208, 0x7ff, 0x6b5);
    d.clrsetbits32(0x22c, 0xffff, 0x2020);
    d.write32(0x71c, 15);
    d.write32(0x720, 0x0106_0200);
    if r.read32(0x21a4) & 15 == 2 {
        r.clrsetbits32(0x2340, 255 << 16, 0x3a << 16);
        r.clrsetbits8(0x21b0, 7, 2);
    }
    d.clrsetbits32(0x84, 0x3_f00f, 2);
    for _ in 0..2 {
        d.setbits16(0x88, 32);
        wait(200_000, || d.read16(0x8a) & 0x800 == 0)
            .inspect_err(|_| fstart_log::error!("DMI: SA retrain timed out at 0x8a"))?;
    }
    let channels = [
        (0x14, 0x2014, 0x8000_0079),
        (0x20, 0x2020, 0x8100_0002),
        (0x2c, 0x2030, 0x8200_0004),
        (0x38, 0x2040, 0x8700_0080),
    ];
    for (off, _, val) in channels {
        d.write32(off, val);
    }
    d.clrsetbits8(4, 7, 1);
    r.clrsetbits32(0x50, 15 << 20, 2 << 20);
    r.setbits32(0x50, (1 << 19) | (1 << 17));
    let _ = r.read32(0x50);
    for (_, off, val) in channels {
        r.write32(off, val);
        let _ = r.read32(off);
    }
    r.setbits32(0x50, 1 << 31);
    let _ = r.read32(0x50);
    for off in [0x201a, 0x2026, 0x2038, 0x2048] {
        wait(200_000, || r.read16(off) & 2 == 0).inspect_err(|_| {
            fstart_log::error!("DMI: PCH VC negotiation timed out at {:#x}", off);
        })?;
    }
    for off in [0x1a, 0x26, 0x32, 0x3e] {
        wait(200_000, || d.read16(off) & 2 == 0).inspect_err(|_| {
            fstart_log::error!("DMI: SA VC negotiation timed out at {:#x}", off);
        })?;
    }
    Ok(())
}

/// Program EPBAR/DMIBAR/PEG internal link topology after DRAM init.
pub(super) fn topology(lp: bool, rcba_base: u32) -> Result<(), ServiceError> {
    let e = ep();
    let d = dmi();
    e.write32(0x14, 0x8000_0001);
    e.write32(4, 1);
    e.write32(0x20, 0x8100_0002);
    wait(200_000, || e.read16(0x26) & 2 == 0)?;
    if !lp {
        d.setbits16(0x98, 64);
        d.setbits32(0x22c, 1 << 31);
        d.setbits32(0x238, 1 << 29);
        d.clrsetbits32(0xc28, 31, 19);
    }
    d.write32(0x1c4, u32::MAX);
    d.write32(0x1d0, u32::MAX);
    if !lp {
        d.setbits16(0x88, 3);
    }
    e.clrsetbits32(0x44, 255 << 16, 1 << 16);
    e.clrsetbits32(0x50, 255 << 16, 0x0001_0001);
    e.write32(0x58, DMI);
    e.write32(0x5c, 0);
    for i in 0..3u8 {
        let peg = EcamDevice::new(0, 1, i);
        if !peg.is_present() {
            continue;
        }
        let off = u32::from(i) * 16;
        e.write32(0x68 + off, (8 + u32::from(i)) << 12);
        e.write32(0x6c + off, 0);
        e.clrsetbits32(0x60 + off, 255 << 16, 0x0001_0001);
        peg.modify32(0x144, !(255 << 16), 1 << 16);
        peg.write32(0x158, EP);
        peg.write32(0x15c, 0);
        peg.modify32(0x150, !(255 << 16), 0x0001_0001);
        peg.or32(0xc4, 0);
    }
    d.clrsetbits32(0x44, 255 << 16, 1 << 16);
    d.clrsetbits32(0x50, 0xffff_0000, 0x0002_0001);
    d.write32(0x58, rcba_base);
    d.write32(0x5c, 0);
    d.write32(0x68, EP);
    d.write32(0x6c, 0);
    d.clrsetbits32(0x60, 255 << 16, 0x0001_0001);
    d.setbits32(4, 0);
    d.setbits32(0x84, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_sharing_levels_start_at_bit_zero() {
        assert_eq!(
            sharing_levels(4 << 18, 5 | (6 << 6) | (7 << 12) | (8 << 18)).1,
            0x0706_0504
        );
    }

    #[test]
    fn mailbox_failures_do_not_feed_follow_up_writes() {
        for failed in [0x27, 0x909, 0x509, 0xa, 0xb] {
            let mut ops = alloc::vec::Vec::new();
            let sharing = sharing_policy(|op, _| {
                ops.push(op);
                if op == failed {
                    Err(ServiceError::Timeout)
                } else {
                    Ok(7)
                }
            });
            assert_eq!(ops, [0x27, 0x909, 0x509, 0xa, 0xb]);
            assert_eq!(sharing.is_none(), matches!(failed, 0xa | 0xb));
        }
        let mut ops = alloc::vec::Vec::new();
        assert!(
            vcu(42, None, |op, _| {
                ops.push(op);
                if op == 0x15 {
                    Err(ServiceError::Timeout)
                } else {
                    Ok(0)
                }
            })
            .is_err()
        );
        assert_eq!(ops, [2, 0x15, 3]);
    }
}
