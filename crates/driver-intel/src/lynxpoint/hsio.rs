//! Native PHY recipes and lane ownership checks for Lynx Point-H and LP.
//!
//! A failed IOBP read must never turn into a subsequent write based on a
//! synthetic zero value.

use super::{
    hsio_tables::*,
    rcba_regs,
    regs::{IOBP_STATUS, dev},
};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

fn transaction(addr: u32, data: Option<u32>) -> Result<u32, ServiceError> {
    let r = rcba_regs();
    let wait = || {
        if fstart_arch::x86::wait_us(10_000, || !r.iobp_stat.is_set(IOBP_STATUS::BUSY)) {
            Ok(())
        } else {
            Err(ServiceError::Timeout)
        }
    };
    wait()?;
    r.iobp_addr.set(addr);
    r.iobp_stat.modify(if data.is_some() {
        IOBP_STATUS::OPCODE::Write
    } else {
        IOBP_STATUS::OPCODE::Read
    });
    if let Some(v) = data {
        r.iobp_data.set(v);
    }
    r.iobp_be.set(0xf000);
    r.iobp_stat.modify(IOBP_STATUS::BUSY::SET);
    wait()?;
    let status = r.iobp_stat.extract();
    if status.read(IOBP_STATUS::RESPONSE) != 0 {
        fstart_log::warn!("IOBP {:#x} status {:#x}", addr, status.get());
        return Err(ServiceError::HardwareError);
    }
    Ok(r.iobp_data.get())
}

/// Apply one optional recipe entry. A failed read skips only its dependent write.
fn recipe(
    addr: u32,
    modify: impl FnOnce(u32) -> u32,
    mut access: impl FnMut(u32, Option<u32>) -> Result<u32, ServiceError>,
) -> bool {
    let result =
        access(addr, None).inspect_err(|e| fstart_log::warn!("IOBP read {:#010x}: {:?}", addr, e));
    result
        .and_then(|v| {
            access(addr, Some(modify(v)))
                .inspect_err(|e| fstart_log::warn!("IOBP write {:#010x}: {:?}", addr, e))
        })
        .is_ok()
}

pub(super) fn add(addr: u32, delta: u32) {
    recipe(addr, |v| v.wrapping_add(delta), transaction);
}

pub(super) fn update(addr: u32, clear: u32, set: u32) -> bool {
    recipe(addr, |v| (v & !clear) | set, transaction)
}

fn sata_shared_lane_owned(lp: bool, owner: u8, addr: u32) -> bool {
    match addr & 0xfe00 {
        0x2000 => owner & 16 == 0,
        0x2200 => owner & 32 == 0,
        0x2400 if lp => owner & 64 == 0,
        0x2600 if lp => owner & 128 == 0,
        _ => true,
    }
}

pub(super) fn sata(lp: bool, mobile: bool, owner: u8) {
    if !lp {
        for &(a, c, s) in SATA_LPT_H_CX {
            update(a, c, s);
        }
    }
    for &(a, c, s) in if lp {
        SATA_SHARED_LPT_LP_BX
    } else {
        SATA_SHARED_LPT_H_CX
    } {
        if sata_shared_lane_owned(lp, owner, a) {
            update(a, c, s);
        }
    }
    let trim = if mobile { 0x4c5a } else { 0x3e67 };
    for a in if lp {
        &[0xea00_2090, 0xea00_2290, 0xea00_2490, 0xea00_2690][..]
    } else {
        &[
            0xea00_2490,
            0xea00_2690,
            0xea00_0890,
            0xea00_0a90,
            0xea00_2090,
            0xea00_2290,
        ]
    } {
        if sata_shared_lane_owned(lp, owner, *a) {
            update(*a, 0xffff, trim);
        }
    }
}

fn xhci_shared_lane_owned(lp: bool, owner: u8, addr: u32) -> bool {
    match (lp, addr & 0xfe00) {
        (true, 0x2400) | (false, 0x2e00) => owner & 3 == 2,
        (true, 0x2600) | (false, 0x2c00) => (owner >> 2) & 3 == 2,
        _ => true,
    }
}

pub(super) fn usb_gbe(lp: bool, owner: u8) {
    let (dedicated, shared) = if lp {
        (XHCI_LPT_LP_BX, XHCI_SHARED_LPT_LP_BX)
    } else {
        (XHCI_LPT_H_CX, XHCI_SHARED_LPT_H_CX)
    };
    for &(a, c, s) in dedicated {
        update(a, c, s);
    }
    for &(a, c, s) in shared {
        if xhci_shared_lane_owned(lp, owner, a) {
            update(a, c, s);
        }
    }
    let strap = EcamDevice::new(0, dev::PCIE, 0).read32(0xfc);
    let port = (strap >> 16) & 7;
    if strap & (1 << 19) != 0
        && if lp {
            port < 6
        } else {
            match port {
                0 => owner & 3 == 1,
                1 => (owner >> 2) & 3 == 1,
                _ => true,
            }
        }
    {
        let lane = if lp { 8 + 2 * port } else { 0x2e - 2 * port };
        update(0xe900_0008 + (lane << 8), 0xf000_0100, 0xe000_0100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_entry_does_not_write_or_cancel_next_entry() {
        let mut trace = alloc::vec::Vec::new();
        for addr in [1, 2] {
            recipe(
                addr,
                |v| v + 1,
                |a, v| {
                    trace.push((a, v));
                    if a == 1 {
                        Err(ServiceError::Timeout)
                    } else {
                        Ok(41)
                    }
                },
            );
        }
        assert_eq!(trace, [(1, None), (2, None), (2, Some(42))]);
    }

    #[test]
    fn lane_ownership_gates_shared_sata_and_xhci_entries() {
        assert!(sata_shared_lane_owned(true, 0x00, 0xea00_2408));
        assert!(!sata_shared_lane_owned(true, 0x40, 0xea00_2408));
        assert!(sata_shared_lane_owned(false, 0x40, 0xea00_2408));
        assert!(!sata_shared_lane_owned(false, 0x10, 0xea00_2008));

        assert!(xhci_shared_lane_owned(true, 0x02, 0xe900_2438));
        assert!(!xhci_shared_lane_owned(true, 0x01, 0xe900_2438));
        assert!(xhci_shared_lane_owned(false, 0x08, 0xe900_2c38));
        assert!(!xhci_shared_lane_owned(false, 0x04, 0xe900_2c38));
    }
}
