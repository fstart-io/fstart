// SPDX-License-Identifier: MIT OR Apache-2.0
//! APOB V2 container and record parsers (`APOB` major version `0x20`).
//!
//! Unlike the linear APOB V1 layout in [`super`], APOB V2 splits the blob into
//! 1..=16 sections indexed by a header offset vector at `+0x18`, with a
//! per-socket stride at `+0x0C` and `0x40`-byte entry headers inside each
//! section starting at section `+0x28`.

use core::ops::Range;
use heapless::Vec;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::ccx::{ABSENT, Core};
use super::event::{AblError, DimmBits, Event};
use super::nps::Nps;
use super::rs::memory::SpdRecord;
use super::sysmap::{Hole, MAX_HOLES, MemoryMap};
use super::{ApobError, SIGNATURE};

/// Exact APOB V2 major version dword (AmdSocSp7WHPei:0xca40f0).
pub const VERSION_MAJOR: u32 = 0x20;
/// Maximum number of sections in the APOB V2 header offset vector.
pub const MAX_SECTIONS: usize = 16;
/// Offset of the section offset vector in the APOB V2 top header.
pub const SECTION_OFFSETS_START: usize = 0x18;
/// Offset of the first entry inside each APOB V2 section.
pub const SECTION_ENTRIES_START: usize = 0x28;
/// Size of an APOB V2 entry header (`group`, `kind`, `instance`, `size` + 0x30 auth/metadata).
pub const ENTRY_HEADER_SIZE: usize = 0x40;
/// Maximum CCDs parsed per die from APOB V2 `3/3`.
pub const MAX_CCDS_PER_DIE: usize = 16;
/// Maximum complexes per CCD parsed from APOB V2 `3/3`.
pub const MAX_CCXS_PER_CCD: usize = 1;
/// Maximum cores per complex parsed from APOB V2 `3/3`.
pub const MAX_CORES_PER_CCX: usize = 32;
/// Maximum DIMM slots parsed from APOB V2 `8/8` (2P × 16 channels × 2 DPC = 64).
pub const MAX_DMI_SLOTS: usize = 64;
/// Maximum ABL events parsed from one APOB V2 `7/6` entry.
pub const MAX_EVENTS_PER_DIE: usize = 64;
/// Maximum ABL events aggregated across all sections.
pub const MAX_ALL_EVENTS: usize = 128;

/// Additional APOB V2 groups beyond V1 [`super::group`].
pub mod group {
    pub use super::super::group::*;
    /// Fabric topology map (`0x0C/0x22`).
    pub const FABRIC_TOPOLOGY: u32 = 0x0c;
}

/// APOB V2 record types verified on WH silicon.
pub mod kind {
    pub const ASP_AUTH: u32 = 0x10;
    pub const DIMM_SPD: u32 = 0x11;
    pub const RAS_CS_BITMAP: u32 = 0x84;
    pub const CCX_TOPOLOGY: u32 = 3;
    pub const EVENT_LOG: u32 = 6;
    pub const SMBIOS_DMI: u32 = 8;
    pub const SYSMAP: u32 = 9;
    pub const NPS: u32 = 0x13;
    pub const FABRIC_TOPOLOGY: u32 = 0x22;
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct HeaderV2Prefix {
    signature: [u8; 4],
    version: U32,
    section_count: U32,
    socket_stride: U32,
    total_size: U32,
    _reserved: U32,
}
const _: () = assert!(size_of::<HeaderV2Prefix>() == SECTION_OFFSETS_START);

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct EntryHeaderV2 {
    group: U32,
    kind: U32,
    instance: U32,
    size: U32,
    _meta: [u8; ENTRY_HEADER_SIZE - 16],
}
const _: () = assert!(size_of::<EntryHeaderV2>() == ENTRY_HEADER_SIZE);

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawEventV2 {
    class: U32,
    info: U32,
    data_a: U32,
    data_b: U32,
}
const _: () = assert!(size_of::<RawEventV2>() == 16);

/// A validated APOB V2 view backed by a byte slice.
#[derive(Clone, Copy)]
pub struct ApobV2<'a> {
    bytes: &'a [u8],
    version: u32,
    section_count: usize,
    socket_stride: u32,
    section_offsets: [u32; MAX_SECTIONS],
}

/// One validated APOB V2 entry slice (including its `0x40`-byte header).
#[derive(Clone, Copy)]
pub struct EntryV2<'a> {
    pub group: u32,
    pub kind: u32,
    pub instance: u32,
    bytes: &'a [u8],
}

impl<'a> EntryV2<'a> {
    /// Full entry bytes including the `0x40`-byte header.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Read a typed little-endian value at byte `offset` from the entry start.
    pub fn at<T: FromBytes + KnownLayout + Immutable + Unaligned>(
        &self,
        offset: usize,
    ) -> Result<&'a T, ApobError> {
        let end = offset
            .checked_add(size_of::<T>())
            .ok_or(ApobError::TooShort {
                group: self.group,
                kind: self.kind,
            })?;
        let slice = self.bytes.get(offset..end).ok_or(ApobError::TooShort {
            group: self.group,
            kind: self.kind,
        })?;
        T::ref_from_bytes(slice).map_err(|_| ApobError::TooShort {
            group: self.group,
            kind: self.kind,
        })
    }

    /// Read `count` contiguous records `T` starting at byte `offset`.
    pub fn slice<T: FromBytes + Immutable + Unaligned>(
        &self,
        offset: usize,
        count: usize,
    ) -> Result<&'a [T], ApobError> {
        let slice = self.bytes.get(offset..).ok_or(ApobError::TooShort {
            group: self.group,
            kind: self.kind,
        })?;
        <[T]>::ref_from_prefix_with_elems(slice, count)
            .map(|(items, _)| items)
            .map_err(|_| ApobError::TooShort {
                group: self.group,
                kind: self.kind,
            })
    }
}

impl<'a> ApobV2<'a> {
    /// Parse and validate an APOB V2 container header and section table.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ApobError> {
        let (hdr, _) = HeaderV2Prefix::ref_from_prefix(bytes).map_err(|_| ApobError::Signature)?;
        if hdr.signature != SIGNATURE {
            return Err(ApobError::Signature);
        }
        let version = hdr.version.get();
        if version != VERSION_MAJOR {
            return Err(ApobError::Version(version));
        }
        let section_count = hdr.section_count.get() as usize;
        let total_size_u32 = hdr.total_size.get();
        let size_err = |first: u32| ApobError::Size {
            size: total_size_u32,
            first,
        };
        if section_count == 0 || section_count > MAX_SECTIONS || total_size_u32 == u32::MAX {
            return Err(size_err(0));
        }
        let header_end = SECTION_OFFSETS_START
            .checked_add(section_count * size_of::<U32>())
            .ok_or_else(|| size_err(0))?;
        let total_size = total_size_u32 as usize;
        if total_size < header_end || total_size > bytes.len() {
            return Err(size_err(header_end as u32));
        }
        let bounded = &bytes[..total_size];
        let (raw_offsets, _) =
            <[U32]>::ref_from_prefix_with_elems(&bounded[SECTION_OFFSETS_START..], section_count)
                .map_err(|_| size_err(header_end as u32))?;
        let first_off = raw_offsets[0].get();
        let mut section_offsets = [0u32; MAX_SECTIONS];
        let mut ranges: [Range<usize>; MAX_SECTIONS] = core::array::from_fn(|_| 0..0);
        for (i, src) in raw_offsets.iter().enumerate() {
            let off = src.get() as usize;
            if off < header_end
                || off
                    .checked_add(SECTION_ENTRIES_START)
                    .is_none_or(|e| e > total_size)
            {
                return Err(size_err(first_off));
            }
            let sec_len = U32::ref_from_prefix(&bounded[off..])
                .map_err(|_| size_err(first_off))?
                .0
                .get() as usize;
            let Some(end) = off.checked_add(sec_len) else {
                return Err(size_err(first_off));
            };
            if sec_len < SECTION_ENTRIES_START || end > total_size {
                return Err(size_err(first_off));
            }
            for prev in &ranges[..i] {
                if off < prev.end && prev.start < end {
                    return Err(size_err(first_off));
                }
            }
            section_offsets[i] = src.get();
            ranges[i] = off..end;
        }
        Ok(Self {
            bytes: bounded,
            version,
            section_count,
            socket_stride: hdr.socket_stride.get(),
            section_offsets,
        })
    }

    /// Parse an APOB V2 placed by the PSP at physical `address`, bounded by
    /// `max_len` bytes (the caller's reserved DRAM window).
    ///
    /// # Safety
    /// - `address` must be non-null and representable as a valid pointer on this target.
    /// - `address..address + max_len` must lie within a single valid, identity-mapped,
    ///   readable DRAM region whose contents will not be mutated for `'static`.
    pub unsafe fn from_address(address: u64, max_len: usize) -> Result<ApobV2<'static>, ApobError> {
        if address == 0 || max_len < SECTION_OFFSETS_START {
            return Err(ApobError::Signature);
        }
        let addr_usize = usize::try_from(address).map_err(|_| ApobError::Signature)?;
        let ptr = addr_usize as *const u8;
        // SAFETY: caller guarantees `max_len` readable bytes from non-null `address`.
        let hdr_bytes = unsafe { core::slice::from_raw_parts(ptr, SECTION_OFFSETS_START) };
        let (hdr, _) =
            HeaderV2Prefix::ref_from_prefix(hdr_bytes).map_err(|_| ApobError::Signature)?;
        if hdr.signature != SIGNATURE {
            return Err(ApobError::Signature);
        }
        let total_size_u32 = hdr.total_size.get();
        let total_size = total_size_u32 as usize;
        if total_size_u32 == u32::MAX || total_size < SECTION_OFFSETS_START || total_size > max_len
        {
            return Err(ApobError::Size {
                size: total_size_u32,
                first: SECTION_OFFSETS_START as u32,
            });
        }
        // SAFETY: validated `total_size <= max_len` within caller's immutable `'static` window.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, total_size) };
        ApobV2::parse(bytes)
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub const fn section_count(&self) -> usize {
        self.section_count
    }

    pub const fn socket_stride(&self) -> u32 {
        self.socket_stride
    }

    /// Validated total byte size of the APOB V2 container.
    pub const fn total_size(&self) -> usize {
        self.bytes.len()
    }

    fn section_range(&self, index: usize) -> Option<Range<usize>> {
        if index >= self.section_count {
            return None;
        }
        let start = self.section_offsets[index] as usize;
        let len = U32::ref_from_prefix(self.bytes.get(start..)?).ok()?.0.get() as usize;
        let end = start.checked_add(len)?;
        (len >= SECTION_ENTRIES_START && end <= self.bytes.len()).then_some(start..end)
    }

    fn scan_section(
        &self,
        section: usize,
        group: u32,
        kind: u32,
        instance: Option<u32>,
    ) -> Result<Option<EntryV2<'a>>, ApobError> {
        let Some(range) = self.section_range(section) else {
            return Ok(None);
        };
        let mut cursor = range.start + SECTION_ENTRIES_START;
        let mut found: Option<EntryV2<'a>> = None;
        while cursor < range.end {
            let (hdr, _) = EntryHeaderV2::ref_from_prefix(&self.bytes[cursor..range.end])
                .map_err(|_| ApobError::TooShort { group, kind })?;
            let stride = hdr.size.get() as usize;
            if stride < ENTRY_HEADER_SIZE {
                return Err(ApobError::TooShort { group, kind });
            }
            let next = cursor
                .checked_add(stride)
                .filter(|&e| e <= range.end)
                .ok_or(ApobError::TooShort { group, kind })?;
            if hdr.group.get() == group
                && hdr.kind.get() == kind
                && instance.is_none_or(|inst| hdr.instance.get() == inst)
            {
                if found.is_some() {
                    return Err(ApobError::Value {
                        group,
                        kind,
                        value: hdr.instance.get(),
                    });
                }
                found = Some(EntryV2 {
                    group,
                    kind,
                    instance: hdr.instance.get(),
                    bytes: &self.bytes[cursor..next],
                });
            }
            cursor = next;
        }
        Ok(found)
    }

    /// Compute the WH APOB V2 section index `k` for `instance`:
    /// `k = (instance >> 8) * socket_stride + (instance & 0xff) + 1`.
    pub fn section_for_instance(&self, instance: u32) -> Option<usize> {
        let socket = instance >> 8;
        let die = instance & 0xff;
        let stride = self.socket_stride;
        if stride == 0 {
            return None;
        }
        let idx = socket
            .checked_mul(stride)?
            .checked_add(die)?
            .checked_add(1)? as usize;
        (idx < self.section_count).then_some(idx)
    }

    /// Find `(group, kind, instance)` using the WH APOB V2 section rule
    /// (`ApobCommonV2.c`, `decomp/00e75860.c`):
    ///
    /// Let `k = (instance >> 8) * socket_stride + (instance & 0xff) + 1`.
    /// - When `k == 1` (`instance == 0`), search sections `0` and `1` and
    ///   require a unique match across both.
    /// - When `k != 1`, search only section `k` (no fallback to section `0`
    ///   or other dies' sections).
    pub fn find(&self, group: u32, kind: u32, instance: u32) -> Result<EntryV2<'a>, ApobError> {
        let socket = instance >> 8;
        let die = instance & 0xff;
        if self.socket_stride == 0 {
            return Err(ApobError::Value {
                group,
                kind,
                value: instance,
            });
        }
        let k = socket
            .checked_mul(self.socket_stride)
            .and_then(|v| v.checked_add(die))
            .and_then(|v| v.checked_add(1))
            .ok_or(ApobError::Value {
                group,
                kind,
                value: instance,
            })? as usize;

        if k == 1 {
            let s0 = self.scan_section(0, group, kind, Some(instance))?;
            let s1 = if self.section_count > 1 {
                self.scan_section(1, group, kind, Some(instance))?
            } else {
                None
            };
            match (s0, s1) {
                (Some(_), Some(_)) => Err(ApobError::Value {
                    group,
                    kind,
                    value: instance,
                }),
                (Some(e), None) | (None, Some(e)) => Ok(e),
                (None, None) => Err(ApobError::NotFound {
                    group,
                    kind,
                    instance,
                }),
            }
        } else {
            if k >= self.section_count {
                return Err(ApobError::NotFound {
                    group,
                    kind,
                    instance,
                });
            }
            self.scan_section(k, group, kind, Some(instance))?
                .ok_or(ApobError::NotFound {
                    group,
                    kind,
                    instance,
                })
        }
    }

    /// Parse APOB V2 `9/9` system memory map (`+0x50` top-of-memory, `+0x58` hole
    /// count, `+0x60` 24-byte `{ base: u64, size: u64, kind: u32, pad: u32 }` entries).
    pub fn system_memory_map(&self) -> Result<MemoryMap, ApobError> {
        let entry = self.find(group::FABRIC, kind::SYSMAP, 0)?;
        let top = entry.at::<U64>(0x50)?.get();
        let count = entry.at::<U32>(0x58)?.get() as usize;
        if count == 0 || count > MAX_HOLES {
            return Err(ApobError::Value {
                group: group::FABRIC,
                kind: kind::SYSMAP,
                value: count as u32,
            });
        }
        #[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
        #[repr(C)]
        struct RawHoleV2 {
            base: U64,
            size: U64,
            kind: U32,
            _pad: U32,
        }
        const _: () = assert!(size_of::<RawHoleV2>() == 24);
        let mut holes: Vec<Hole, MAX_HOLES> = Vec::new();
        for i in 0..count {
            let too_short = ApobError::TooShort {
                group: group::FABRIC,
                kind: kind::SYSMAP,
            };
            let off = 0x60usize
                .checked_add(i.checked_mul(24).ok_or(too_short)?)
                .ok_or(too_short)?;
            let raw = entry.at::<RawHoleV2>(off)?;
            let _ = holes.push(Hole {
                base: raw.base.get(),
                size: raw.size.get(),
                kind: raw.kind.get(),
            });
        }
        MemoryMap::from_parts(top, holes).map_err(|_| ApobError::Value {
            group: group::FABRIC,
            kind: kind::SYSMAP,
            value: 0,
        })
    }

    /// Raw APOB V2 `9/0x13` actual-NPS selector byte (`+0x41`):
    /// `0 = NPS0`, `1 = NPS1`, `2 = NPS2`, `3 = NPS4`.
    pub fn nps(&self) -> Result<u8, ApobError> {
        let entry = self.find(group::FABRIC, kind::NPS, 0)?;
        Ok(*entry.at::<u8>(0x41)?)
    }

    /// Typed APOB V2 `9/0x13` actual NUMA nodes per socket (`ApobV2GetActualNps`).
    pub fn actual_nps(&self) -> Result<Nps, ApobError> {
        match self.nps()? {
            0 => Ok(Nps::Nps0),
            1 => Ok(Nps::Nps1),
            2 => Ok(Nps::Nps2),
            3 => Ok(Nps::Nps4),
            v => Err(ApobError::Value {
                group: group::FABRIC,
                kind: kind::NPS,
                value: v.into(),
            }),
        }
    }

    /// Parse APOB V2 `0x0C/0x22` fabric topology counts (`+0x40..0x44`):
    /// CPU sockets, total CPU IOD nodes, GPU sockets, GPU nodes.
    pub fn fabric_topology(&self) -> Result<FabricTopology, ApobError> {
        let entry = self.find(group::FABRIC_TOPOLOGY, kind::FABRIC_TOPOLOGY, 0)?;
        Ok(FabricTopology {
            cpu_sockets: *entry.at::<u8>(0x40)?,
            cpu_nodes: *entry.at::<u8>(0x41)?,
            gpu_sockets: *entry.at::<u8>(0x42)?,
            gpu_nodes: *entry.at::<u8>(0x43)?,
        })
    }

    /// Parse APOB V2 `3/3` dynamic-stride core topology header for `(socket, die)`.
    pub fn core_topology_header(
        &self,
        socket: u8,
        die: u8,
    ) -> Result<CoreTopologyHeader, ApobError> {
        let instance = super::instance(socket, die);
        let entry = self.find(group::CCX, kind::CCX_TOPOLOGY, instance)?;
        Ok(CoreTopologyHeader {
            first_ccd_offset: entry.at::<U32>(0x40)?.get(),
            complexes_per_ccd: entry.at::<U32>(0x4c)?.get(),
            cores_per_complex: entry.at::<U32>(0x50)?.get(),
        })
    }

    /// Parse APOB V2 `3/3` dynamic-stride core topology for `(socket, die)`.
    ///
    /// Layout verified from `ApobV2GetCcdCountOnDie` / `AmdGetApobV2CcdMapBaseAddressOffset`:
    /// - `+0x40` (`u32`): byte offset from entry start to first CCD record
    /// - `+0x4c` (`u32`): complexes per CCD (must equal `1`, `ApobWhPei:0xe75f30`)
    /// - `+0x50` (`u32`): cores per complex (`1..=32`)
    /// - Each CCD record has stride `1 + complexes * (1 + 3 * cores)`:
    ///   `phys_ccd: u8` (`0xff` = absent), followed by `complexes` records of
    ///   `phys_complex: u8` (`0xff` = absent) and `cores` records of
    ///   `[phys_core: u8, thread0_en: u8, thread1_en: u8]`.
    pub fn core_topology(&self, socket: u8, die: u8) -> Result<CoreMapV2, ApobError> {
        let instance = super::instance(socket, die);
        let entry = self.find(group::CCX, kind::CCX_TOPOLOGY, instance)?;
        let first_off = entry.at::<U32>(0x40)?.get() as usize;
        let complexes = entry.at::<U32>(0x4c)?.get() as usize;
        let cores = entry.at::<U32>(0x50)?.get() as usize;
        let bad = |value: u32| ApobError::Value {
            group: group::CCX,
            kind: kind::CCX_TOPOLOGY,
            value,
        };
        if first_off < 0x54
            || first_off > entry.bytes().len()
            || complexes == 0
            || complexes > MAX_CCXS_PER_CCD
            || cores == 0
            || cores > MAX_CORES_PER_CCX
        {
            return Err(bad(cores as u32));
        }
        let ccx_stride = 1 + 3 * cores;
        let ccd_stride = 1 + complexes * ccx_stride;
        let tail = &entry.bytes()[first_off..];
        let total_slots = tail.len() / ccd_stride;

        let mut ccds: Vec<CcdV2, MAX_CCDS_PER_DIE> = Vec::new();
        for ccd_idx in 0..total_slots {
            let ccd_slice = &tail[ccd_idx * ccd_stride..(ccd_idx + 1) * ccd_stride];
            let phys_ccd = ccd_slice[0];
            if phys_ccd == ABSENT {
                // ApobV2GetCcdCountOnDie stops at the first absent logical CCD.
                break;
            }
            if ccds.iter().any(|c| c.id == phys_ccd) {
                return Err(bad(u32::from(phys_ccd)));
            }
            let mut ccxs: Vec<CcxV2, MAX_CCXS_PER_CCD> = Vec::new();
            for cpx_idx in 0..complexes {
                let cpx_off = 1 + cpx_idx * ccx_stride;
                let cpx_slice = &ccd_slice[cpx_off..cpx_off + ccx_stride];
                let phys_cpx = cpx_slice[0];
                if phys_cpx == ABSENT {
                    continue;
                }
                if ccxs.iter().any(|c| c.id == phys_cpx) {
                    return Err(bad(u32::from(phys_cpx)));
                }
                let mut core_list: Vec<Core, MAX_CORES_PER_CCX> = Vec::new();
                for core_idx in 0..cores {
                    let off = 1 + core_idx * 3;
                    let phys_core = cpx_slice[off];
                    if phys_core == ABSENT {
                        continue;
                    }
                    if core_list.iter().any(|c| c.id == phys_core) {
                        return Err(bad(u32::from(phys_core)));
                    }
                    core_list
                        .push(Core {
                            id: phys_core,
                            threads: [cpx_slice[off + 1] != 0, cpx_slice[off + 2] != 0],
                        })
                        .map_err(|_| bad(u32::from(phys_core)))?;
                }
                if !core_list.is_empty() {
                    ccxs.push(CcxV2 {
                        id: phys_cpx,
                        cores: core_list,
                    })
                    .map_err(|_| bad(u32::from(phys_cpx)))?;
                }
            }
            if !ccxs.is_empty() {
                ccds.push(CcdV2 { id: phys_ccd, ccxs })
                    .map_err(|_| bad(u32::from(phys_ccd)))?;
            }
        }
        Ok(CoreMapV2 {
            complexes_per_ccd: complexes as u8,
            cores_per_complex: cores as u8,
            ccds,
        })
    }

    /// Parse APOB V2 `8/8` ECC capability (`ApobV2GetEccCapability`: bit 7 of `+0x48`).
    pub fn ecc_capable(&self) -> Result<bool, ApobError> {
        let entry = self.find(group::SMBIOS, kind::SMBIOS_DMI, 0)?;
        Ok((*entry.at::<u8>(0x48)? >> 7) & 1 != 0)
    }

    /// Parse APOB V2 `8/8` SMBIOS DMI physical DIMM table.
    ///
    /// Verified against `ApobV2GetDmiPhysEntry*` and `PopulatePhysicalDimmInfo`:
    /// - `+0x40` (`u32`): byte offset from entry start to physical DIMM array
    /// - `+0x49` (`u8`): `MaxPhysicalDimms` count
    /// - Each row is `0x20` bytes: `+0x00 socket`, `+0x01 channel`, `+0x02 dimm`,
    ///   `+0x03 populated`, `+0x06 handle (u16)`, `+0x08 clock_mhz (u16)`,
    ///   `+0x0a voltage_mv (u16)`.
    pub fn dmi_slots(&self) -> Result<Vec<DmiSlot, MAX_DMI_SLOTS>, ApobError> {
        let entry = self.find(group::SMBIOS, kind::SMBIOS_DMI, 0)?;
        let table_offset = entry.at::<U32>(0x40)?.get() as usize;
        let count = *entry.at::<u8>(0x49)? as usize;
        if count > MAX_DMI_SLOTS {
            return Err(ApobError::Value {
                group: group::SMBIOS,
                kind: kind::SMBIOS_DMI,
                value: count as u32,
            });
        }
        if count > 0 && table_offset < 0x4a {
            return Err(ApobError::Value {
                group: group::SMBIOS,
                kind: kind::SMBIOS_DMI,
                value: table_offset as u32,
            });
        }
        let mut out = Vec::new();
        for i in 0..count {
            let too_short = ApobError::TooShort {
                group: group::SMBIOS,
                kind: kind::SMBIOS_DMI,
            };
            let base = table_offset
                .checked_add(i.checked_mul(0x20).ok_or(too_short)?)
                .ok_or(too_short)?;
            let socket = *entry.at::<u8>(base)?;
            let channel = *entry.at::<u8>(base + 1)?;
            let dimm = *entry.at::<u8>(base + 2)?;
            let populated = *entry.at::<u8>(base + 3)? != 0;
            let handle = entry.at::<U16>(base + 6)?.get();
            let clock_mhz = entry.at::<U16>(base + 8)?.get();
            let voltage_mv = entry.at::<U16>(base + 10)?.get();
            let _ = out.push(DmiSlot {
                socket,
                channel,
                dimm,
                populated,
                handle,
                clock_mhz,
                voltage_mv,
            });
        }
        Ok(out)
    }

    /// Look up one DDR5 SPD record from APOB V2 `1/0x11` (`ApobV2GetDimmSpdDataSilWh`).
    ///
    /// Instance is `(socket << 8) | die`. `+0x40` holds the byte offset from entry
    /// start to the SPD array, `+0x48` holds `max_channels_per_die`, each channel
    /// occupies `2 * size_of::<SpdRecord>()` (`0x818`) bytes, and each DIMM slot
    /// is one `0x40c`-byte [`SpdRecord`].
    pub fn spd_record(
        &self,
        socket: u8,
        die: u8,
        channel_on_die: u8,
        dimm: u8,
    ) -> Result<Option<&'a SpdRecord>, ApobError> {
        if dimm >= 2 {
            return Err(ApobError::Value {
                group: group::MEMORY,
                kind: kind::DIMM_SPD,
                value: dimm.into(),
            });
        }
        let entry = self.find(group::MEMORY, kind::DIMM_SPD, super::instance(socket, die))?;
        let base_off = entry.at::<U32>(0x40)?.get() as usize;
        let max_channels = entry.at::<U32>(0x48)?.get() as usize;
        if max_channels == 0 || usize::from(channel_on_die) >= max_channels || base_off < 0x4c {
            return Err(ApobError::Value {
                group: group::MEMORY,
                kind: kind::DIMM_SPD,
                value: channel_on_die.into(),
            });
        }
        let channel_stride = 2 * size_of::<SpdRecord>();
        let off = base_off
            .checked_add(usize::from(channel_on_die) * channel_stride)
            .and_then(|o| o.checked_add(usize::from(dimm) * size_of::<SpdRecord>()))
            .ok_or(ApobError::TooShort {
                group: group::MEMORY,
                kind: kind::DIMM_SPD,
            })?;
        let rec = entry.at::<SpdRecord>(off)?;
        Ok(rec.is_present().then_some(rec))
    }

    /// Decode all DDR5 DIMMs described by APOB V2 `8/8` + `1/0x11` for SMBIOS Type 17.
    ///
    /// Channels `0..8` map to die 0 (`channel_on_die = channel`) and `8..16` map to
    /// die 1 (`channel_on_die = channel - 8`), matching WH dual-IOD UMC layout.
    #[cfg(feature = "smbios")]
    pub fn memory_inventory(&self) -> Result<Vec<super::smbios::Dimm, MAX_DMI_SLOTS>, ApobError> {
        use fstart_core::memory_info::MemoryDevice;
        let slots = self.dmi_slots()?;
        let mut dimms = Vec::new();
        for slot in slots {
            let die = slot.channel / 8;
            let ch_on_die = slot.channel % 8;
            let device = if slot.populated {
                let spd = self
                    .spd_record(slot.socket, die, ch_on_die, slot.dimm)?
                    .ok_or(ApobError::Value {
                        group: group::MEMORY,
                        kind: kind::DIMM_SPD,
                        value: u32::from(slot.channel),
                    })?;
                let mts = slot.configured_mts().ok_or(ApobError::Value {
                    group: group::SMBIOS,
                    kind: kind::SMBIOS_DMI,
                    value: u32::from(slot.clock_mhz),
                })?;
                let mut dev =
                    fstart_driver_spd::ddr5::inventory(&spd.data, mts, slot.channel, slot.dimm)
                        .map_err(|_| ApobError::Value {
                            group: group::MEMORY,
                            kind: kind::DIMM_SPD,
                            value: u32::from(slot.channel),
                        })?;
                dev.voltage_mv = slot.voltage_mv.into();
                dev
            } else {
                MemoryDevice::empty(slot.channel, slot.dimm)
            };
            dimms
                .push(super::smbios::Dimm {
                    socket: slot.socket,
                    die,
                    device,
                })
                .map_err(|_| ApobError::Value {
                    group: group::SMBIOS,
                    kind: kind::SMBIOS_DMI,
                    value: dimms.len() as u32,
                })?;
        }
        Ok(dimms)
    }

    /// Parse APOB V2 `7/6` ABL event log for `(socket, die)` (`+0x40` `u16` count,
    /// `+0x44` array of 16-byte [`Event`]s).
    pub fn event_log(
        &self,
        socket: u8,
        die: u8,
    ) -> Result<Vec<Event, MAX_EVENTS_PER_DIE>, ApobError> {
        let entry = self.find(
            group::GENERAL,
            kind::EVENT_LOG,
            super::instance(socket, die),
        )?;
        let count = usize::from(entry.at::<U16>(0x40)?.get());
        if count > MAX_EVENTS_PER_DIE {
            return Err(ApobError::Value {
                group: group::GENERAL,
                kind: kind::EVENT_LOG,
                value: count as u32,
            });
        }
        let raw = entry.slice::<RawEventV2>(0x44, count)?;
        let mut out = Vec::new();
        for ev in raw {
            let _ = out.push(Event {
                class: ev.class.get(),
                info: ev.info.get(),
                data_a: ev.data_a.get(),
                data_b: ev.data_b.get(),
            });
        }
        Ok(out)
    }

    /// Collect all APOB V2 `7/6` ABL events across every matching record in
    /// every section (`0..section_count`). Returns `(events, truncated)` where
    /// `truncated` is `true` if any valid event did not fit in [`MAX_ALL_EVENTS`].
    pub fn all_events(&self) -> (Vec<Event, MAX_ALL_EVENTS>, bool) {
        let mut out = Vec::new();
        let mut truncated = false;
        for sec in 0..self.section_count {
            let Some(range) = self.section_range(sec) else {
                continue;
            };
            let mut cursor = range.start + SECTION_ENTRIES_START;
            while cursor < range.end {
                let Ok((hdr, _)) = EntryHeaderV2::ref_from_prefix(&self.bytes[cursor..range.end])
                else {
                    break;
                };
                let stride = hdr.size.get() as usize;
                let Some(next) = cursor
                    .checked_add(stride)
                    .filter(|&e| stride >= ENTRY_HEADER_SIZE && e <= range.end)
                else {
                    break;
                };
                if hdr.group.get() == group::GENERAL && hdr.kind.get() == kind::EVENT_LOG {
                    let entry = EntryV2 {
                        group: group::GENERAL,
                        kind: kind::EVENT_LOG,
                        instance: hdr.instance.get(),
                        bytes: &self.bytes[cursor..next],
                    };
                    if let Ok(count) = entry.at::<U16>(0x40).map(|c| usize::from(c.get()))
                        && let Ok(raw) = entry.slice::<RawEventV2>(0x44, count)
                    {
                        for ev in raw {
                            if out
                                .push(Event {
                                    class: ev.class.get(),
                                    info: ev.info.get(),
                                    data_a: ev.data_a.get(),
                                    data_b: ev.data_b.get(),
                                })
                                .is_err()
                            {
                                truncated = true;
                            }
                        }
                    }
                }
                cursor = next;
            }
        }
        (out, truncated)
    }

    /// Decode boot-time ABL errors (DIMM training/test, CCD BIST, DDR5 PMIC) across
    /// all `7/6` event logs for ACPI BERT and BMC SEL reporting. Returns
    /// `(errors, truncated)` if either the event scan or error list overflowed.
    pub fn abl_errors(&self, bits: DimmBits) -> (Vec<AblError, MAX_EVENTS_PER_DIE>, bool) {
        let (events, mut truncated) = self.all_events();
        let mut out = Vec::new();
        for ev in events {
            for err in ev.errors(bits) {
                if out.push(err).is_err() {
                    truncated = true;
                }
            }
        }
        (out, truncated)
    }

    /// Parse APOB V2 `1/0x10` ASP authentication mode byte (`+0xD8`).
    pub fn asp_auth_mode(&self) -> Result<u8, ApobError> {
        let entry = self.find(group::MEMORY, kind::ASP_AUTH, 0)?;
        Ok(*entry.at::<u8>(0xd8)?)
    }

    /// Parse APOB V2 `1/0x84` RAS CS bitmap (`+0x58`).
    pub fn ras_cs_bitmap(&self, instance: u32) -> Result<u64, ApobError> {
        let entry = self.find(group::MEMORY, kind::RAS_CS_BITMAP, instance)?;
        Ok(entry.at::<U64>(0x58)?.get())
    }
}

/// Fabric topology summary from APOB V2 `0x0C/0x22`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FabricTopology {
    pub cpu_sockets: u8,
    pub cpu_nodes: u8,
    pub gpu_sockets: u8,
    pub gpu_nodes: u8,
}

/// Dynamic-stride CCX topology header from APOB V2 `3/3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreTopologyHeader {
    pub first_ccd_offset: u32,
    pub complexes_per_ccd: u32,
    pub cores_per_complex: u32,
}

/// One complex in an APOB V2 `3/3` core topology map (up to 32 cores/CCX).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcxV2 {
    pub id: u8,
    pub cores: Vec<Core, MAX_CORES_PER_CCX>,
}

/// One CCD in an APOB V2 `3/3` core topology map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcdV2 {
    pub id: u8,
    pub ccxs: Vec<CcxV2, MAX_CCXS_PER_CCD>,
}

/// Present CCDs, complexes, and cores on one IOD from APOB V2 `3/3`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoreMapV2 {
    pub complexes_per_ccd: u8,
    pub cores_per_complex: u8,
    pub ccds: Vec<CcdV2, MAX_CCDS_PER_DIE>,
}

impl CoreMapV2 {
    /// Enabled hardware threads on this die.
    pub fn threads(&self) -> usize {
        self.cores()
            .map(|(_, _, c)| c.threads.iter().filter(|&&t| t).count())
            .sum()
    }

    /// Bitmask of present physical CCD IDs (`bits 0..16`).
    pub fn ccd_mask(&self) -> u16 {
        self.ccds
            .iter()
            .filter(|d| d.id < 16)
            .fold(0u16, |mask, d| mask | (1u16 << d.id))
    }

    /// `(phys_ccd, phys_ccx, core)` for every present core on this die.
    pub fn cores(&self) -> impl Iterator<Item = (u8, u8, &Core)> {
        self.ccds
            .iter()
            .flat_map(|d| d.ccxs.iter().map(move |x| (d.id, x)))
            .flat_map(|(d, x)| x.cores.iter().map(move |c| (d, x.id, c)))
    }
}

/// One physical DIMM slot record from APOB V2 `8/8` (`APOB_MEM_DMI_PHYSICAL_DIMM_V2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmiSlot {
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub populated: bool,
    pub handle: u16,
    /// Configured DRAM clock in MHz (multiply by 2 for MT/s).
    pub clock_mhz: u16,
    /// Configured DRAM VDD in mV.
    pub voltage_mv: u16,
}

impl DmiSlot {
    /// Configured data rate in MT/s (`clock_mhz * 2`).
    pub const fn configured_mts(&self) -> Option<u16> {
        self.clock_mhz.checked_mul(2)
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec as AllocVec;

    fn make_entry(group: u32, kind: u32, instance: u32, body: &[u8]) -> AllocVec<u8> {
        let size = (ENTRY_HEADER_SIZE + body.len()) as u32;
        let mut out = vec![0u8; size as usize];
        out[0..4].copy_from_slice(&group.to_le_bytes());
        out[4..8].copy_from_slice(&kind.to_le_bytes());
        out[8..12].copy_from_slice(&instance.to_le_bytes());
        out[12..16].copy_from_slice(&size.to_le_bytes());
        out[ENTRY_HEADER_SIZE..].copy_from_slice(body);
        out
    }

    fn build_apob_v2(socket_stride: u32, sections: &[&[AllocVec<u8>]]) -> AllocVec<u8> {
        let hdr_len = SECTION_OFFSETS_START + sections.len() * 4;
        let mut buf = vec![0u8; hdr_len];
        let mut offsets = AllocVec::new();
        for entries in sections {
            offsets.push(buf.len() as u32);
            let mut sec = vec![0u8; SECTION_ENTRIES_START];
            for e in *entries {
                sec.extend_from_slice(e);
            }
            let slen = sec.len() as u32;
            sec[0..4].copy_from_slice(&slen.to_le_bytes());
            buf.extend_from_slice(&sec);
        }
        let total = buf.len() as u32;
        buf[0..4].copy_from_slice(&SIGNATURE);
        buf[4..8].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        buf[8..12].copy_from_slice(&(sections.len() as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&socket_stride.to_le_bytes());
        buf[16..20].copy_from_slice(&total.to_le_bytes());
        for (i, off) in offsets.iter().enumerate() {
            let pos = SECTION_OFFSETS_START + i * 4;
            buf[pos..pos + 4].copy_from_slice(&off.to_le_bytes());
        }
        buf
    }

    fn make_ccx_body(complexes: usize, cores: usize, ccds: &[(u8, usize)]) -> AllocVec<u8> {
        let mut body = vec![0u8; 0x14];
        body[0..4].copy_from_slice(&0x54u32.to_le_bytes());
        body[0x0c..0x10].copy_from_slice(&(complexes as u32).to_le_bytes());
        body[0x10..0x14].copy_from_slice(&(cores as u32).to_le_bytes());
        for &(ccd_id, active_cores) in ccds {
            body.push(ccd_id);
            for cpx in 0..complexes {
                body.push(if cpx == 0 { 0 } else { ABSENT });
                for c in 0..cores {
                    if cpx == 0 && c < active_cores {
                        body.extend_from_slice(&[c as u8, 1, 1]);
                    } else {
                        body.extend_from_slice(&[ABSENT, 0, 0]);
                    }
                }
            }
        }
        // Trailing absent CCD slot.
        body.push(ABSENT);
        for _ in 0..complexes {
            body.extend_from_slice(&[ABSENT]);
            for _ in 0..cores {
                body.extend_from_slice(&[ABSENT, 0, 0]);
            }
        }
        body
    }

    #[test]
    fn parses_sysmap_nps_fabric_ccx_dmi_spd_events_asp_and_ras() {
        let mut sysmap_body = vec![0u8; 0x20 + 24];
        sysmap_body[0x10..0x18].copy_from_slice(&(16u64 << 30).to_le_bytes());
        sysmap_body[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        sysmap_body[0x20..0x28].copy_from_slice(&0x8000_0000u64.to_le_bytes());
        sysmap_body[0x28..0x30].copy_from_slice(&0x8000_0000u64.to_le_bytes());
        sysmap_body[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());

        // 9/0x13: +0x41 (body +0x01) = 3 (NPS4).
        let nps_body = vec![0u8, 3u8];
        let fab_body = vec![2u8, 4u8, 0u8, 0u8];
        let ccx0_body = make_ccx_body(1, 16, &[(0, 16), (1, 12)]);
        let ccx1_body = make_ccx_body(1, 32, &[(4, 32)]);

        // 8/8: +0x40 (body +0x00) = u32 table offset (0x50),
        //      +0x48 (body +0x08) = ECC bit 7 set (0x80),
        //      +0x49 (body +0x09) = u8 count (2),
        //      row0 at entry +0x50 (body +0x10), row1 at entry +0x70 (body +0x30).
        let mut dmi_body = vec![0u8; 0x10 + 2 * 0x20];
        dmi_body[0..4].copy_from_slice(&0x50u32.to_le_bytes());
        dmi_body[0x08] = 0x80; // ECC capable
        dmi_body[0x09] = 2;
        // Row 0: socket 0, channel 3 (die 0, ch 3), dimm 0, populated, 3200 MHz (6400 MT/s), 1100 mV
        dmi_body[0x10] = 0;
        dmi_body[0x11] = 3;
        dmi_body[0x12] = 0;
        dmi_body[0x13] = 1;
        dmi_body[0x16..0x18].copy_from_slice(&0x0010u16.to_le_bytes());
        dmi_body[0x18..0x1a].copy_from_slice(&3200u16.to_le_bytes());
        dmi_body[0x1a..0x1c].copy_from_slice(&1100u16.to_le_bytes());
        // Row 1: empty slot on channel 8 (die 1, ch 0), dimm 0
        dmi_body[0x30] = 0;
        dmi_body[0x31] = 8;
        dmi_body[0x32] = 0;
        dmi_body[0x33] = 0;

        // 1/0x11: instance (0, 0), +0x40 = 0x50, +0x48 = 8 channels/die
        let mut spd_body = vec![0u8; 0x10 + 8 * 2 * size_of::<SpdRecord>()];
        spd_body[0..4].copy_from_slice(&0x50u32.to_le_bytes());
        spd_body[0x08..0x0c].copy_from_slice(&8u32.to_le_bytes());
        let ch3_off = 0x10 + 3 * (2 * size_of::<SpdRecord>());
        spd_body[ch3_off + 1] = 1; // present
        spd_body[ch3_off + 8] = 0; // socket
        spd_body[ch3_off + 9] = 3; // channel
        spd_body[ch3_off + 10] = 0; // dimm
        spd_body[ch3_off + 12 + 2] = 0x12; // DDR5 marker byte in SPD

        // 7/6: instance (0, 1), 1 PMU training failure event on socket 0 ch 9 dimm 0
        let mut evt_body = vec![0u8; 4 + 16];
        evt_body[0..2].copy_from_slice(&1u16.to_le_bytes());
        evt_body[4..8].copy_from_slice(&9u32.to_le_bytes()); // class
        evt_body[8..12].copy_from_slice(&super::super::event::id::PMU_TRAINING.to_le_bytes());
        let data_a: u32 = (1u32 << 20) | (9u32 << 8); // socket 0, ch 9, dimm 0 (RS bits)
        evt_body[12..16].copy_from_slice(&data_a.to_le_bytes());
        evt_body[16..20].copy_from_slice(&0xdead_beefu32.to_le_bytes());

        let mut asp_body = vec![0u8; 0xa0];
        asp_body[0x98] = 0x03;

        let mut ras_body = vec![0u8; 0x20];
        ras_body[0x18..0x20].copy_from_slice(&0x0000_ffffu64.to_le_bytes());

        let s0 = [
            make_entry(group::FABRIC, kind::SYSMAP, 0, &sysmap_body),
            make_entry(group::FABRIC, kind::NPS, 0, &nps_body),
            make_entry(group::FABRIC_TOPOLOGY, kind::FABRIC_TOPOLOGY, 0, &fab_body),
            make_entry(group::SMBIOS, kind::SMBIOS_DMI, 0, &dmi_body),
            make_entry(group::MEMORY, kind::ASP_AUTH, 0, &asp_body),
            make_entry(group::MEMORY, kind::RAS_CS_BITMAP, 0, &ras_body),
        ];
        let s1 = [
            make_entry(group::CCX, kind::CCX_TOPOLOGY, 0x0000, &ccx0_body),
            make_entry(group::MEMORY, kind::DIMM_SPD, 0x0000, &spd_body),
        ];
        let s2 = [
            make_entry(group::CCX, kind::CCX_TOPOLOGY, 0x0001, &ccx1_body),
            make_entry(group::GENERAL, kind::EVENT_LOG, 0x0001, &evt_body),
        ];
        let blob = build_apob_v2(2, &[&s0, &s1, &s2]);

        let apob = ApobV2::parse(&blob).unwrap();
        assert_eq!(apob.total_size(), blob.len());
        let mm = apob.system_memory_map().unwrap();
        assert_eq!(mm.top, 16u64 << 30);
        assert_eq!(mm.holes().len(), 1);
        assert_eq!(mm.low_top(), 0x8000_0000);
        assert_eq!(apob.nps().unwrap(), 3);
        assert_eq!(apob.actual_nps().unwrap(), Nps::Nps4);
        assert_eq!(
            apob.fabric_topology().unwrap(),
            FabricTopology {
                cpu_sockets: 2,
                cpu_nodes: 4,
                gpu_sockets: 0,
                gpu_nodes: 0,
            }
        );

        let topo0 = apob.core_topology(0, 0).unwrap();
        assert_eq!(topo0.cores_per_complex, 16);
        assert_eq!(topo0.ccd_mask(), (1 << 0) | (1 << 1));
        assert_eq!(topo0.threads(), (16 + 12) * 2);

        let topo1 = apob.core_topology(0, 1).unwrap();
        assert_eq!(topo1.cores_per_complex, 32);
        assert_eq!(topo1.ccd_mask(), 1 << 4);
        assert_eq!(topo1.threads(), 64);

        assert!(apob.ecc_capable().unwrap());
        let dmi = apob.dmi_slots().unwrap();
        assert_eq!(dmi.len(), 2);
        assert!(dmi[0].populated);
        assert_eq!(
            (dmi[0].channel, dmi[0].configured_mts(), dmi[0].voltage_mv),
            (3, Some(6400), 1100)
        );
        assert!(!dmi[1].populated);

        let spd = apob.spd_record(0, 0, 3, 0).unwrap().unwrap();
        assert_eq!(spd.channel, 3);
        assert_eq!(spd.data[2], 0x12);
        assert!(apob.spd_record(0, 0, 0, 0).unwrap().is_none());

        let events = apob.event_log(0, 1).unwrap();
        assert_eq!(events.len(), 1);
        let (errs, truncated) = apob.abl_errors(DimmBits::RS);
        assert!(!truncated);
        assert_eq!(
            errs.as_slice(),
            &[AblError::Dimm {
                event: super::super::event::id::PMU_TRAINING,
                socket: 0,
                channel: 9,
                dimm: 0,
                data_a,
                data_b: 0xdead_beef,
            }]
        );

        assert_eq!(apob.asp_auth_mode().unwrap(), 0x03);
        assert_eq!(apob.ras_cs_bitmap(0).unwrap(), 0xffff);
    }

    #[test]
    fn compact_single_section_and_overlap_validation() {
        let compact = build_apob_v2(1, &[&[]]);
        assert_eq!(
            compact.len(),
            SECTION_OFFSETS_START + 4 + SECTION_ENTRIES_START
        );
        assert!(ApobV2::parse(&compact).is_ok());

        let mut overlap = build_apob_v2(1, &[&[], &[]]);
        let sec0 = u32::from_le_bytes(overlap[0x18..0x1c].try_into().unwrap());
        overlap[0x1c..0x20].copy_from_slice(&sec0.to_le_bytes());
        assert!(matches!(
            ApobV2::parse(&overlap),
            Err(ApobError::Size { .. })
        ));
    }

    #[test]
    fn rejects_bad_version_truncated_and_invalid_sysmap() {
        let mut blob = build_apob_v2(2, &[&[]]);
        blob[4..8].copy_from_slice(&0x18u32.to_le_bytes());
        assert_eq!(ApobV2::parse(&blob).err(), Some(ApobError::Version(0x18)));
        blob[4..8].copy_from_slice(&0x2000u32.to_le_bytes());
        assert_eq!(ApobV2::parse(&blob).err(), Some(ApobError::Version(0x2000)));
        blob[4..8].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        let short = &blob[..blob.len() - 4];
        assert!(matches!(ApobV2::parse(short), Err(ApobError::Size { .. })));

        let mut zero_holes = vec![0u8; 0x20];
        zero_holes[0x10..0x18].copy_from_slice(&(4u64 << 30).to_le_bytes());
        let s0 = [make_entry(group::FABRIC, kind::SYSMAP, 0, &zero_holes)];
        let b = build_apob_v2(1, &[&s0]);
        assert!(matches!(
            ApobV2::parse(&b).unwrap().system_memory_map(),
            Err(ApobError::Value {
                group: group::FABRIC,
                kind: kind::SYSMAP,
                ..
            })
        ));

        let mut wrap_hole = vec![0u8; 0x20 + 24];
        wrap_hole[0x10..0x18].copy_from_slice(&(4u64 << 30).to_le_bytes());
        wrap_hole[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        wrap_hole[0x20..0x28].copy_from_slice(&u64::MAX.to_le_bytes());
        wrap_hole[0x28..0x30].copy_from_slice(&0x1000u64.to_le_bytes());
        let s0 = [make_entry(group::FABRIC, kind::SYSMAP, 0, &wrap_hole)];
        let b = build_apob_v2(1, &[&s0]);
        assert!(ApobV2::parse(&b).unwrap().system_memory_map().is_err());

        assert_eq!(
            unsafe { ApobV2::from_address(0, 4096) }.err(),
            Some(ApobError::Signature)
        );
    }

    #[test]
    fn find_matches_opensil_section_rules_and_rejects_cross_section_duplicates() {
        let mut nps = vec![0u8; 4];
        nps[1] = 2; // NPS2
        // Duplicate instance 0 in both section 0 and section 1 must be rejected.
        let s0 = [make_entry(group::FABRIC, kind::NPS, 0, &nps)];
        let s1 = [make_entry(group::FABRIC, kind::NPS, 0, &nps)];
        let dup = build_apob_v2(2, &[&s0, &s1]);
        assert!(matches!(
            ApobV2::parse(&dup).unwrap().nps(),
            Err(ApobError::Value {
                group: group::FABRIC,
                kind: kind::NPS,
                ..
            })
        ));

        // Instance (0, 1) -> k = 2 searches ONLY section 2, never falling back to section 0 or 1.
        let s0_only = [make_entry(
            group::CCX,
            kind::CCX_TOPOLOGY,
            0x0001,
            &[0u8; 0x20],
        )];
        let s1_only = [make_entry(
            group::CCX,
            kind::CCX_TOPOLOGY,
            0x0001,
            &[0u8; 0x20],
        )];
        let misfiled = build_apob_v2(2, &[&s0_only, &s1_only, &[]]);
        assert!(matches!(
            ApobV2::parse(&misfiled).unwrap().core_topology(0, 1),
            Err(ApobError::NotFound {
                group: group::CCX,
                kind: kind::CCX_TOPOLOGY,
                instance: 0x0001,
            })
        ));
    }

    #[test]
    fn wh_rejects_multiple_complexes_per_ccd() {
        let entry = [make_entry(
            group::CCX,
            kind::CCX_TOPOLOGY,
            0,
            &make_ccx_body(2, 1, &[(0, 1)]),
        )];
        let blob = build_apob_v2(2, &[&[], &entry]);
        assert!(ApobV2::parse(&blob).unwrap().core_topology(0, 0).is_err());
    }

    #[test]
    fn core_topology_rejects_duplicate_ids_and_ccd_overflow() {
        // Two CCDs both claiming phys_ccd = 0 -> rejected.
        let ccd_stride = 1 + (1 + 3 * 2);
        let mut body = vec![0xffu8; 0x14 + 2 * ccd_stride];
        body[0..4].copy_from_slice(&0x54u32.to_le_bytes());
        body[0x0c..0x10].copy_from_slice(&1u32.to_le_bytes());
        body[0x10..0x14].copy_from_slice(&2u32.to_le_bytes());
        for slot in 0..2 {
            let base = 0x14 + slot * ccd_stride;
            body[base] = 0; // duplicate phys_ccd = 0
            body[base + 1] = 0; // phys_cpx = 0
            body[base + 2..base + 5].copy_from_slice(&[0, 1, 1]);
        }
        let s1 = [make_entry(group::CCX, kind::CCX_TOPOLOGY, 0, &body)];
        let blob = build_apob_v2(2, &[&[], &s1]);
        assert!(matches!(
            ApobV2::parse(&blob).unwrap().core_topology(0, 0),
            Err(ApobError::Value {
                group: group::CCX,
                kind: kind::CCX_TOPOLOGY,
                ..
            })
        ));

        // Data beyond an absent CCD is not another logical CCD.
        body[0x14] = ABSENT;
        let terminated = [make_entry(group::CCX, kind::CCX_TOPOLOGY, 0, &body)];
        let blob = build_apob_v2(2, &[&[], &terminated]);
        assert!(
            ApobV2::parse(&blob)
                .unwrap()
                .core_topology(0, 0)
                .unwrap()
                .ccds
                .is_empty()
        );

        // 17 distinct present CCDs (> MAX_CCDS_PER_DIE = 16) -> rejected, not truncated.
        let mut over = vec![0xffu8; 0x14 + (MAX_CCDS_PER_DIE + 1) * ccd_stride];
        over[0..4].copy_from_slice(&0x54u32.to_le_bytes());
        over[0x0c..0x10].copy_from_slice(&1u32.to_le_bytes());
        over[0x10..0x14].copy_from_slice(&2u32.to_le_bytes());
        for slot in 0..=MAX_CCDS_PER_DIE {
            let base = 0x14 + slot * ccd_stride;
            over[base] = slot as u8;
            over[base + 1] = 0;
            over[base + 2..base + 5].copy_from_slice(&[0, 1, 0]);
        }
        let s1_over = [make_entry(group::CCX, kind::CCX_TOPOLOGY, 0, &over)];
        let blob_over = build_apob_v2(2, &[&[], &s1_over]);
        assert!(matches!(
            ApobV2::parse(&blob_over).unwrap().core_topology(0, 0),
            Err(ApobError::Value {
                group: group::CCX,
                kind: kind::CCX_TOPOLOGY,
                ..
            })
        ));
    }

    #[test]
    fn all_events_scans_multiple_records_per_section_and_reports_truncation() {
        let make_ev_body = |n: usize, tag: u32| {
            let mut b = vec![0u8; 4 + n * 16];
            b[0..2].copy_from_slice(&(n as u16).to_le_bytes());
            for i in 0..n {
                let off = 4 + i * 16;
                b[off..off + 4].copy_from_slice(&9u32.to_le_bytes());
                b[off + 4..off + 8].copy_from_slice(&tag.to_le_bytes());
                b[off + 8..off + 12].copy_from_slice(&(i as u32).to_le_bytes());
            }
            b
        };
        let e1 = make_entry(group::GENERAL, kind::EVENT_LOG, 0, &make_ev_body(2, 0x1111));
        let e2 = make_entry(group::GENERAL, kind::EVENT_LOG, 1, &make_ev_body(3, 0x2222));
        let blob = build_apob_v2(2, &[&[e1, e2]]);
        let (evs, trunc) = ApobV2::parse(&blob).unwrap().all_events();
        assert!(!trunc);
        assert_eq!(evs.len(), 5);

        // Overflow MAX_ALL_EVENTS -> truncated = true.
        let big = make_entry(
            group::GENERAL,
            kind::EVENT_LOG,
            0,
            &make_ev_body(MAX_ALL_EVENTS + 4, 0x3333),
        );
        let blob_big = build_apob_v2(2, &[&[big]]);
        let (evs, trunc) = ApobV2::parse(&blob_big).unwrap().all_events();
        assert!(trunc);
        assert_eq!(evs.len(), MAX_ALL_EVENTS);
    }
}
