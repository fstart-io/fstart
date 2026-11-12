//! Shared PCI vocabulary, constants, and config-space helpers.
//!
//! This crate is deliberately backend-neutral: it re-exports the canonical
//! `pci_types` address/configuration API and adds fstart's resource windows,
//! standard register constants, typed overlays, and ECAM allocator.

#![no_std]

pub mod config;
pub mod ecam;
pub mod ecam_host;
pub mod overlay;
pub mod register;
pub mod window;

pub use config::*;
pub use ecam::EcamDevice;
pub use ecam_host::{
    MAX_PCI_FIXED_BARS, MAX_PCI_ROOTS, PciEcam, PciEcamError, PciFixedBar, PciFixedBarType,
    PciFixedBars,
};
pub use overlay::{PciType0Config, PciType1Config};
pub use pci_types::{ConfigRegionAccess, HeaderType, PciAddress, PciHeader, capability};
pub use register::{ConfigRegister, bdf, devfn, devfn_parts};
pub use window::{PciWindow, PciWindowKind};

#[doc(hidden)]
pub mod fstart_core {
    pub use fstart_core::*;
}

pub use fstart_core::description::PciRootInfo;

/// The most ranges each view of one PCI root bridge holds: what it
/// forwards, what the allocator may use, and its motherboard resources.
pub const MAX_PCI_ROOT_WINDOWS: usize = 16;

type Ranges = heapless::Vec<PciWindow, MAX_PCI_ROOT_WINDOWS>;

/// What a range of a root bridge's address space is for.
///
/// | role        | forwarded (`_CRS`) | allocator | motherboard resource |
/// |-------------|--------------------|-----------|----------------------|
/// | `Window`    | yes                | yes       | no                   |
/// | `Forwarded` | yes                | no        | no                   |
/// | `Kept`      | yes                | never     | yes                  |
/// | `Own`       | never              | never     | yes                  |
///
/// Ranges may overlap. A `Window` inside a `Forwarded` range is the part of
/// it PCI devices get. `Kept` and `Own` take their range away from the
/// allocator whatever window covers it, and `Own` from what is forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciRole {
    /// Forwarded to PCI devices: the allocator's.
    Window,
    /// Forwarded: to devices at fixed addresses that describe themselves in
    /// the tables (legacy I/O, option ROMs, an FCH's fixed decodes), and to
    /// PCI devices where a `Window` says so.
    Forwarded,
    /// Forwarded, and kept by the firmware as a motherboard resource (a TPM
    /// on LPC, which may have no node of its own).
    Kept,
    /// Decoded by the chipset itself and not forwarded (the ECAM window,
    /// chipset BARs, an NBIO's non-PCI pool): a motherboard resource.
    Own,
}

impl PciRole {
    /// `size` ports from `base` in this role.
    pub const fn io(self, base: u64, size: u64) -> PciResource {
        PciResource {
            role: self,
            range: PciWindow::io(base, size),
        }
    }

    /// `size` bytes of memory from `base` in this role.
    pub const fn mmio(self, base: u64, size: u64) -> PciResource {
        PciResource {
            role: self,
            range: PciWindow::mmio(base, size),
        }
    }
}

/// One line of a root bridge's description: a range and what it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciResource {
    pub role: PciRole,
    pub range: PciWindow,
}

/// A PCI root bridge: its identity and what its address space is for.
///
/// This is the one description of a root bridge and the only way into the
/// allocator. A chipset or SoC lists its resources once ([`PciRole`]), from
/// what decides them (everything that is not DRAM on an Intel host bridge,
/// a device tree, the Data Fabric's routes). Three readers take everything
/// from it and nothing from anywhere else:
///
/// * the allocator ([`PciEcam::new`]) works in [`Self::allocatable`];
/// * the host bridge's `_CRS` reports [`Self::forwarded`];
/// * a motherboard device (`PNP0C02`) claims [`Self::motherboard`].
///
/// What the allocator may use is by construction part of what `_CRS`
/// reports, and nothing of a motherboard resource is in either unless the
/// bridge forwards it.
///
/// The description is the bridge's, not the board's. What a board or a
/// chipset device holds inside a window (a fixed BAR, an embedded
/// controller's ports) is given to the allocator itself
/// ([`PciEcam::add_fixed_bars`], [`PciEcam::reserve_io_range`],
/// [`PciEcam::reserve_mmio_range`]) and described by that device's own
/// node; it does not belong in this list.
#[derive(Debug, Clone)]
pub struct PciRoot {
    info: PciRootInfo,
    forwarded: Ranges,
    allocatable: Ranges,
    motherboard: Ranges,
}

/// `ranges` with `range` joined in: merged into the first range it touches
/// or overlaps, with every other range the result then touches, or added at
/// the end. The order of description stays.
fn join(ranges: &mut Ranges, range: PciWindow) -> Result<(), PciEcamError> {
    let touches =
        |a: &PciWindow, b: &PciWindow| a.kind == b.kind && a.base <= b.end() && b.base <= a.end();
    let Some(mut first) = ranges.iter().position(|other| touches(other, &range)) else {
        return ranges
            .push(range)
            .map_err(|_| PciEcamError::ResourceRangeLimit);
    };
    let mut joined = range;
    let mut take = Some(first);
    while let Some(index) = take {
        let end = joined.end().max(ranges[index].end());
        joined.base = joined.base.min(ranges[index].base);
        joined.size = end - joined.base;
        if index != first {
            ranges.remove(index);
            first -= usize::from(index < first);
        }
        ranges[first] = joined;
        take = (0..ranges.len()).find(|&other| other != first && touches(&ranges[other], &joined));
    }
    Ok(())
}

/// `ranges` with `range` listed as it was described. Motherboard resources
/// are not joined: an OS reserves each one whole or not at all, and not at
/// all one that is partly in a window of the bridge (a range kept next to
/// one owned) or partly a driver's already.
fn listed(ranges: &mut Ranges, range: PciWindow) -> Result<(), PciEcamError> {
    ranges
        .push(range)
        .map_err(|_| PciEcamError::ResourceRangeLimit)
}

/// `ranges` without `hole`, a range cut in two staying in its place.
fn cut(ranges: &Ranges, hole: &PciWindow) -> Result<Ranges, PciEcamError> {
    let mut left = Ranges::new();
    for range in ranges {
        let parts = if range.kind == hole.kind {
            [
                (range.base, hole.base.min(range.end())),
                (hole.end().max(range.base), range.end()),
            ]
        } else {
            [(range.base, range.end()), (0, 0)]
        };
        for (base, end) in parts {
            if base < end {
                left.push(PciWindow {
                    kind: range.kind,
                    base,
                    size: end - base,
                })
                .map_err(|_| PciEcamError::ResourceRangeLimit)?;
            }
        }
    }
    Ok(left)
}

impl PciRoot {
    /// The root bridge `info` with `resources`. Empty ranges are dropped;
    /// a description with more ranges than [`MAX_PCI_ROOT_WINDOWS`] in one
    /// of its views is refused, not cut short.
    pub fn new(
        info: PciRootInfo,
        resources: impl IntoIterator<Item = PciResource>,
    ) -> Result<Self, PciEcamError> {
        if info.bus_end < info.bus_start {
            return Err(PciEcamError::ConfigError);
        }
        let (mut forwarded, mut allocatable, mut motherboard) =
            (Ranges::new(), Ranges::new(), Ranges::new());
        let mut taken: heapless::Vec<PciResource, { 2 * MAX_PCI_ROOT_WINDOWS }> =
            heapless::Vec::new();
        for resource in resources {
            let range = resource.range;
            if range.size == 0 {
                continue;
            }
            let end = range.base.checked_add(range.size);
            let ports = range.kind == PciWindowKind::Io;
            if end.is_none_or(|end| ports && end > 0x1_0000) {
                return Err(PciEcamError::ConfigError);
            }
            match resource.role {
                PciRole::Window => {
                    join(&mut forwarded, range)?;
                    join(&mut allocatable, range)?;
                }
                PciRole::Forwarded => join(&mut forwarded, range)?,
                PciRole::Kept => {
                    join(&mut forwarded, range)?;
                    listed(&mut motherboard, range)?;
                }
                PciRole::Own => listed(&mut motherboard, range)?,
            }
            if matches!(resource.role, PciRole::Kept | PciRole::Own) {
                taken
                    .push(resource)
                    .map_err(|_| PciEcamError::ResourceRangeLimit)?;
            }
        }
        // The allocator gives way to what is kept and owned; what the
        // chipset decodes itself is not forwarded, whatever else covers it.
        for resource in &taken {
            allocatable = cut(&allocatable, &resource.range)?;
            if resource.role == PciRole::Own {
                forwarded = cut(&forwarded, &resource.range)?;
            }
        }
        Ok(Self {
            info,
            forwarded,
            allocatable,
            motherboard,
        })
    }

    /// The root bridge's identity and place in the ECAM.
    pub fn info(&self) -> PciRootInfo {
        self.info
    }

    /// The buses the root bridge owns.
    pub fn buses(&self) -> core::ops::RangeInclusive<u8> {
        self.info.bus_start..=self.info.bus_end
    }

    /// What the root bridge forwards, to PCI devices and to fixed ones: its
    /// `_CRS`. Disjoint, in the order of description.
    pub fn forwarded(&self) -> &[PciWindow] {
        &self.forwarded
    }

    /// What the allocator may give to PCI devices: part of
    /// [`Self::forwarded`]. Disjoint; the allocator fills it in this order.
    pub fn allocatable(&self) -> &[PciWindow] {
        &self.allocatable
    }

    /// What the firmware keeps and the chipset decodes itself: the
    /// resources of a motherboard device, one for one as described.
    pub fn motherboard(&self) -> &[PciWindow] {
        &self.motherboard
    }

    /// The forwarded I/O as port ranges.
    pub fn io(&self) -> impl Iterator<Item = core::ops::RangeInclusive<u16>> + '_ {
        self.ranges(PciWindowKind::Io)
            .map(|range| *range.start() as u16..=*range.end() as u16)
    }

    /// The forwarded memory as address ranges.
    pub fn mmio(&self) -> impl Iterator<Item = core::ops::RangeInclusive<u64>> + '_ {
        self.ranges(PciWindowKind::Mmio)
    }

    fn ranges(
        &self,
        kind: PciWindowKind,
    ) -> impl Iterator<Item = core::ops::RangeInclusive<u64>> + '_ {
        self.forwarded
            .iter()
            .filter(move |window| window.kind == kind)
            .map(|window| window.base..=window.end() - 1)
    }
}
