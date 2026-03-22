//! Raw FDT blob patcher — patches properties and inserts nodes without
//! alloc or tree conversion.
//!
//! Operates directly on the FDT binary format: walks the structure block
//! to locate target nodes, then either overwrites existing properties
//! in-place or inserts new ones by shifting the tail of the blob.
//!
//! ## Supported operations
//!
//! - [`fdt_set_bootargs`] — set or replace `/chosen/bootargs`.
//! - [`fdt_set_memory`] — create or update `/memory@...` node with
//!   `device_type` and `reg` properties. Reads root `#address-cells`
//!   and `#size-cells` to encode `reg` correctly.
//!
//! ## Buffer requirements
//!
//! The DTB must reside in a writable buffer with at least 256 bytes of
//! headroom beyond `totalsize` (for property insertion + strings growth).
//! In practice this is always satisfied — the DTB is loaded to a DRAM
//! address with megabytes of free space after it.

use core::ptr;

// ---- FDT constants (big-endian on the wire) --------------------------------

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

// FDT header field byte offsets
const HDR_TOTALSIZE: usize = 4;
const HDR_OFF_DT_STRUCT: usize = 8;
const HDR_OFF_DT_STRINGS: usize = 12;
const HDR_SIZE_DT_STRINGS: usize = 32;
const HDR_SIZE_DT_STRUCT: usize = 36;

// ---- Error type ------------------------------------------------------------

/// Errors from FDT patching operations.
#[derive(Debug)]
pub enum FdtPatchError {
    /// First four bytes are not 0xD00DFEED.
    BadMagic,
    /// The working buffer is too small for the patched DTB.
    BufferTooSmall,
    /// `/chosen` node was not found in the structure block.
    ChosenNotFound,
    /// Root's `#address-cells` or `#size-cells` property is missing or
    /// has an unsupported value (only 1 and 2 are supported).
    UnsupportedCellSize,
    /// Unexpected token or premature end of the structure block.
    StructureCorrupt,
}

// ---- Helpers ---------------------------------------------------------------

/// Read a big-endian u32 from `dtb[off..off+4]`.
///
/// # Safety
///
/// `off + 4` must be within the allocated buffer.
#[inline]
unsafe fn read_be32(dtb: *const u8, off: usize) -> u32 {
    // SAFETY: caller guarantees bounds.
    let p = unsafe { dtb.add(off) };
    u32::from_be_bytes(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] })
}

/// Write a big-endian u32 to `dtb[off..off+4]`.
///
/// # Safety
///
/// `off + 4` must be within the allocated buffer, and the buffer must be
/// writable.
#[inline]
unsafe fn write_be32(dtb: *mut u8, off: usize, val: u32) {
    let bytes = val.to_be_bytes();
    // SAFETY: caller guarantees bounds + writability.
    let p = unsafe { dtb.add(off) };
    unsafe {
        *p = bytes[0];
        *p.add(1) = bytes[1];
        *p.add(2) = bytes[2];
        *p.add(3) = bytes[3];
    }
}

/// Align `off` up to the next 4-byte boundary.
#[inline]
const fn align4(off: usize) -> usize {
    (off + 3) & !3
}

/// Read a NUL-terminated node name starting at `dtb[off]`.
/// Returns the offset of the byte *after* the NUL terminator.
///
/// # Safety
///
/// The name must be within the structure block bounds.
unsafe fn skip_node_name(dtb: *const u8, off: usize) -> usize {
    let mut i = off;
    // SAFETY: structure block was validated by Fdt::from_raw.
    while unsafe { *dtb.add(i) } != 0 {
        i += 1;
    }
    i + 1 // past the NUL
}

/// Compare `len` bytes at `dtb[off]` against `needle`.
///
/// # Safety
///
/// `off + needle.len()` must be within bounds.
unsafe fn bytes_eq(dtb: *const u8, off: usize, needle: &[u8]) -> bool {
    for (i, &b) in needle.iter().enumerate() {
        // SAFETY: caller guarantees bounds.
        if unsafe { *dtb.add(off + i) } != b {
            return false;
        }
    }
    true
}

/// Search the strings block for a NUL-terminated string.
/// Returns the offset *within* the strings block (i.e. the `nameoff` value
/// for FDT_PROP).
///
/// # Safety
///
/// `str_start + str_size` must be within the buffer.
unsafe fn find_in_strings(
    dtb: *const u8,
    str_start: usize,
    str_size: usize,
    needle: &[u8], // without NUL
) -> Option<usize> {
    let mut i = 0;
    while i + needle.len() < str_size {
        // SAFETY: within strings block bounds.
        if unsafe { bytes_eq(dtb, str_start + i, needle) }
            && unsafe { *dtb.add(str_start + i + needle.len()) } == 0
        {
            return Some(i);
        }
        // Advance past current string (skip to next NUL + 1).
        while i < str_size && unsafe { *dtb.add(str_start + i) } != 0 {
            i += 1;
        }
        i += 1; // past NUL
    }
    None
}

// ---- Public API ------------------------------------------------------------

/// Set or replace `/chosen/bootargs` in a raw FDT blob.
///
/// The blob at `dtb` must be writable with at least `max_size` usable bytes
/// (i.e. the buffer backing the DTB must be at least `max_size` bytes,
/// starting at `dtb`). On success returns the new `totalsize` of the DTB.
///
/// # Safety
///
/// - `dtb` must point to a valid, writable FDT blob.
/// - The buffer must be at least `max_size` bytes.
/// - No other references to the buffer may exist during the call.
pub unsafe fn fdt_set_bootargs(
    dtb: *mut u8,
    max_size: usize,
    bootargs: &str,
) -> Result<usize, FdtPatchError> {
    // ---- Read header fields ------------------------------------------------

    // SAFETY: caller guarantees dtb points to a valid FDT with room.
    let magic = unsafe { read_be32(dtb, 0) };
    if magic != FDT_MAGIC {
        return Err(FdtPatchError::BadMagic);
    }

    let totalsize = unsafe { read_be32(dtb, HDR_TOTALSIZE) } as usize;
    let off_struct = unsafe { read_be32(dtb, HDR_OFF_DT_STRUCT) } as usize;
    let off_strings = unsafe { read_be32(dtb, HDR_OFF_DT_STRINGS) } as usize;
    let size_strings = unsafe { read_be32(dtb, HDR_SIZE_DT_STRINGS) } as usize;
    let size_struct = unsafe { read_be32(dtb, HDR_SIZE_DT_STRUCT) } as usize;

    // ---- Step 1: Find /chosen in the structure block -----------------------

    let mut off = off_struct;
    let struct_end = off_struct + size_struct;
    let mut depth: u32 = 0;
    let mut chosen_begin: Option<usize> = None;

    while off < struct_end {
        // SAFETY: off is within [off_struct .. off_struct+size_struct).
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_off = off + 4;
                // SAFETY: name is within structure block.
                let name_end = unsafe { skip_node_name(dtb, name_off) };
                // depth == 2 means a direct child of root (root = depth 1).
                if depth == 2 {
                    let name_len = name_end - 1 - name_off; // exclude NUL
                    if name_len == 6 && unsafe { bytes_eq(dtb, name_off, b"chosen") } {
                        chosen_begin = Some(off);
                    }
                }
                off = align4(name_end);
            }
            FDT_END_NODE => {
                depth = depth.saturating_sub(1);
                off += 4;
            }
            FDT_PROP => {
                // SAFETY: property header within bounds.
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                off = align4(off + 12 + prop_len);
            }
            FDT_NOP => off += 4,
            FDT_END => break,
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    let chosen_start = chosen_begin.ok_or(FdtPatchError::ChosenNotFound)?;

    // ---- Step 2: Scan /chosen for existing bootargs and insertion point -----

    // Skip chosen's FDT_BEGIN_NODE token + name.
    let mut off = chosen_start + 4;
    // SAFETY: within structure block.
    off = align4(unsafe { skip_node_name(dtb, off) });

    let mut existing_prop: Option<(usize, usize)> = None; // (offset_of_FDT_PROP, value_len)
    let mut insert_off = off; // default: right after node header
    let mut inner_depth: u32 = 0;

    loop {
        // SAFETY: within structure block.
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_PROP if inner_depth == 0 => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                let prop_nameoff = unsafe { read_be32(dtb, off + 8) } as usize;

                // Check if this property's name is "bootargs".
                let name_abs = off_strings + prop_nameoff;
                let is_bootargs = unsafe { bytes_eq(dtb, name_abs, b"bootargs\0") };

                let next = align4(off + 12 + prop_len);
                if is_bootargs {
                    existing_prop = Some((off, prop_len));
                }
                // Track position after the last top-level property — this is
                // where we'd insert a new one (before any child nodes).
                insert_off = next;
                off = next;
            }
            FDT_PROP => {
                // Property inside a child of /chosen — skip.
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                off = align4(off + 12 + prop_len);
            }
            FDT_BEGIN_NODE => {
                inner_depth += 1;
                off = align4(unsafe { skip_node_name(dtb, off + 4) });
            }
            FDT_END_NODE => {
                if inner_depth == 0 {
                    // This is /chosen's own END_NODE. If we didn't find any
                    // properties at all, insert before this token.
                    break;
                }
                inner_depth -= 1;
                off += 4;
            }
            FDT_NOP => off += 4,
            FDT_END => return Err(FdtPatchError::StructureCorrupt),
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    // ---- Step 3: In-place overwrite if existing property is large enough ----

    let new_value_len = bootargs.len() + 1; // include NUL terminator

    if let Some((prop_off, old_len)) = existing_prop {
        if new_value_len <= old_len {
            // Fits — overwrite value, update length, done.
            // SAFETY: property value region is within the structure block.
            unsafe {
                write_be32(dtb, prop_off + 4, new_value_len as u32);
                let val_start = prop_off + 12;
                ptr::copy_nonoverlapping(
                    bootargs.as_bytes().as_ptr(),
                    dtb.add(val_start),
                    bootargs.len(),
                );
                *dtb.add(val_start + bootargs.len()) = 0; // NUL
                                                          // Zero leftover bytes from old value (keeps padding clean).
                for i in new_value_len..old_len {
                    *dtb.add(val_start + i) = 0;
                }
            }
            return Ok(totalsize);
        }

        // Doesn't fit — NOP out the old property, fall through to insert.
        let old_total = align4(12 + old_len);
        let nop_words = old_total / 4;
        for i in 0..nop_words {
            // SAFETY: within structure block.
            unsafe { write_be32(dtb, prop_off + i * 4, FDT_NOP) };
        }
        // insert_off is already set past this property's position.
    }

    // ---- Step 4: Ensure "bootargs" is in the strings block -----------------

    // SAFETY: strings block is within the DTB.
    let bootargs_nameoff = unsafe { find_in_strings(dtb, off_strings, size_strings, b"bootargs") };

    // We'll add the string after the struct shift (if needed), because the
    // shift moves the strings block forward and we can append to it then.
    // For now just determine the nameoff we'll write into the FDT_PROP.
    let (nameoff, string_add_len) = match bootargs_nameoff {
        Some(n) => (n, 0usize),
        None => (size_strings, b"bootargs\0".len()), // will append after shift
    };

    // ---- Step 5: Insert new property at insert_off -------------------------

    // New property entry: FDT_PROP(4) + len(4) + nameoff(4) + value + padding.
    let new_prop_total = align4(12 + new_value_len);

    let new_totalsize = totalsize + new_prop_total + string_add_len;
    if new_totalsize > max_size {
        return Err(FdtPatchError::BufferTooSmall);
    }

    // Shift everything from insert_off to end of DTB forward by new_prop_total.
    // The strings block (which follows the structure block) moves with it.
    // Use ptr::copy for overlapping regions (memmove semantics).
    let tail_len = totalsize - insert_off;
    // SAFETY: both regions are within [dtb .. dtb+max_size).
    unsafe {
        ptr::copy(
            dtb.add(insert_off),
            dtb.add(insert_off + new_prop_total),
            tail_len,
        );
    }

    // Write the new FDT_PROP entry.
    // SAFETY: insert region was just cleared by the shift.
    unsafe {
        write_be32(dtb, insert_off, FDT_PROP);
        write_be32(dtb, insert_off + 4, new_value_len as u32);
        write_be32(dtb, insert_off + 8, nameoff as u32);
        ptr::copy_nonoverlapping(
            bootargs.as_bytes().as_ptr(),
            dtb.add(insert_off + 12),
            bootargs.len(),
        );
        *dtb.add(insert_off + 12 + bootargs.len()) = 0; // NUL
                                                        // Zero alignment padding.
        let pad_start = insert_off + 12 + new_value_len;
        let pad_end = insert_off + new_prop_total;
        for i in pad_start..pad_end {
            *dtb.add(i) = 0;
        }
    }

    // ---- Step 6: Append "bootargs\0" to strings block if needed ------------

    if string_add_len > 0 {
        // Strings block has moved forward by new_prop_total bytes.
        let new_off_strings = off_strings + new_prop_total;
        let str_dest = new_off_strings + size_strings;
        // SAFETY: within [dtb .. dtb+max_size).
        unsafe {
            ptr::copy_nonoverlapping(b"bootargs\0".as_ptr(), dtb.add(str_dest), string_add_len);
        }
    }

    // ---- Step 7: Update header fields --------------------------------------

    // SAFETY: header is at dtb[0..40].
    unsafe {
        write_be32(dtb, HDR_TOTALSIZE, new_totalsize as u32);
        write_be32(
            dtb,
            HDR_SIZE_DT_STRUCT,
            (size_struct + new_prop_total) as u32,
        );
        write_be32(
            dtb,
            HDR_OFF_DT_STRINGS,
            (off_strings + new_prop_total) as u32,
        );
        write_be32(
            dtb,
            HDR_SIZE_DT_STRINGS,
            (size_strings + string_add_len) as u32,
        );
    }

    Ok(new_totalsize)
}

// ---- Initrd properties in /chosen ------------------------------------------

/// Set `linux,initrd-start` and `linux,initrd-end` in `/chosen`.
///
/// These properties tell the Linux kernel where the initramfs resides
/// in memory. Both are encoded as 64-bit (8-byte) big-endian values
/// regardless of the root cell sizes — Linux always reads them as u64
/// from the `/chosen` node.
///
/// On success returns the new `totalsize` of the DTB.
///
/// # Safety
///
/// - `dtb` must point to a valid, writable FDT blob.
/// - The buffer must be at least `max_size` bytes.
/// - No other references to the buffer may exist during the call.
pub unsafe fn fdt_set_initrd(
    dtb: *mut u8,
    max_size: usize,
    initrd_start: u64,
    initrd_end: u64,
) -> Result<usize, FdtPatchError> {
    // SAFETY: caller guarantees dtb points to a valid FDT.
    let magic = unsafe { read_be32(dtb, 0) };
    if magic != FDT_MAGIC {
        return Err(FdtPatchError::BadMagic);
    }

    let totalsize = unsafe { read_be32(dtb, HDR_TOTALSIZE) } as usize;
    let off_struct = unsafe { read_be32(dtb, HDR_OFF_DT_STRUCT) } as usize;
    let off_strings = unsafe { read_be32(dtb, HDR_OFF_DT_STRINGS) } as usize;
    let size_strings = unsafe { read_be32(dtb, HDR_SIZE_DT_STRINGS) } as usize;
    let size_struct = unsafe { read_be32(dtb, HDR_SIZE_DT_STRUCT) } as usize;

    // ---- Find /chosen node -------------------------------------------------

    let mut off = off_struct;
    let struct_end = off_struct + size_struct;
    let mut depth: u32 = 0;
    let mut chosen_begin: Option<usize> = None;

    while off < struct_end {
        // SAFETY: off is within structure block.
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_off = off + 4;
                // SAFETY: name is within structure block.
                let name_end = unsafe { skip_node_name(dtb, name_off) };
                if depth == 2 {
                    let name_len = name_end - 1 - name_off;
                    if name_len == 6 && unsafe { bytes_eq(dtb, name_off, b"chosen") } {
                        chosen_begin = Some(off);
                    }
                }
                off = align4(name_end);
            }
            FDT_END_NODE => {
                depth = depth.saturating_sub(1);
                off += 4;
            }
            FDT_PROP => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                off = align4(off + 12 + prop_len);
            }
            FDT_NOP => off += 4,
            FDT_END => break,
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    let chosen_start = chosen_begin.ok_or(FdtPatchError::ChosenNotFound)?;

    // ---- Find insertion point and existing properties -----------------------

    let mut off = chosen_start + 4;
    // SAFETY: within structure block.
    off = align4(unsafe { skip_node_name(dtb, off) });

    let mut existing_start: Option<(usize, usize)> = None; // (prop_off, val_len)
    let mut existing_end: Option<(usize, usize)> = None;
    let mut insert_off = off;
    let mut inner_depth: u32 = 0;

    loop {
        // SAFETY: within structure block.
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_PROP if inner_depth == 0 => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                let prop_nameoff = unsafe { read_be32(dtb, off + 8) } as usize;
                let name_abs = off_strings + prop_nameoff;

                if unsafe { bytes_eq(dtb, name_abs, b"linux,initrd-start\0") } {
                    existing_start = Some((off, prop_len));
                } else if unsafe { bytes_eq(dtb, name_abs, b"linux,initrd-end\0") } {
                    existing_end = Some((off, prop_len));
                }

                let next = align4(off + 12 + prop_len);
                insert_off = next;
                off = next;
            }
            FDT_PROP => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                off = align4(off + 12 + prop_len);
            }
            FDT_BEGIN_NODE => {
                inner_depth += 1;
                off = align4(unsafe { skip_node_name(dtb, off + 4) });
            }
            FDT_END_NODE => {
                if inner_depth == 0 {
                    break;
                }
                inner_depth -= 1;
                off += 4;
            }
            FDT_NOP => off += 4,
            FDT_END => return Err(FdtPatchError::StructureCorrupt),
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    // ---- Overwrite existing properties if they fit (8 bytes each) -----------

    // Both linux,initrd-start and linux,initrd-end are 8-byte (u64) values.
    let val_len = 8usize;

    if let Some((prop_off, old_len)) = existing_start {
        if old_len >= val_len {
            // SAFETY: property value region is within structure block.
            unsafe {
                write_be32(dtb, prop_off + 4, val_len as u32);
                write_be32(dtb, prop_off + 12, (initrd_start >> 32) as u32);
                write_be32(dtb, prop_off + 16, initrd_start as u32);
            }
        } else {
            // NOP out old, will insert below.
            let old_total = align4(12 + old_len);
            for i in 0..old_total / 4 {
                unsafe { write_be32(dtb, prop_off + i * 4, FDT_NOP) };
            }
            existing_start = None;
        }
    }

    if let Some((prop_off, old_len)) = existing_end {
        if old_len >= val_len {
            unsafe {
                write_be32(dtb, prop_off + 4, val_len as u32);
                write_be32(dtb, prop_off + 12, (initrd_end >> 32) as u32);
                write_be32(dtb, prop_off + 16, initrd_end as u32);
            }
        } else {
            let old_total = align4(12 + old_len);
            for i in 0..old_total / 4 {
                unsafe { write_be32(dtb, prop_off + i * 4, FDT_NOP) };
            }
            existing_end = None;
        }
    }

    if existing_start.is_some() && existing_end.is_some() {
        // Both overwritten in place, done.
        return Ok(totalsize);
    }

    // ---- Insert missing properties -----------------------------------------

    // Ensure property name strings exist.
    let start_nameoff =
        unsafe { find_in_strings(dtb, off_strings, size_strings, b"linux,initrd-start") };
    let end_nameoff =
        unsafe { find_in_strings(dtb, off_strings, size_strings, b"linux,initrd-end") };

    let mut new_strings_len = 0usize;
    let start_nameoff_val = match start_nameoff {
        Some(n) => n,
        None => {
            let n = size_strings + new_strings_len;
            new_strings_len += b"linux,initrd-start\0".len();
            n
        }
    };
    let end_nameoff_val = match end_nameoff {
        Some(n) => n,
        None => {
            let n = size_strings + new_strings_len;
            new_strings_len += b"linux,initrd-end\0".len();
            n
        }
    };

    // Build properties to insert.
    // Each property: FDT_PROP(4) + len(4) + nameoff(4) + value(8) = 20 bytes.
    let prop_total_each = align4(12 + val_len); // 20 bytes, already aligned
    let mut props_to_insert = 0usize;
    let mut prop_buf = [0u8; 48]; // room for 2 properties
    let mut p = 0usize;

    if existing_start.is_none() {
        unsafe {
            write_be32(prop_buf.as_mut_ptr(), p, FDT_PROP);
            write_be32(prop_buf.as_mut_ptr(), p + 4, val_len as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 8, start_nameoff_val as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 12, (initrd_start >> 32) as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 16, initrd_start as u32);
        }
        p += prop_total_each;
        props_to_insert += 1;
    }

    if existing_end.is_none() {
        unsafe {
            write_be32(prop_buf.as_mut_ptr(), p, FDT_PROP);
            write_be32(prop_buf.as_mut_ptr(), p + 4, val_len as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 8, end_nameoff_val as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 12, (initrd_end >> 32) as u32);
            write_be32(prop_buf.as_mut_ptr(), p + 16, initrd_end as u32);
        }
        p += prop_total_each;
        props_to_insert += 1;
    }

    let insert_total = p;
    if insert_total == 0 {
        return Ok(totalsize);
    }

    let new_totalsize = totalsize + insert_total + new_strings_len;
    if new_totalsize > max_size {
        return Err(FdtPatchError::BufferTooSmall);
    }

    // Shift tail.
    let tail_len = totalsize - insert_off;
    unsafe {
        ptr::copy(
            dtb.add(insert_off),
            dtb.add(insert_off + insert_total),
            tail_len,
        );
        ptr::copy_nonoverlapping(prop_buf.as_ptr(), dtb.add(insert_off), insert_total);
    }

    // Append new strings.
    if new_strings_len > 0 {
        let new_off_strings = off_strings + insert_total;
        let mut str_dest = new_off_strings + size_strings;

        if start_nameoff.is_none() {
            let s = b"linux,initrd-start\0";
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
            str_dest += s.len();
        }
        if end_nameoff.is_none() {
            let s = b"linux,initrd-end\0";
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
        }
    }

    // Update header.
    unsafe {
        write_be32(dtb, HDR_TOTALSIZE, new_totalsize as u32);
        write_be32(dtb, HDR_SIZE_DT_STRUCT, (size_struct + insert_total) as u32);
        write_be32(dtb, HDR_OFF_DT_STRINGS, (off_strings + insert_total) as u32);
        write_be32(
            dtb,
            HDR_SIZE_DT_STRINGS,
            (size_strings + new_strings_len) as u32,
        );
    }

    Ok(new_totalsize)
}

// ---- Memory node -----------------------------------------------------------

/// Maximum size of the node blob we build on the stack.
///
/// Layout (worst case, 64-bit cells):
///   FDT_BEGIN_NODE(4) + "memory@XXXXXXXX\0"(20 aligned) +
///   FDT_PROP(12) + "memory\0"(8 aligned) +  // device_type
///   FDT_PROP(12) + reg_value(16) +           // reg (#addr-cells=2, #size-cells=2)
///   FDT_END_NODE(4)
/// Total: 76 bytes. 128 gives generous headroom.
const MEMORY_NODE_MAX: usize = 128;

/// Write a value as 1 or 2 big-endian u32 cells into `buf[off..]`.
/// Returns the number of bytes written (4 or 8).
///
/// # Safety
///
/// `off + cells * 4` must be within `buf`.
unsafe fn write_cells(buf: *mut u8, off: usize, val: u64, cells: u32) -> usize {
    if cells == 2 {
        // SAFETY: caller guarantees bounds.
        unsafe {
            write_be32(buf, off, (val >> 32) as u32);
            write_be32(buf, off + 4, val as u32);
        }
        8
    } else {
        // cells == 1
        // SAFETY: caller guarantees bounds.
        unsafe { write_be32(buf, off, val as u32) };
        4
    }
}

/// Create or update the `/memory@...` node in a raw FDT blob.
///
/// If a depth-1 node whose name starts with `memory` already exists,
/// its `reg` property is overwritten (or inserted). If no such node
/// exists, a new `/memory@<base_hex>` node is created just before the
/// root's `FDT_END_NODE`.
///
/// The function reads the root node's `#address-cells` and `#size-cells`
/// properties to encode `reg` correctly (supports values 1 and 2).
///
/// On success returns the new `totalsize` of the DTB.
///
/// # Arguments
///
/// - `dtb` — pointer to a valid, writable FDT blob.
/// - `max_size` — total writable buffer size starting at `dtb`.
/// - `base` — physical base address of the memory region.
/// - `size` — size of the memory region in bytes.
///
/// # Safety
///
/// - `dtb` must point to a valid, writable FDT blob.
/// - The buffer must be at least `max_size` bytes.
/// - No other references to the buffer may exist during the call.
pub unsafe fn fdt_set_memory(
    dtb: *mut u8,
    max_size: usize,
    base: u64,
    size: u64,
) -> Result<usize, FdtPatchError> {
    // ---- Read header fields ------------------------------------------------

    // SAFETY: caller guarantees dtb points to a valid FDT.
    let magic = unsafe { read_be32(dtb, 0) };
    if magic != FDT_MAGIC {
        return Err(FdtPatchError::BadMagic);
    }

    let totalsize = unsafe { read_be32(dtb, HDR_TOTALSIZE) } as usize;
    let off_struct = unsafe { read_be32(dtb, HDR_OFF_DT_STRUCT) } as usize;
    let off_strings = unsafe { read_be32(dtb, HDR_OFF_DT_STRINGS) } as usize;
    let size_strings = unsafe { read_be32(dtb, HDR_SIZE_DT_STRINGS) } as usize;
    let size_struct = unsafe { read_be32(dtb, HDR_SIZE_DT_STRUCT) } as usize;

    // ---- Step 1: Walk structure block to find root's cell sizes and
    //              any existing /memory* node ---------------------------------

    let mut off = off_struct;
    let struct_end = off_struct + size_struct;
    let mut depth: u32 = 0;
    let mut addr_cells: u32 = 1; // default per DT spec
    let mut size_cells: u32 = 1; // default per DT spec
    let mut memory_node_begin: Option<usize> = None;
    let mut root_end_node: Option<usize> = None; // offset of root's FDT_END_NODE

    while off < struct_end {
        // SAFETY: off is within structure block.
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_off = off + 4;
                // SAFETY: name is within structure block.
                let name_end = unsafe { skip_node_name(dtb, name_off) };
                let name_len = name_end - 1 - name_off; // exclude NUL

                // depth == 2: direct child of root
                if depth == 2 {
                    // Match "memory" or "memory@..." at depth 1.
                    let is_memory = if name_len == 6 {
                        unsafe { bytes_eq(dtb, name_off, b"memory") }
                    } else if name_len > 7 {
                        unsafe { bytes_eq(dtb, name_off, b"memory@") }
                    } else {
                        false
                    };
                    if is_memory && memory_node_begin.is_none() {
                        memory_node_begin = Some(off);
                    }
                }
                off = align4(name_end);
            }
            FDT_END_NODE => {
                if depth == 1 {
                    // This is root's FDT_END_NODE.
                    root_end_node = Some(off);
                }
                depth = depth.saturating_sub(1);
                off += 4;
            }
            FDT_PROP => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                let prop_nameoff = unsafe { read_be32(dtb, off + 8) } as usize;

                // Read root-level #address-cells / #size-cells (depth == 1).
                if depth == 1 && prop_len == 4 {
                    let name_abs = off_strings + prop_nameoff;
                    if unsafe { bytes_eq(dtb, name_abs, b"#address-cells\0") } {
                        addr_cells = unsafe { read_be32(dtb, off + 12) };
                    } else if unsafe { bytes_eq(dtb, name_abs, b"#size-cells\0") } {
                        size_cells = unsafe { read_be32(dtb, off + 12) };
                    }
                }
                off = align4(off + 12 + prop_len);
            }
            FDT_NOP => off += 4,
            FDT_END => break,
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    // Validate cell sizes — we only support 1 or 2.
    if !(addr_cells == 1 || addr_cells == 2) || !(size_cells == 1 || size_cells == 2) {
        return Err(FdtPatchError::UnsupportedCellSize);
    }

    let reg_value_len = (addr_cells + size_cells) as usize * 4; // 8 or 12 or 16 bytes

    // ---- Step 2: Ensure required strings exist in strings block -------------

    // We need "device_type" and "reg" in the strings block.
    // SAFETY: strings block is within the DTB.
    let dt_nameoff = unsafe { find_in_strings(dtb, off_strings, size_strings, b"device_type") };
    let reg_nameoff = unsafe { find_in_strings(dtb, off_strings, size_strings, b"reg") };

    // Compute string additions (appended after any struct shift).
    let mut new_strings_len = 0usize;
    let dt_nameoff_val = match dt_nameoff {
        Some(n) => n,
        None => {
            let n = size_strings + new_strings_len;
            new_strings_len += b"device_type\0".len();
            n
        }
    };
    let reg_nameoff_val = match reg_nameoff {
        Some(n) => n,
        None => {
            let n = size_strings + new_strings_len;
            new_strings_len += b"reg\0".len();
            n
        }
    };

    // ---- Step 3: Handle existing node vs new node --------------------------

    if let Some(mem_start) = memory_node_begin {
        // Existing /memory* node found — set the `reg` property inside it.
        return unsafe {
            set_reg_in_existing_memory_node(
                dtb,
                max_size,
                totalsize,
                off_struct,
                off_strings,
                size_strings,
                size_struct,
                mem_start,
                base,
                size,
                addr_cells,
                size_cells,
                reg_value_len,
                reg_nameoff_val,
                dt_nameoff_val,
                new_strings_len,
            )
        };
    }

    // ---- Step 4: No existing memory node — create a new one ----------------

    // Build the new node blob on the stack.
    let mut node_buf = [0u8; MEMORY_NODE_MAX];
    let mut p = 0usize;

    // Node name: "memory@XXXXXXXX" where XXXXXXXX is the hex base address.
    // For base 0x40000000: "memory@40000000\0" = 16 bytes + NUL = 17 bytes.
    // FDT_BEGIN_NODE
    unsafe { write_be32(node_buf.as_mut_ptr(), p, FDT_BEGIN_NODE) };
    p += 4;

    // Write "memory@" prefix
    let prefix = b"memory@";
    node_buf[p..p + prefix.len()].copy_from_slice(prefix);
    p += prefix.len();
    // Write hex digits of base address (skip leading zeros, minimum 1 digit)
    p += write_hex_to_buf(&mut node_buf[p..], base);
    node_buf[p] = 0; // NUL terminator
    p += 1;
    p = align4(p);

    // device_type = "memory\0" (7 bytes, padded to 8)
    let dt_value = b"memory\0";
    let dt_value_len = dt_value.len();
    unsafe { write_be32(node_buf.as_mut_ptr(), p, FDT_PROP) };
    p += 4;
    unsafe { write_be32(node_buf.as_mut_ptr(), p, dt_value_len as u32) };
    p += 4;
    unsafe { write_be32(node_buf.as_mut_ptr(), p, dt_nameoff_val as u32) };
    p += 4;
    node_buf[p..p + dt_value_len].copy_from_slice(dt_value);
    p += dt_value_len;
    // Pad to 4-byte alignment
    while p % 4 != 0 {
        node_buf[p] = 0;
        p += 1;
    }

    // reg = <base size> encoded per #address-cells / #size-cells
    unsafe { write_be32(node_buf.as_mut_ptr(), p, FDT_PROP) };
    p += 4;
    unsafe { write_be32(node_buf.as_mut_ptr(), p, reg_value_len as u32) };
    p += 4;
    unsafe { write_be32(node_buf.as_mut_ptr(), p, reg_nameoff_val as u32) };
    p += 4;
    unsafe { p += write_cells(node_buf.as_mut_ptr(), p, base, addr_cells) };
    unsafe { p += write_cells(node_buf.as_mut_ptr(), p, size, size_cells) };
    // reg_value_len is already 4-byte aligned (4*N cells), no padding needed.

    // FDT_END_NODE
    unsafe { write_be32(node_buf.as_mut_ptr(), p, FDT_END_NODE) };
    p += 4;

    let node_total = p; // total bytes of the new node

    // Insert at root's FDT_END_NODE position.
    let insert_off = root_end_node.ok_or(FdtPatchError::StructureCorrupt)?;

    let new_totalsize = totalsize + node_total + new_strings_len;
    if new_totalsize > max_size {
        return Err(FdtPatchError::BufferTooSmall);
    }

    // Shift everything from insert_off to end of DTB forward by node_total.
    let tail_len = totalsize - insert_off;
    // SAFETY: both regions are within [dtb .. dtb+max_size).
    unsafe {
        ptr::copy(
            dtb.add(insert_off),
            dtb.add(insert_off + node_total),
            tail_len,
        );
    }

    // Copy the node blob into the gap.
    // SAFETY: the gap at [insert_off .. insert_off + node_total] is writable.
    unsafe {
        ptr::copy_nonoverlapping(node_buf.as_ptr(), dtb.add(insert_off), node_total);
    }

    // ---- Step 5: Append new strings if needed ------------------------------

    if new_strings_len > 0 {
        let new_off_strings = off_strings + node_total;
        let mut str_dest = new_off_strings + size_strings;

        if dt_nameoff.is_none() {
            let s = b"device_type\0";
            // SAFETY: within [dtb .. dtb+max_size).
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
            str_dest += s.len();
        }
        if reg_nameoff.is_none() {
            let s = b"reg\0";
            // SAFETY: within [dtb .. dtb+max_size).
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
        }
    }

    // ---- Step 6: Update header fields --------------------------------------

    // SAFETY: header is at dtb[0..40].
    unsafe {
        write_be32(dtb, HDR_TOTALSIZE, new_totalsize as u32);
        write_be32(dtb, HDR_SIZE_DT_STRUCT, (size_struct + node_total) as u32);
        write_be32(dtb, HDR_OFF_DT_STRINGS, (off_strings + node_total) as u32);
        write_be32(
            dtb,
            HDR_SIZE_DT_STRINGS,
            (size_strings + new_strings_len) as u32,
        );
    }

    Ok(new_totalsize)
}

/// Write a u64 value as lowercase hex digits (without "0x" prefix) into `buf`.
/// Leading zeros are skipped (minimum 1 digit). Returns the number of bytes written.
fn write_hex_to_buf(buf: &mut [u8], val: u64) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }

    // Count the number of hex digits needed (1-16).
    let digits = ((64 - val.leading_zeros() + 3) / 4) as usize;

    for i in 0..digits {
        let shift = (digits - 1 - i) * 4;
        let nibble = ((val >> shift) & 0xF) as u8;
        buf[i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
    }
    digits
}

/// Set the `reg` property inside an existing `/memory*` node.
///
/// Finds the existing `reg` property and overwrites it in-place if it
/// fits, or inserts a new one. Also ensures `device_type = "memory"`
/// is present.
///
/// # Safety
///
/// Same requirements as [`fdt_set_memory`].
#[allow(clippy::too_many_arguments)]
unsafe fn set_reg_in_existing_memory_node(
    dtb: *mut u8,
    max_size: usize,
    totalsize: usize,
    _off_struct: usize,
    off_strings: usize,
    size_strings: usize,
    size_struct: usize,
    mem_node_start: usize,
    base: u64,
    size: u64,
    addr_cells: u32,
    size_cells: u32,
    reg_value_len: usize,
    reg_nameoff: usize,
    dt_nameoff: usize,
    new_strings_len: usize,
) -> Result<usize, FdtPatchError> {
    // Skip the node's FDT_BEGIN_NODE + name.
    let mut off = mem_node_start + 4;
    // SAFETY: within structure block.
    off = align4(unsafe { skip_node_name(dtb, off) });

    let mut existing_reg: Option<(usize, usize)> = None; // (prop_off, value_len)
    let mut has_device_type = false;
    let mut insert_off = off; // after node header, before first child node
    let mut inner_depth: u32 = 0;

    loop {
        // SAFETY: within structure block.
        let token = unsafe { read_be32(dtb, off) };
        match token {
            FDT_PROP if inner_depth == 0 => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                let prop_nameoff = unsafe { read_be32(dtb, off + 8) } as usize;

                let name_abs = off_strings + prop_nameoff;
                if unsafe { bytes_eq(dtb, name_abs, b"reg\0") } {
                    existing_reg = Some((off, prop_len));
                } else if unsafe { bytes_eq(dtb, name_abs, b"device_type\0") } {
                    has_device_type = true;
                }

                let next = align4(off + 12 + prop_len);
                insert_off = next;
                off = next;
            }
            FDT_PROP => {
                let prop_len = unsafe { read_be32(dtb, off + 4) } as usize;
                off = align4(off + 12 + prop_len);
            }
            FDT_BEGIN_NODE => {
                inner_depth += 1;
                off = align4(unsafe { skip_node_name(dtb, off + 4) });
            }
            FDT_END_NODE => {
                if inner_depth == 0 {
                    break;
                }
                inner_depth -= 1;
                off += 4;
            }
            FDT_NOP => off += 4,
            FDT_END => return Err(FdtPatchError::StructureCorrupt),
            _ => return Err(FdtPatchError::StructureCorrupt),
        }
    }

    // --- Overwrite existing reg if it fits, else NOP + insert ----------------

    let mut current_totalsize = totalsize;
    let mut current_off_strings = off_strings;
    let mut current_size_strings = size_strings;
    let mut current_size_struct = size_struct;

    if let Some((prop_off, old_len)) = existing_reg {
        if reg_value_len <= old_len {
            // Fits — overwrite in place.
            unsafe {
                write_be32(dtb, prop_off + 4, reg_value_len as u32);
                let val_start = prop_off + 12;
                let mut p = val_start;
                p += write_cells(dtb, p, base, addr_cells);
                p += write_cells(dtb, p, size, size_cells);
                // Zero leftover
                while p < val_start + old_len {
                    *dtb.add(p) = 0;
                    p += 1;
                }
            }
            // Still need to add device_type if missing (handle below).
            if has_device_type {
                return Ok(totalsize);
            }
            // Fall through to add device_type if needed.
        } else {
            // NOP out old reg, fall through to insert.
            let old_total = align4(12 + old_len);
            let nop_words = old_total / 4;
            for i in 0..nop_words {
                unsafe { write_be32(dtb, prop_off + i * 4, FDT_NOP) };
            }
        }
    }

    // --- Insert missing properties ------------------------------------------

    // Build what we need to insert: optionally device_type, optionally reg.
    let need_reg =
        existing_reg.is_none() || existing_reg.is_some_and(|(_, old)| reg_value_len > old);
    let need_dt = !has_device_type;

    let mut prop_buf = [0u8; 64]; // enough for device_type + reg properties
    let mut p = 0usize;

    if need_dt {
        let dt_value = b"memory\0";
        let dt_value_len = dt_value.len();
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, FDT_PROP) };
        p += 4;
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, dt_value_len as u32) };
        p += 4;
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, dt_nameoff as u32) };
        p += 4;
        prop_buf[p..p + dt_value_len].copy_from_slice(dt_value);
        p += dt_value_len;
        while p % 4 != 0 {
            prop_buf[p] = 0;
            p += 1;
        }
    }

    if need_reg {
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, FDT_PROP) };
        p += 4;
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, reg_value_len as u32) };
        p += 4;
        unsafe { write_be32(prop_buf.as_mut_ptr(), p, reg_nameoff as u32) };
        p += 4;
        unsafe { p += write_cells(prop_buf.as_mut_ptr(), p, base, addr_cells) };
        unsafe { p += write_cells(prop_buf.as_mut_ptr(), p, size, size_cells) };
    }

    let props_total = p;

    if props_total == 0 {
        // Nothing to insert (reg was overwritten in place, device_type exists).
        return Ok(current_totalsize);
    }

    let new_totalsize = current_totalsize + props_total + new_strings_len;
    if new_totalsize > max_size {
        return Err(FdtPatchError::BufferTooSmall);
    }

    // Shift tail forward by props_total.
    let tail_len = current_totalsize - insert_off;
    unsafe {
        ptr::copy(
            dtb.add(insert_off),
            dtb.add(insert_off + props_total),
            tail_len,
        );
    }

    // Copy property blob into the gap.
    unsafe {
        ptr::copy_nonoverlapping(prop_buf.as_ptr(), dtb.add(insert_off), props_total);
    }

    current_totalsize = new_totalsize;
    current_size_struct += props_total;
    current_off_strings += props_total;

    // Append new strings if needed.
    if new_strings_len > 0 {
        let mut str_dest = current_off_strings + current_size_strings;

        // Check if device_type string needs appending (nameoff >= original size_strings).
        if dt_nameoff >= size_strings {
            let s = b"device_type\0";
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
            str_dest += s.len();
        }
        if reg_nameoff >= size_strings {
            let s = b"reg\0";
            unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dtb.add(str_dest), s.len()) };
        }
        current_size_strings += new_strings_len;
    }

    // Update header.
    unsafe {
        write_be32(dtb, HDR_TOTALSIZE, current_totalsize as u32);
        write_be32(dtb, HDR_SIZE_DT_STRUCT, current_size_struct as u32);
        write_be32(dtb, HDR_OFF_DT_STRINGS, current_off_strings as u32);
        write_be32(dtb, HDR_SIZE_DT_STRINGS, current_size_strings as u32);
    }

    Ok(current_totalsize)
}
