//! Flattened device trees, read and patched in place without alloc or tree
//! conversion: [`blob_from_addr`] finds a blob in memory, [`set_bootargs`] /
//! [`set_memory`] (and their raw-pointer wrappers [`fdt_set_bootargs`] /
//! [`fdt_set_memory`]) patch it.
//!
//! ## Patching
//!
//! Operates directly on the FDT binary format over a `&mut [u8]` slice using
//! `zerocopy` big-endian wire types: walks the structure block to locate target
//! nodes, then either overwrites existing properties in-place or inserts new
//! ones by shifting the tail of the blob.
//!
//! ## Supported operations
//!
//! - [`set_bootargs`] / [`fdt_set_bootargs`] — set or replace `/chosen/bootargs`.
//! - [`set_memory`] / [`fdt_set_memory`] — create or update `/memory@...` node
//!   with `device_type` and `reg` properties. Reads root `#address-cells` and
//!   `#size-cells` to encode `reg` correctly.
//!
//! ## Buffer requirements
//!
//! The DTB must be a v17+ blob in standard block order (header -> mem_rsvmap ->
//! dt_struct -> dt_strings) residing in a writable buffer with headroom beyond
//! `totalsize` (for property insertion + strings growth).

#![no_std]

use zerocopy::byteorder::big_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

// ---- FDT constants (big-endian on the wire) --------------------------------

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_VERSION_MIN: u32 = 17;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// Standard 40-byte Flattened Device Tree header (v17).
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct FdtHeader {
    magic: U32,
    totalsize: U32,
    off_dt_struct: U32,
    off_dt_strings: U32,
    off_mem_rsvmap: U32,
    version: U32,
    last_comp_version: U32,
    boot_cpuid_phys: U32,
    size_dt_strings: U32,
    size_dt_struct: U32,
}

const FDT_HEADER_SIZE: usize = core::mem::size_of::<FdtHeader>();

/// Header of an `FDT_PROP` payload immediately following the `FDT_PROP` token.
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct FdtPropHeader {
    len: U32,
    nameoff: U32,
}

/// Validated mutable view of the header offsets we update during patching.
#[derive(Clone, Copy)]
struct HeaderLayout {
    totalsize: usize,
    off_struct: usize,
    off_strings: usize,
    size_strings: usize,
    size_struct: usize,
}

// ---- Error type ------------------------------------------------------------

/// Errors from FDT patching operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdtPatchError {
    /// First four bytes are not 0xD00DFEED.
    BadMagic,
    /// The working buffer is too small for the patched DTB.
    BufferTooSmall,
    /// `/chosen` node was not found in the structure block.
    ChosenNotFound,
    /// Root's `#address-cells` or `#size-cells` property has an unsupported
    /// value (only 1 and 2 are supported) or cannot represent the given region.
    UnsupportedCellSize,
    /// Unexpected token, unsupported header/block layout, or premature end of
    /// the structure block.
    StructureCorrupt,
}

// ---- Helpers ---------------------------------------------------------------

/// Align `off` up to the next 4-byte boundary.
#[inline]
const fn align4(off: usize) -> Option<usize> {
    match off.checked_add(3) {
        Some(v) => Some(v & !3),
        None => None,
    }
}

/// Read a big-endian `u32` at `buf[off..off+4]`.
fn read_be32(buf: &[u8], off: usize) -> Result<u32, FdtPatchError> {
    let slice = buf.get(off..).ok_or(FdtPatchError::StructureCorrupt)?;
    let (val, _) = U32::read_from_prefix(slice).map_err(|_| FdtPatchError::StructureCorrupt)?;
    Ok(val.get())
}

/// Write a big-endian `u32` to `buf[off..off+4]`.
fn write_be32(buf: &mut [u8], off: usize, val: u32) -> Result<(), FdtPatchError> {
    let slice = buf.get_mut(off..).ok_or(FdtPatchError::StructureCorrupt)?;
    let (slot, _) = U32::mut_from_prefix(slice).map_err(|_| FdtPatchError::StructureCorrupt)?;
    slot.set(val);
    Ok(())
}

/// Read an `FDT_PROP`'s `(len, nameoff)` starting at `buf[off..off+8]`.
fn read_prop_header(buf: &[u8], off: usize) -> Result<(usize, usize), FdtPatchError> {
    let slice = buf.get(off..).ok_or(FdtPatchError::StructureCorrupt)?;
    let (hdr, _) =
        FdtPropHeader::read_from_prefix(slice).map_err(|_| FdtPatchError::StructureCorrupt)?;
    Ok((hdr.len.get() as usize, hdr.nameoff.get() as usize))
}

/// Copy `src` into `dst[off..off+src.len()]` without panicking.
fn copy_bytes(dst: &mut [u8], off: usize, src: &[u8]) -> Result<(), FdtPatchError> {
    let end = off
        .checked_add(src.len())
        .ok_or(FdtPatchError::BufferTooSmall)?;
    let out = dst.get_mut(off..end).ok_or(FdtPatchError::BufferTooSmall)?;
    for (d, &s) in out.iter_mut().zip(src.iter()) {
        *d = s;
    }
    Ok(())
}

/// Zero `dst[start..end]` without panicking.
fn zero_bytes(dst: &mut [u8], start: usize, end: usize) -> Result<(), FdtPatchError> {
    let out = dst
        .get_mut(start..end)
        .ok_or(FdtPatchError::BufferTooSmall)?;
    for b in out {
        *b = 0;
    }
    Ok(())
}

/// Shift `buf[start..end]` right by `delta` bytes without panicking.
fn shift_right(
    buf: &mut [u8],
    start: usize,
    end: usize,
    delta: usize,
) -> Result<(), FdtPatchError> {
    if delta == 0 || start >= end {
        return Ok(());
    }
    let dst_end = end
        .checked_add(delta)
        .ok_or(FdtPatchError::BufferTooSmall)?;
    let window = buf
        .get_mut(start..dst_end)
        .ok_or(FdtPatchError::BufferTooSmall)?;
    let count = end - start;
    let mut i = count;
    while i > 0 {
        i -= 1;
        if let Some(&b) = window.get(i)
            && let Some(slot) = window.get_mut(i + delta)
        {
            *slot = b;
        }
    }
    Ok(())
}

/// Validate the FDT v17+ header and enforce standard non-overlapping block
/// ordering: header -> mem_rsvmap -> dt_struct -> dt_strings.
fn parse_header(buf: &[u8]) -> Result<HeaderLayout, FdtPatchError> {
    let (magic, _) = U32::read_from_prefix(buf).map_err(|_| FdtPatchError::BadMagic)?;
    if magic.get() != FDT_MAGIC {
        return Err(FdtPatchError::BadMagic);
    }
    let (hdr, _) = FdtHeader::ref_from_prefix(buf).map_err(|_| FdtPatchError::StructureCorrupt)?;

    if hdr.version.get() < FDT_VERSION_MIN || hdr.last_comp_version.get() > FDT_VERSION_MIN {
        return Err(FdtPatchError::StructureCorrupt);
    }

    let totalsize = hdr.totalsize.get() as usize;
    let off_rsvmap = hdr.off_mem_rsvmap.get() as usize;
    let off_struct = hdr.off_dt_struct.get() as usize;
    let off_strings = hdr.off_dt_strings.get() as usize;
    let size_strings = hdr.size_dt_strings.get() as usize;
    let size_struct = hdr.size_dt_struct.get() as usize;

    if totalsize < FDT_HEADER_SIZE {
        return Err(FdtPatchError::StructureCorrupt);
    }
    if totalsize > buf.len() {
        return Err(FdtPatchError::BufferTooSmall);
    }
    let struct_end = off_struct
        .checked_add(size_struct)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let strings_end = off_strings
        .checked_add(size_strings)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let rsvmap_min_end = off_rsvmap
        .checked_add(16)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    if off_rsvmap < FDT_HEADER_SIZE
        || !off_rsvmap.is_multiple_of(8)
        || off_struct < rsvmap_min_end
        || !off_struct.is_multiple_of(4)
        || !size_struct.is_multiple_of(4)
        || struct_end > totalsize
        || off_strings < struct_end
        || strings_end > totalsize
    {
        return Err(FdtPatchError::StructureCorrupt);
    }

    Ok(HeaderLayout {
        totalsize,
        off_struct,
        off_strings,
        size_strings,
        size_struct,
    })
}

/// Write the updated layout fields back into the FDT header.
fn write_header(buf: &mut [u8], layout: HeaderLayout) -> Result<(), FdtPatchError> {
    let (hdr, _) = FdtHeader::mut_from_prefix(buf).map_err(|_| FdtPatchError::StructureCorrupt)?;
    hdr.totalsize
        .set(u32::try_from(layout.totalsize).map_err(|_| FdtPatchError::BufferTooSmall)?);
    hdr.off_dt_struct
        .set(u32::try_from(layout.off_struct).map_err(|_| FdtPatchError::BufferTooSmall)?);
    hdr.off_dt_strings
        .set(u32::try_from(layout.off_strings).map_err(|_| FdtPatchError::BufferTooSmall)?);
    hdr.size_dt_strings
        .set(u32::try_from(layout.size_strings).map_err(|_| FdtPatchError::BufferTooSmall)?);
    hdr.size_dt_struct
        .set(u32::try_from(layout.size_struct).map_err(|_| FdtPatchError::BufferTooSmall)?);
    Ok(())
}

/// Read a NUL-terminated node name starting at `buf[off..struct_end]`.
/// Returns `(name_without_nul, next_4byte_aligned_offset)`.
fn read_node_name(
    buf: &[u8],
    off: usize,
    struct_end: usize,
) -> Result<(&[u8], usize), FdtPatchError> {
    let tail = buf
        .get(off..struct_end)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let nul_pos = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let next = align4(off + nul_pos + 1).ok_or(FdtPatchError::StructureCorrupt)?;
    if next > struct_end {
        return Err(FdtPatchError::StructureCorrupt);
    }
    let name = tail.get(..nul_pos).ok_or(FdtPatchError::StructureCorrupt)?;
    Ok((name, next))
}

/// Compare the NUL-terminated property name at `nameoff` in the strings block
/// against `needle` (which does NOT include the trailing NUL).
fn prop_name_eq(buf: &[u8], layout: HeaderLayout, nameoff: usize, needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    let Some(strings) = buf.get(layout.off_strings..layout.off_strings + layout.size_strings)
    else {
        return false;
    };
    let Some(rest) = strings.get(nameoff..) else {
        return false;
    };
    rest.get(..needle.len()) == Some(needle) && rest.get(needle.len()) == Some(&0)
}

/// Search the strings block for a NUL-terminated string (including valid
/// NUL-terminated suffixes, as allowed by the FDT specification).
/// Returns the offset *within* the strings block (`nameoff` for `FDT_PROP`).
fn find_in_strings(strings: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut i = 0;
    while let Some(rest) = strings.get(i..) {
        if rest.is_empty() {
            break;
        }
        let nul_rel = rest.iter().position(|&b| b == 0)?;
        if nul_rel >= needle.len() {
            let candidate = i + nul_rel - needle.len();
            if strings.get(candidate..i + nul_rel) == Some(needle) {
                return Some(candidate);
            }
        }
        i += nul_rel + 1;
    }
    None
}

/// Advance past an `FDT_PROP` token at `off` (validating bounds within `struct_end`).
fn next_after_prop(off: usize, prop_len: usize, struct_end: usize) -> Result<usize, FdtPatchError> {
    let end = off
        .checked_add(12)
        .and_then(|v| v.checked_add(prop_len))
        .and_then(align4)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    if end > struct_end {
        return Err(FdtPatchError::StructureCorrupt);
    }
    Ok(end)
}

/// Overwrite `[start..end)` (4-byte aligned) with `FDT_NOP` tokens.
fn fill_nops(buf: &mut [u8], start: usize, end: usize) -> Result<(), FdtPatchError> {
    debug_assert!(start <= end && start.is_multiple_of(4) && end.is_multiple_of(4));
    let mut off = start;
    while off < end {
        write_be32(buf, off, FDT_NOP)?;
        off += 4;
    }
    Ok(())
}

/// Preflight that adding `add_struct` structure bytes and `add_strings` string
/// bytes fits in `buf` and in `u32` header fields.
fn check_growth(
    buf_len: usize,
    layout: HeaderLayout,
    add_struct: usize,
    add_strings: usize,
) -> Result<usize, FdtPatchError> {
    let new_totalsize = layout
        .totalsize
        .checked_add(add_struct)
        .and_then(|v| v.checked_add(add_strings))
        .ok_or(FdtPatchError::BufferTooSmall)?;
    if new_totalsize > buf_len || u32::try_from(new_totalsize).is_err() {
        return Err(FdtPatchError::BufferTooSmall);
    }
    Ok(new_totalsize)
}

/// Insert `struct_bytes` (must be 4-byte aligned) into the structure block at
/// `insert_off`, append any `extra_strings` (each NUL-terminated) to the
/// strings block, and update the FDT header.
///
/// Requires standard block order (`off_strings >= off_struct + size_struct`),
/// which [`parse_header`] enforces.
fn splice_struct_and_strings(
    buf: &mut [u8],
    layout: &mut HeaderLayout,
    insert_off: usize,
    struct_bytes: &[u8],
    extra_strings: &[&[u8]],
) -> Result<usize, FdtPatchError> {
    debug_assert!(struct_bytes.len().is_multiple_of(4));
    let add_struct = struct_bytes.len();
    let add_strings: usize = extra_strings.iter().map(|s| s.len()).sum();
    check_growth(buf.len(), *layout, add_struct, add_strings)?;

    if add_struct > 0 {
        shift_right(buf, insert_off, layout.totalsize, add_struct)?;
        copy_bytes(buf, insert_off, struct_bytes)?;
        layout.size_struct += add_struct;
        layout.off_strings += add_struct;
        layout.totalsize += add_struct;
    }

    if add_strings > 0 {
        let strings_end = layout.off_strings + layout.size_strings;
        if strings_end < layout.totalsize {
            shift_right(buf, strings_end, layout.totalsize, add_strings)?;
        }
        let mut dest = strings_end;
        for s in extra_strings {
            copy_bytes(buf, dest, s)?;
            dest += s.len();
        }
        layout.size_strings += add_strings;
        layout.totalsize += add_strings;
    }

    write_header(buf, *layout)?;
    Ok(layout.totalsize)
}

/// Write a value as 1 or 2 big-endian `u32` cells into `buf[off..]`.
/// Returns the number of bytes written (4 or 8), or `UnsupportedCellSize` if
/// `cells == 1` and `val > u32::MAX`.
fn write_cells(buf: &mut [u8], off: usize, val: u64, cells: u32) -> Result<usize, FdtPatchError> {
    if cells == 2 {
        write_be32(buf, off, (val >> 32) as u32)?;
        write_be32(buf, off + 4, val as u32)?;
        Ok(8)
    } else {
        let v = u32::try_from(val).map_err(|_| FdtPatchError::UnsupportedCellSize)?;
        write_be32(buf, off, v)?;
        Ok(4)
    }
}

/// Encode a single `FDT_PROP` entry into `dst[off..]`, returning the new offset.
fn encode_prop(
    dst: &mut [u8],
    off: usize,
    nameoff: usize,
    value: &[u8],
) -> Result<usize, FdtPatchError> {
    let aligned_total = align4(12 + value.len()).ok_or(FdtPatchError::StructureCorrupt)?;
    if off + aligned_total > dst.len() {
        return Err(FdtPatchError::BufferTooSmall);
    }
    write_be32(dst, off, FDT_PROP)?;
    write_be32(
        dst,
        off + 4,
        u32::try_from(value.len()).map_err(|_| FdtPatchError::BufferTooSmall)?,
    )?;
    write_be32(
        dst,
        off + 8,
        u32::try_from(nameoff).map_err(|_| FdtPatchError::BufferTooSmall)?,
    )?;
    copy_bytes(dst, off + 12, value)?;
    zero_bytes(dst, off + 12 + value.len(), off + aligned_total)?;
    Ok(off + aligned_total)
}

// ---- Shared structure-block walkers ----------------------------------------

struct TreeInfo {
    chosen_begin: Option<usize>,
    memory_begin: Option<usize>,
    root_end_node: Option<usize>,
    addr_cells: u32,
    size_cells: u32,
}

/// Validate the entire structure block (balanced tree ending in `FDT_END`) and
/// collect the locations of `/chosen`, `/memory*`, the root `FDT_END_NODE`,
/// and root `#address-cells` / `#size-cells`.
fn scan_tree(buf: &[u8], layout: HeaderLayout) -> Result<TreeInfo, FdtPatchError> {
    let struct_end = layout.off_struct + layout.size_struct;
    let mut off = layout.off_struct;
    let mut depth: u32 = 0;
    let mut saw_root = false;
    let mut saw_end = false;
    let mut info = TreeInfo {
        chosen_begin: None,
        memory_begin: None,
        root_end_node: None,
        addr_cells: 2, // default per Devicetree Specification §2.3.5
        size_cells: 1, // default per Devicetree Specification §2.3.5
    };

    while off < struct_end {
        match read_be32(buf, off)? {
            FDT_BEGIN_NODE => {
                if depth == 0 && saw_root {
                    return Err(FdtPatchError::StructureCorrupt);
                }
                depth += 1;
                if depth == 1 {
                    saw_root = true;
                }
                let (name, next) = read_node_name(buf, off + 4, struct_end)?;
                if depth == 2 {
                    if name == b"chosen" {
                        info.chosen_begin = Some(off);
                    } else if info.memory_begin.is_none()
                        && (name == b"memory" || (name.len() > 7 && name.starts_with(b"memory@")))
                    {
                        info.memory_begin = Some(off);
                    }
                }
                off = next;
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Err(FdtPatchError::StructureCorrupt);
                }
                if depth == 1 {
                    info.root_end_node = Some(off);
                }
                depth -= 1;
                off += 4;
            }
            FDT_PROP => {
                if depth == 0 {
                    return Err(FdtPatchError::StructureCorrupt);
                }
                let (prop_len, prop_nameoff) = read_prop_header(buf, off + 4)?;
                let next = next_after_prop(off, prop_len, struct_end)?;
                if depth == 1 && prop_len == 4 {
                    if prop_name_eq(buf, layout, prop_nameoff, b"#address-cells") {
                        info.addr_cells = read_be32(buf, off + 12)?;
                    } else if prop_name_eq(buf, layout, prop_nameoff, b"#size-cells") {
                        info.size_cells = read_be32(buf, off + 12)?;
                    }
                }
                off = next;
            }
            FDT_NOP => off += 4,
            FDT_END => {
                if depth != 0 || !saw_root || off + 4 != struct_end {
                    return Err(FdtPatchError::StructureCorrupt);
                }
                saw_end = true;
                break;
            }
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }
    if !saw_end {
        return Err(FdtPatchError::StructureCorrupt);
    }
    Ok(info)
}

struct NodeScan {
    prop_a: Option<(usize, usize)>,
    prop_b: Option<(usize, usize)>,
    insert_off: usize,
}

/// Scan a node's immediate properties for up to two named properties and
/// return the insertion offset right after the node's last immediate property.
fn scan_node_props(
    buf: &[u8],
    layout: HeaderLayout,
    node_start: usize,
    name_a: &[u8],
    name_b: &[u8],
) -> Result<NodeScan, FdtPatchError> {
    let struct_end = layout.off_struct + layout.size_struct;
    let (_, mut off) = read_node_name(buf, node_start + 4, struct_end)?;
    let mut scan = NodeScan {
        prop_a: None,
        prop_b: None,
        insert_off: off,
    };
    let mut inner_depth: u32 = 0;

    loop {
        if off >= struct_end {
            return Err(FdtPatchError::StructureCorrupt);
        }
        match read_be32(buf, off)? {
            FDT_PROP => {
                let (prop_len, prop_nameoff) = read_prop_header(buf, off + 4)?;
                let next = next_after_prop(off, prop_len, struct_end)?;
                if inner_depth == 0 {
                    if prop_name_eq(buf, layout, prop_nameoff, name_a) {
                        scan.prop_a = Some((off, prop_len));
                    } else if prop_name_eq(buf, layout, prop_nameoff, name_b) {
                        scan.prop_b = Some((off, prop_len));
                    }
                    scan.insert_off = next;
                }
                off = next;
            }
            FDT_BEGIN_NODE => {
                inner_depth += 1;
                let (_, next) = read_node_name(buf, off + 4, struct_end)?;
                off = next;
            }
            FDT_END_NODE => {
                if inner_depth == 0 {
                    break;
                }
                inner_depth -= 1;
                off += 4;
            }
            FDT_NOP => off += 4,
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }
    Ok(scan)
}

// ---- Public safe slice API -------------------------------------------------

/// Set or replace `/chosen/bootargs` in a mutable FDT buffer.
///
/// The buffer must contain a valid FDT blob at the start and have enough
/// capacity for any growth. On failure the buffer is left unmodified.
/// On success returns the new `totalsize` of the DTB.
pub fn set_bootargs(buf: &mut [u8], bootargs: &str) -> Result<usize, FdtPatchError> {
    let mut layout = parse_header(buf)?;
    let struct_end = layout.off_struct + layout.size_struct;
    let tree = scan_tree(buf, layout)?;
    let chosen_start = tree.chosen_begin.ok_or(FdtPatchError::ChosenNotFound)?;
    let node = scan_node_props(buf, layout, chosen_start, b"bootargs", b"")?;

    let new_value_len = bootargs
        .len()
        .checked_add(1)
        .ok_or(FdtPatchError::BufferTooSmall)?;

    if let Some((prop_off, old_len)) = node.prop_a {
        let old_aligned_end = next_after_prop(prop_off, old_len, struct_end)?;
        let new_aligned_end =
            next_after_prop(prop_off, new_value_len, struct_end).unwrap_or(usize::MAX);

        if new_aligned_end <= old_aligned_end {
            write_be32(
                buf,
                prop_off + 4,
                u32::try_from(new_value_len).map_err(|_| FdtPatchError::BufferTooSmall)?,
            )?;
            let val_start = prop_off + 12;
            copy_bytes(buf, val_start, bootargs.as_bytes())?;
            zero_bytes(buf, val_start + bootargs.len(), new_aligned_end)?;
            fill_nops(buf, new_aligned_end, old_aligned_end)?;
            return Ok(layout.totalsize);
        }
    }

    let strings = buf
        .get(layout.off_strings..layout.off_strings + layout.size_strings)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let (nameoff, extra_strings): (usize, &[&[u8]]) = match find_in_strings(strings, b"bootargs") {
        Some(n) => (n, &[]),
        None => (layout.size_strings, &[b"bootargs\0"]),
    };

    let new_prop_total = 12usize
        .checked_add(new_value_len)
        .and_then(align4)
        .ok_or(FdtPatchError::BufferTooSmall)?;
    let add_strings: usize = extra_strings.iter().map(|s| s.len()).sum();
    check_growth(buf.len(), layout, new_prop_total, add_strings)?;

    if let Some((prop_off, old_len)) = node.prop_a {
        let old_aligned_end = next_after_prop(prop_off, old_len, struct_end)?;
        fill_nops(buf, prop_off, old_aligned_end)?;
    }

    shift_right(buf, node.insert_off, layout.totalsize, new_prop_total)?;
    write_be32(buf, node.insert_off, FDT_PROP)?;
    write_be32(
        buf,
        node.insert_off + 4,
        u32::try_from(new_value_len).map_err(|_| FdtPatchError::BufferTooSmall)?,
    )?;
    write_be32(
        buf,
        node.insert_off + 8,
        u32::try_from(nameoff).map_err(|_| FdtPatchError::BufferTooSmall)?,
    )?;
    let val_start = node.insert_off + 12;
    copy_bytes(buf, val_start, bootargs.as_bytes())?;
    zero_bytes(
        buf,
        val_start + bootargs.len(),
        node.insert_off + new_prop_total,
    )?;

    layout.size_struct += new_prop_total;
    layout.off_strings += new_prop_total;
    layout.totalsize += new_prop_total;

    splice_struct_and_strings(buf, &mut layout, node.insert_off, &[], extra_strings)
}

/// Validate the leading FDT header fields and zero-initialize the writable
/// headroom `[totalsize..max_size)` before borrowing `&mut [u8]` over the
/// full buffer capacity.
///
/// # Safety
///
/// - `dtb` must point to a single writable allocation of `max_size` bytes
///   (`max_size <= isize::MAX as usize`), with at least the existing FDT blob
///   (first 8 bytes, and up to `totalsize` when valid) already initialized.
/// - No other references to that allocation may exist during the lifetime `'a`.
unsafe fn raw_dtb_mut<'a>(dtb: *mut u8, max_size: usize) -> Result<&'a mut [u8], FdtPatchError> {
    if dtb.is_null() {
        return Err(FdtPatchError::BadMagic);
    }
    if max_size < 8 {
        return Err(FdtPatchError::BadMagic);
    }
    if max_size > isize::MAX as usize {
        return Err(FdtPatchError::BufferTooSmall);
    }
    // SAFETY: caller guarantees at least the first 8 bytes are initialized.
    let prefix = unsafe { core::slice::from_raw_parts(dtb, 8) };
    let magic = read_be32(prefix, 0)?;
    if magic != FDT_MAGIC {
        return Err(FdtPatchError::BadMagic);
    }
    let totalsize = read_be32(prefix, 4)? as usize;
    if totalsize < FDT_HEADER_SIZE {
        return Err(FdtPatchError::StructureCorrupt);
    }
    if totalsize > max_size {
        return Err(FdtPatchError::BufferTooSmall);
    }
    // Initialize the writable headroom beyond `totalsize` so forming
    // `&mut [u8]` over `max_size` never exposes uninitialized memory.
    // SAFETY: `totalsize <= max_size <= isize::MAX` and `dtb..dtb+max_size`
    // is uniquely writable.
    unsafe {
        core::ptr::write_bytes(dtb.add(totalsize), 0, max_size - totalsize);
        Ok(core::slice::from_raw_parts_mut(dtb, max_size))
    }
}

/// Set or replace `/chosen/bootargs` in a raw FDT blob.
///
/// Thin `unsafe` wrapper over [`set_bootargs`].
///
/// # Safety
///
/// - `dtb` must point to a single writable allocation of at least `max_size`
///   bytes (`max_size <= isize::MAX as usize`), with the existing FDT blob
///   initialized at the start (headroom beyond `totalsize` may be
///   uninitialized and will be zeroed).
/// - No other references to that allocation may exist during the call.
pub unsafe fn fdt_set_bootargs(
    dtb: *mut u8,
    max_size: usize,
    bootargs: &str,
) -> Result<usize, FdtPatchError> {
    // SAFETY: forwarded from caller.
    let buf = unsafe { raw_dtb_mut(dtb, max_size)? };
    set_bootargs(buf, bootargs)
}

// ---- Memory node -----------------------------------------------------------

/// Maximum size of the node blob we build on the stack.
///
/// Layout (worst case, 64-bit cells):
///   FDT_BEGIN_NODE(4) + "memory@XXXXXXXXXXXXXXXX\0"(24 aligned) +
///   FDT_PROP(12) + "memory\0"(8 aligned) +  // device_type
///   FDT_PROP(12) + reg_value(16) +           // reg (#addr-cells=2, #size-cells=2)
///   FDT_END_NODE(4)
/// Total: 80 bytes. 128 gives generous headroom.
const MEMORY_NODE_MAX: usize = 128;

/// Create or update the `/memory@...` node in a mutable FDT buffer.
///
/// If a depth-1 node whose name is `memory` or starts with `memory@` already
/// exists, its `reg` property is overwritten (or inserted) and `device_type`
/// is ensured. Otherwise a new `/memory@<base_hex>` node is created just
/// before the root's `FDT_END_NODE`.
///
/// Reads the root node's `#address-cells` and `#size-cells` properties to
/// encode `reg` correctly (supports values 1 and 2; defaults per the
/// Devicetree Specification are `#address-cells = 2`, `#size-cells = 1`).
///
/// On failure the buffer is left unmodified. On success returns the new
/// `totalsize` of the DTB.
pub fn set_memory(buf: &mut [u8], base: u64, size: u64) -> Result<usize, FdtPatchError> {
    let mut layout = parse_header(buf)?;
    let tree = scan_tree(buf, layout)?;

    if !(tree.addr_cells == 1 || tree.addr_cells == 2)
        || !(tree.size_cells == 1 || tree.size_cells == 2)
    {
        return Err(FdtPatchError::UnsupportedCellSize);
    }

    let mut reg_val = [0u8; 16];
    let mut r = write_cells(&mut reg_val, 0, base, tree.addr_cells)?;
    r += write_cells(&mut reg_val, r, size, tree.size_cells)?;
    let reg_bytes = reg_val.get(..r).ok_or(FdtPatchError::StructureCorrupt)?;

    if let Some(mem_start) = tree.memory_begin {
        return set_reg_in_existing_memory_node(buf, &mut layout, mem_start, reg_bytes);
    }

    let plan = plan_memory_strings(buf, layout, true, true)?;

    let mut node_buf = [0u8; MEMORY_NODE_MAX];
    let mut p = 0usize;

    write_be32(&mut node_buf, p, FDT_BEGIN_NODE)?;
    p += 4;

    copy_bytes(&mut node_buf, p, b"memory@")?;
    p += 7;
    p += write_hex_to_buf(&mut node_buf, p, base)?;
    let aligned_p = align4(p + 1).ok_or(FdtPatchError::StructureCorrupt)?;
    zero_bytes(&mut node_buf, p, aligned_p)?;
    p = aligned_p;

    p = encode_prop(&mut node_buf, p, plan.dt_nameoff, b"memory\0")?;
    p = encode_prop(&mut node_buf, p, plan.reg_nameoff, reg_bytes)?;

    write_be32(&mut node_buf, p, FDT_END_NODE)?;
    p += 4;

    let insert_off = tree.root_end_node.ok_or(FdtPatchError::StructureCorrupt)?;
    let node_slice = node_buf.get(..p).ok_or(FdtPatchError::BufferTooSmall)?;
    splice_struct_and_strings(buf, &mut layout, insert_off, node_slice, plan.extra())
}

struct MemoryStringsPlan {
    dt_nameoff: usize,
    reg_nameoff: usize,
    extra: [&'static [u8]; 2],
    extra_count: usize,
}

impl MemoryStringsPlan {
    fn extra(&self) -> &[&'static [u8]] {
        self.extra.get(..self.extra_count).unwrap_or(&[])
    }
}

/// Plan string-table offsets and additions strictly for the properties that
/// will actually be inserted (`need_dt` / `need_reg`).
fn plan_memory_strings(
    buf: &[u8],
    layout: HeaderLayout,
    need_dt: bool,
    need_reg: bool,
) -> Result<MemoryStringsPlan, FdtPatchError> {
    let strings = buf
        .get(layout.off_strings..layout.off_strings + layout.size_strings)
        .ok_or(FdtPatchError::StructureCorrupt)?;
    let mut extra: [&'static [u8]; 2] = [&[], &[]];
    let mut extra_count = 0usize;
    let mut pending_len = 0usize;

    let dt_nameoff = if need_dt {
        match find_in_strings(strings, b"device_type") {
            Some(n) => n,
            None => {
                let n = layout.size_strings + pending_len;
                if let Some(slot) = extra.get_mut(extra_count) {
                    *slot = b"device_type\0";
                }
                extra_count += 1;
                pending_len += b"device_type\0".len();
                n
            }
        }
    } else {
        0
    };

    let reg_nameoff = if need_reg {
        match find_in_strings(strings, b"reg") {
            Some(n) => n,
            None => {
                let n = layout.size_strings + pending_len;
                if let Some(slot) = extra.get_mut(extra_count) {
                    *slot = b"reg\0";
                }
                extra_count += 1;
                n
            }
        }
    } else {
        0
    };

    Ok(MemoryStringsPlan {
        dt_nameoff,
        reg_nameoff,
        extra,
        extra_count,
    })
}

/// Create or update the `/memory@...` node in a raw FDT blob.
///
/// Thin `unsafe` wrapper over [`set_memory`].
///
/// # Safety
///
/// - `dtb` must point to a single writable allocation of at least `max_size`
///   bytes (`max_size <= isize::MAX as usize`), with the existing FDT blob
///   initialized at the start (headroom beyond `totalsize` may be
///   uninitialized and will be zeroed).
/// - No other references to that allocation may exist during the call.
pub unsafe fn fdt_set_memory(
    dtb: *mut u8,
    max_size: usize,
    base: u64,
    size: u64,
) -> Result<usize, FdtPatchError> {
    // SAFETY: forwarded from caller.
    let buf = unsafe { raw_dtb_mut(dtb, max_size)? };
    set_memory(buf, base, size)
}

/// Write a u64 value as lowercase hex digits (without "0x" prefix) into `buf[off..]`.
/// Leading zeros are skipped (minimum 1 digit). Returns the number of bytes written.
fn write_hex_to_buf(buf: &mut [u8], off: usize, val: u64) -> Result<usize, FdtPatchError> {
    if val == 0 {
        copy_bytes(buf, off, b"0")?;
        return Ok(1);
    }

    let digits = (64 - val.leading_zeros()).div_ceil(4) as usize;
    let out = buf
        .get_mut(off..off + digits)
        .ok_or(FdtPatchError::BufferTooSmall)?;
    for (i, digit) in out.iter_mut().enumerate() {
        let shift = (digits - 1 - i) * 4;
        let nibble = ((val >> shift) & 0xF) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
    }
    Ok(digits)
}

/// Set the `reg` property inside an existing `/memory*` node.
/// Preflights all growth before mutating the buffer.
fn set_reg_in_existing_memory_node(
    buf: &mut [u8],
    layout: &mut HeaderLayout,
    mem_node_start: usize,
    reg_bytes: &[u8],
) -> Result<usize, FdtPatchError> {
    let struct_end = layout.off_struct + layout.size_struct;
    let node = scan_node_props(buf, *layout, mem_node_start, b"reg", b"device_type")?;

    let in_place_reg = node
        .prop_a
        .filter(|&(_, old_len)| old_len == reg_bytes.len());
    let need_reg = in_place_reg.is_none();
    let need_dt = node.prop_b.is_none();

    let plan = plan_memory_strings(buf, *layout, need_dt, need_reg)?;

    let mut prop_buf = [0u8; 64];
    let mut p = 0usize;
    if need_dt {
        p = encode_prop(&mut prop_buf, p, plan.dt_nameoff, b"memory\0")?;
    }
    if need_reg {
        p = encode_prop(&mut prop_buf, p, plan.reg_nameoff, reg_bytes)?;
    }

    let extra_slice = plan.extra();
    let add_strings: usize = extra_slice.iter().map(|s| s.len()).sum();
    check_growth(buf.len(), *layout, p, add_strings)?;

    if let Some((prop_off, _)) = in_place_reg {
        copy_bytes(buf, prop_off + 12, reg_bytes)?;
    } else if let Some((prop_off, old_len)) = node.prop_a {
        let old_aligned_end = next_after_prop(prop_off, old_len, struct_end)?;
        fill_nops(buf, prop_off, old_aligned_end)?;
    }

    if p == 0 {
        return Ok(layout.totalsize);
    }

    let prop_slice = prop_buf.get(..p).ok_or(FdtPatchError::BufferTooSmall)?;
    splice_struct_and_strings(buf, layout, node.insert_off, prop_slice, extra_slice)
}

/// Read an FDT (Flattened Device Tree) blob from a raw memory address.
///
/// Validates the FDT magic at offset +0 (`0xD00DFEED`), reads the
/// `totalsize` field at offset +4, and returns a static slice covering
/// the entire blob. Returns `None` if `addr` is zero, unrepresentable on this
/// target, or the header is invalid.
///
/// Used by the UEFI payload path to obtain the platform-provided FDT blob
/// for passing to CrabEFI.
///
/// # Safety
///
/// - `addr` must point to a single initialized allocation of at least 8
///   readable bytes.
/// - If the magic validates, the full `totalsize` bytes starting at `addr`
///   must be readable, initialized, and immutable for `'static` (no concurrent
///   or subsequent writes while the returned slice is live).
pub unsafe fn blob_from_addr(addr: u64) -> Option<&'static [u8]> {
    if addr == 0 {
        return None;
    }
    let base = usize::try_from(addr).ok()?;
    base.checked_add(8)?;
    let ptr = base as *const u8;
    // SAFETY: caller guarantees at least 8 readable, initialized bytes at `addr`.
    let head = unsafe { core::slice::from_raw_parts(ptr, 8) };
    let ([magic, size], _) = <[U32; 2]>::read_from_prefix(head).ok()?;
    if magic.get() != FDT_MAGIC {
        return None;
    }
    let total = usize::try_from(size.get()).ok()?;
    if total < FDT_HEADER_SIZE || total > isize::MAX as usize {
        return None;
    }
    base.checked_add(total)?;
    // SAFETY: magic validated; caller guarantees `total` readable, initialized,
    // immutable bytes for `'static`.
    Some(unsafe { core::slice::from_raw_parts(ptr, total) })
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// Build a minimal valid FDT blob for testing.
    struct DtbBuilder {
        struct_block: Vec<u8>,
        strings: Vec<u8>,
    }

    impl DtbBuilder {
        fn new() -> Self {
            Self {
                struct_block: Vec::new(),
                strings: Vec::new(),
            }
        }

        fn push_u32(&mut self, v: u32) {
            self.struct_block.extend_from_slice(&v.to_be_bytes());
        }

        fn pad4(&mut self) {
            while !self.struct_block.len().is_multiple_of(4) {
                self.struct_block.push(0);
            }
        }

        fn intern(&mut self, s: &[u8]) -> u32 {
            if let Some(off) = find_in_strings(&self.strings, s) {
                return off as u32;
            }
            let off = self.strings.len() as u32;
            self.strings.extend_from_slice(s);
            self.strings.push(0);
            off
        }

        fn begin_node(&mut self, name: &str) {
            self.push_u32(FDT_BEGIN_NODE);
            self.struct_block.extend_from_slice(name.as_bytes());
            self.struct_block.push(0);
            self.pad4();
        }

        fn end_node(&mut self) {
            self.push_u32(FDT_END_NODE);
        }

        fn prop(&mut self, name: &str, value: &[u8]) {
            let nameoff = self.intern(name.as_bytes());
            self.prop_with_nameoff(nameoff, value);
        }

        fn prop_with_nameoff(&mut self, nameoff: u32, value: &[u8]) {
            self.push_u32(FDT_PROP);
            self.push_u32(value.len() as u32);
            self.push_u32(nameoff);
            self.struct_block.extend_from_slice(value);
            self.pad4();
        }

        fn prop_u32(&mut self, name: &str, value: u32) {
            self.prop(name, &value.to_be_bytes());
        }

        fn finish(mut self, capacity: usize) -> Vec<u8> {
            self.push_u32(FDT_END);
            let off_mem_rsvmap = FDT_HEADER_SIZE;
            let rsvmap_size = 16; // single zero terminator entry
            let off_struct = off_mem_rsvmap + rsvmap_size;
            let size_struct = self.struct_block.len();
            let off_strings = off_struct + size_struct;
            let size_strings = self.strings.len();
            let totalsize = off_strings + size_strings;

            let mut buf = vec![0u8; capacity.max(totalsize)];
            let hdr = FdtHeader {
                magic: U32::new(FDT_MAGIC),
                totalsize: U32::new(totalsize as u32),
                off_dt_struct: U32::new(off_struct as u32),
                off_dt_strings: U32::new(off_strings as u32),
                off_mem_rsvmap: U32::new(off_mem_rsvmap as u32),
                version: U32::new(17),
                last_comp_version: U32::new(16),
                boot_cpuid_phys: U32::new(0),
                size_dt_strings: U32::new(size_strings as u32),
                size_dt_struct: U32::new(size_struct as u32),
            };
            buf[..FDT_HEADER_SIZE].copy_from_slice(hdr.as_bytes());
            buf[off_struct..off_struct + size_struct].copy_from_slice(&self.struct_block);
            buf[off_strings..off_strings + size_strings].copy_from_slice(&self.strings);
            buf
        }
    }

    /// Walk a DTB and collect `(node_name, prop_name, value)` pairs while
    /// asserting balanced nodes, `FDT_END`, and properties-before-children.
    fn collect_props(buf: &[u8]) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let layout = parse_header(buf).expect("valid header");
        let struct_end = layout.off_struct + layout.size_struct;
        let strings = &buf[layout.off_strings..layout.off_strings + layout.size_strings];
        let mut off = layout.off_struct;
        let mut stack: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut saw_end = false;
        let mut out = Vec::new();

        while off < struct_end {
            match read_be32(buf, off).expect("token") {
                FDT_BEGIN_NODE => {
                    if let Some((_, saw_child)) = stack.last_mut() {
                        *saw_child = true;
                    }
                    let (name, next) = read_node_name(buf, off + 4, struct_end).expect("name");
                    stack.push((name.to_vec(), false));
                    off = next;
                }
                FDT_END_NODE => {
                    stack.pop().expect("balanced end_node");
                    off += 4;
                }
                FDT_PROP => {
                    let (node, saw_child) = stack.last().expect("prop inside node");
                    assert!(!saw_child, "property appeared after child node");
                    let (len, nameoff) = read_prop_header(buf, off + 4).expect("prop hdr");
                    let next = next_after_prop(off, len, struct_end).expect("prop bounds");
                    let rest = &strings[nameoff..];
                    let nul = rest.iter().position(|&b| b == 0).expect("string nul");
                    let pname = rest[..nul].to_vec();
                    let val = buf[off + 12..off + 12 + len].to_vec();
                    out.push((node.clone(), pname, val));
                    off = next;
                }
                FDT_NOP => off += 4,
                FDT_END => {
                    saw_end = true;
                    break;
                }
                tok => panic!("unexpected token {tok:#x} at {off}"),
            }
        }
        assert!(saw_end, "missing FDT_END");
        assert!(stack.is_empty(), "unclosed nodes");
        out
    }

    fn base_dtb(
        addr_cells: Option<u32>,
        size_cells: Option<u32>,
        bootargs: Option<&str>,
    ) -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        if let Some(ac) = addr_cells {
            b.prop_u32("#address-cells", ac);
        }
        if let Some(sc) = size_cells {
            b.prop_u32("#size-cells", sc);
        }
        b.begin_node("chosen");
        if let Some(args) = bootargs {
            let mut v = args.as_bytes().to_vec();
            v.push(0);
            b.prop("bootargs", &v);
        }
        b.end_node();
        b.end_node();
        b.finish(512)
    }

    #[test]
    fn bootargs_insert_when_missing() {
        let mut dtb = base_dtb(Some(2), Some(2), None);
        let new_size = set_bootargs(&mut dtb, "console=ttyS0,115200").unwrap();
        assert!(new_size <= dtb.len());
        let props = collect_props(&dtb);
        assert!(props.iter().any(|(node, name, val)| {
            node == b"chosen" && name == b"bootargs" && val == b"console=ttyS0,115200\0"
        }));
    }

    #[test]
    fn bootargs_shrink_and_same_alignment_grow_in_place() {
        let mut dtb = base_dtb(Some(2), Some(2), Some("console=ttyS0,115200 earlycon=sbi"));
        let orig_size = parse_header(&dtb).unwrap().totalsize;
        let new_size = set_bootargs(&mut dtb, "quiet").unwrap();
        assert_eq!(new_size, orig_size);
        let props = collect_props(&dtb);
        assert!(props.iter().any(|(node, name, val)| {
            node == b"chosen" && name == b"bootargs" && val == b"quiet\0"
        }));

        // Same 4-byte aligned slot: "ab\0" (3 -> 4) grows to "abc\0" (4 -> 4) in place.
        let mut dtb2 = base_dtb(Some(2), Some(2), Some("ab"));
        let orig2 = parse_header(&dtb2).unwrap().totalsize;
        assert_eq!(set_bootargs(&mut dtb2, "abc").unwrap(), orig2);
        let props2 = collect_props(&dtb2);
        assert!(props2.iter().any(|(node, name, val)| {
            node == b"chosen" && name == b"bootargs" && val == b"abc\0"
        }));
    }

    #[test]
    fn bootargs_grow_nops_old_and_failed_grow_leaves_blob_intact() {
        let mut dtb = base_dtb(Some(2), Some(2), Some("hi"));
        let orig_size = parse_header(&dtb).unwrap().totalsize;

        // Failed grow must not NOP out the existing property.
        let mut tight = dtb[..orig_size].to_vec();
        let snapshot = tight.clone();
        assert_eq!(
            set_bootargs(&mut tight, "a much longer bootargs string"),
            Err(FdtPatchError::BufferTooSmall)
        );
        assert_eq!(tight, snapshot);

        let long = "console=ttyAMA0,115200 root=/dev/mmcblk0p2 rw";
        let new_size = set_bootargs(&mut dtb, long).unwrap();
        assert!(new_size > orig_size);
        let props = collect_props(&dtb);
        let matches: Vec<_> = props
            .iter()
            .filter(|(node, name, _)| node == b"chosen" && name == b"bootargs")
            .collect();
        assert_eq!(matches.len(), 1);
        let mut expected = long.as_bytes().to_vec();
        expected.push(0);
        assert_eq!(matches[0].2, expected);
    }

    #[test]
    fn header_and_structure_validation_rejects_malformed_blobs() {
        let mut bad = vec![0u8; 64];
        assert_eq!(set_bootargs(&mut bad, "a"), Err(FdtPatchError::BadMagic));

        // Reject v16 header (lacks size_dt_struct).
        let mut v16 = base_dtb(Some(2), Some(2), None);
        write_be32(&mut v16, 20, 16).unwrap();
        assert_eq!(
            set_bootargs(&mut v16, "a"),
            Err(FdtPatchError::StructureCorrupt)
        );

        // Reject strings-before-struct ordering.
        let mut swapped = base_dtb(Some(2), Some(2), None);
        let layout = parse_header(&swapped).unwrap();
        write_be32(&mut swapped, 12, (layout.off_struct - 4) as u32).unwrap();
        assert_eq!(
            set_bootargs(&mut swapped, "a"),
            Err(FdtPatchError::StructureCorrupt)
        );

        // Reject missing /chosen.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.end_node();
        let mut no_chosen = b.finish(256);
        assert_eq!(
            set_bootargs(&mut no_chosen, "a"),
            Err(FdtPatchError::ChosenNotFound)
        );
    }

    #[test]
    fn memory_creates_node_for_default_and_mixed_cell_sizes() {
        // Omitted #address-cells / #size-cells defaults to (2, 1) per DT spec.
        let mut dtb_def = base_dtb(None, None, None);
        set_memory(&mut dtb_def, 0x1_0000_0000, 0x4000_0000).unwrap();
        let props_def = collect_props(&dtb_def);
        let mut expected_def = Vec::new();
        expected_def.extend_from_slice(&0x1_0000_0000u64.to_be_bytes());
        expected_def.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        assert!(props_def.iter().any(|(node, name, val)| {
            node == b"memory@100000000" && name == b"reg" && val == &expected_def
        }));

        // (1, 1) rejects > 32-bit values instead of truncating.
        let mut dtb32 = base_dtb(Some(1), Some(1), None);
        assert_eq!(
            set_memory(&mut dtb32, 0x1_0000_0000, 0x2000_0000),
            Err(FdtPatchError::UnsupportedCellSize)
        );
        set_memory(&mut dtb32, 0x4000_0000, 0x2000_0000).unwrap();
        let props32 = collect_props(&dtb32);
        let mut expected32 = Vec::new();
        expected32.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        expected32.extend_from_slice(&0x2000_0000u32.to_be_bytes());
        assert!(props32.iter().any(|(node, name, val)| {
            node == b"memory@40000000" && name == b"reg" && val == &expected32
        }));

        // (1, 2) mixed cells.
        let mut dtb12 = base_dtb(Some(1), Some(2), None);
        set_memory(&mut dtb12, 0x8000_0000, 0x2_0000_0000).unwrap();
        let props12 = collect_props(&dtb12);
        let mut expected12 = Vec::new();
        expected12.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        expected12.extend_from_slice(&0x2_0000_0000u64.to_be_bytes());
        assert!(props12.iter().any(|(node, name, val)| {
            node == b"memory@80000000" && name == b"reg" && val == &expected12
        }));
    }

    #[test]
    fn memory_handles_suffix_string_interning_and_existing_node() {
        // Existing memory node references "device_type" as a suffix inside
        // "vendor,device_type\0" and has no "reg" string yet.
        let mut b = DtbBuilder::new();
        let vendor_off = b.intern(b"vendor,device_type");
        let suffix_off = vendor_off + b"vendor,".len() as u32;
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@80000000");
        b.prop_with_nameoff(suffix_off, b"memory\0");
        b.end_node();
        b.end_node();
        let mut dtb = b.finish(512);

        set_memory(&mut dtb, 0x8000_0000, 0x4000_0000).unwrap();
        let props = collect_props(&dtb);
        let mut expected = Vec::new();
        expected.extend_from_slice(&0x8000_0000u64.to_be_bytes());
        expected.extend_from_slice(&0x4000_0000u64.to_be_bytes());
        assert!(props.iter().any(|(node, name, val)| {
            node == b"memory@80000000" && name == b"reg" && val == &expected
        }));
        assert!(props.iter().any(|(node, name, val)| {
            node == b"memory@80000000" && name == b"device_type" && val == b"memory\0"
        }));
    }

    #[test]
    fn memory_failed_update_leaves_blob_intact() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory");
        b.prop("reg", &[0u8; 8]);
        b.end_node();
        b.end_node();
        let mut tight = b.finish(0);
        let snapshot = tight.clone();
        assert_eq!(
            set_memory(&mut tight, 0x8000_0000, 0x2000_0000),
            Err(FdtPatchError::BufferTooSmall)
        );
        assert_eq!(tight, snapshot);
    }

    #[test]
    fn memory_rejects_unsupported_cell_sizes() {
        let mut dtb = base_dtb(Some(3), Some(2), None);
        assert_eq!(
            set_memory(&mut dtb, 0x8000_0000, 0x1000_0000),
            Err(FdtPatchError::UnsupportedCellSize)
        );
    }

    #[test]
    fn blob_from_addr_validates_magic_and_size() {
        assert_eq!(unsafe { blob_from_addr(0) }, None);

        let dtb = base_dtb(Some(2), Some(2), Some("ok"));
        let totalsize = parse_header(&dtb).unwrap().totalsize;
        let slice = unsafe { blob_from_addr(dtb.as_ptr() as u64) }.unwrap();
        assert_eq!(slice.len(), totalsize);
        assert_eq!(slice, &dtb[..totalsize]);
    }
}
