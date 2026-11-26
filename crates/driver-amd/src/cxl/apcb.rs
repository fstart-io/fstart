// SPDX-License-Identifier: MIT OR Apache-2.0
//! The vendor DXE's APCB loop (CxlV2Manager `CxlMgrUpdateApcb` `0x80E0`,
//! both Venice donors; Turin's CxlManager the same): size every CXL device
//! after enumeration, build the 48-entry region array of APCB DF type
//! `0xCC` from the sizes, and compare it with the array the ABL booted
//! with. A different array is written to the flash and the system
//! warm-resets so the ABL maps the new windows; an equal one with an
//! excluded device clears the failure tracking without a reset
//! (reports/research/CXL_COMPLETE.md §2.1).
//!
//! Here are the rules, on what the platform read of each device
//! ([`Device`]); the region format is `fstart_amdfw_format::apcb`'s, the
//! flash write and the reset are the platform's.
//!
//! Termination (§2.1.2): there is no reset counter. A stable device yields
//! the same array after one reset; a failing one counts up its 2-bit
//! failure count and is excluded at 2 (at most two extra resets), and an
//! unchanged array with an exclusion clears the tracking without a reset.

use fstart_amdfw_format::apcb::{
    ALIGN_4GIB, CXL_ARRAY_SIZE, CXL_REGION_SIZE, CxlMemory, CxlRegion, MAX_CXL_REGIONS,
};
use fstart_pci::cxl::dvsec::{DeviceCapability, RangeSizeLow};
use heapless::Vec;
use tock_registers::LocalRegisterCopy;

use super::DeviceType;

/// The pools of a device are 256 MiB aligned.
pub const POOL_ALIGN: u64 = 1 << 28;
/// A DVSEC range above this is refused (`0x434C_090B`).
pub const MAX_RANGE: u64 = 1 << 56;
/// Most pools of one device.
pub const MAX_POOLS: usize = 8;
/// The failure count that excludes a device.
pub const EXCLUDE_AT: u8 = 2;

/// The memory of a pool (the region's `+0x12`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    Volatile = 0,
    Persistent = 1,
    /// A type-2 device's (or type 1's, size 0).
    DeviceCoherent = 2,
    /// Volatile memory of a device that is neither a memory controller
    /// (class 05h) nor a ... (class 07h): "dual mode".
    DualMode = 3,
}

impl PoolType {
    const fn of(raw: u8) -> Self {
        match raw {
            1 => Self::Persistent,
            2 => Self::DeviceCoherent,
            3 => Self::DualMode,
            _ => Self::Volatile,
        }
    }

    fn memory(self) -> CxlMemory {
        match self {
            Self::Persistent => CxlMemory::Persistent,
            Self::DualMode => CxlMemory::OtherPersistent,
            Self::DeviceCoherent => CxlMemory::Cache,
            Self::Volatile => CxlMemory::Volatile,
        }
    }
}

/// One memory pool of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    pub size: u64,
    pub kind: PoolType,
}

/// The vendor's per-device bookkeeping kept in the region's `+0x1B`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tracking {
    /// Bit 0 memory inactive, bit 1 media/HDM checks, bit 3 link speed
    /// changed.
    pub error: u8,
    pub exclude: bool,
    /// The region's 2-bit count, saturating at [`Self::MAX_FAILURES`]
    /// (CXL_COMPLETE.md §2.1.2: a monotonic count, excluded at
    /// [`EXCLUDE_AT`]).
    pub failures: u8,
}

impl Tracking {
    pub const INACTIVE: u8 = 1 << 0;
    /// fstart's flag for failed media status / HDM capability checks (the
    /// vendor's flag value for them is not decoded).
    pub const CHECKS: u8 = 1 << 1;
    pub const LINK_SPEED: u8 = 1 << 3;
    /// The most the region's 2-bit count holds.
    pub const MAX_FAILURES: u8 = 3;

    /// From a region's `+0x1B`.
    pub const fn of(status: u8) -> Self {
        Self {
            error: status & 0xf,
            exclude: status & 0x10 != 0,
            failures: (status >> 5) & 3,
        }
    }

    /// The region's `+0x1B`.
    pub const fn status(&self) -> u8 {
        let failures = if self.failures < Self::MAX_FAILURES {
            self.failures
        } else {
            Self::MAX_FAILURES
        };
        (self.error & 0xf) | (self.exclude as u8) << 4 | failures << 5
    }

    /// `CxlMgrIncreaseFailureCount`: one more failure; at
    /// [`EXCLUDE_AT`] the device is excluded. The count saturates in its 2
    /// bits (one pass can fail a device more than once). Returns whether
    /// it is excluded.
    pub fn fail(&mut self, error: u8) -> bool {
        self.error |= error;
        self.failures = (self.failures + 1).min(Self::MAX_FAILURES);
        if self.failures >= EXCLUDE_AT {
            self.exclude = true;
        }
        self.exclude
    }
}

/// One DVSEC range as the device reports it: Size High and Size Low.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawRange {
    pub size_high: u32,
    pub size_low: u32,
}

/// What the platform read of one CXL device (or a switch's upstream port)
/// for sizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `(segment, bus, device, function)` of the endpoint.
    pub address: (u8, u8, u8, u8),
    /// `(segment, bus)` the APCB region is found by: the top switch's for
    /// an endpoint behind a switch, else the endpoint's.
    pub key: (u8, u8),
    pub socket: u8,
    pub die: u8,
    /// The root port's PCIe core and its port in the core.
    pub core: u8,
    pub port: u8,
    /// The link's negotiated width (lanes) and current speed (Link Status
    /// [3:0]), and whether it runs in flit mode.
    pub link_width: u8,
    pub link_speed: u8,
    pub flit: bool,
    /// A switch's upstream port: its pools are its endpoints'.
    pub switch: bool,
    /// The device DVSEC's CXL Capability (`0` for a switch).
    pub capability: u16,
    pub ranges: [RawRange; 2],
    /// The lengths of the CDAT's DSMAS ranges, in table order.
    pub dsmas: Vec<u64, MAX_POOLS>,
    /// The CDAT's first DSEMTS EFI memory type.
    pub dsemts: Option<u8>,
    /// A Dynamic Capacity Device's region lengths (the CEL lists Get DC
    /// Configuration).
    pub dcd: Option<Vec<u64, MAX_POOLS>>,
    /// Class code (base class in bits 23:16).
    pub class: u32,
    /// The DVSEC range 1 reports Memory_Active (after the vendor's poll).
    pub memory_active: bool,
    /// The media status and HDM capability checks passed.
    pub healthy: bool,
}

impl Device {
    /// Its CXL device type (`& 5`: 1 type 1, 5 type 2, 4 type 3).
    pub fn device_type(&self) -> DeviceType {
        if self.switch {
            DeviceType::Type3
        } else {
            DeviceType::of_capability(self.capability)
        }
    }

    fn cap(&self) -> LocalRegisterCopy<u16, DeviceCapability::Register> {
        LocalRegisterCopy::new(self.capability)
    }

    /// Whether the device sits behind a memory or "other" class (05h, 07h
    /// by the vendor's check).
    fn memory_class(&self) -> bool {
        matches!(self.class >> 16, 0x05 | 0x07)
    }
}

/// The sizing of one device: its pools, their sum and its tracking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sizing {
    pub pools: Vec<Pool, MAX_POOLS>,
    pub aggregate: Option<Pool>,
    pub tracking: Tracking,
    /// The device's desired interleave (DVSEC Size Low [12:8], max).
    pub max_interleave: u8,
    /// A Dynamic Capacity Device.
    pub dcd: bool,
    /// The APCB had a region for it.
    pub found: bool,
    /// A DVSEC range was larger than [`MAX_RANGE`].
    pub range_too_large: bool,
}

/// What the sizing takes of the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizingPolicy {
    pub dual_hdm_ranges: bool,
}

/// The region of `regions` the device's tracking is kept in
/// (`SearchApcbEntry` `0x5DE0`: by `(segment, bus)`; an excluded device's
/// region has size 0, an unused one bus 0).
pub fn search(regions: &[CxlRegion], key: (u8, u8)) -> Option<&CxlRegion> {
    regions
        .iter()
        .find(|r| (r.size != 0 || r.bus != 0) && (r.segment, r.bus) == key)
}

/// Size `device` (`CxlMgrUpdateApcb`'s per-device steps): DCD regions,
/// else the DVSEC ranges or the CDAT's DSMAS for small ones, the failure
/// tracking of the region `current` it had, the memory-active check when
/// the ABL mapped windows (`apob`). Switches get their endpoints' sum
/// afterwards ([`add_to_switch`]).
pub fn size(
    device: &Device,
    current: Option<&CxlRegion>,
    apob: bool,
    policy: SizingPolicy,
) -> Sizing {
    let mut out = Sizing {
        found: current.is_some(),
        ..Sizing::default()
    };
    if let Some(region) = current {
        out.tracking = Tracking::of(region.status);
        // CxlMgrCheckLinkSpeedChange.
        if region.link_speed != device.link_speed {
            out.tracking.fail(Tracking::LINK_SPEED);
        }
    }
    if let Some(regions) = &device.dcd {
        out.dcd = true;
        let size = regions.iter().fold(0u64, |a, &l| a.saturating_add(l));
        let pool = Pool {
            size,
            kind: PoolType::Volatile,
        };
        let _ = out.pools.push(pool);
        out.aggregate = Some(pool);
        if out.tracking.exclude {
            zero(&mut out);
        }
        return out;
    }
    if device.switch {
        return out;
    }
    if !out.tracking.exclude {
        pools(device, policy, &mut out);
        if apob && current.is_some() && !device.memory_active {
            // CxlMgrMarkPoolInactive.
            out.tracking.fail(Tracking::INACTIVE);
        }
        if !device.healthy {
            out.tracking.fail(Tracking::CHECKS);
        }
    }
    if device.device_type() == DeviceType::Type2 {
        for pool in out.pools.iter_mut() {
            pool.kind = PoolType::DeviceCoherent;
        }
    } else if !device.memory_class() {
        for pool in out
            .pools
            .iter_mut()
            .filter(|p| p.kind == PoolType::Volatile)
        {
            pool.kind = PoolType::DualMode;
        }
    }
    if out.tracking.exclude {
        zero(&mut out);
    }
    // The aggregate: the pools' sum, of the last pool's type (dual mode
    // as persistent); an excluded device's (no pools) is empty, and its
    // region keeps its tracking.
    let size = out.pools.iter().fold(0u64, |a, p| a.saturating_add(p.size));
    let last = match out.pools.last().map_or(PoolType::Volatile, |p| p.kind) {
        PoolType::DualMode => PoolType::Persistent,
        other => other,
    };
    out.aggregate =
        (!out.pools.is_empty() || out.tracking.exclude).then_some(Pool { size, kind: last });
    // CxlMgrReadCdatforMediaType: a non-zero DSEMTS type overrides.
    if let (Some(t), Some(agg)) = (device.dsemts.filter(|&t| t != 0), out.aggregate.as_mut()) {
        agg.kind = PoolType::of(t);
    }
    out
}

/// `CxlMgrGetMemoryPools` `0x70E0`.
fn pools(device: &Device, policy: SizingPolicy, out: &mut Sizing) {
    let kind = device.device_type();
    if kind == DeviceType::Type1 {
        let _ = out.pools.push(Pool {
            size: 0,
            kind: PoolType::DeviceCoherent,
        });
        return;
    }
    let cap = device.cap();
    let hdm_count = cap.read(DeviceCapability::HDM_COUNT);
    let default_kind = if kind == DeviceType::Type3 {
        PoolType::Volatile
    } else {
        PoolType::DeviceCoherent
    };
    let ranges = if hdm_count == 2 && policy.dual_hdm_ranges {
        2
    } else {
        1
    };
    for (n, range) in device.ranges[..ranges].iter().enumerate() {
        let low = LocalRegisterCopy::<u32, RangeSizeLow::Register>::new(range.size_low);
        if !low.is_set(RangeSizeLow::MEMORY_INFO_VALID) {
            continue;
        }
        let raw = u64::from(range.size_high) << 32 | u64::from(range.size_low);
        if n == 0 && raw < POOL_ALIGN {
            // CxlMgrGetMemoryRangeFromCdat: one pool per non-empty DSMAS.
            let most = if policy.dual_hdm_ranges { 2 } else { 1 };
            for &length in device.dsmas.iter().filter(|&&l| l != 0).take(most) {
                let _ = out.pools.push(Pool {
                    size: length,
                    kind: default_kind,
                });
            }
            return;
        }
        let mut size = raw & !(POOL_ALIGN - 1);
        if size > MAX_RANGE {
            out.range_too_large = true;
            size = 0;
        }
        let pool_kind = if low.read(RangeSizeLow::MEDIA_TYPE) & 1 != 0 {
            PoolType::Persistent
        } else {
            default_kind
        };
        out.max_interleave = out
            .max_interleave
            .max(low.read(RangeSizeLow::DESIRED_INTERLEAVE) as u8);
        let _ = out.pools.push(Pool {
            size,
            kind: pool_kind,
        });
    }
}

fn zero(out: &mut Sizing) {
    for pool in out.pools.iter_mut() {
        pool.size = 0;
    }
    if let Some(agg) = out.aggregate.as_mut() {
        agg.size = 0;
    }
}

/// A switch's upstream port gets each endpoint's aggregate.
pub fn add_to_switch(switch: &mut Sizing, endpoint: &Sizing) {
    let Some(add) = endpoint.aggregate else {
        return;
    };
    match switch.aggregate.as_mut() {
        Some(agg) => agg.size = agg.size.saturating_add(add.size),
        None => switch.aggregate = Some(add),
    }
}

/// `CxlMgrNbioIdToBitmap` `0x13D0`: the core's bit (cores below 8).
pub const fn core_bitmap(core: u8) -> u8 {
    if core < 8 { 1 << core } else { 0 }
}

/// The region array of `devices` (`CxlMgrBuildApcbEntry` `0x7CC0`, in
/// device order, at most 48): one region per pool of a device with more
/// than one (not a DCD, not a switch), else one from its aggregate.
pub fn regions(devices: &[(Device, Sizing)]) -> Vec<CxlRegion, MAX_CXL_REGIONS> {
    let mut out: Vec<CxlRegion, MAX_CXL_REGIONS> = Vec::new();
    // (socket, core) with an x8 link so far.
    let mut x8: Vec<(u8, u8), MAX_CXL_REGIONS> = Vec::new();
    for (device, sizing) in devices {
        let sub = sub_interleave(device, &x8);
        if device.link_width == 8 && !x8.contains(&(device.socket, device.core)) {
            let _ = x8.push((device.socket, device.core));
        }
        let per_pool = !sizing.dcd && !device.switch && sizing.pools.len() > 1;
        let pools: Vec<Pool, MAX_POOLS> = if per_pool {
            sizing.pools.clone()
        } else {
            sizing.aggregate.into_iter().collect()
        };
        for pool in pools {
            if out.is_full() {
                return out;
            }
            let _ = out.push(region(device, sizing, pool, sub));
        }
    }
    out
}

/// SubIntlvMap: an x8 link takes two quarters (port 0 the low ones), x16
/// all four, a narrower link one (after its core's x8 link one up).
fn sub_interleave(device: &Device, x8: &[(u8, u8)]) -> [u8; 4] {
    let mut map = [0u8; 4];
    match device.link_width {
        16 => map = [1; 4],
        8 if device.port == 0 => map[..2].fill(1),
        8 => map[2..].fill(1),
        _ => {
            let shift = usize::from(x8.contains(&(device.socket, device.core)));
            if let Some(slot) = map.get_mut(usize::from(device.port) + shift) {
                *slot = 1;
            }
        }
    }
    map
}

fn region(device: &Device, sizing: &Sizing, pool: Pool, sub: [u8; 4]) -> CxlRegion {
    let cache = pool.kind == PoolType::DeviceCoherent;
    let (segment, bus, dev, function) = device.address;
    CxlRegion {
        size: pool.size,
        alignment: if cache { 0 } else { ALIGN_4GIB },
        socket: device.socket,
        die: device.die,
        nbio_map: core_bitmap(device.core),
        interleave_size: if cache { 0 } else { 0xff },
        sub_interleave_map: sub,
        sub_interleave_size: if cache { 0 } else { 0xff },
        memory: pool.kind.memory(),
        device_type: device.device_type() as u8,
        link_speed: device.link_speed,
        flit: device.flit.into(),
        segment,
        bus,
        device: dev,
        function,
        status: sizing.tracking.status(),
    }
}

/// The regions of an array (unused ones, size 0, included).
pub fn parse(array: &[u8; CXL_ARRAY_SIZE]) -> Vec<CxlRegion, MAX_CXL_REGIONS> {
    array
        .as_chunks::<CXL_REGION_SIZE>()
        .0
        .iter()
        .map(CxlRegion::parse)
        .collect()
}

/// What to do with the APCB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// "APCB descriptors are up-to-date".
    UpToDate,
    /// Equal arrays with an excluded device: clear every region's
    /// tracking and write, without a reset (`CxlMgrClearFailureTracking`).
    ClearTracking,
    /// Different arrays: write the new one and warm-reset
    /// (`CxlMgrApcbSetAndFlush`).
    Rewrite,
}

/// The decision of `CxlMgrUpdateApcb` (`memcmp` of the 0x540 bytes).
pub fn decide(
    current: &[u8; CXL_ARRAY_SIZE],
    new: &[u8; CXL_ARRAY_SIZE],
    excluded: bool,
) -> Decision {
    if current != new {
        Decision::Rewrite
    } else if excluded {
        Decision::ClearTracking
    } else {
        Decision::UpToDate
    }
}

/// `CxlMgrClearFailureTracking` `0x8020`: `+0x1B` bits 6:0 of every region.
pub fn clear_tracking(array: &mut [u8; CXL_ARRAY_SIZE]) {
    for region in array.as_chunks_mut::<CXL_REGION_SIZE>().0 {
        region[0x1b] &= 0x80;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    /// The array of `regions` (the format's encoder).
    fn array(regions: &[CxlRegion]) -> [u8; CXL_ARRAY_SIZE] {
        fstart_amdfw_format::apcb::cxl_array_of(regions).unwrap()
    }

    /// A type-3 expander with a 16 GiB volatile range 1, x16, Gen5.
    fn expander(bus: u8) -> Device {
        Device {
            address: (0, bus, 0, 0),
            key: (0, bus),
            socket: 0,
            die: 1,
            core: 1,
            port: 0,
            link_width: 16,
            link_speed: 5,
            flit: false,
            switch: false,
            capability: 0b100 | 1 << 4,
            ranges: [
                RawRange {
                    size_high: 4,
                    size_low: 0b11 | 1 << 8,
                },
                RawRange::default(),
            ],
            dsmas: Vec::new(),
            dsemts: None,
            dcd: None,
            class: 0x05_02_10,
            memory_active: true,
            healthy: true,
        }
    }

    const POLICY: SizingPolicy = SizingPolicy {
        dual_hdm_ranges: false,
    };

    #[test]
    fn dvsec_cdat_and_dcd_sizing() {
        let d = expander(0x41);
        let s = size(&d, None, false, POLICY);
        assert_eq!(
            s.pools[..],
            [Pool {
                size: 16 * G,
                kind: PoolType::Volatile
            }]
        );
        assert_eq!(s.aggregate.unwrap().size, 16 * G);
        assert_eq!(s.max_interleave, 1);

        // Persistent media (Media_Type 001).
        let mut pmem = expander(0x41);
        pmem.ranges[0].size_low |= 1 << 2;
        assert_eq!(
            size(&pmem, None, false, POLICY).aggregate.unwrap().kind,
            PoolType::Persistent
        );

        // A range below 256 MiB: the CDAT's first non-empty DSMAS.
        let mut small = expander(0x41);
        small.ranges[0] = RawRange {
            size_high: 0,
            size_low: 0b11,
        };
        small.dsmas = Vec::from_slice(&[0, 8 * G, 4 * G]).unwrap();
        assert_eq!(size(&small, None, false, POLICY).pools[0].size, 8 * G);
        let dual = SizingPolicy {
            dual_hdm_ranges: true,
        };
        assert_eq!(size(&small, None, false, dual).pools.len(), 2);

        // Not valid: no pool.
        let mut invalid = expander(0x41);
        invalid.ranges[0].size_low = 0;
        assert!(size(&invalid, None, false, POLICY).aggregate.is_none());

        // A DCD: one pool, the regions' sum.
        let mut dcd = expander(0x41);
        dcd.dcd = Some(Vec::from_slice(&[G, 3 * G]).unwrap());
        let s = size(&dcd, None, false, POLICY);
        assert!(s.dcd);
        assert_eq!(s.aggregate.unwrap().size, 4 * G);

        // Type 2: device-coherent; type 1: one empty pool.
        let mut t2 = expander(0x41);
        t2.capability |= 1;
        assert_eq!(
            size(&t2, None, false, POLICY).pools[0].kind,
            PoolType::DeviceCoherent
        );
        let mut t1 = expander(0x41);
        t1.capability = 1;
        let s = size(&t1, None, false, POLICY);
        assert_eq!(s.pools[0].size, 0);

        // Another class: dual mode, aggregated as persistent.
        let mut other = expander(0x41);
        other.class = 0x12_00_00;
        let s = size(&other, None, false, POLICY);
        assert_eq!(s.pools[0].kind, PoolType::DualMode);
        assert_eq!(s.aggregate.unwrap().kind, PoolType::Persistent);

        // A non-zero DSEMTS type overrides the aggregate's.
        let mut semts = expander(0x41);
        semts.dsemts = Some(1);
        assert_eq!(
            size(&semts, None, false, POLICY).aggregate.unwrap().kind,
            PoolType::Persistent
        );
    }

    #[test]
    fn failures_count_up_to_exclusion() {
        let d = expander(0x41);
        let first = regions(&[(d.clone(), size(&d, None, false, POLICY))]);
        // Inactive memory with the ABL's windows: one failure.
        let mut inactive = d.clone();
        inactive.memory_active = false;
        let s = size(&inactive, Some(&first[0]), true, POLICY);
        assert_eq!(
            s.tracking,
            Tracking {
                error: 1,
                exclude: false,
                failures: 1
            }
        );
        assert_eq!(s.aggregate.unwrap().size, 16 * G);
        let second = regions(&[(inactive.clone(), s)]);
        assert_eq!(second[0].status, 0x21);
        // A second failure excludes: size 0.
        let s = size(&inactive, Some(&second[0]), true, POLICY);
        assert!(s.tracking.exclude);
        assert_eq!(s.aggregate.unwrap().size, 0);
        let third = regions(&[(inactive.clone(), s)]);
        assert_eq!(third[0].status, 0x51);
        // Excluded stays excluded (no new pools), and the arrays compare
        // equal: the tracking is cleared without a reset.
        let s = size(&inactive, Some(&third[0]), true, POLICY);
        let fourth = regions(&[(inactive, s)]);
        assert_eq!(fourth[0].status & 0x10, 0x10);
        // The 2-bit count saturates in the region: same bytes.
        assert_eq!(
            decide(&array(&third), &array(&fourth), true),
            Decision::ClearTracking
        );
        let mut cleared = array(&fourth);
        clear_tracking(&mut cleared);
        assert_eq!(parse(&cleared)[0].status, 0);
        assert_eq!(
            decide(&array(&first), &array(&first), false),
            Decision::UpToDate
        );
        assert_eq!(
            decide(&array(&first), &array(&second), false),
            Decision::Rewrite
        );

        // A link that trains at another speed rewrites the region.
        let mut slower = d.clone();
        slower.link_speed = 4;
        let s = size(&slower, Some(&first[0]), false, POLICY);
        assert_eq!(s.tracking.error, Tracking::LINK_SPEED);
        assert_eq!(s.tracking.failures, 1);
    }

    #[test]
    fn the_failure_count_saturates_in_its_two_bits() {
        // From a stored 2, one pass failing three times (link speed,
        // inactive, checks) stays at 3, excluded: never 0 or 1.
        let mut t = Tracking::of(2 << 5);
        assert_eq!(t.failures, 2);
        for error in [Tracking::LINK_SPEED, Tracking::INACTIVE, Tracking::CHECKS] {
            assert!(t.fail(error));
        }
        assert_eq!(t.failures, Tracking::MAX_FAILURES);
        assert_eq!(t.status(), 0x0b | 0x10 | 3 << 5);
        assert_eq!(Tracking::of(t.status()), t);
        // A count set from outside the 2 bits is stored saturated.
        let wide = Tracking {
            failures: 5,
            ..Tracking::default()
        };
        assert_eq!(wide.status() >> 5, 3);
    }

    #[test]
    fn regions_follow_the_vendor_builder() {
        let mut x8a = expander(0x41);
        x8a.link_width = 8;
        let mut x8b = expander(0x42);
        x8b.link_width = 8;
        x8b.port = 2;
        let mut x4 = expander(0x43);
        x4.link_width = 4;
        x4.port = 1;
        let mut t2 = expander(0x44);
        t2.capability |= 1;
        t2.core = 3;
        // Two pools (dual ranges): one region each.
        let mut two = expander(0x45);
        two.capability = 0b100 | 2 << 4;
        two.ranges[1] = two.ranges[0];
        let dual = SizingPolicy {
            dual_hdm_ranges: true,
        };
        let devices = [x8a, x8b, x4, t2, two]
            .map(|d| {
                let s = size(&d, None, false, dual);
                (d, s)
            })
            .to_vec();
        let r = regions(&devices);
        assert_eq!(r.len(), 6);
        assert_eq!(r[0].sub_interleave_map, [1, 1, 0, 0]);
        assert_eq!(r[1].sub_interleave_map, [0, 0, 1, 1]);
        // x4 port 1 after an x8 link on the same core: one up.
        assert_eq!(r[2].sub_interleave_map, [0, 0, 1, 0]);
        assert_eq!(
            (r[0].alignment, r[0].interleave_size, r[0].nbio_map),
            (0x20, 0xff, 0b10)
        );
        assert_eq!(
            (r[3].alignment, r[3].interleave_size, r[3].memory),
            (0, 0, CxlMemory::Cache)
        );
        assert_eq!(r[3].nbio_map, 0b1000);
        assert_eq!((r[4].bus, r[5].bus), (0x45, 0x45));
        assert_eq!(r[0].device_type, 3);
        assert_eq!(r[0].link_speed, 5);
        let bytes = array(&r);
        assert_eq!(parse(&bytes)[..6], r[..]);
        assert_eq!(core_bitmap(6), 0x40);
        assert_eq!(core_bitmap(8), 0);

        // A switch carries its endpoints' sum as one region.
        let mut usp = expander(0x50);
        usp.switch = true;
        let mut s = size(&usp, None, false, POLICY);
        assert!(s.aggregate.is_none());
        add_to_switch(&mut s, &devices[0].1);
        add_to_switch(&mut s, &devices[1].1);
        assert_eq!(s.aggregate.unwrap().size, 32 * G);
        assert_eq!(regions(&[(usp, s)]).len(), 1);
    }
}
