// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native single-socket Rs I/O-space ownership, not factory address replay.
//! The primary IOS (the FCH's root) retains all fixed FCH ports below 1000h;
//! the other live roots split the rest in physical order.
use fstart_driver_amd::df::v4::PRIMARY_IOS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoWindow {
    pub start: u32,
    pub end: u32,
    /// First port the PCI allocator may hand out.
    pub pci_start: u32,
}

/// Each root's 16-KiB window: the primary first at 0, then the others in
/// physical order; the last one extends to 10000h so the partition is
/// complete. Genoa (IOS0..3): 2 → 0, 0 → 4000h, 1 → 8000h, 3 → C000h.
/// Siena (IOS1 fused off): 2 → 0, 0 → 4000h, 3 → 8000h..10000h.
pub fn io_window(roots: &[u8], physical: u8) -> Option<IoWindow> {
    if !roots.contains(&PRIMARY_IOS) || !roots.contains(&physical) || roots.len() > 4 {
        return None;
    }
    let others = roots.iter().filter(|&&r| r != PRIMARY_IOS);
    let index = if physical == PRIMARY_IOS {
        0
    } else {
        1 + others.clone().filter(|&&r| r < physical).count() as u32
    };
    let last = index as usize + 1 == roots.len();
    let start = index * 0x4000;
    Some(IoWindow {
        start,
        end: if last { 0x10000 } else { start + 0x4000 },
        pci_start: if physical == PRIMARY_IOS {
            0x1000
        } else {
            start
        },
    })
}

/// Route the complete 16-bit I/O space to the live roots and prove the
/// primary still reaches the FCH (PM1 control mirror, live PM timer at
/// `pmbase`). Returns each root's PCI I/O budget, in `roots` order.
/// # Safety
/// Checked one-socket Rs, BSP-exclusive DF, interrupts disabled on all CPUs,
/// SMI generation disabled, and no port-I/O consumer may run during
/// installation. On any error halt without issuing POST/port I/O.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn initialize(
    roots: &[fstart_driver_amd::df::v4::Root],
    fch: &fstart_driver_amd::fch::Fch<impl fstart_driver_amd::fch::Bus>,
    pmbase: u16,
) -> Result<heapless::Vec<(u64, u64), 4>, ()> {
    use fstart_driver_amd::{
        df::v4::{self, io},
        fch,
    };
    use tock_registers::interfaces::Readable;
    let physical: heapless::Vec<u8, 4> = roots.iter().map(|r| r.ios.physical_index).collect();
    if roots.is_empty()
        || roots.len() > 4
        || fch.reg(fch::PM_ACPI_PM1_CNT).get() != pmbase + 4
        || fch.reg(fch::PM_ACPI_PM_TMR).get() != pmbase + 8
        || !fch
            .reg(fch::SMI_TRIGGER0)
            .is_set(fch::SmiTrigger0::SMI_DISABLE)
    {
        return Err(());
    }
    let mut budgets = heapless::Vec::new();
    let mut routes = heapless::Vec::<io::Route, 4>::new();
    let mut seen = 0u8;
    let mut primary = None;
    for root in roots {
        let index = root.ios.physical_index;
        let window = io_window(&physical, index).ok_or(())?;
        if seen & (1 << index) != 0 {
            return Err(());
        }
        seen |= 1 << index;
        if index == PRIMARY_IOS {
            primary = Some(root.ios.fabric_id);
        }
        budgets
            .push((
                u64::from(window.pci_start),
                u64::from(window.end - window.pci_start),
            ))
            .map_err(|_| ())?;
        routes
            .push(io::Route {
                start: window.start,
                end: window.end,
                destination: root.ios.fabric_id,
                isa: false,
            })
            .map_err(|_| ())?;
    }
    routes.sort_unstable_by_key(|r| r.start);
    // SAFETY: caller's checked/exclusive DF4 contract; ECAM/FICAA3 MMIO.
    let df = unsafe { v4::Df::new(fstart_pci::ecam::GlobalEcam) };
    let table = io::snapshot(&df).map_err(|_| ())?;
    for (pair, raw) in table.iter().enumerate() {
        fstart_log::info!(
            "rs: DF I/O preflight pair={} base={:#x} limit={:#x}",
            pair,
            raw.base,
            raw.limit
        );
    }
    let plan = io::Plan::prepare(&table, primary.ok_or(())?, &routes).map_err(|error| {
        fstart_log::error!("rs: unsupported DF I/O reset layout: {:?}", error);
    })?;
    // SAFETY: disjoint complete 16-bit I/O partition, primary fixed FCH
    // prefix retained; checked reset-only prior ownership, no PIO in driver.
    unsafe { plan.install(&df) }.map_err(|error| {
        fstart_log::error!("rs: DF I/O install failed: {:?}; halt without PIO", error);
    })?;
    // Prove the selected primary also reaches real FCH I/O, not only DF
    // readback: stable PM1 control mirror and a live matching PM timer.
    let control = fch.bus().reg(fch::Reg::<u16>::io(pmbase + 4)).get();
    let before = fch.bus().reg(fch::Reg::<u32>::io(pmbase + 8)).get();
    let mirror = fch.reg(fch::acpi::at::<u32, ()>(0x08)).get();
    let after = fch.bus().reg(fch::Reg::<u32>::io(pmbase + 8)).get();
    let delta = after.wrapping_sub(before) & 0x00ffffff;
    if control == u16::MAX
        || control != fch.reg(fch::acpi::at::<u16, ()>(0x04)).get()
        || delta == 0
        || delta > 0x10000
        || (mirror.wrapping_sub(before) & 0x00ffffff) > delta
    {
        fstart_log::error!("rs: FCH I/O mirror/timer verification failed; no further PIO");
        return Err(());
    }
    fstart_log::info!(
        "rs: FCH I/O verified PM1-control={:#x} timer-delta={}",
        control,
        delta
    );
    for route in routes {
        fstart_log::info!(
            "rs: DF I/O {:#x}..{:#x} destination={:#x}",
            route.start,
            route.end,
            route.destination
        );
    }
    Ok(budgets)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn genoa_keeps_its_four_window_partition() {
        let roots = [0, 1, 2, 3];
        let windows = [2, 0, 1, 3].map(|i| io_window(&roots, i).unwrap());
        assert_eq!(windows[0].start, 0);
        assert_eq!(windows[0].pci_start, 0x1000);
        for pair in windows.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        assert_eq!(
            [windows[1].start, windows[2].start, windows[3].start],
            [0x4000, 0x8000, 0xc000]
        );
        assert_eq!(windows[3].end, 0x10000);
        assert_eq!(io_window(&roots, 4), None);
    }
    #[test]
    fn a_fused_root_leaves_no_gap() {
        let roots = [0, 2, 3];
        let windows = [2, 0, 3].map(|i| io_window(&roots, i).unwrap());
        assert_eq!(
            windows.map(|w| (w.start, w.end)),
            [(0, 0x4000), (0x4000, 0x8000), (0x8000, 0x10000)]
        );
        assert_eq!(io_window(&roots, 1), None);
        assert_eq!(io_window(&[0, 3], 0), None, "the primary root is required");
    }
}
