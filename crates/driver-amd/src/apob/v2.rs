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

use super::sysmap::{Hole, MAX_HOLES, MemoryMap};
use super::{ApobError, SIGNATURE};

/// APOB V2 major version (`version >> 8 == 0x20`, or raw `0x20` / `0x2000`).
pub const VERSION_MAJOR: u32 = 0x20;
/// Maximum number of sections in the APOB V2 header offset vector.
pub const MAX_SECTIONS: usize = 16;
/// Offset of the section offset vector in the APOB V2 top header.
pub const SECTION_OFFSETS_START: usize = 0x18;
/// Offset of the first entry inside each APOB V2 section.
pub const SECTION_ENTRIES_START: usize = 0x28;
/// Size of an APOB V2 entry header (`group`, `kind`, `instance`, `size` + 0x30 auth/metadata).
pub const ENTRY_HEADER_SIZE: usize = 0x40;

/// Additional APOB V2 groups beyond V1 [`super::group`].
pub mod group {
    pub use super::super::group::*;
    /// Fabric topology map (`0x0C/0x22`).
    pub const FABRIC_TOPOLOGY: u32 = 0x0c;
}

/// APOB V2 record types verified on WH silicon.
pub mod kind {
    pub const ASP_AUTH: u32 = 0x10;
    pub const RAS_CS_BITMAP: u32 = 0x84;
    pub const CCX_TOPOLOGY: u32 = 3;
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
}

impl<'a> ApobV2<'a> {
    /// Parse and validate an APOB V2 container header and section table.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ApobError> {
        let (hdr, _) = HeaderV2Prefix::ref_from_prefix(bytes).map_err(|_| ApobError::Signature)?;
        if hdr.signature != SIGNATURE {
            return Err(ApobError::Signature);
        }
        let version = hdr.version.get();
        let major = if version > 0xff {
            version >> 8
        } else {
            version
        };
        if major != VERSION_MAJOR {
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
        let k = socket
            .checked_mul(self.socket_stride)?
            .checked_add(die)?
            .checked_add(1)?;
        usize::try_from(k)
            .ok()
            .filter(|&idx| idx < self.section_count)
    }

    /// Find an exact `(group, kind, instance)` entry.
    ///
    /// When the derived section `k == 1`, searches both common section `0`
    /// and section `1` (matching WH `ApobGetEntryInstance`).
    pub fn find(&self, group: u32, kind: u32, instance: u32) -> Result<EntryV2<'a>, ApobError> {
        let k = self.section_for_instance(instance);
        if (k == Some(1) || k.is_none())
            && let Some(entry) = self.scan_section(0, group, kind, Some(instance))?
        {
            return Ok(entry);
        }
        if let Some(sec) = k
            && let Some(entry) = self.scan_section(sec, group, kind, Some(instance))?
        {
            return Ok(entry);
        }
        Err(ApobError::NotFound {
            group,
            kind,
            instance,
        })
    }

    /// Parse APOB V2 `9/9` system memory map (`+0x50` TOM, `+0x58` hole count > 0,
    /// `+0x60` array of 24-byte `{ base, size, kind, _pad }`).
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

    /// Parse APOB V2 `9/0x13` NUMA nodes per socket (`+0x41`).
    pub fn nps(&self) -> Result<u8, ApobError> {
        let entry = self.find(group::FABRIC, kind::NPS, 0)?;
        Ok(*entry.at::<u8>(0x41)?)
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

    /// Parse APOB V2 `8/8` SMBIOS DMI physical DIMM table (`+0x40` `u32` table
    /// offset from entry start, `+0x49` `u8` DIMM count, stride `0x20`).
    pub fn dmi_slots(&self) -> Result<Vec<DmiSlot, 32>, ApobError> {
        let entry = self.find(group::SMBIOS, kind::SMBIOS_DMI, 0)?;
        let table_offset = entry.at::<U32>(0x40)?.get() as usize;
        let count = *entry.at::<u8>(0x49)? as usize;
        if count > 32 {
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
            let populated = *entry.at::<u8>(base)? != 0;
            let socket = *entry.at::<u8>(base + 1)?;
            let channel = *entry.at::<u8>(base + 2)?;
            let dimm = *entry.at::<u8>(base + 3)?;
            let size_mib = entry.at::<U32>(base + 4)?.get();
            let speed_mt = entry.at::<U16>(base + 8)?.get();
            let _ = out.push(DmiSlot {
                populated,
                socket,
                channel,
                dimm,
                size_mib,
                speed_mt,
            });
        }
        Ok(out)
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

/// One DIMM slot record from APOB V2 `8/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmiSlot {
    pub populated: bool,
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub size_mib: u32,
    pub speed_mt: u16,
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
        buf[4..8].copy_from_slice(&0x2000u32.to_le_bytes());
        buf[8..12].copy_from_slice(&(sections.len() as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&socket_stride.to_le_bytes());
        buf[16..20].copy_from_slice(&total.to_le_bytes());
        for (i, off) in offsets.iter().enumerate() {
            let pos = SECTION_OFFSETS_START + i * 4;
            buf[pos..pos + 4].copy_from_slice(&off.to_le_bytes());
        }
        buf
    }

    #[test]
    fn parses_sysmap_nps_fabric_ccx_dmi_asp_and_ras() {
        let mut sysmap_body = vec![0u8; 0x20 + 24];
        sysmap_body[0x10..0x18].copy_from_slice(&(16u64 << 30).to_le_bytes());
        sysmap_body[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        sysmap_body[0x20..0x28].copy_from_slice(&0x8000_0000u64.to_le_bytes());
        sysmap_body[0x28..0x30].copy_from_slice(&0x8000_0000u64.to_le_bytes());
        sysmap_body[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());

        let nps_body = vec![0u8, 4u8];
        let fab_body = vec![2u8, 4u8, 0u8, 0u8];
        let mut ccx_body = vec![0u8; 0x18];
        ccx_body[0..4].copy_from_slice(&0x54u32.to_le_bytes());
        ccx_body[0x0c..0x10].copy_from_slice(&1u32.to_le_bytes());
        ccx_body[0x10..0x14].copy_from_slice(&16u32.to_le_bytes());

        // 8/8: +0x40 (body +0x00) = u32 table offset from entry start (0x50),
        //      +0x49 (body +0x09) = u8 count (1), row at entry +0x50 (body +0x10).
        let mut dmi_body = vec![0u8; 0x10 + 0x20];
        dmi_body[0..4].copy_from_slice(&0x50u32.to_le_bytes());
        dmi_body[0x09] = 1;
        dmi_body[0x10] = 1; // populated
        dmi_body[0x11] = 0; // socket
        dmi_body[0x12] = 3; // channel
        dmi_body[0x13] = 0; // dimm
        dmi_body[0x14..0x18].copy_from_slice(&(65536u32).to_le_bytes());
        dmi_body[0x18..0x1a].copy_from_slice(&(6400u16).to_le_bytes());

        let mut asp_body = vec![0u8; 0xa0];
        asp_body[0x98] = 0x03; // entry +0xD8 = body +0x98

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
        let s1 = [make_entry(
            group::CCX,
            kind::CCX_TOPOLOGY,
            0x0000,
            &ccx_body,
        )];
        let s2 = [make_entry(
            group::CCX,
            kind::CCX_TOPOLOGY,
            0x0001,
            &ccx_body,
        )];
        let blob = build_apob_v2(2, &[&s0, &s1, &s2]);

        let apob = ApobV2::parse(&blob).unwrap();
        assert_eq!(apob.total_size(), blob.len());
        let mm = apob.system_memory_map().unwrap();
        assert_eq!(mm.top, 16u64 << 30);
        assert_eq!(mm.holes().len(), 1);
        assert_eq!(mm.low_top(), 0x8000_0000);
        assert_eq!(apob.nps().unwrap(), 4);
        assert_eq!(
            apob.fabric_topology().unwrap(),
            FabricTopology {
                cpu_sockets: 2,
                cpu_nodes: 4,
                gpu_sockets: 0,
                gpu_nodes: 0,
            }
        );
        assert_eq!(
            apob.core_topology_header(0, 1).unwrap().cores_per_complex,
            16
        );
        let dmi = apob.dmi_slots().unwrap();
        assert_eq!(dmi.len(), 1);
        assert_eq!(
            (dmi[0].channel, dmi[0].size_mib, dmi[0].speed_mt),
            (3, 65536, 6400)
        );
        assert_eq!(apob.asp_auth_mode().unwrap(), 0x03);
        assert_eq!(apob.ras_cs_bitmap(0).unwrap(), 0xffff);
    }

    #[test]
    fn compact_single_section_and_overlap_validation() {
        // 1-section header is 0x18 + 4 = 0x1C bytes (smaller than 16-entry 0x58).
        let compact = build_apob_v2(1, &[&[]]);
        assert_eq!(
            compact.len(),
            SECTION_OFFSETS_START + 4 + SECTION_ENTRIES_START
        );
        assert!(ApobV2::parse(&compact).is_ok());

        // Overlapping sections must be rejected.
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
        let short = &blob[..blob.len() - 4];
        assert!(matches!(ApobV2::parse(short), Err(ApobError::Size { .. })));

        // Zero holes and wrapping hole must be rejected.
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

        // Null address rejected before dereference.
        assert_eq!(
            unsafe { ApobV2::from_address(0, 4096) }.err(),
            Some(ApobError::Signature)
        );
    }
}
