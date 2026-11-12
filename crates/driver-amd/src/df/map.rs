// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric (DF2) address maps Raven/Picasso and Zeppelin share, as
//! routed: the PCI bus maps and the MMIO and I/O base/limit pairs of
//! D18F0. What AGESA calls a broadcast access is a direct access repeated
//! on every die's DF: every die routes the whole map. What the maps route
//! to one root complex is its [`RootComplex`]. New routes come from
//! [`super::plan`] through [`super::program`].
//!
//! The bus maps are at D18F0xa0 and the MMIO pairs at D18F0x200 up to
//! Cezanne and Mendocino; Phoenix, Glinda and Genoa moved them to 0xc80 and
//! 0xd80.
use super::{Df, DfReg, Die};
use crate::soc::Soc;
use core::ops::RangeInclusive;
use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::interfaces::{Readable, Writeable};

use super::{
    regs::{self, FBICNT, IO_BASE},
    v2::{
        CFG_MAP, IO_LIMIT, MMIO_CTRL, cfg_map, io_base, io_limit, mmio_base, mmio_ctrl, mmio_limit,
    },
};

/// The error status of every instance, zero when clear.
const ERR_STATUS: [DfReg; 2] = [regs::PIE_RAS_STATUS_LO, regs::PIE_RAS_STATUS_HI];

/// Clear the error status the reset left on every fabric instance. DF error
/// status takes a zero write, not a write-one-to-clear.
pub fn clear_errors(df: &Df<impl ConfigRegionAccess>) {
    let instances = df.reg(regs::FBICNT).read(FBICNT::COUNT) as u8;
    for instance in 0..instances {
        for status in ERR_STATUS {
            let status = df.instance(status, instance);
            if status.get() != 0 {
                status.set(0);
            }
        }
    }
}

pub const CFG_MAPS: usize = 8;
pub const MMIO_PAIRS: usize = 8;
pub const IO_PAIRS: usize = 8;

// ---- The routes in place ----

/// A DF MMIO or I/O base/limit pair as the hardware has it; empty if the
/// limit is below the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub base: u64,
    pub size: u64,
    /// Fabric ID of the destination (an IOMS).
    pub destination: u8,
    pub read: bool,
    pub write: bool,
    /// MMIO only.
    pub non_posted: bool,
}
impl Route {
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
    /// Decodes reads and writes.
    pub const fn enabled(&self) -> bool {
        self.read && self.write
    }
    /// Decodes neither: the pair can take a new window.
    pub const fn free(&self) -> bool {
        !self.read && !self.write
    }
}

/// MMIO pair `pair` of `df`.
pub fn mmio_route(df: &Df<impl ConfigRegionAccess>, pair: u8) -> Route {
    let ctrl = df.reg(mmio_ctrl(pair)).extract();
    let base = u64::from(df.reg(mmio_base(pair)).get()) << 16;
    let end = (u64::from(df.reg(mmio_limit(pair)).get()) + 1) << 16;
    Route {
        base,
        size: end.saturating_sub(base),
        destination: ctrl.read(MMIO_CTRL::DST) as u8,
        read: ctrl.is_set(MMIO_CTRL::RE),
        write: ctrl.is_set(MMIO_CTRL::WE),
        non_posted: ctrl.is_set(MMIO_CTRL::NP),
    }
}

/// I/O pair `pair` of `df`.
pub fn io_route(df: &Df<impl ConfigRegionAccess>, pair: u8) -> Route {
    let base = df.reg(io_base(pair)).extract();
    let limit = df.reg(io_limit(pair)).extract();
    let start = u64::from(base.read(IO_BASE::BASE)) << 12;
    Route {
        base: start,
        size: ((u64::from(limit.read(IO_LIMIT::LIMIT)) + 1) << 12).saturating_sub(start),
        destination: limit.read(IO_LIMIT::DST) as u8,
        read: base.is_set(IO_BASE::RE),
        write: base.is_set(IO_BASE::WE),
        non_posted: false,
    }
}

/// The buses of the first enabled bus map of `df` to the fabric ID
/// `destination`.
pub fn buses(df: &Df<impl ConfigRegionAccess>, destination: u8) -> Option<RangeInclusive<u8>> {
    (0..CFG_MAPS as u8)
        .map(|map| df.reg(cfg_map(map)).extract())
        .find(|map| {
            map.is_set(CFG_MAP::RE)
                && map.is_set(CFG_MAP::WE)
                && map.read(CFG_MAP::DST) == u32::from(destination)
        })
        .map(|map| map.read(CFG_MAP::BUS_BASE) as u8..=map.read(CFG_MAP::BUS_LIMIT) as u8)
}

/// The enabled windows of `routes` to the fabric ID `destination`, as
/// (base, size).
fn windows<const N: usize>(
    routes: impl Iterator<Item = Route>,
    destination: u8,
) -> Vec<(u64, u64), N> {
    routes
        .filter(|route| route.enabled() && route.destination == destination)
        .map(|route| (route.base, route.size))
        .collect()
}

/// The enabled MMIO windows of `df` routed to the fabric ID `destination`,
/// as (base, size).
pub fn mmio_windows(
    df: &Df<impl ConfigRegionAccess>,
    destination: u8,
) -> Vec<(u64, u64), MMIO_PAIRS> {
    windows(
        (0..MMIO_PAIRS as u8).map(|pair| mmio_route(df, pair)),
        destination,
    )
}

/// The enabled I/O windows of `df` routed to the fabric ID `destination`,
/// as (base, size).
pub fn io_windows(df: &Df<impl ConfigRegionAccess>, destination: u8) -> Vec<(u64, u64), IO_PAIRS> {
    windows(
        (0..IO_PAIRS as u8).map(|pair| io_route(df, pair)),
        destination,
    )
}

/// What the DF routes to one root complex (the IOMS of a die), the
/// PCI domain of its root bridge. The windows are (base, size) in pair
/// order, the MMIO ones below and above 4 GiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootComplex {
    pub buses: RangeInclusive<u8>,
    pub mmio: Vec<(u64, u64), MMIO_PAIRS>,
    pub io: Vec<(u64, u64), IO_PAIRS>,
}

/// The root complex of `die` as the DF routes it, `None` without a bus map.
pub fn root_complex<S: Soc, P: ConfigRegionAccess + Copy>(die: &Die<S, P>) -> Option<RootComplex> {
    let (df, ioms) = (die.df(), die.ioms_fabric_id());
    Some(RootComplex {
        buses: buses(&df, ioms)?,
        mmio: mmio_windows(&df, ioms),
        io: io_windows(&df, ioms),
    })
}
