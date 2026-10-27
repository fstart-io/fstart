// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric (DF2) address maps Raven/Picasso and Zeppelin share: the
//! PCI bus maps, the MMIO and I/O base/limit pairs and the VGA route of
//! D18F0. What AGESA calls a broadcast access
//! is a direct access repeated on every die's DF: every die routes the whole
//! map. What the maps route to one root complex is its [`RootComplex`].
//!
//! The bus maps are at D18F0xa0 and the MMIO pairs at D18F0x200 up to
//! Cezanne and Mendocino; Phoenix, Glinda and Genoa moved them to 0xc80 and
//! 0xd80.
use super::{Df, DfReg, Die};
use crate::soc::Soc;
use core::ops::RangeInclusive;
use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    pub CfgMap [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST_FABRIC_ID OFFSET(4) NUMBITS(8) [],
        BUS_BASE OFFSET(16) NUMBITS(8) [],
        BUS_LIMIT OFFSET(24) NUMBITS(8) []
    ],
    pub MmioCtrl [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST_FABRIC_ID OFFSET(4) NUMBITS(8) [],
        NON_POSTED OFFSET(12) NUMBITS(1) []
    ],
    pub IoBase [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(13) []
    ],
    pub IoLimit [
        DST_FABRIC_ID OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(12) NUMBITS(13) []
    ],
    pub VgaEn [
        ENABLE OFFSET(0) NUMBITS(1) [],
        NP OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST_FABRIC_ID OFFSET(4) NUMBITS(8) []
    ],
    pub InstanceCount [COUNT OFFSET(0) NUMBITS(8) []],
    pub DramBase [VALID OFFSET(0) NUMBITS(1) [], HOLE_EN OFFSET(1) NUMBITS(1) []]
];

pub const INSTANCE_COUNT: DfReg<InstanceCount::Register> = DfReg::new(0, 0x040);
/// DRAM address range `i` (of a CS instance).
pub const fn dram_base(i: u16) -> DfReg<DramBase::Register> {
    DfReg::new(0, 0x110 + 8 * i)
}
/// The error status of every instance, zero when clear.
const ERR_STATUS: [DfReg; 2] = [DfReg::new(6, 0x1f0), DfReg::new(6, 0x1f4)];

/// Clear the error status the reset left on every fabric instance. DF error
/// status takes a zero write, not a write-one-to-clear.
pub fn clear_errors(df: &Df<impl ConfigRegionAccess>) {
    let instances = df.reg(INSTANCE_COUNT).read(InstanceCount::COUNT) as u8;
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

/// PCI bus map `map`.
pub const fn cfg_map(map: u8) -> DfReg<CfgMap::Register> {
    DfReg::new(0, 0x0a0 + 4 * map as u16)
}

/// [31:0] base >> 16.
pub const fn mmio_base(pair: u8) -> DfReg {
    DfReg::new(0, 0x200 + 0x10 * pair as u16)
}
/// [31:0] limit >> 16.
pub const fn mmio_limit(pair: u8) -> DfReg {
    DfReg::new(0, 0x204 + 0x10 * pair as u16)
}
pub const fn mmio_ctrl(pair: u8) -> DfReg<MmioCtrl::Register> {
    DfReg::new(0, 0x208 + 0x10 * pair as u16)
}
pub const fn io_base(pair: u8) -> DfReg<IoBase::Register> {
    DfReg::new(0, 0x0c0 + 8 * pair as u16)
}
pub const fn io_limit(pair: u8) -> DfReg<IoLimit::Register> {
    DfReg::new(0, 0x0c4 + 8 * pair as u16)
}
pub const VGA_EN: DfReg<VgaEn::Register> = DfReg::new(0, 0x080);

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
        destination: ctrl.read(MmioCtrl::DST_FABRIC_ID) as u8,
        read: ctrl.is_set(MmioCtrl::RE),
        write: ctrl.is_set(MmioCtrl::WE),
        non_posted: ctrl.is_set(MmioCtrl::NON_POSTED),
    }
}

/// I/O pair `pair` of `df`.
pub fn io_route(df: &Df<impl ConfigRegionAccess>, pair: u8) -> Route {
    let base = df.reg(io_base(pair)).extract();
    let limit = df.reg(io_limit(pair)).extract();
    let start = u64::from(base.read(IoBase::BASE)) << 12;
    Route {
        base: start,
        size: ((u64::from(limit.read(IoLimit::LIMIT)) + 1) << 12).saturating_sub(start),
        destination: limit.read(IoLimit::DST_FABRIC_ID) as u8,
        read: base.is_set(IoBase::RE),
        write: base.is_set(IoBase::WE),
        non_posted: false,
    }
}

/// The buses of the first enabled bus map of `df` to the fabric ID
/// `destination`.
pub fn buses(df: &Df<impl ConfigRegionAccess>, destination: u8) -> Option<RangeInclusive<u8>> {
    (0..CFG_MAPS as u8)
        .map(|map| df.reg(cfg_map(map)).extract())
        .find(|map| {
            map.is_set(CfgMap::RE)
                && map.is_set(CfgMap::WE)
                && map.read(CfgMap::DST_FABRIC_ID) == u32::from(destination)
        })
        .map(|map| map.read(CfgMap::BUS_BASE) as u8..=map.read(CfgMap::BUS_LIMIT) as u8)
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

/// Route the legacy VGA ranges to the fabric ID `destination`; call on every
/// die's DF.
pub fn route_vga(df: &Df<impl ConfigRegionAccess>, destination: u8) {
    df.reg(VGA_EN).modify(
        VgaEn::ENABLE::SET + VgaEn::CPU_DIS::CLEAR + VgaEn::DST_FABRIC_ID.val(destination.into()),
    );
}

// ---- New routes ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// TOM or the ECAM window are outside the DF MMIO model.
    Inputs,
    /// No room for a die's MMIO window.
    NoSpace(u8),
    TooManyWindows,
}

/// A DF MMIO or I/O base/limit pair routed to a die's IOMS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub pair: u8,
    pub base: u64,
    pub size: u64,
    pub die: u8,
}
impl Window {
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// The windows to program, [`program`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub mmio: Vec<Window, MMIO_PAIRS>,
    pub io: Vec<Window, IO_PAIRS>,
}

const ALIGN_16M: u64 = !0xff_ffff;
const GIB: u64 = 1 << 30;

fn push(
    windows: &mut Vec<Window, MMIO_PAIRS>,
    pair: usize,
    base: u64,
    size: u64,
    die: u8,
) -> Result<(), Error> {
    if pair >= MMIO_PAIRS {
        return Err(Error::TooManyWindows);
    }
    windows
        .push(Window {
            pair: pair as u8,
            base,
            size,
            die,
        })
        .map_err(|_| Error::TooManyWindows)
}

/// The vendor MMIO/IO distribution for one socket with `n` dies.
///
/// The windows below 4 GiB split the space between TOM and the PCIe
/// configuration space and the space between the configuration space and
/// 0xfe000000 between the dies; die 0's window also covers up to 0xfebfffff.
/// Above 4 GiB, every die gets an equal part of the larger of
/// [TOM2, 0xfd_0000_0000) and [1 TiB, 0x7fd_0000_0000); the last die gets the
/// rest up to 2^48. A spare window routes the HPET range to die 0.
pub fn plan(tom: u64, tom2: u64, cfg_size: u64, n: u8) -> Result<Plan, Error> {
    let cfg_end = 0xf000_0000 + cfg_size;
    if cfg_end > 0xfe00_0000 || tom > 0xf000_0000 || n == 0 || n > 8 {
        return Err(Error::Inputs);
    }
    let nn = u64::from(n);
    let above = 0xfe00_0000 - cfg_end;
    let below = 0xf000_0000 - tom;
    let mut total = above + below;
    let (mut above_small, mut below_small) = (false, false);
    if n > 1 {
        if above < (below / nn) * 8 / 10 {
            total -= above;
            above_small = true;
        }
        if below < (above / nn) * 8 / 10 {
            total -= below;
            below_small = true;
        }
        if above_small && below_small {
            return Err(Error::NoSpace(0));
        }
    }
    let mut plan = Plan::default();
    let per = total / nn;
    let per_al = per & ALIGN_16M;
    let (dies_above, mut size, mut cursor) = if above_small {
        (0, below - per_al * (nn - 1), 0xf000_0000u64)
    } else {
        let mut dies_above = above / (per & 0xffff_ffff);
        if above - per * dies_above > per / 2 {
            dies_above += 1;
        }
        let dies_above = dies_above.max(1);
        (
            dies_above,
            above - per_al * (dies_above - 1),
            0xfe00_0000u64,
        )
    };
    let mut left = dies_above;
    let mut below_used = 0u32;
    let mut per_b_al = 0;
    for k in 0..u64::from(n) {
        below_used |= 1 << k;
        if left == 0 {
            if below_small {
                return Err(Error::NoSpace(k as u8));
            }
            if k == dies_above {
                let m = nn - k;
                per_b_al = (below / m) & ALIGN_16M;
                size = below - per_b_al * (m - 1);
                cursor = 0xf000_0000;
            }
            if k == dies_above + 1 {
                size = per_b_al;
            }
        } else {
            if k == 1 {
                size = per_al;
            }
            left -= 1;
        }
        cursor -= size;
        let window = if k == 0 && !above_small {
            size + 0xc0_0000
        } else {
            size
        };
        push(&mut plan.mmio, 2 * k as usize, cursor, window, k as u8)?;
    }

    let mut second = None;
    if n < 8 && ((above_small && above != 0) || (below != 0 && (below_small || n == 1))) {
        let d = (0..8)
            .find(|d| below_used & (1 << d) == 0)
            .ok_or(Error::TooManyWindows)?;
        second = Some(d);
        if above_small {
            push(&mut plan.mmio, 2 * d, cfg_end, 0xfec0_0000 - cfg_end, 0)?;
        } else {
            push(&mut plan.mmio, 2 * d, tom, below, 0)?;
        }
    }

    // Above 4 GiB, skipping the HyperTransport hole at 0xfd_0000_0000.
    let mut above_used = 0u32;
    let tib = 1u64 << 40;
    let mut base = tom2.max(4 * GIB);
    let mut limit = 0x7fdu64 << 32;
    let size1 = if (base >> 32) < 0xfd {
        (0xfd << 32) - base
    } else {
        0
    };
    let size2 = if limit > tib {
        limit - base.max(tib)
    } else {
        0
    };
    if size1 == 0 && size2 == 0 {
        limit = 0;
    } else if size2 <= size1 {
        limit = 0xfd << 32;
    } else {
        base = tom2.max(tib);
    }
    if limit > base {
        let span = limit - base;
        let per_die = (span / nn) & ALIGN_16M;
        for k in 0..n {
            let last = k == n - 1;
            let size = if last {
                span - per_die * (nn - 1)
            } else {
                per_die
            };
            above_used |= 1 << k;
            if size >= 0x1_0000 {
                let window = if last { (1 << 48) - base } else { size };
                push(&mut plan.mmio, 2 * usize::from(k) + 1, base, window, k)?;
            }
            base += size;
        }
    }

    // HPET: the first unused below-4G pair, else the first unused above-4G pair.
    for d in 0..8usize {
        if below_used & (1 << d) == 0 && second != Some(d) {
            push(&mut plan.mmio, 2 * d, 0xfed0_0000, 0x1_0000, 0)?;
            break;
        }
        if below_used & (1 << d) != 0 && above_used & (1 << d) == 0 {
            push(&mut plan.mmio, 2 * d + 1, 0xfed0_0000, 0x1_0000, 0)?;
            break;
        }
    }

    // I/O: die 0 gets an extra 4 KiB, the last die the rest of the 32 MiB space.
    let q = (0xf000 / u64::from(n)) & !0xfff;
    let mut io_cursor = 0;
    for k in 0..n {
        let size = if k == n - 1 {
            0x200_0000 - io_cursor
        } else if k == 0 {
            q + 0x1000
        } else {
            q
        };
        plan.io
            .push(Window {
                pair: k,
                base: io_cursor,
                size,
                die: k,
            })
            .map_err(|_| Error::TooManyWindows)?;
        io_cursor += size;
    }
    Ok(plan)
}

/// Program the plan into every die's DF, each window routed to the IOMS of
/// its die. Base, limit and control each go to every die, in that order.
pub fn program<S: Soc, P: ConfigRegionAccess + Copy>(plan: &Plan, dies: &[Die<S, P>]) {
    let Some(df0) = dies.first().map(Die::df) else {
        return;
    };
    for w in &plan.mmio {
        let mut ctrl = df0.reg(mmio_ctrl(w.pair)).extract();
        ctrl.modify(
            MmioCtrl::DST_FABRIC_ID.val(S::ioms_fabric_id(w.die).into())
                + MmioCtrl::RE::SET
                + MmioCtrl::WE::SET,
        );
        dies.iter().for_each(|die| {
            die.df()
                .reg(mmio_base(w.pair))
                .set(((w.base + 0xffff) >> 16) as u32)
        });
        dies.iter().for_each(|die| {
            die.df()
                .reg(mmio_limit(w.pair))
                .set(((w.end() - 1) >> 16) as u32)
        });
        dies.iter()
            .for_each(|die| die.df().reg(mmio_ctrl(w.pair)).set(ctrl.get()));
    }
    for w in &plan.io {
        let end = (w.end() - 1) as u32;
        let mut limit = df0.reg(io_limit(w.pair)).extract();
        limit.modify(
            IoLimit::DST_FABRIC_ID.val(S::ioms_fabric_id(w.die).into())
                + IoLimit::LIMIT.val(end >> 12),
        );
        let base = || IoBase::RE::SET + IoBase::WE::SET + IoBase::BASE.val(w.base as u32 >> 12);
        dies.iter()
            .for_each(|die| die.df().reg(io_base(w.pair)).write(base()));
        dies.iter()
            .for_each(|die| die.df().reg(io_limit(w.pair)).set(limit.get()));
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn mj11_single_die_plan() {
        // MJ11-EC1: TOM 0xe0000000, TOM2 0x420000000, 128-bus ECAM.
        let plan = plan(0xe000_0000, 0x4_2000_0000, 128 << 20, 1).unwrap();
        let mmio: std::vec::Vec<_> = plan
            .mmio
            .iter()
            .map(|w| (w.pair, w.base, w.end(), w.die))
            .collect();
        assert_eq!(
            mmio,
            [
                // Die 0: [ECAM end, 0xfe000000) plus the 12 MiB up to 0xfec00000.
                (0, 0xf800_0000, 0xfec0_0000, 0),
                // The space between TOM and the ECAM.
                (2, 0xe000_0000, 0xf000_0000, 0),
                // [1 TiB, 0x7fd_0000_0000) is larger than [TOM2, 0xfd_0000_0000).
                (1, 1 << 40, 1 << 48, 0),
                (4, 0xfed0_0000, 0xfed1_0000, 0),
            ]
        );
        assert_eq!(
            plan.io.as_slice(),
            [Window {
                pair: 0,
                base: 0,
                size: 0x200_0000,
                die: 0
            }]
        );
    }
}
