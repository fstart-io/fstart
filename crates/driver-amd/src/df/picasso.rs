// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso Data Fabric sequencing (AGESA AmdFabricRv).
use super::{
    Df, DfReg, DfRecord, apply_df, df_rmw, df_write,
    map::{
        self, DramBase, IO_PAIRS, MMIO_PAIRS, Plan, Window, dram_base, io_route, mmio_route,
    },
};
use crate::picasso::Die;
use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::interfaces::Readable;

const IOAPIC: u64 = 0xfec0_0000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MemoryBoundary,
    NoMmioWindow,
    NoIoWindow,
}

/// The windows [`ensure_legacy_routes`] adds to the PSP-established ones:
/// MMIO from `low_top` up to the IOAPIC and I/O 0-0xffff to the IOMS
/// `ioms`, each on the first free pair, unless a window covers it already.
/// Fails without a free pair for a missing window.
pub fn legacy_routes(
    fabric: &Df<impl ConfigRegionAccess>,
    ioms: u8,
    low_top: u32,
) -> Result<Plan, Error> {
    let low_top = u64::from(low_top);
    if low_top == 0 || low_top >= IOAPIC || low_top & 0xffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    let mmio: Vec<_, MMIO_PAIRS> = (0..MMIO_PAIRS as u8).map(|i| mmio_route(fabric, i)).collect();
    let io: Vec<_, IO_PAIRS> = (0..IO_PAIRS as u8).map(|i| io_route(fabric, i)).collect();
    let routed = |route: &map::Route| route.enabled() && route.destination == ioms;
    let have_mmio = mmio
        .iter()
        .any(|r| routed(r) && !r.non_posted && r.base <= low_top && r.end() >= IOAPIC);
    let have_io = io.iter().any(|r| routed(r) && r.base == 0 && r.end() >= 0x1_0000);
    let free = |routes: &[map::Route]| routes.iter().position(map::Route::free).map(|i| i as u8);
    let mut plan = Plan::default();
    if !have_mmio {
        let pair = free(&mmio).ok_or(Error::NoMmioWindow)?;
        let window = Window {
            pair,
            base: low_top,
            size: IOAPIC - low_top,
            die: 0,
        };
        plan.mmio.push(window).map_err(|_| Error::NoMmioWindow)?;
    }
    if !have_io {
        let pair = free(&io).ok_or(Error::NoIoWindow)?;
        let window = Window {
            pair,
            base: 0,
            size: 0x1_0000,
            die: 0,
        };
        plan.io.push(window).map_err(|_| Error::NoIoWindow)?;
    }
    Ok(plan)
}

/// Route peripheral MMIO and I/O to IOMS, preserving PSP-established windows.
/// Discover and validate BOTH free windows before changing either of them.
pub fn ensure_legacy_routes(
    die: &Die<impl ConfigRegionAccess + Copy>,
    low_top: u32,
) -> Result<(), Error> {
    let plan = legacy_routes(&die.df(), die.ioms_fabric_id(), low_top)?;
    map::program(&plan, core::slice::from_ref(die));
    map::route_vga(&die.df(), die.ioms_fabric_id());
    Ok(())
}

const F0_CLK_GATE: DfReg = DfReg::new(0, 0x3f0);
const F0_RAS_CTRL: DfReg = DfReg::new(0, 0x3f8);
const F2_048: DfReg = DfReg::new(2, 0x048);
const F2_050: DfReg = DfReg::new(2, 0x050);

/// The vendor UMC coordination values: their meaning isn't known.
const EARLY_CS: [DfRecord; 4] = [
    df_rmw(DfReg::new(2, 0x040), None, 0, 1 << 30),
    df_rmw(DfReg::new(2, 0x044), None, 1 << 26, 0),
    df_rmw(F2_050, None, 0, 1 << 5),
    df_rmw(F2_048, None, 0, 0x8040),
];
const fn on(record: DfRecord, instance: u8) -> DfRecord {
    DfRecord {
        reg: (record.reg.0, Some(instance)),
        ..record
    }
}

/// Only changes DF coordination registers; never rewrites PSP-trained UMC data.
pub fn early_init(fabric: &Df<impl ConfigRegionAccess>) {
    apply_df(fabric, [df_write(F0_CLK_GATE, None, 1)]);
    apply_df(
        fabric,
        (0..2).flat_map(|instance| EARLY_CS.map(|record| on(record, instance))),
    );
    apply_df(
        fabric,
        [
            df_write(DfReg::new(2, 0x068), None, 0x65019),
            df_write(DfReg::new(2, 0x06c), None, 0x14060619),
        ],
    );
    for instance in 0..2 {
        if !fabric.instance(dram_base(0), instance).is_set(DramBase::VALID) {
            apply_df(fabric, [df_rmw(F2_048, Some(instance), 1 << 19, 0)]);
        }
    }
    apply_df(
        fabric,
        [
            df_rmw(F2_050, None, 1, 0),
            df_write(DfReg::new(4, 0x0c4), Some(2), 0x20001),
            df_write(DfReg::new(4, 0x200), None, 0x1020000),
            df_rmw(DfReg::new(3, 0x104), Some(2), 0, 1 << 28),
            df_rmw(DfReg::new(3, 0x108), Some(2), 0, 1 << 6),
            df_write(DfReg::new(3, 0x20c), None, 3),
        ],
    );
    let channels = (0..2)
        .filter(|&instance| {
            let first = fabric.instance(dram_base(0), instance).get();
            let second = fabric.instance(dram_base(1), instance).get();
            (first | second) & 1 != 0
        })
        .count();
    apply_df(
        fabric,
        [
            df_rmw(
                DfReg::new(4, 0x0c0),
                Some(6),
                0xff0,
                if channels == 2 { 0x480 } else { 0x300 },
            ),
            df_rmw(DfReg::new(3, 0x04c), Some(9), 0, 1 << 5),
            df_rmw(DfReg::new(3, 0x300), Some(9), 0, 1 << 8),
        ],
    );
    map::clear_errors(fabric);
    apply_df(fabric, [df_rmw(F0_RAS_CTRL, None, 1 << 3, 0)]);
}

/// The vendor PIE power values: their meaning isn't known.
pub fn late_init(fabric: &Df<impl ConfigRegionAccess>, raven2: bool) {
    let pie = |offset| DfReg::new(5, offset);
    apply_df(
        fabric,
        [
            df_rmw(
                pie(0x300),
                None,
                (1 << 5) | if raven2 { 1 << 11 } else { 0 },
                if raven2 { 0x3300 } else { 0x3b00 },
            ),
            df_rmw(pie(0x338), None, 0, 1 << 3),
            df_rmw(pie(0x364), None, 0, 1 << 1),
            df_rmw(pie(0x348), None, 0x7f7f7f7f, 0x14171710),
            df_rmw(pie(0x34c), None, 0x00ff0000, 0x00590000),
            df_rmw(pie(0x360), None, 1 << 21, u32::from(raven2)),
        ],
    );
    if raven2 {
        apply_df(fabric, [df_rmw(pie(0x350), None, 0xff000000, 0x0e000000)]);
    }
}
